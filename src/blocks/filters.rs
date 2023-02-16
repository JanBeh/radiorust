//! Digital filters

use crate::bufferpool::*;
use crate::flow::*;
use crate::impl_block_trait;
use crate::numbers::*;
use crate::signal::*;
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
/// length of received [`Signal::Samples`] as well as the selected [`Window`]
/// function.
/// Using [`Kaiser::with_null_at_bin(x)`] results in a frequency resolution in
/// hertz of `x * sample_rate / chunk.len()`, but higher `x` improve stop band
/// attenuation (`x` must be `>= 1.0` and defaults to `2.0`).
/// To increase frequency resolution without worsening stop band attenuation,
/// increase the chunk length of the received samples. Note, however, that this
/// will also increase the delay of the filter.
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
/// [`sample_rate`]: Signal::Samples::sample_rate
/// [`chunk`]: Signal::Samples::chunk
/// [`Kaiser::with_null_at_bin(x)`]: Kaiser::with_null_at_bin
/// [`Rechunker`]: crate::blocks::chunks::Rechunker
/// [`Downsampler`]: crate::blocks::resampling::Downsampler
/// [`Upsampler`]: crate::blocks::resampling::Upsampler
pub struct Filter<Flt> {
    receiver_connector: ReceiverConnector<Signal<Complex<Flt>>>,
    sender_connector: SenderConnector<Signal<Complex<Flt>>>,
    params: watch::Sender<FilterParams>,
}

impl_block_trait! { <Flt> Consumer<Signal<Complex<Flt>>> for Filter<Flt> }
impl_block_trait! { <Flt> Producer<Signal<Complex<Flt>>> for Filter<Flt> }

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
        let (mut receiver, receiver_connector) = new_receiver::<Signal<Complex<Flt>>>();
        let (sender, sender_connector) = new_sender::<Signal<Complex<Flt>>>();
        let (params_send, mut params_recv) = watch::channel(FilterParams { freq_resp, window });
        spawn(async move {
            let mut buf_pool = ChunkBufPool::<Complex<Flt>>::new();
            let mut prev_sample_rate: Option<f64> = None;
            let mut prev_input_chunk_len: Option<usize> = None;
            let mut previous_chunk: Option<Chunk<Complex<Flt>>> = None;
            let mut fft_planner_f64 = FftPlanner::<f64>::new();
            let mut fft_planner = FftPlanner::<Flt>::new();
            let mut fft: Option<Arc<dyn Fft<Flt>>> = Default::default();
            let mut ifft: Option<Arc<dyn Fft<Flt>>> = Default::default();
            let mut extended_response: Vec<Complex<Flt>> = Default::default();
            loop {
                let Ok(signal) = receiver.recv().await else { return; };
                match signal {
                    Signal::Samples {
                        sample_rate,
                        chunk: input_chunk,
                    } => {
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
                            fft_planner_f64
                                .plan_fft_inverse(n)
                                .process(&mut response);
                            for i in 0..(n / 2) {
                                response.swap(i, i + n / 2);
                            }
                            let mut energy_pre: f64 = 0.0;
                            let mut energy_post: f64 = 0.0;
                            for i in 0..n {
                                energy_pre += response[i].norm_sqr();
                                response[i] *= Complex::from(
                                    params
                                        .window
                                        .relative_value_at(2.0 * (i as f64 + 0.5) / n_flt - 1.0),
                                );
                                energy_post += response[i].norm_sqr();
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
                            fft = Some(fft_planner.plan_fft_forward(n * 2));
                            ifft = Some(fft_planner.plan_fft_inverse(n * 2));
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
                            let Ok(()) = sender.send(Signal::Samples {
                                sample_rate,
                                chunk: output_chunk.finalize(),
                             }).await
                            else { return; };
                        }
                        previous_chunk = Some(input_chunk);
                    }
                    Signal::Event(event) => {
                        if event.is_interrupt() {
                            previous_chunk = None;
                        }
                        let Ok(()) = sender.send(Signal::Event(event)).await
                        else { return; };
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

/// Block which limits the slew rate of I/Q values
///
/// The `slew_rate` passed to the [`new`] function or [`set_slew_rate`] method
/// is the norm of the difference between samples one second apart.
///
/// [`new`]: SlewRateLimiter::new
/// [`set_slew_rate`]: SlewRateLimiter::set_slew_rate
pub struct SlewRateLimiter<Flt> {
    receiver_connector: ReceiverConnector<Signal<Complex<Flt>>>,
    sender_connector: SenderConnector<Signal<Complex<Flt>>>,
    slew_rate: watch::Sender<f64>,
}

impl_block_trait! { <Flt> Consumer<Signal<Complex<Flt>>> for SlewRateLimiter<Flt> }
impl_block_trait! { <Flt> Producer<Signal<Complex<Flt>>> for SlewRateLimiter<Flt> }

impl<Flt> SlewRateLimiter<Flt>
where
    Flt: Float,
{
    /// Create new `SlewRateLimiter` block
    pub fn new(slew_rate: f64) -> Self {
        let (mut receiver, receiver_connector) = new_receiver::<Signal<Complex<Flt>>>();
        let (sender, sender_connector) = new_sender::<Signal<Complex<Flt>>>();
        let (slew_rate_send, mut slew_rate_recv) = watch::channel(slew_rate);
        spawn(async move {
            let mut slew_rate = slew_rate;
            let mut buf_pool = ChunkBufPool::<Complex<Flt>>::new();
            let mut previous_sample: Complex<Flt> = Complex::from(Flt::zero());
            loop {
                let Ok(signal) = receiver.recv().await else { return; };
                match signal {
                    Signal::Samples {
                        sample_rate,
                        chunk: input_chunk,
                    } => {
                        if slew_rate_recv.has_changed().unwrap_or(false) {
                            slew_rate = slew_rate_recv.borrow_and_update().clone();
                        }
                        let max_diff = flt!(slew_rate / sample_rate);
                        let mut output_chunk = buf_pool.get_with_capacity(input_chunk.len());
                        for &(mut sample) in input_chunk.iter() {
                            let diff = sample - previous_sample;
                            let norm = diff.norm();
                            if norm > max_diff {
                                sample = previous_sample + diff / norm * max_diff;
                            }
                            output_chunk.push(sample);
                            previous_sample = sample;
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
            slew_rate: slew_rate_send,
        }
    }
    /// Get slew rate
    pub fn slew_rate(&self) -> f64 {
        self.slew_rate.borrow().clone()
    }
    /// Set slew rate
    pub fn set_slew_rate(&self, slew_rate: f64) {
        self.slew_rate.send_replace(slew_rate);
    }
}

#[cfg(test)]
mod tests {}
