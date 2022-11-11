//! Sample-rate conversion

use crate::bufferpool::*;
use crate::flow::*;
use crate::flt;
use crate::impl_block_trait;
use crate::math::*;
use crate::numbers::*;
use crate::signal::*;
use crate::windowing::{self, Window};

use tokio::task::spawn;

/// Reduce sample rate
pub struct Downsampler<Flt> {
    receiver_connector: ReceiverConnector<Signal<Complex<Flt>>>,
    sender_connector: SenderConnector<Signal<Complex<Flt>>>,
}

impl_block_trait! { <Flt> Consumer<Signal<Complex<Flt>>> for Downsampler<Flt> }
impl_block_trait! { <Flt> Producer<Signal<Complex<Flt>>> for Downsampler<Flt> }

impl<Flt> Downsampler<Flt>
where
    Flt: Float,
{
    /// Create new `Downsampler` block
    ///
    /// This call corresponds to [`Downsampler::with_quality`] with a `quality`
    /// value of `3.0`.
    ///
    /// Connected [`Producer`]s must emit [`Signal::Samples`] with a
    /// [`sample_rate`] equal to or higher than `output_rate`; otherwise a
    /// panic occurs.
    ///
    /// Aliasing is suppressed for frequencies lower than `bandwidth`.
    ///
    /// [`sample_rate`]: Signal::Samples::sample_rate
    pub fn new(output_chunk_len: usize, output_rate: f64, bandwidth: f64) -> Self {
        Self::with_quality(output_chunk_len, output_rate, bandwidth, 3.0)
    }
    /// Create new `Downsampler` block with `quality` setting
    ///
    /// Same as [`Downsampler::new`], but allows to specify a `quality`
    /// setting, which must be equal to or greater than `1.0`.
    pub fn with_quality(
        output_chunk_len: usize,
        output_rate: f64,
        bandwidth: f64,
        quality: f64,
    ) -> Self {
        assert!(output_rate >= 0.0, "output sample rate must be positive");
        assert!(bandwidth >= 0.0, "bandwidth must be positive");
        assert!(
            bandwidth < output_rate,
            "bandwidth must be smaller than output sample rate"
        );
        let (mut receiver, receiver_connector) = new_receiver::<Signal<Complex<Flt>>>();
        let (sender, sender_connector) = new_sender::<Signal<Complex<Flt>>>();
        let mut buf_pool = ChunkBufPool::<Complex<Flt>>::new();
        let mut output_chunk = buf_pool.get_with_capacity(output_chunk_len);
        spawn(async move {
            let margin = (output_rate - bandwidth) / 2.0;
            let mut prev_input_rate: Option<f64> = None;
            let mut ir: Vec<Flt> = Default::default();
            let mut ringbuf: Vec<Complex<Flt>> = Default::default();
            let mut ringbuf_pos: usize = Default::default();
            let mut pos: f64 = Default::default();
            loop {
                let Ok(signal) = receiver.recv().await else { return; };
                match signal {
                    Signal::Samples {
                        sample_rate: input_rate,
                        chunk: input_chunk,
                    } => {
                        if Some(input_rate) != prev_input_rate {
                            prev_input_rate = Some(input_rate);
                            assert!(input_rate >= 0.0, "input sample rate must be positive");
                            assert!(
                                input_rate >= output_rate,
                                "input sample rate must be greater than or equal to output sample rate"
                            );
                            let ir_len: usize = (input_rate / margin * quality).ceil() as usize;
                            assert!(ir_len > 0);
                            let ir_len_flt = ir_len as f64;
                            let window = windowing::Kaiser::with_null_at_bin(
                                ir_len_flt * margin / input_rate,
                            );
                            let mut ir_buf: Vec<f64> = Vec::with_capacity(ir_len);
                            let mut energy = 0.0;
                            for i in 0..ir_len {
                                let x = (i as f64 + 0.5) - ir_len_flt / 2.0;
                                let y = sinc(x * output_rate / input_rate)
                                    * window.relative_value_at(x * 2.0 / ir_len_flt);
                                ir_buf.push(y);
                                energy += y * y;
                            }
                            let scale = energy.sqrt().recip();
                            ir = ir_buf.into_iter().map(|y| flt!(y * scale)).collect();
                            ringbuf = vec![Complex::from(Flt::zero()); ir_len];
                            ringbuf_pos = 0;
                            pos = 0.0;
                        }
                        for &sample in input_chunk.iter() {
                            ringbuf[ringbuf_pos] = sample;
                            ringbuf_pos += 1;
                            if ringbuf_pos == ir.len() {
                                ringbuf_pos = 0;
                            }
                            pos += output_rate;
                            if pos >= input_rate {
                                pos -= input_rate;
                                let mut sum: Complex<Flt> = Complex::from(Flt::zero());
                                let mut ir_iter = ir.iter();
                                let mut next_ir = || *ir_iter.next().unwrap();
                                for i in ringbuf_pos..ir.len() {
                                    sum += ringbuf[i] * next_ir();
                                }
                                for i in 0..ringbuf_pos {
                                    sum += ringbuf[i] * next_ir();
                                }
                                output_chunk.push(sum);
                                if output_chunk.len() >= output_chunk_len {
                                    let Ok(()) = sender
                                        .send(Signal::Samples {
                                            sample_rate: output_rate,
                                            chunk: output_chunk.finalize(),
                                        })
                                        .await
                                    else { return; };
                                    output_chunk = buf_pool.get_with_capacity(output_chunk_len);
                                }
                            }
                        }
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
        }
    }
}

/// Increase sample rate
pub struct Upsampler<Flt> {
    receiver_connector: ReceiverConnector<Signal<Complex<Flt>>>,
    sender_connector: SenderConnector<Signal<Complex<Flt>>>,
}

impl_block_trait! { <Flt> Consumer<Signal<Complex<Flt>>> for Upsampler<Flt> }
impl_block_trait! { <Flt> Producer<Signal<Complex<Flt>>> for Upsampler<Flt> }

impl<Flt> Upsampler<Flt>
where
    Flt: Float,
{
    /// Create new `Upsampler` block
    ///
    /// This call corresponds to [`Upsampler::with_quality`] with a `quality`
    /// value of `3.0`.
    ///
    /// Connected [`Producer`]s must emit [`Signal::Samples`] with a
    /// [`sample_rate`] equal to or smaller than `output_rate` but larger than
    /// `bandwidth`; otherwise a panic occurs.
    ///
    /// Aliasing is suppressed for frequencies lower than `bandwidth`.
    ///
    /// [`sample_rate`]: Signal::Samples::sample_rate
    pub fn new(output_chunk_len: usize, output_rate: f64, bandwidth: f64) -> Self {
        Self::with_quality(output_chunk_len, output_rate, bandwidth, 3.0)
    }
    /// Create new `Upsampler` block with `quality` setting
    ///
    /// Same as [`Upsampler::new`], but allows to specify a `quality` setting,
    /// which must be equal to or greater than `1.0`.
    pub fn with_quality(
        output_chunk_len: usize,
        output_rate: f64,
        bandwidth: f64,
        quality: f64,
    ) -> Self {
        assert!(output_rate >= 0.0, "output sample rate must be positive");
        assert!(bandwidth >= 0.0, "bandwidth must be positive");
        let (mut receiver, receiver_connector) = new_receiver::<Signal<Complex<Flt>>>();
        let (sender, sender_connector) = new_sender::<Signal<Complex<Flt>>>();
        let mut buf_pool = ChunkBufPool::<Complex<Flt>>::new();
        let mut output_chunk = buf_pool.get_with_capacity(output_chunk_len);
        spawn(async move {
            let mut prev_input_rate: Option<f64> = None;
            let mut ir: Vec<Flt> = Default::default();
            let mut ringbuf: Vec<Complex<Flt>> = Default::default();
            let mut ringbuf_pos: usize = Default::default();
            let mut pos: f64 = Default::default();
            loop {
                let Ok(signal) = receiver.recv().await else { return; };
                match signal {
                    Signal::Samples {
                        sample_rate: input_rate,
                        chunk: input_chunk,
                    } => {
                        if Some(input_rate) != prev_input_rate {
                            prev_input_rate = Some(input_rate);
                            assert!(input_rate >= 0.0, "input sample rate must be positive");
                            assert!(
                                input_rate <= output_rate,
                                "input sample rate must be smaller than or equal to output sample rate"
                            );
                            assert!(
                                bandwidth < input_rate,
                                "bandwidth must be smaller than input sample rate"
                            );
                            let margin = (input_rate - bandwidth) / 2.0;
                            let ir_len: usize = (output_rate / margin * quality).ceil() as usize;
                            assert!(ir_len > 0);
                            let ir_len_flt = ir_len as f64;
                            let window = windowing::Kaiser::with_null_at_bin(
                                ir_len_flt * margin / output_rate,
                            );
                            let mut ir_buf: Vec<f64> = Vec::with_capacity(ir_len);
                            let mut energy = 0.0;
                            for i in 0..ir_len {
                                let x = (i as f64 + 0.5) - ir_len_flt / 2.0;
                                let y = sinc(x * input_rate / output_rate)
                                    * window.relative_value_at(x * 2.0 / ir_len_flt);
                                ir_buf.push(y);
                                energy += y * y;
                            }
                            let scale = energy.sqrt().recip();
                            ir = ir_buf.into_iter().map(|y| flt!(y * scale)).collect();
                            ringbuf = vec![Complex::from(Flt::zero()); ir_len];
                            ringbuf_pos = 0;
                            pos = 0.0;
                        }
                        for &sample in input_chunk.iter() {
                            let mut ir_iter = ir.iter();
                            let mut next_ir = || *ir_iter.next().unwrap();
                            for i in ringbuf_pos..ir.len() {
                                ringbuf[i] += sample * next_ir();
                            }
                            for i in 0..ringbuf_pos {
                                ringbuf[i] += sample * next_ir();
                            }
                            while pos < output_rate {
                                output_chunk.push(ringbuf[ringbuf_pos]);
                                ringbuf[ringbuf_pos] = Complex::from(Flt::zero());
                                if output_chunk.len() >= output_chunk_len {
                                    let Ok(()) = sender
                                        .send(Signal::Samples {
                                            sample_rate: output_rate,
                                            chunk: output_chunk.finalize(),
                                        })
                                        .await
                                    else { return; };
                                    output_chunk = buf_pool.get_with_capacity(output_chunk_len);
                                }
                                ringbuf_pos += 1;
                                if ringbuf_pos >= ir.len() {
                                    ringbuf_pos = 0;
                                }
                                pos += input_rate;
                            }
                            pos -= output_rate;
                        }
                    }
                    event @ Signal::Event { .. } => {
                        let Ok(()) = sender.send(event).await else { return; };
                    }
                }
            }
        });
        Upsampler {
            receiver_connector,
            sender_connector,
        }
    }
}

#[cfg(test)]
mod tests {}
