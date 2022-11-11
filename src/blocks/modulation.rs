//! Modulators and demodulators (e.g. FM)

use crate::bufferpool::*;
use crate::flow::*;
use crate::flt;
use crate::impl_block_trait;
use crate::numbers::*;
use crate::signal::*;

use tokio::sync::watch;
use tokio::task::spawn;

/// FM modulator block
pub struct FmMod<Flt> {
    receiver_connector: ReceiverConnector<Signal<Complex<Flt>>>,
    sender_connector: SenderConnector<Signal<Complex<Flt>>>,
    deviation: watch::Sender<f64>,
}

impl_block_trait! { <Flt> Consumer<Signal<Complex<Flt>>> for FmMod<Flt> }
impl_block_trait! { <Flt> Producer<Signal<Complex<Flt>>> for FmMod<Flt> }

impl<Flt> FmMod<Flt>
where
    Flt: Float,
{
    /// Create new FM modulator with given frequency deviation in hertz
    pub fn new(mut deviation: f64) -> Self {
        use std::f64::consts::TAU;
        let (mut receiver, receiver_connector) = new_receiver::<Signal<Complex<Flt>>>();
        let (sender, sender_connector) = new_sender::<Signal<Complex<Flt>>>();
        let (deviation_send, mut deviation_recv) = watch::channel(deviation);
        let mut buf_pool = ChunkBufPool::<Complex<Flt>>::new();
        let mut current_phase: Flt = Flt::zero();
        spawn(async move {
            loop {
                let Ok(signal) = receiver.recv().await else { return; };
                match signal {
                    Signal::Samples {
                        sample_rate,
                        chunk: input_chunk,
                    } => {
                        if deviation_recv.has_changed().unwrap_or(false) {
                            deviation = deviation_recv.borrow_and_update().clone();
                        }
                        let factor: Flt = flt!(deviation / sample_rate * TAU);
                        let mut output_chunk = buf_pool.get_with_capacity(input_chunk.len());
                        for &sample in input_chunk.iter() {
                            current_phase += sample.re * factor;
                            current_phase %= Flt::TAU();
                            let (im, re) = current_phase.sin_cos();
                            output_chunk.push(Complex::<Flt>::new(re, im));
                        }
                        let Ok(()) = sender.send(Signal::Samples {
                            sample_rate,
                            chunk: output_chunk.finalize(),
                        }).await
                        else { return; };
                    }
                    event @ Signal::Event { .. } => {
                        let Ok(()) = sender.send(event).await else { return; };
                    }
                }
            }
        });
        Self {
            receiver_connector,
            sender_connector,
            deviation: deviation_send,
        }
    }
    /// Get frequency deviation in hertz
    pub fn deviation(&self) -> f64 {
        self.deviation.borrow().clone()
    }
    /// Set frequency deviation in hertz
    pub fn set_deviation(&self, deviation: f64) -> &Self {
        self.deviation.send_replace(deviation);
        self
    }
}

/// FM demodulator block
pub struct FmDemod<Flt> {
    receiver_connector: ReceiverConnector<Signal<Complex<Flt>>>,
    sender_connector: SenderConnector<Signal<Complex<Flt>>>,
    deviation: watch::Sender<f64>,
}

impl_block_trait! { <Flt> Consumer<Signal<Complex<Flt>>> for FmDemod<Flt> }
impl_block_trait! { <Flt> Producer<Signal<Complex<Flt>>> for FmDemod<Flt> }

impl<Flt> FmDemod<Flt>
where
    Flt: Float,
{
    /// Create new FM demodulator with given frequency deviation in hertz
    pub fn new(mut deviation: f64) -> Self {
        use std::f64::consts::TAU;
        let (mut receiver, receiver_connector) = new_receiver::<Signal<Complex<Flt>>>();
        let (sender, sender_connector) = new_sender::<Signal<Complex<Flt>>>();
        let (deviation_send, mut deviation_recv) = watch::channel(deviation);
        let mut buf_pool = ChunkBufPool::<Complex<Flt>>::new();
        let mut previous_sample: Option<Complex<Flt>> = None;
        let mut output_sample = Complex::<Flt>::from(Flt::zero());
        spawn(async move {
            loop {
                let Ok(signal) = receiver.recv().await else { return; };
                match signal {
                    Signal::Samples {
                        sample_rate,
                        chunk: input_chunk,
                    } => {
                        if deviation_recv.has_changed().unwrap_or(false) {
                            deviation = deviation_recv.borrow_and_update().clone();
                        }
                        let factor: Flt = flt!(sample_rate / deviation / TAU);
                        let mut output_chunk = buf_pool.get_with_capacity(input_chunk.len());
                        for &sample in input_chunk.iter() {
                            if let Some(previous_sample) = previous_sample {
                                output_sample = Complex::<Flt>::from(
                                    (sample * previous_sample.conj()).arg() * factor,
                                )
                            };
                            output_chunk.push(output_sample);
                            previous_sample = Some(sample);
                        }
                        let Ok(()) = sender.send(Signal::Samples {
                            sample_rate,
                            chunk: output_chunk.finalize(),
                        }).await
                        else { return; };
                    }
                    Signal::Event { interrupt, payload } => {
                        if interrupt {
                            previous_sample = None;
                        }
                        let Ok(()) = sender.send(Signal::Event { interrupt, payload }).await
                        else { return; };
                    }
                }
            }
        });
        Self {
            receiver_connector,
            sender_connector,
            deviation: deviation_send,
        }
    }
    /// Get frequency deviation in hertz
    pub fn deviation(&self) -> f64 {
        self.deviation.borrow().clone()
    }
    /// Set frequency deviation in hertz
    pub fn set_deviation(&self, deviation: f64) -> &Self {
        self.deviation.send_replace(deviation);
        self
    }
}

#[cfg(test)]
mod tests {}
