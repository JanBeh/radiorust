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
/// The `double_percentile` parameter determines how much energy is allowed to
/// be outside the measured bandwidth (a useful value may be `0.01`).
///
/// [`blocks::Fourier`]: crate::blocks::Fourier
pub fn bandwidth<Flt>(double_percentile: f64, fourier: &Samples<Complex<Flt>>) -> f64
where
    Flt: Float,
{
    fn norm_sqr<Flt: Float>(x: &Complex<Flt>) -> f64 {
        x.norm_sqr().to_f64().unwrap()
    }
    fn discount_bins<I: Iterator<Item = usize>, Flt: Float>(
        bins: &[Complex<Flt>],
        energy_limit: f64,
        idcs: I,
    ) -> f64 {
        let mut old_energy: f64 = 0.0;
        let mut used_bins: f64 = 0.0;
        for idx in idcs {
            let new_energy: f64 = old_energy + norm_sqr(&bins[idx]);
            if new_energy > energy_limit {
                used_bins += (energy_limit - old_energy) / (new_energy - old_energy);
                break;
            }
            used_bins += 1.0;
            old_energy = new_energy;
        }
        used_bins
    }
    let &Samples {
        sample_rate,
        chunk: ref bins,
    } = fourier;
    let n: usize = bins.len();
    let total_energy: f64 = bins.iter().map(norm_sqr).sum();
    let energy_limit: f64 = total_energy * double_percentile / 2.0;
    let wrap_idx = n.checked_add(1).unwrap() / 2;
    let idcs = (wrap_idx..n).chain(0..wrap_idx);
    let mut used_bins = 0.0;
    used_bins += discount_bins(bins, energy_limit, idcs.clone());
    used_bins += discount_bins(bins, energy_limit, idcs.rev());
    let bw = (n as f64 - used_bins) * sample_rate as f64 / n as f64;
    if bw > 0.0 {
        bw
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bufferpool::*;
    const PRECISION: f64 = 1e-10;
    fn assert_approx(a: f64, b: f64) {
        if !((a - b).abs() <= PRECISION || (a / b).ln().abs() <= PRECISION) {
            panic!("{a} and {b} are not approximately equal");
        }
    }
    #[test]
    fn test_level_complex_osc() {
        const SQRT_HALF: f64 = 1.0 / std::f64::consts::SQRT_2;
        let vec: Vec<Complex<f64>> = vec![
            Complex::new(1.0, 0.0),
            Complex::new(SQRT_HALF, SQRT_HALF),
            Complex::new(0.0, 1.0),
            Complex::new(-SQRT_HALF, SQRT_HALF),
            Complex::new(-1.0, 0.0),
            Complex::new(-SQRT_HALF, -SQRT_HALF),
            Complex::new(0.0, -1.0),
            Complex::new(SQRT_HALF, -SQRT_HALF),
        ];
        assert_approx(level(&vec).log10() * 10.0, 0.0);
    }
    #[test]
    fn test_bandwidth_silence() {
        let mut buf_pool = ChunkBufPool::<Complex<f64>>::new();
        let mut chunk_buf = buf_pool.get();
        chunk_buf.push(Complex::new(0.0, 0.0));
        chunk_buf.push(Complex::new(0.0, 0.0));
        let chunk = chunk_buf.finalize();
        assert_approx(
            bandwidth(
                0.01,
                &Samples {
                    sample_rate: 48000.0,
                    chunk,
                },
            ),
            0.0,
        );
    }
    #[test]
    fn test_bandwidth_spreadspectrum() {
        let mut buf_pool = ChunkBufPool::<Complex<f64>>::new();
        let mut chunk_buf = buf_pool.get();
        chunk_buf.push(Complex::new(1.0, 0.0));
        chunk_buf.push(Complex::new(1.0, 0.0));
        chunk_buf.push(Complex::new(1.0, 0.0));
        chunk_buf.push(Complex::new(1.0, 0.0));
        chunk_buf.push(Complex::new(1.0, 0.0));
        chunk_buf.push(Complex::new(1.0, 0.0));
        chunk_buf.push(Complex::new(-1.0, 0.0));
        chunk_buf.push(Complex::new(f64::sqrt(0.5), -f64::sqrt(0.5)));
        let chunk = chunk_buf.finalize();
        assert_approx(
            bandwidth(
                0.01,
                &Samples {
                    sample_rate: 48000.0,
                    chunk,
                },
            ),
            0.99 * 48000.0,
        );
    }
    #[test]
    fn test_bandwidth_spreadspectrum_odd() {
        let mut buf_pool = ChunkBufPool::<Complex<f64>>::new();
        let mut chunk_buf = buf_pool.get();
        chunk_buf.push(Complex::new(7.4, -2.1));
        chunk_buf.push(Complex::new(7.4, -2.1));
        chunk_buf.push(Complex::new(7.4, -2.1));
        let chunk = chunk_buf.finalize();
        assert_approx(
            bandwidth(
                0.01,
                &Samples {
                    sample_rate: 48000.0,
                    chunk,
                },
            ),
            0.99 * 48000.0,
        );
    }
    #[test]
    fn test_bandwidth_carrier() {
        let mut buf_pool = ChunkBufPool::<Complex<f64>>::new();
        let mut chunk_buf = buf_pool.get();
        chunk_buf.push(Complex::new(0.0, 0.0));
        chunk_buf.push(Complex::new(0.0, 0.0));
        chunk_buf.push(Complex::new(0.0, 0.0));
        chunk_buf.push(Complex::new(0.0, 0.0));
        chunk_buf.push(Complex::new(0.0, 0.0));
        chunk_buf.push(Complex::new(0.0, 0.0));
        chunk_buf.push(Complex::new(2.1, 0.0));
        chunk_buf.push(Complex::new(0.0, 0.0));
        let chunk = chunk_buf.finalize();
        assert_approx(
            bandwidth(
                0.01,
                &Samples {
                    sample_rate: 48000.0,
                    chunk,
                },
            ),
            0.99 * 48000.0 / 8.0,
        );
    }
    #[test]
    fn test_bandwidth_two_carriers() {
        let mut buf_pool = ChunkBufPool::<Complex<f64>>::new();
        let mut chunk_buf = buf_pool.get();
        chunk_buf.push(Complex::new(1.5, 0.0));
        chunk_buf.push(Complex::new(0.0, 0.0));
        chunk_buf.push(Complex::new(0.0, 0.0));
        chunk_buf.push(Complex::new(0.0, 0.0));
        chunk_buf.push(Complex::new(0.0, 0.0));
        chunk_buf.push(Complex::new(0.0, 0.0));
        chunk_buf.push(Complex::new(1.5, 0.0));
        chunk_buf.push(Complex::new(0.0, 0.0));
        let chunk = chunk_buf.finalize();
        assert_approx(
            bandwidth(
                0.01,
                &Samples {
                    sample_rate: 48000.0,
                    chunk,
                },
            ),
            2.98 * 48000.0 / 8.0,
        );
    }
}
