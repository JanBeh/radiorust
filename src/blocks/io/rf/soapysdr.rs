//! Interface to RF hardware through SoapySDR (using the [`soapysdr`] crate)

use crate::bufferpool::*;
use crate::flow::*;
use crate::impl_block_trait;
use crate::numbers::*;
use crate::signal::*;

use tokio::select;
use tokio::sync::watch;
use tokio::task::{spawn, spawn_blocking, JoinHandle};
use tokio::time::sleep;

use std::time::{Duration, Instant};

pub use soapysdr::Error;

#[derive(Clone, PartialEq, Eq, Debug)]
enum Request {
    Deactivate,
    Activate,
    Close,
}

#[derive(Clone, Debug)]
enum State {
    Inactive,
    Active,
    Closed(Result<(), Error>),
}

/// Block which wraps an [`::soapysdr::RxStream`] and acts as a
/// [`Producer<Signal<Complex<Flt>>>`]
pub struct SoapySdrRx {
    sender_connector: SenderConnector<Signal<Complex<f32>>>,
    request_send: watch::Sender<Request>,
    state_recv: watch::Receiver<State>,
    join_handle: JoinHandle<soapysdr::RxStream<Complex<f32>>>,
}

impl_block_trait! { Producer<Signal<Complex<f32>>> for SoapySdrRx }

impl SoapySdrRx {
    /// Create new [`SoapySdrRx`] block
    ///
    /// The passed `rx_stream` should not have been activated at this point.
    /// Instead, the stream must be activated by invoking
    /// [`SoapySdrRx::activate`].
    pub fn new(mut rx_stream: soapysdr::RxStream<Complex<f32>>, sample_rate: f64) -> Self {
        let (sender, sender_connector) = new_sender::<Signal<Complex<f32>>>();
        let (request_send, mut request_recv) = watch::channel(Request::Deactivate);
        let (state_send, state_recv) = watch::channel(State::Inactive);
        let mtu: usize = rx_stream.mtu().unwrap();
        let join_handle = spawn(async move {
            let result = 'task: loop {
                loop {
                    let Ok(()) = request_recv.changed().await else { break 'task Ok(()); };
                    let request = request_recv.borrow_and_update().clone();
                    match request {
                        Request::Deactivate => (),
                        Request::Activate => break,
                        Request::Close => break 'task Ok(()),
                    }
                }
                let result;
                (result, rx_stream) = spawn_blocking(move || {
                    let result = rx_stream.activate(None);
                    (result, rx_stream)
                })
                .await
                .unwrap();
                if let Err(err) = result {
                    break 'task Err(err);
                }
                state_send.send_replace(State::Active);
                let mut buf_pool = ChunkBufPool::<Complex<f32>>::new();
                loop {
                    match request_recv.has_changed() {
                        Ok(false) => (),
                        Ok(true) => {
                            let request = request_recv.borrow_and_update().clone();
                            match request {
                                Request::Deactivate => break,
                                Request::Activate => (),
                                Request::Close => {
                                    let result;
                                    (result, rx_stream) = spawn_blocking(move || {
                                        let result = rx_stream.deactivate(None);
                                        (result, rx_stream)
                                    })
                                    .await
                                    .unwrap();
                                    break 'task result;
                                }
                            }
                        }
                        Err(_) => break 'task Ok(()),
                    }
                    let mut buffer = buf_pool.get();
                    buffer.resize_with(mtu, Default::default);
                    let result;
                    (result, (rx_stream, buffer)) = spawn_blocking(move || {
                        let result = rx_stream.read(&[&mut buffer], 1000000);
                        (result, (rx_stream, buffer))
                    })
                    .await
                    .unwrap();
                    let count = match result {
                        Ok(x) => x,
                        Err(err) => {
                            rx_stream = spawn_blocking(move || {
                                rx_stream.deactivate(None).ok();
                                rx_stream
                            })
                            .await
                            .unwrap();
                            break 'task Err(err);
                        }
                    };
                    buffer.truncate(count);
                    let signal = Signal::Samples {
                        sample_rate,
                        chunk: buffer.finalize(),
                    };
                    let Ok(()) = sender.send(signal).await else { break 'task Ok(()); };
                }
                let result;
                (result, rx_stream) = spawn_blocking(move || {
                    let result = rx_stream.deactivate(None);
                    (result, rx_stream)
                })
                .await
                .unwrap();
                if let Err(err) = result {
                    break 'task Err(err);
                }
                state_send.send_replace(State::Inactive);
            };
            state_send.send_replace(State::Closed(result));
            rx_stream
        });
        Self {
            sender_connector,
            request_send,
            state_recv,
            join_handle,
        }
    }
    /// Activate streaming
    pub async fn activate(&self) -> Result<(), Error> {
        let mut state_recv = self.state_recv.clone();
        let state = state_recv.borrow_and_update().clone();
        match state {
            State::Active => Ok(()),
            State::Inactive => {
                self.request_send.send_replace(Request::Activate);
                loop {
                    if let Err(_) = state_recv.changed().await {
                        let state = state_recv.borrow_and_update().clone();
                        match state {
                            State::Closed(result) => return result,
                            _ => panic!("SoapySdrRx task ended unexpectedly"),
                        }
                    }
                    let state = state_recv.borrow_and_update().clone();
                    match state {
                        State::Active => return Ok(()),
                        State::Inactive => (),
                        State::Closed(result) => return result,
                    }
                }
            }
            State::Closed(result) => result,
        }
    }
    /// Deactivate streaming
    pub async fn deactivate(&self) -> Result<(), Error> {
        let mut state_recv = self.state_recv.clone();
        let state = state_recv.borrow_and_update().clone();
        match state {
            State::Active => {
                self.request_send.send_replace(Request::Deactivate);
                loop {
                    if let Err(_) = state_recv.changed().await {
                        let state = state_recv.borrow_and_update().clone();
                        match state {
                            State::Closed(result) => return result,
                            _ => panic!("SoapySdrRx task ended unexpectedly"),
                        }
                    }
                    let state = state_recv.borrow_and_update().clone();
                    match state {
                        State::Active => (),
                        State::Inactive => return Ok(()),
                        State::Closed(result) => return result,
                    }
                }
            }
            State::Inactive => Ok(()),
            State::Closed(result) => result,
        }
    }
    /// Deactivate streaming and return inner [`::soapysdr::RxStream`]
    pub async fn into_inner(mut self) -> Result<soapysdr::RxStream<Complex<f32>>, Error> {
        self.request_send.send_replace(Request::Close);
        let rx_stream = self.join_handle.await.unwrap();
        let state = self.state_recv.borrow_and_update().clone();
        match state {
            State::Closed(Ok(())) => Ok(rx_stream),
            State::Closed(Err(err)) => Err(err),
            _ => panic!("SoapySdrRx task ended unexpectedly"),
        }
    }
}

/// Block which wraps an [`::soapysdr::TxStream`] and acts as a
/// [`Producer<Signal<Complex<Flt>>>`]
///
/// As a workaround for bad driver implementations, the following extra
/// measures are taken by the `SoapySdrTx` block:
///
/// * Invocation of [`::soapysdr::TxStream::write_all`] is throttled to cope
///   with implementations which do not provide backpressure.
/// * Upon creation of the block, a zero sample is transmitted to silence the
///   transmitter.
pub struct SoapySdrTx {
    receiver_connector: ReceiverConnector<Signal<Complex<f32>>>,
    event_handlers: EventHandlers,
    request_send: watch::Sender<Request>,
    state_recv: watch::Receiver<State>,
    join_handle: JoinHandle<soapysdr::TxStream<Complex<f32>>>,
}

impl_block_trait! { Consumer<Signal<Complex<f32>>> for SoapySdrTx }
impl_block_trait! { EventHandling for SoapySdrTx }

impl SoapySdrTx {
    /// Create new [`SoapySdrTx`] block
    ///
    /// The passed `tx_stream` should not have been activated at this point.
    /// Instead, the stream must be activated by invoking
    /// [`SoapySdrTx::activate`].
    pub fn new(mut tx_stream: soapysdr::TxStream<Complex<f32>>) -> Self {
        let (mut receiver, receiver_connector) = new_receiver::<Signal<Complex<f32>>>();
        let (request_send, mut request_recv) = watch::channel(Request::Deactivate);
        let (state_send, state_recv) = watch::channel(State::Inactive);
        let event_handlers = EventHandlers::new();
        let evhdl_clone = event_handlers.clone();
        let join_handle = spawn(async move {
            let mut first_run = true;
            let result = 'task: loop {
                if first_run {
                    let result;
                    (result, tx_stream) = spawn_blocking(move || {
                        let result = tx_stream.activate(None);
                        (result, tx_stream)
                    })
                    .await
                    .unwrap();
                    if let Err(err) = result {
                        break 'task Err(err);
                    }
                    first_run = false;
                }
                let result;
                (result, tx_stream) = spawn_blocking(move || {
                    let result = tx_stream.write_all(
                        &[&[Complex::new(0.0f32, 0.0f32)]],
                        None,
                        false,
                        1000000,
                    );
                    (result, tx_stream)
                })
                .await
                .unwrap();
                if let Err(err) = result {
                    tx_stream = spawn_blocking(move || {
                        tx_stream.deactivate(None).ok();
                        tx_stream
                    })
                    .await
                    .unwrap();
                    break 'task Err(err);
                }
                let result;
                (result, tx_stream) = spawn_blocking(move || {
                    let result = tx_stream.deactivate(None);
                    (result, tx_stream)
                })
                .await
                .unwrap();
                if let Err(err) = result {
                    break 'task Err(err);
                }
                state_send.send_replace(State::Inactive);
                loop {
                    let Ok(()) = request_recv.changed().await else { break 'task Ok(()); };
                    let request = request_recv.borrow_and_update().clone();
                    match request {
                        Request::Deactivate => (),
                        Request::Activate => break,
                        Request::Close => break 'task Ok(()),
                    }
                }
                let result;
                (result, tx_stream) = spawn_blocking(move || {
                    let result = tx_stream.activate(None);
                    (result, tx_stream)
                })
                .await
                .unwrap();
                if let Err(err) = result {
                    break 'task Err(err);
                }
                state_send.send_replace(State::Active);
                let mut block_until: Option<Instant> = None;
                loop {
                    select! {
                        changed = request_recv.changed() => match changed {
                            Err(_) => break 'task Ok(()),
                            Ok(()) => {
                                let request = request_recv.borrow_and_update().clone();
                                match request {
                                    Request::Deactivate => break,
                                    Request::Activate => (),
                                    Request::Close => break 'task Ok(()),
                                }
                            }
                        },
                        received = receiver.recv() => match received {
                            Err(_) => break 'task Ok(()),
                            Ok(signal) => match signal {
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
                                            sleep(sleep_time).await;
                                        }
                                        None => {
                                            block_until = Some(now + duration);
                                        }
                                    }
                                    let result;
                                    (result, tx_stream) = spawn_blocking(move || {
                                        let result = tx_stream.write_all(
                                            &[&chunk], None, false, 1000000,
                                        );
                                        (result, tx_stream)
                                    })
                                    .await
                                    .unwrap();
                                    if let Err(err) = result {
                                        tx_stream = spawn_blocking(move || {
                                            tx_stream.write_all(
                                                &[&[Complex::new(0.0f32, 0.0f32)]],
                                                None, false, 1000000,
                                            ).ok();
                                            tx_stream
                                        })
                                        .await
                                        .unwrap();
                                        tx_stream = spawn_blocking(move || {
                                            tx_stream.deactivate(None).ok();
                                            tx_stream
                                        })
                                        .await
                                        .unwrap();
                                        break 'task Err(err);
                                    }
                                }
                                Signal::Event { payload, .. } => evhdl_clone.invoke(&payload),
                            }
                        },
                    }
                }
            };
            state_send.send_replace(State::Closed(result));
            tx_stream
        });
        Self {
            receiver_connector,
            event_handlers,
            request_send,
            state_recv,
            join_handle,
        }
    }
    /// Activate streaming
    pub async fn activate(&self) -> Result<(), soapysdr::Error> {
        let mut state_recv = self.state_recv.clone();
        let state = state_recv.borrow_and_update().clone();
        match state {
            State::Active => Ok(()),
            State::Inactive => {
                self.request_send.send_replace(Request::Activate);
                loop {
                    if let Err(_) = state_recv.changed().await {
                        let state = state_recv.borrow_and_update().clone();
                        match state {
                            State::Closed(result) => return result,
                            _ => panic!("SoapySdrTx task ended unexpectedly"),
                        }
                    }
                    let state = state_recv.borrow_and_update().clone();
                    match state {
                        State::Active => return Ok(()),
                        State::Inactive => (),
                        State::Closed(result) => return result,
                    }
                }
            }
            State::Closed(result) => result,
        }
    }
    /// Deactivate (pause) streaming
    pub async fn deactivate(&self) -> Result<(), soapysdr::Error> {
        let mut state_recv = self.state_recv.clone();
        let state = state_recv.borrow_and_update().clone();
        match state {
            State::Active => {
                self.request_send.send_replace(Request::Deactivate);
                loop {
                    if let Err(_) = state_recv.changed().await {
                        let state = state_recv.borrow_and_update().clone();
                        match state {
                            State::Closed(result) => return result,
                            _ => panic!("SoapySdrTx task ended unexpectedly"),
                        }
                    }
                    let state = state_recv.borrow_and_update().clone();
                    match state {
                        State::Active => (),
                        State::Inactive => return Ok(()),
                        State::Closed(result) => return result,
                    }
                }
            }
            State::Inactive => Ok(()),
            State::Closed(result) => result,
        }
    }
    /// Deactivate streaming and return inner [`::soapysdr::TxStream`]
    pub async fn into_inner(mut self) -> Result<soapysdr::TxStream<Complex<f32>>, Error> {
        self.request_send.send_replace(Request::Close);
        let tx_stream = self.join_handle.await.unwrap();
        let state = self.state_recv.borrow_and_update().clone();
        match state {
            State::Closed(Ok(())) => Ok(tx_stream),
            State::Closed(Err(err)) => Err(err),
            _ => panic!("SoapySdrTx task ended unexpectedly"),
        }
    }
}

#[cfg(test)]
mod tests {}
