//! Interface to RF hardware through SoapySDR (using the [`soapysdr`] crate)

use crate::bufferpool::*;
use crate::flow::*;
use crate::numbers::*;
use crate::samples::*;

use tokio::runtime;
use tokio::sync::{watch, Mutex};
use tokio::task::{spawn_blocking, JoinHandle};

use std::mem::take;

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

/// Block which wraps an [`soapysdr::RxStream`] and acts as a
/// [`Producer<Samples<Complex<Flt>>>`]
pub struct SoapySdrRx {
    sender: Sender<Samples<Complex<f32>>>,
    sender_connector: SenderConnector<Samples<Complex<f32>>>,
    sample_rate: f64,
    state: Mutex<SoapySdrRxState>,
}

impl Producer<Samples<Complex<f32>>> for SoapySdrRx {
    fn sender_connector(&self) -> &SenderConnector<Samples<Complex<f32>>> {
        &self.sender_connector
    }
}

impl SoapySdrRx {
    /// Create new [`SoapySdrRx`] block
    ///
    /// The passed `rx_stream` should not have been activated at this point.
    /// Instead, the stream must be activated by invoking
    /// [`SoapySdrRx::activate`].
    pub fn new(rx_stream: soapysdr::RxStream<Complex<f32>>, sample_rate: f64) -> Self {
        let (sender, sender_connector) = new_sender::<Samples<Complex<f32>>>();
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
                let join_handle = spawn_blocking(move || {
                    let rt = runtime::Handle::current();
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
                        if let Err(_) = rt.block_on(sender.send(Samples {
                            sample_rate,
                            chunk: buffer.finalize(),
                        })) {
                            break;
                        }
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
                let retval = join_handle.await.unwrap();
                *state_guard = SoapySdrRxState::Idle(retval.rx_stream);
                retval.result
            }
        }
    }
}

#[cfg(test)]
mod tests {}
