//! Modulators and demodulators (e.g. FM)

use crate::blocks::Samples;
use crate::bufferpool::*;
use crate::flow::*;
use crate::flt;
use crate::genfloat::Float;

use num::Complex;
use tokio::sync::watch;
use tokio::task::spawn;

/// FM demodulator block
pub struct FmDemod<Flt> {
    receiver: Receiver<Samples<Complex<Flt>>>,
    sender: Sender<Samples<Complex<Flt>>>,
    deviation: watch::Sender<f64>,
}

impl<Flt> Consumer<Samples<Complex<Flt>>> for FmDemod<Flt>
where
    Flt: Clone,
{
    fn receiver(&self) -> &Receiver<Samples<Complex<Flt>>> {
        &self.receiver
    }
}

impl<Flt> Producer<Samples<Complex<Flt>>> for FmDemod<Flt>
where
    Flt: Clone,
{
    fn connector(&self) -> SenderConnector<Samples<Complex<Flt>>> {
        self.sender.connector()
    }
}

impl<Flt> FmDemod<Flt>
where
    Flt: Float,
{
    /// Create new FM demodulator with given frequency deviation in hertz
    pub fn new(mut deviation: f64) -> Self {
        use std::f64::consts::TAU;
        let receiver = Receiver::<Samples<Complex<Flt>>>::new();
        let sender = Sender::<Samples<Complex<Flt>>>::new();
        let (deviation_send, mut deviation_recv) = watch::channel(deviation);
        let mut input = receiver.stream();
        let output = sender.clone();
        let mut buf_pool = ChunkBufPool::<Complex<Flt>>::new();
        let mut previous_sample: Option<Complex<Flt>> = None;
        let mut output_sample = Complex::<Flt>::from(Flt::zero());
        spawn(async move {
            loop {
                match input.recv().await {
                    Ok(Samples {
                        sample_rate,
                        chunk: input_chunk,
                    }) => {
                        match deviation_recv.has_changed() {
                            Ok(false) => (),
                            Ok(true) => deviation = deviation_recv.borrow_and_update().clone(),
                            Err(_) => return,
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
                        output
                            .send(Samples {
                                sample_rate,
                                chunk: output_chunk.finalize(),
                            })
                            .await;
                    }
                    Err(err) => {
                        previous_sample = None;
                        output.forward_error(err).await;
                        if err == RecvError::Closed {
                            return;
                        }
                    }
                }
            }
        });
        Self {
            receiver,
            sender,
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
