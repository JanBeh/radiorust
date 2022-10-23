//! Signal analysis / metering

use crate::bufferpool::*;
use crate::flow::*;
use crate::flt;
use crate::numbers::*;
use crate::samples::*;
use crate::windowing::{self, Window};

use rustfft::{Fft, FftPlanner};
use tokio::task::spawn;

use std::sync::Arc;

/// Block performing a Fourier analysis
pub struct Fourier<Flt> {
    receiver_connector: ReceiverConnector<Samples<Complex<Flt>>>,
    sender_connector: SenderConnector<Samples<Complex<Flt>>>,
}

impl<Flt> Consumer<Samples<Complex<Flt>>> for Fourier<Flt> {
    fn receiver_connector(&self) -> &ReceiverConnector<Samples<Complex<Flt>>> {
        &self.receiver_connector
    }
}

impl<Flt> Producer<Samples<Complex<Flt>>> for Fourier<Flt> {
    fn sender_connector(&self) -> &SenderConnector<Samples<Complex<Flt>>> {
        &self.sender_connector
    }
}

impl<Flt> Fourier<Flt>
where
    Flt: Float,
{
    /// Create `Fourier` block without windowing
    pub fn new() -> Self {
        Self::with_window(windowing::Rectangular)
    }
    /// Create `Fourier` block with windowing
    pub fn with_window<W>(window: W) -> Self
    where
        W: Window + Send + 'static,
    {
        let (mut receiver, receiver_connector) = new_receiver::<Samples<Complex<Flt>>>();
        let (sender, sender_connector) = new_sender::<Samples<Complex<Flt>>>();
        spawn(async move {
            let mut buf_pool = ChunkBufPool::new();
            let mut previous_chunk_len: Option<usize> = None;
            let mut fft: Option<Arc<dyn Fft<Flt>>> = Default::default();
            let mut scratch: Vec<f64> = Default::default();
            let mut window_values: Vec<Flt> = Default::default();
            loop {
                match receiver.recv().await {
                    Ok(Samples {
                        sample_rate,
                        chunk: input_chunk,
                    }) => {
                        let n: usize = input_chunk.len();
                        if Some(n) != previous_chunk_len {
                            fft = Some(FftPlanner::<Flt>::new().plan_fft_forward(n));
                            scratch.clear();
                            scratch.reserve_exact(n);
                            let mut energy: f64 = 0.0;
                            for idx in 0..n {
                                let value = window
                                    .relative_value_at(2.0 * (idx as f64 + 0.5) / n as f64 - 1.0);
                                scratch.push(value);
                                energy += value * value;
                            }
                            let scale: f64 = (n as f64 / energy).sqrt();
                            window_values.clear();
                            window_values.reserve_exact(n);
                            for &value in scratch.iter() {
                                window_values.push(flt!(value * scale));
                            }
                            previous_chunk_len = Some(n);
                        }
                        let mut output_chunk = buf_pool.get_with_capacity(input_chunk.len());
                        output_chunk.extend_from_slice(&input_chunk);
                        for idx in 0..n {
                            output_chunk[idx] *= window_values[idx];
                        }
                        fft.as_ref().unwrap().process(&mut output_chunk);
                        if let Err(_) = sender
                            .send(Samples {
                                sample_rate,
                                chunk: output_chunk.finalize(),
                            })
                            .await
                        {
                            return;
                        }
                    }
                    Err(err) => {
                        if let Err(_) = sender.forward_error(err).await {
                            return;
                        }
                        if err == RecvError::Closed {
                            return;
                        }
                    }
                }
            }
        });
        Self {
            receiver_connector,
            sender_connector,
        }
    }
}

#[cfg(test)]
mod tests {}
