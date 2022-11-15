//! Interface to RF hardware through SoapySDR (using the [`soapysdr`] crate)

use crate::bufferpool::*;
use crate::flow::*;
use crate::impl_block_trait;
use crate::numbers::*;
use crate::signal::*;

use tokio::runtime;
use tokio::sync::{watch, Mutex};
use tokio::task::spawn_blocking;

use std::mem::take;
use std::thread::{sleep, JoinHandle};
use std::time::{Duration, Instant};

struct SoapySdrRxRetval {
    rx_stream: soapysdr::RxStream<Complex<f32>>,
    result: Result<(), soapysdr::Error>,
}

struct SoapySdrRxActive {
    abort: watch::Sender<()>,
    join_handle: JoinHandle<SoapySdrRxRetval>,
}

enum SoapySdrRxState {
    Active(SoapySdrRxActive),
    Idle(soapysdr::RxStream<Complex<f32>>),
    Invalid,
}

impl Default for SoapySdrRxState {
    fn default() -> Self {
        SoapySdrRxState::Invalid
    }
}

/// Block which wraps an [`::soapysdr::RxStream`] and acts as a
/// [`Producer<Signal<Complex<Flt>>>`]
pub struct SoapySdrRx {
    sender: Sender<Signal<Complex<f32>>>,
    sender_connector: SenderConnector<Signal<Complex<f32>>>,
    sample_rate: f64,
    state: Mutex<SoapySdrRxState>,
}

impl_block_trait! { Producer<Signal<Complex<f32>>> for SoapySdrRx }

impl SoapySdrRx {
    /// Create new [`SoapySdrRx`] block
    ///
    /// The passed `rx_stream` should not have been activated at this point.
    /// Instead, the stream must be activated by invoking
    /// [`SoapySdrRx::activate`].
    pub fn new(rx_stream: soapysdr::RxStream<Complex<f32>>, sample_rate: f64) -> Self {
        let (sender, sender_connector) = new_sender::<Signal<Complex<f32>>>();
        let state = Mutex::new(SoapySdrRxState::Idle(rx_stream));
        Self {
            sender,
            sender_connector,
            sample_rate,
            state,
        }
    }
    /// Activate streaming
    pub async fn activate(&mut self) -> Result<(), soapysdr::Error> {
        let mut state_guard = self.state.lock().await;
        match take(&mut *state_guard) {
            SoapySdrRxState::Invalid => panic!("invalid state in SoapySdrRx"),
            SoapySdrRxState::Active(x) => {
                *state_guard = SoapySdrRxState::Active(x);
                Ok(())
            }
            SoapySdrRxState::Idle(mut rx_stream) => {
                let (mut rx_stream, mtu) = match spawn_blocking(move || {
                    let mtu = match rx_stream.mtu() {
                        Ok(x) => x,
                        Err(err) => return Err((rx_stream, err)),
                    };
                    match rx_stream.activate(None) {
                        Ok(x) => x,
                        Err(err) => return Err((rx_stream, err)),
                    };
                    Ok((rx_stream, mtu))
                })
                .await
                .unwrap()
                {
                    Ok(x) => x,
                    Err((rx_stream, err)) => {
                        *state_guard = SoapySdrRxState::Idle(rx_stream);
                        return Err(err);
                    }
                };
                let sample_rate = self.sample_rate;
                let sender = self.sender.clone();
                let (abort_send, abort_recv) = watch::channel::<()>(());
                let rt = runtime::Handle::current();
                let join_handle = std::thread::spawn(move || {
                    let mut buf_pool = ChunkBufPool::<Complex<f32>>::new();
                    let mut result = Ok(());
                    while !abort_recv.has_changed().unwrap_or(true) {
                        let mut buffer = buf_pool.get();
                        buffer.resize_with(mtu, Default::default);
                        let count = match rx_stream.read(&[&mut buffer], 1000000) {
                            Ok(x) => x,
                            Err(err) => {
                                result = Err(err);
                                break;
                            }
                        };
                        buffer.truncate(count);
                        let Ok(()) = rt.block_on(sender.send(Signal::Samples {
                            sample_rate,
                            chunk: buffer.finalize(),
                        }))
                        else { break; };
                    }
                    if let Err(err) = rx_stream.deactivate(None) {
                        if result.is_ok() {
                            result = Err(err);
                        }
                    }
                    SoapySdrRxRetval { rx_stream, result }
                });
                *state_guard = SoapySdrRxState::Active(SoapySdrRxActive {
                    abort: abort_send,
                    join_handle,
                });
                Ok(())
            }
        }
    }
    /// Deactivate (pause) streaming
    pub async fn deactivate(&mut self) -> Result<(), soapysdr::Error> {
        let mut state_guard = self.state.lock().await;
        match take(&mut *state_guard) {
            SoapySdrRxState::Invalid => panic!("invalid state in SoapySdrRx"),
            SoapySdrRxState::Idle(x) => {
                *state_guard = SoapySdrRxState::Idle(x);
                Ok(())
            }
            SoapySdrRxState::Active(SoapySdrRxActive { abort, join_handle }) => {
                drop(abort);
                let retval = runtime::Handle::current()
                    .spawn_blocking(move || join_handle.join().unwrap())
                    .await
                    .unwrap();
                *state_guard = SoapySdrRxState::Idle(retval.rx_stream);
                retval.result
            }
        }
    }
}

struct SoapySdrTxRetval {
    tx_stream: soapysdr::TxStream<Complex<f32>>,
    result: Result<(), soapysdr::Error>,
}

struct SoapySdrTxActive {
    abort: watch::Sender<()>,
    join_handle: JoinHandle<SoapySdrTxRetval>,
}

enum SoapySdrTxState {
    Active(SoapySdrTxActive),
    Idle(soapysdr::TxStream<Complex<f32>>),
    Invalid,
}

impl Default for SoapySdrTxState {
    fn default() -> Self {
        SoapySdrTxState::Invalid
    }
}

/// Block which wraps an [`::soapysdr::TxStream`] and acts as a
/// [`Producer<Signal<Complex<Flt>>>`]
pub struct SoapySdrTx {
    receiver_connector: ReceiverConnector<Signal<Complex<f32>>>,
    event_handlers: EventHandlers,
    state: Mutex<SoapySdrTxState>,
}

impl_block_trait! { Consumer<Signal<Complex<f32>>> for SoapySdrTx }
impl_block_trait! { EventHandling for SoapySdrTx }

impl SoapySdrTx {
    /// Create new [`SoapySdrTx`] block
    ///
    /// The passed `tx_stream` should not have been activated at this point.
    /// Instead, the stream must be activated by invoking
    /// [`SoapySdrTx::activate`].
    pub fn new(tx_stream: soapysdr::TxStream<Complex<f32>>) -> Self {
        let receiver_connector = ReceiverConnector::<Signal<Complex<f32>>>::new();
        let state = Mutex::new(SoapySdrTxState::Idle(tx_stream));
        let event_handlers = EventHandlers::new();
        Self {
            receiver_connector,
            event_handlers,
            state,
        }
    }
    /// Activate streaming
    pub async fn activate(&mut self) -> Result<(), soapysdr::Error> {
        let mut state_guard = self.state.lock().await;
        match take(&mut *state_guard) {
            SoapySdrTxState::Invalid => panic!("invalid state in SoapySdrTx"),
            SoapySdrTxState::Active(x) => {
                *state_guard = SoapySdrTxState::Active(x);
                Ok(())
            }
            SoapySdrTxState::Idle(mut tx_stream) => {
                let mut tx_stream = match spawn_blocking(move || {
                    match tx_stream.activate(None) {
                        Ok(x) => x,
                        Err(err) => return Err((tx_stream, err)),
                    };
                    Ok(tx_stream)
                })
                .await
                .unwrap()
                {
                    Ok(x) => x,
                    Err((tx_stream, err)) => {
                        *state_guard = SoapySdrTxState::Idle(tx_stream);
                        return Err(err);
                    }
                };
                let mut receiver = self.receiver_connector.stream();
                let (abort_send, abort_recv) = watch::channel::<()>(());
                let evhdl_clone = self.event_handlers.clone();
                let rt = runtime::Handle::current();
                let join_handle = std::thread::spawn(move || {
                    let mut result = Ok(());
                    let mut block_until: Option<Instant> = None;
                    while !abort_recv.has_changed().unwrap_or(true) {
                        let Ok(signal) = rt.block_on(receiver.recv()) else { break; };
                        match signal {
                            Signal::Samples { sample_rate, chunk } => {
                                let duration =
                                    Duration::from_secs_f64(chunk.len() as f64 / sample_rate);
                                let now = Instant::now();
                                match block_until {
                                    Some(mut t) => {
                                        let sleep_time = t.saturating_duration_since(now);
                                        t += duration;
                                        if t < now {
                                            t = now;
                                        }
                                        block_until = Some(t);
                                        sleep(sleep_time);
                                    }
                                    None => {
                                        block_until = Some(now + duration);
                                    }
                                }
                                match tx_stream.write_all(&[&chunk], None, false, 1000000) {
                                    Ok(x) => x,
                                    Err(err) => {
                                        result = Err(err);
                                        break;
                                    }
                                }
                            }
                            Signal::Event { payload, .. } => evhdl_clone.invoke(&payload),
                        }
                    }
                    if let Err(err) = tx_stream.deactivate(None) {
                        if result.is_ok() {
                            result = Err(err);
                        }
                    }
                    SoapySdrTxRetval { tx_stream, result }
                });
                *state_guard = SoapySdrTxState::Active(SoapySdrTxActive {
                    abort: abort_send,
                    join_handle,
                });
                Ok(())
            }
        }
    }
    /// Deactivate (pause) streaming
    pub async fn deactivate(&mut self) -> Result<(), soapysdr::Error> {
        let mut state_guard = self.state.lock().await;
        match take(&mut *state_guard) {
            SoapySdrTxState::Invalid => panic!("invalid state in SoapySdrTx"),
            SoapySdrTxState::Idle(x) => {
                *state_guard = SoapySdrTxState::Idle(x);
                Ok(())
            }
            SoapySdrTxState::Active(SoapySdrTxActive { abort, join_handle }) => {
                drop(abort);
                let retval = runtime::Handle::current()
                    .spawn_blocking(move || join_handle.join().unwrap())
                    .await
                    .unwrap();
                *state_guard = SoapySdrTxState::Idle(retval.tx_stream);
                retval.result
            }
        }
    }
}

#[cfg(test)]
mod tests {}
