//! Metering (e.g. level or bandwidth measurement)

use crate::numbers::*;
use crate::samples::*;

/// Calculate mean square norm
///
/// # Example
///
/// ```
/// use radiorust::metering::level;
/// use radiorust::numbers::Complex;
///
/// let chunk = vec![
///     Complex::new(0.0, 0.0),
///     Complex::new(0.0, -0.5),
///     Complex::new(1.0, 0.0),
/// ];
///
/// assert!(level(&chunk) - 0.41666667 < 0.001);
/// ```
pub fn level<Flt>(chunk: &[Complex<Flt>]) -> f64
where
    Flt: Float,
{
    let mut square_average: f64 = 0.0;
    for sample in chunk.iter() {
        square_average += sample.norm_sqr().to_f64().unwrap();
    }
    square_average / chunk.len() as f64
}

/// Calculate bandwidth in hertz from fourier transformed samples
///
/// Note: The [`Samples`] must be already in Fourier transformed form,
/// e.g. by using [`blocks::Fourier`].
///
/// The `quantile` parameter determines how much energy is allowed to be
/// outside the measured bandwidth (a useful value may be `0.01`).
///
/// [`blocks::Fourier`]: crate::blocks::Fourier
pub fn bandwidth<Flt>(quantile: f64, fourier: &Samples<Complex<Flt>>) -> f64
where
    Flt: Float,
{
    let &Samples {
        sample_rate,
        ref chunk,
    } = fourier;
    let n: usize = chunk.len();
    let mut bins: Vec<f64> = chunk
        .into_iter()
        .map(|x| x.norm_sqr().to_f64().unwrap())
        .collect();
    let mut total = 0.0;
    for &bin in bins.iter() {
        total += bin;
    }
    for bin in bins.iter_mut() {
        *bin /= total;
    }
    let mut covered = 0.0;
    let wrap = (n + 1) / 2;
    let mut bw_count: usize = 0;
    // TODO: improve precision
    for idx in (wrap..n).chain(0..wrap) {
        covered += bins[idx];
        if covered >= quantile / 2.0 && covered < (1.0 - quantile / 2.0) {
            bw_count += 1;
        }
    }
    bw_count as f64 * sample_rate as f64 / n as f64
}
