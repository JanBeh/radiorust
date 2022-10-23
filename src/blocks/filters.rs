//! Digital filters

use crate::bufferpool::*;
use crate::flow::*;
use crate::flt;
use crate::numbers::*;
use crate::samples::*;
use crate::windowing::{Kaiser, Rectangular, Window};

use rustfft::{Fft, FftPlanner};
use tokio::sync::watch;
use tokio::task::spawn;

use std::sync::Arc;

/// Complex amplification factor for deemphasis in frequency demodulation
///
/// The time constant `tau` corresponds to the product of the resistance *R*
/// and the capacity *C* of a passive first-order RC low-pass, e.g. `50e-6`.
pub fn deemphasis_factor(tau: f64, frequency: f64) -> Complex<f64> {
    use std::f64::consts::TAU as TWO_PI;
    Complex {
        re: 1.0,
        im: tau * TWO_PI * frequency,
    }
    .finv()
}

// TODO: use trait alias in public interface
// (but this currently requires extra type annotations on usage)
trait FreqRespFunc: Fn(isize, f64) -> Complex<f64> {}
impl<T: ?Sized> FreqRespFunc for T where T: Fn(isize, f64) -> Complex<f64> {}

struct FilterParams {
    freq_resp: Box<dyn FreqRespFunc + Send + Sync>,
    window: Box<dyn Window + Send + Sync>,
}

/// General purpose frequency filter using fast convolution
///
/// Behavior of the filter is controlled by passing a closure to one of the
/// filter's methods. The closure is called with a DFT bin index (which may be
/// ignored in most cases) and the corresponding *signed* frequency (as [`f64`]
/// in hertz) as arguments. The closure must then return a complex
/// amplification factor (as [`Complex<f64>`]) for the given frequency.
///
/// ```
/// # fn doc() {
/// use radiorust::{blocks, numbers::Complex};
/// // low-pass filter with cutoff at 16 kHz
/// let my_filter = blocks::filters::Filter::<f32>::new(|_, freq| {
///     if freq.abs() <= 16e3 {
///         Complex::from(1.0)
///     } else {
///         Complex::from(0.0)
///     }
/// });
/// # }
/// ```
///
/// This allows easy implementation of (linear phase) low-pass, high-pass,
/// band-pass, band-stop, and sideband filters, as well as (non-linear phase)
/// all-pass filters, or any combination thereof.
///
/// Frequency resolution depends on the [`sample_rate`] and [`chunk`]
/// length of received [`Samples`] as well as the selected [`Window`] function.
/// Using [`Kaiser::with_null_at_bin(x)`] results in a frequency resolution in
/// hertz of `x * sample_rate / chunk.len()`, but higher `x` improve stop band
/// attenuation (`x` must be `>= 1.0` and defaults to `2.0`).
/// To increase frequency resolution without worsening stop band attenuation,
/// increase the chunk length of the received `Samples`. Note, however, that
/// this will also increase the delay of the filter.
///
/// You may use a [`Rechunker`] block to adjust the chunk length which the
/// filter is operating with if this cannot be achieved otherwise, e.g. through
/// an existing [`Downsampler`] or [`Upsampler`] block.
///
/// The impulse response is equal to the `chunk` length and the delay of the
/// filter is one `chunk`, i.e. after the second chunk has been received, the
/// first output chunk is ready to be sent out.
///
/// When implementing DC blockers or notch filters, frequency resolution plays
/// an important role. To aid filter implementation, the closure calculating
/// the frequency response gets the DFT bin number as first argument.
/// A filter with DC blocker can, for example, be implemented as follows:
///
/// ```
/// # fn doc() {
/// use radiorust::{blocks, numbers::Complex};
/// let dc_blocker = blocks::filters::Filter::<f32>::new(|bin, _| {
///     // NOTE: window function defaults to `Kaiser::with_null_at_bin(2.0)`,
///     // thus `2` is used as boundary below:
///     if bin.abs() < 2 {
///         Complex::from(0.0)
///     } else {
///         /* … */
/// #       Complex::from(1.0)
///     }
/// });
/// # }
/// ```
///
/// [`sample_rate`]: Samples::sample_rate
/// [`chunk`]: Samples::chunk
/// [`Kaiser::with_null_at_bin(x)`]: Kaiser::with_null_at_bin
/// [`Rechunker`]: crate::blocks::chunks::Rechunker
/// [`Downsampler`]: crate::blocks::resampling::Downsampler
/// [`Upsampler`]: crate::blocks::resampling::Upsampler
pub struct Filter<Flt> {
    receiver_connector: ReceiverConnector<Samples<Complex<Flt>>>,
    sender_connector: SenderConnector<Samples<Complex<Flt>>>,
    params: watch::Sender<FilterParams>,
}

impl<Flt> Consumer<Samples<Complex<Flt>>> for Filter<Flt> {
    fn receiver_connector(&self) -> &ReceiverConnector<Samples<Complex<Flt>>> {
        &self.receiver_connector
    }
}

impl<Flt> Producer<Samples<Complex<Flt>>> for Filter<Flt> {
    fn sender_connector(&self) -> &SenderConnector<Samples<Complex<Flt>>> {
        &self.sender_connector
    }
}

impl<Flt> Filter<Flt>
where
    Flt: Float,
{
    /// Create new `Filter` block with given frequency response with Kaiser window
    ///
    /// The used [`Window`] function is [`Kaiser::with_null_at_bin(2.0)`].
    ///
    /// [`Kaiser::with_null_at_bin(2.0)`]: Kaiser::with_null_at_bin
    pub fn new<F>(freq_resp: F) -> Self
    where
        F: Fn(isize, f64) -> Complex<f64> + Send + Sync + 'static,
    {
        Self::new_internal(Box::new(freq_resp), Box::new(Kaiser::with_null_at_bin(2.0)))
    }
    /// Create new `Filter` block with given frequency response with rectangular window
    ///
    /// Compared to [`Filter::new`], this creates a filter with better
    /// frequency resolution but worse stop band attenuation.
    pub fn new_rectangular<F>(freq_resp: F) -> Self
    where
        F: Fn(isize, f64) -> Complex<f64> + Send + Sync + 'static,
    {
        Self::new_internal(Box::new(freq_resp), Box::new(Rectangular))
    }
    /// Create new `Filter` block with given frequency response and window
    /// function
    pub fn with_window<F, W>(freq_resp: F, window: W) -> Self
    where
        F: Fn(isize, f64) -> Complex<f64> + Send + Sync + 'static,
        W: Window + Send + Sync + 'static,
    {
        Self::new_internal(Box::new(freq_resp), Box::new(window))
    }
    fn new_internal(
        freq_resp: Box<dyn FreqRespFunc + Send + Sync>,
        window: Box<dyn Window + Send + Sync>,
    ) -> Self {
        let (mut receiver, receiver_connector) = new_receiver::<Samples<Complex<Flt>>>();
        let (sender, sender_connector) = new_sender::<Samples<Complex<Flt>>>();
        let (params_send, mut params_recv) = watch::channel(FilterParams { freq_resp, window });
        spawn(async move {
            let mut buf_pool = ChunkBufPool::<Complex<Flt>>::new();
            let mut prev_sample_rate: Option<f64> = None;
            let mut prev_input_chunk_len: Option<usize> = None;
            let mut previous_chunk: Option<Chunk<Complex<Flt>>> = None;
            let mut fft: Option<Arc<dyn Fft<Flt>>> = Default::default();
            let mut ifft: Option<Arc<dyn Fft<Flt>>> = Default::default();
            let mut extended_response: Vec<Complex<Flt>> = Default::default();
            loop {
                match receiver.recv().await {
                    Ok(Samples {
                        sample_rate,
                        chunk: input_chunk,
                    }) => {
                        let n = input_chunk.len();
                        let recalculate: bool = params_recv.has_changed().unwrap_or(false)
                            || Some(sample_rate) != prev_sample_rate
                            || Some(n) != prev_input_chunk_len;
                        prev_sample_rate = Some(sample_rate);
                        prev_input_chunk_len = Some(n);
                        if recalculate {
                            let n_flt = n as f64;
                            let scale = 2.0 * n_flt * n_flt;
                            previous_chunk = None;
                            let mut response = vec![Complex::<f64>::from(0.0); n];
                            let freq_step: f64 = sample_rate / n_flt;
                            let max_bin_abs = (n - 1) / 2;
                            let params = params_recv.borrow_and_update();
                            let freq_resp_func = &params.freq_resp;
                            for i in 0..=max_bin_abs {
                                let freq = i as f64 * freq_step;
                                response[i] = freq_resp_func(i as isize, freq) / scale;
                                if i > 0 {
                                    response[n - i] = freq_resp_func(-(i as isize), -freq) / scale;
                                }
                            }
                            FftPlanner::<f64>::new()
                                .plan_fft_inverse(n)
                                .process(&mut response);
                            for i in 0..(n / 2) {
                                response.swap(i, i + n / 2);
                            }
                            let mut energy_pre = Complex::<f64>::from(0.0);
                            let mut energy_post = Complex::<f64>::from(0.0);
                            for i in 0..n {
                                energy_pre += response[i] * response[i];
                                response[i] *= Complex::from(
                                    params
                                        .window
                                        .relative_value_at(2.0 * (i as f64 + 0.5) / n_flt - 1.0),
                                );
                                energy_post += response[i] * response[i];
                            }
                            drop(params);
                            let scale = (energy_pre / energy_post).sqrt();
                            for y in response.iter_mut() {
                                *y *= scale;
                            }
                            extended_response.clear();
                            extended_response.reserve(n * 2);
                            extended_response.resize(n, Complex::from(flt!(0.0)));
                            extended_response.extend(response.into_iter().map(|x| Complex {
                                re: flt!(x.re),
                                im: flt!(x.im),
                            }));
                            fft = Some(FftPlanner::<Flt>::new().plan_fft_forward(n * 2));
                            ifft = Some(FftPlanner::<Flt>::new().plan_fft_inverse(n * 2));
                            fft.as_ref().unwrap().process(&mut extended_response);
                        }
                        if let Some(previous_chunk) = &previous_chunk {
                            let mut output_chunk = buf_pool.get_with_capacity(n * 2);
                            output_chunk.extend_from_slice(previous_chunk);
                            output_chunk.extend_from_slice(&input_chunk);
                            fft.as_ref().unwrap().process(&mut output_chunk);
                            for i in 0..n * 2 {
                                output_chunk[i] *= extended_response[i];
                            }
                            ifft.as_ref().unwrap().process(&mut output_chunk);
                            output_chunk.truncate(n);
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
                        previous_chunk = Some(input_chunk);
                    }
                    Err(err) => {
                        previous_chunk = None;
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
            params: params_send,
        }
    }
    /// Update frequency response and leave window function unchanged
    pub fn update<F>(&self, freq_resp: F)
    where
        F: Fn(isize, f64) -> Complex<f64> + Send + Sync + 'static,
    {
        self.params.send_modify(|params| {
            params.freq_resp = Box::new(freq_resp);
        });
    }
    /// Update frequency response and window function
    pub fn update_with_window<F, W>(&self, freq_resp: F, window: W)
    where
        F: Fn(isize, f64) -> Complex<f64> + Send + Sync + 'static,
        W: Window + Send + Sync + 'static,
    {
        self.params.send_replace(FilterParams {
            freq_resp: Box::new(freq_resp),
            window: Box::new(window),
        });
    }
}

#[cfg(test)]
mod tests {}
