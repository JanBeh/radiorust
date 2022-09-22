use crate::bufferpool::*;
use crate::flow::*;
use crate::numbers::Complex;
use crate::samples::Samples;

use tokio::{
    runtime,
    task::{spawn_blocking, JoinHandle},
};

use std::mem::take;
use std::sync::{Arc, Mutex};

struct SoapySdrRxRetval {
    rx_stream: soapysdr::RxStream<Complex<f32>>,
    result: Result<(), soapysdr::Error>,
}

struct SoapySdrRxActive {
    keepalive: Arc<()>,
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
    sample_rate: f64,
    state: Mutex<SoapySdrRxState>,
}

impl Producer<Samples<Complex<f32>>> for SoapySdrRx {
    fn connector(&self) -> SenderConnector<Samples<Complex<f32>>> {
        self.sender.connector()
    }
}

impl SoapySdrRx {
    /// Create new [`SoapySdrRx`] block
    ///
    /// The passed `rx_stream` should not have been activated at this point.
    /// Instead, the stream must be activated by invoking
    /// [`SoapySdrRx::activate`].
    pub fn new(rx_stream: soapysdr::RxStream<Complex<f32>>, sample_rate: f64) -> Self {
        let sender = Sender::new();
        let state = Mutex::new(SoapySdrRxState::Idle(rx_stream));
        Self {
            sender,
            sample_rate,
            state,
        }
    }
    /// Activate streaming
    pub fn activate(&mut self) -> Result<(), soapysdr::Error> {
        let mut state_guard = self.state.lock().unwrap();
        match take(&mut *state_guard) {
            SoapySdrRxState::Invalid => panic!("invalid state in SoapySdrRx"),
            SoapySdrRxState::Active(x) => {
                *state_guard = SoapySdrRxState::Active(x);
                Ok(())
            }
            SoapySdrRxState::Idle(mut rx_stream) => {
                let mtu = match (|| {
                    let mtu = rx_stream.mtu()?;
                    rx_stream.activate(None)?;
                    Ok::<_, soapysdr::Error>(mtu)
                })() {
                    Ok(x) => x,
                    Err(err) => {
                        *state_guard = SoapySdrRxState::Idle(rx_stream);
                        return Err(err);
                    }
                };
                let sample_rate = self.sample_rate;
                let output = self.sender.clone();
                let keepalive_send = Arc::new(());
                let keepalive_recv = keepalive_send.clone();
                let join_handle = spawn_blocking(move || {
                    let rt = runtime::Handle::current();
                    let mut buf_pool = ChunkBufPool::<Complex<f32>>::new();
                    let mut result = Ok(());
                    while Arc::strong_count(&keepalive_recv) > 1 {
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
                        rt.block_on(output.send(Samples {
                            sample_rate,
                            chunk: buffer.finalize(),
                        }));
                    }
                    SoapySdrRxRetval { rx_stream, result }
                });
                *state_guard = SoapySdrRxState::Active(SoapySdrRxActive {
                    keepalive: keepalive_send,
                    join_handle,
                });
                Ok(())
            }
        }
    }
    /// Deactivate (pause) streaming
    pub fn deactivate(&mut self) -> Result<(), soapysdr::Error> {
        let mut state_guard = self.state.lock().unwrap();
        match take(&mut *state_guard) {
            SoapySdrRxState::Invalid => panic!("invalid state in SoapySdrRx"),
            SoapySdrRxState::Idle(x) => {
                *state_guard = SoapySdrRxState::Idle(x);
                Ok(())
            }
            SoapySdrRxState::Active(SoapySdrRxActive {
                keepalive,
                join_handle,
            }) => {
                drop(keepalive);
                let rt = runtime::Handle::current();
                let retval = rt.block_on(join_handle).unwrap();
                *state_guard = SoapySdrRxState::Idle(retval.rx_stream);
                retval.result
            }
        }
    }
}

#[cfg(test)]
mod tests {}
