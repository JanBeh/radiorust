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
///
/// Note that [`Fourier::new`] and [`Fourier::with_window`] will result in the
/// DC bin at index `0` and negative frequencies being located in the second
/// half.
/// To rotate the DC frequency to the center (and have negative frequencies
/// left of it and positive frequencies right of it), use
/// [`Fourier::new_center_dc`] or [`Fourier::with_window_center_dc`],
/// respectively.
/// In case of an even [`Chunk`] length `n`, the rotation will result in the
/// DC bin being at index `n / 2`.
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
        Self::new_internal(windowing::Rectangular, false)
    }
    /// Create `Fourier` block without windowing but rotating DC to center
    pub fn new_center_dc() -> Self {
        Self::new_internal(windowing::Rectangular, true)
    }
    /// Create `Fourier` block with windowing
    pub fn with_window<W>(window: W) -> Self
    where
        W: Window + Send + 'static,
    {
        Self::new_internal(window, false)
    }
    /// Create `Fourier` block with windowing and rotating DC to center
    pub fn with_window_center_dc<W>(window: W) -> Self
    where
        W: Window + Send + 'static,
    {
        Self::new_internal(window, true)
    }
    fn new_internal<W>(window: W, center_dc: bool) -> Self
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
                        if center_dc {
                            output_chunk.rotate_right(n / 2);
                        }
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
mod tests {
    use super::*;
    const PRECISION: f64 = 1e-10;
    pub fn assert_approx(a: f64, b: f64) {
        if !((a - b).abs() <= PRECISION || (a / b).ln().abs() <= PRECISION) {
            panic!("{a} and {b} are not approximately equal");
        }
    }
    #[tokio::test]
    async fn test_fourier() {
        let (sender, sender_connector) = new_sender();
        let fourier1 = Fourier::<f64>::new();
        let fourier2 = Fourier::<f64>::new_center_dc();
        let (mut receiver1, receiver1_connector) = new_receiver();
        let (mut receiver2, receiver2_connector) = new_receiver();
        fourier1.feed_from(&sender_connector);
        fourier2.feed_from(&sender_connector);
        fourier1.feed_into(&receiver1_connector);
        fourier2.feed_into(&receiver2_connector);
        let mut buf_pool = ChunkBufPool::new();
        let mut chunk_buf = buf_pool.get();
        chunk_buf.push(Complex::new(1.0, 0.0));
        chunk_buf.push(Complex::new(1.0, 0.0));
        chunk_buf.push(Complex::new(1.0, 0.0));
        sender.send(Samples {
            sample_rate: 48000.0,
            chunk: chunk_buf.finalize(),
        }).await.unwrap();
        let output1 = receiver1.recv().await.unwrap();
        let output2 = receiver2.recv().await.unwrap();
        assert_approx(output1.chunk[0].re, 3.0);
        assert_approx(output1.chunk[0].im, 0.0);
        assert_approx(output1.chunk[1].re, 0.0);
        assert_approx(output1.chunk[1].im, 0.0);
        assert_approx(output1.chunk[2].re, 0.0);
        assert_approx(output1.chunk[2].im, 0.0);
        assert_approx(output2.chunk[0].re, 0.0);
        assert_approx(output2.chunk[0].im, 0.0);
        assert_approx(output2.chunk[1].re, 3.0);
        assert_approx(output2.chunk[1].im, 0.0);
        assert_approx(output2.chunk[2].re, 0.0);
        assert_approx(output2.chunk[2].im, 0.0);
        let mut chunk_buf = buf_pool.get();
        chunk_buf.push(Complex::new(1.0, 0.0));
        chunk_buf.push(Complex::new(1.5, 0.0));
        chunk_buf.push(Complex::new(1.0, 0.0));
        chunk_buf.push(Complex::new(0.5, 0.0));
        sender.send(Samples {
            sample_rate: 48000.0,
            chunk: chunk_buf.finalize(),
        }).await.unwrap();
        let output1 = receiver1.recv().await.unwrap();
        let output2 = receiver2.recv().await.unwrap();
        assert_approx(output1.chunk[0].re, 4.0);
        assert_approx(output1.chunk[0].im, 0.0);
        assert_approx(output1.chunk[1].re, 0.0);
        assert_approx(output1.chunk[1].im, -1.0);
        assert_approx(output1.chunk[2].re, 0.0);
        assert_approx(output1.chunk[2].im, 0.0);
        assert_approx(output1.chunk[3].re, 0.0);
        assert_approx(output1.chunk[3].im, 1.0);
        assert_approx(output2.chunk[0].re, 0.0);
        assert_approx(output2.chunk[0].im, 0.0);
        assert_approx(output2.chunk[1].re, 0.0);
        assert_approx(output2.chunk[1].im, 1.0);
        assert_approx(output2.chunk[2].re, 4.0);
        assert_approx(output2.chunk[2].im, 0.0);
        assert_approx(output2.chunk[3].re, 0.0);
        assert_approx(output2.chunk[3].im, -1.0);
    }
}
