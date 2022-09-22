//! Interface to RF hardware through SoapySDR (using the [`soapysdr`] crate)

use crate::bufferpool::*;
use crate::flow::*;
use crate::numbers::*;
use crate::samples::*;

use tokio::{
    runtime,
    task::{spawn_blocking, JoinHandle},
};

use std::fmt;
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
                        // TODO: The following method call may hang even when
                        // `keepalive_recv.is_alive()` becomes false.
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

#[non_exhaustive]
/// Error type returned by various methods of the [`SoapySdrTx`] block
#[derive(Debug)]
pub enum SoapySdrTxError {
    /// Wrapped [`soapysdr::Error`]
    Driver(soapysdr::Error),
    /// Sample rates did not match
    SampleRate,
    /// Discontinuity, e.g. buffer underflow or disconnected [`Producer`]
    Discontinuity,
    /// Transmission was aborted on explicit request ([`SoapySdrTx::deactivate`])
    AbortRequested,
}

impl fmt::Display for SoapySdrTxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SoapySdrTxError::Driver(err) => err.fmt(f),
            SoapySdrTxError::SampleRate => write!(f, "sample rate mismatch"),
            SoapySdrTxError::Discontinuity => write!(f, "discontinuity or buffer underflow"),
            SoapySdrTxError::AbortRequested => write!(f, "transmission abort requested"),
        }
    }
}

impl std::error::Error for SoapySdrTxError {}

struct SoapySdrTxRetval {
    tx_stream: soapysdr::TxStream<Complex<f32>>,
    result: Result<(), SoapySdrTxError>,
}

struct SoapySdrTxActive {
    keepalive: Arc<()>,
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

/// Block which wraps an [`soapysdr::TxStream`] and acts as a
/// [`Consumer<Samples<Complex<Flt>>>`]
///
/// **Note:** Deactivating the transmitter is still buggy and won't work when
/// there is no incoming data.
pub struct SoapySdrTx {
    receiver: Receiver<Samples<Complex<f32>>>,
    sample_rate: f64,
    state: Mutex<SoapySdrTxState>,
}

impl Consumer<Samples<Complex<f32>>> for SoapySdrTx {
    fn receiver(&self) -> &Receiver<Samples<Complex<f32>>> {
        &self.receiver
    }
}

impl SoapySdrTx {
    /// Create new [`SoapySdrTx`] block
    ///
    /// The passed `tx_stream` should not have been activated at this point.
    /// Instead, the stream must be activated by invoking
    /// [`SoapySdrTx::activate`].
    pub fn new(tx_stream: soapysdr::TxStream<Complex<f32>>, sample_rate: f64) -> Self {
        let receiver = Receiver::new();
        let state = Mutex::new(SoapySdrTxState::Idle(tx_stream));
        Self {
            receiver,
            sample_rate,
            state,
        }
    }
    /// Activate RF transmitter
    pub fn activate(&self) -> Result<(), SoapySdrTxError> {
        self.activate_internal(false, false)
    }
    /// Activate RF transmitter but automatically break on any discontinuity (e.g. underflow)
    pub fn activate_break_on_discont(&self) -> Result<(), SoapySdrTxError> {
        self.activate_internal(true, false)
    }
    fn activate_internal(
        &self,
        break_on_discont: bool,
        break_on_finish: bool,
    ) -> Result<(), SoapySdrTxError> {
        let mut state_guard = self.state.lock().unwrap();
        match take(&mut *state_guard) {
            SoapySdrTxState::Invalid => panic!("invalid state in SoapySdrTx"),
            SoapySdrTxState::Active(x) => {
                *state_guard = SoapySdrTxState::Active(x);
                Ok(())
            }
            SoapySdrTxState::Idle(mut tx_stream) => {
                match tx_stream.activate(None) {
                    Ok(()) => (),
                    Err(err) => {
                        *state_guard = SoapySdrTxState::Idle(tx_stream);
                        return Err(SoapySdrTxError::Driver(err));
                    }
                }
                let sample_rate = self.sample_rate;
                let mut input = self.receiver.stream();
                let keepalive_send = Arc::new(());
                let keepalive_recv = keepalive_send.clone();
                let join_handle = spawn_blocking(move || {
                    let rt = runtime::Handle::current();
                    let mut result = Ok(());
                    while Arc::strong_count(&keepalive_recv) > 1 {
                        match rt.block_on(input.recv_lowlat(1)) {
                            Ok(Samples {
                                sample_rate: rcvd_sample_rate,
                                chunk,
                            }) => {
                                if rcvd_sample_rate != sample_rate {
                                    result = Err(SoapySdrTxError::Driver(soapysdr::Error {
                                        code: soapysdr::ErrorCode::Other,
                                        message: format!("expected sample rate {sample_rate} but got samples with sample rate {rcvd_sample_rate}"),
                                    }));
                                    break;
                                }
                                match tx_stream.write_all(&[&chunk], None, false, 1000000) {
                                    Ok(()) => (),
                                    Err(err) => {
                                        result = Err(SoapySdrTxError::Driver(err));
                                        break;
                                    }
                                }
                            }
                            Err(RecvError::Closed) => break,
                            Err(RecvError::Reset) => {
                                if break_on_discont {
                                    result = Err(SoapySdrTxError::Discontinuity);
                                    break;
                                }
                            }
                            Err(RecvError::Finished) => {
                                if break_on_finish {
                                    break;
                                }
                            }
                        }
                    }
                    let result2 =
                        tx_stream.write_all(&[&[Complex::from(0.0)]], None, false, 1000000);
                    if result.is_ok() {
                        result = match result2 {
                            Ok(()) => Ok(()),
                            Err(err) => Err(SoapySdrTxError::Driver(err)),
                        }
                    }
                    SoapySdrTxRetval { tx_stream, result }
                });
                *state_guard = SoapySdrTxState::Active(SoapySdrTxActive {
                    keepalive: keepalive_send,
                    join_handle,
                });
                Ok(())
            }
        }
    }
    /// Deactivate RF transmitter immediately
    pub async fn deactivate(&self) -> Result<(), SoapySdrTxError> {
        let mut state_guard = self.state.lock().unwrap();
        match take(&mut *state_guard) {
            SoapySdrTxState::Invalid => panic!("invalid state in SoapySdrTx"),
            SoapySdrTxState::Idle(x) => {
                *state_guard = SoapySdrTxState::Idle(x);
                Ok(())
            }
            SoapySdrTxState::Active(SoapySdrTxActive {
                keepalive,
                join_handle,
            }) => {
                drop(keepalive);
                let retval = join_handle.await.unwrap();
                *state_guard = SoapySdrTxState::Idle(retval.tx_stream);
                retval.result
            }
        }
    }
    /// Activate RF transmitter until data is finished or a buffer underflow occurred
    pub async fn activate_once(&self) -> Result<(), SoapySdrTxError> {
        self.activate_internal(true, true)?;
        let mut state_guard = self.state.lock().unwrap();
        match take(&mut *state_guard) {
            SoapySdrTxState::Invalid => panic!("invalid state in SoapySdrTx"),
            SoapySdrTxState::Idle(x) => {
                *state_guard = SoapySdrTxState::Idle(x);
                Err(SoapySdrTxError::AbortRequested)
            }
            SoapySdrTxState::Active(SoapySdrTxActive {
                keepalive: _,
                join_handle,
            }) => {
                let retval = join_handle.await.unwrap();
                *state_guard = SoapySdrTxState::Idle(retval.tx_stream);
                retval.result
            }
        }
    }
}

#[cfg(test)]
mod tests {}
