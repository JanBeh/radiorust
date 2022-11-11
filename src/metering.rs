//! Metering (e.g. level or bandwidth measurement)

use crate::numbers::*;

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
/// Note: The data (`bins`) must be already in Fourier transformed form,
/// e.g. by using [`blocks::analysis::Fourier`].
///
/// The `double_percentile` parameter determines how much energy is allowed to
/// be outside the measured bandwidth (a useful value may be `0.01`).
///
/// [`blocks::analysis::Fourier`]: crate::blocks::analysis::Fourier
pub fn bandwidth<Flt>(double_percentile: f64, sample_rate: f64, bins: &[Complex<Flt>]) -> f64
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

/// Converts a slice of complex numbers (e.g. a Fourier transformed chunk) into
/// a given count (`resolution`) of real numbers reflecting the energy
///
/// Note that this function expects there to be no wraparound in the middle of
/// the input, e.g. the output of a Fourier transform should be shifted such
/// that the center frequency is in the middle of `input` before this function
/// is called.
pub fn rescale_energy<Flt>(output: &mut Vec<Flt>, resolution: usize, input: &[Complex<Flt>])
where
    Flt: Float,
{
    let n: usize = input.len();
    assert!(n > 0);
    output.resize(resolution, Flt::zero());
    for (output_idx, output) in output.iter_mut().enumerate() {
        let left: Flt = flt!(output_idx) / flt!(resolution) * flt!(n);
        let right: Flt = (flt!(output_idx) + Flt::one()) / flt!(resolution) * flt!(n);
        let left_floor: usize = (left.floor().to_usize().unwrap()).min(n - 1);
        let right_ceil: usize = (right.ceil().to_usize().unwrap()).min(n);
        *output = Flt::zero();
        for input_idx in left_floor..right_ceil {
            let left_bounded: Flt = flt!(input_idx).max(left);
            let right_bounded: Flt = (flt!(input_idx) + Flt::one()).min(right);
            let scale: Flt = right_bounded - left_bounded;
            *output += input[input_idx].norm_sqr() * scale;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::assert_approx;
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
        assert_approx(
            bandwidth(
                0.01,
                48000.0,
                &[Complex::new(0.0, 0.0), Complex::new(0.0, 0.0)],
            ),
            0.0,
        );
    }
    #[test]
    fn test_bandwidth_spreadspectrum() {
        assert_approx(
            bandwidth(
                0.01,
                48000.0,
                &[
                    Complex::new(1.0, 0.0),
                    Complex::new(1.0, 0.0),
                    Complex::new(1.0, 0.0),
                    Complex::new(1.0, 0.0),
                    Complex::new(1.0, 0.0),
                    Complex::new(1.0, 0.0),
                    Complex::new(-1.0, 0.0),
                    Complex::new(f64::sqrt(0.5), -f64::sqrt(0.5)),
                ],
            ),
            0.99 * 48000.0,
        );
    }
    #[test]
    fn test_bandwidth_spreadspectrum_odd() {
        assert_approx(
            bandwidth(
                0.01,
                48000.0,
                &[
                    Complex::new(7.4, -2.1),
                    Complex::new(7.4, -2.1),
                    Complex::new(7.4, -2.1),
                ],
            ),
            0.99 * 48000.0,
        );
    }
    #[test]
    fn test_bandwidth_carrier() {
        assert_approx(
            bandwidth(
                0.01,
                48000.0,
                &[
                    Complex::new(0.0, 0.0),
                    Complex::new(0.0, 0.0),
                    Complex::new(0.0, 0.0),
                    Complex::new(0.0, 0.0),
                    Complex::new(0.0, 0.0),
                    Complex::new(0.0, 0.0),
                    Complex::new(2.1, 0.0),
                    Complex::new(0.0, 0.0),
                ],
            ),
            0.99 * 48000.0 / 8.0,
        );
    }
    #[test]
    fn test_bandwidth_two_carriers() {
        assert_approx(
            bandwidth(
                0.01,
                48000.0,
                &[
                    Complex::new(1.5, 0.0),
                    Complex::new(0.0, 0.0),
                    Complex::new(0.0, 0.0),
                    Complex::new(0.0, 0.0),
                    Complex::new(0.0, 0.0),
                    Complex::new(0.0, 0.0),
                    Complex::new(1.5, 0.0),
                    Complex::new(0.0, 0.0),
                ],
            ),
            2.98 * 48000.0 / 8.0,
        );
    }
    #[test]
    fn test_rescale_energy_same_size() {
        let input = vec![
            Complex::new(0.0, 0.0),
            Complex::new(2.0, 1.0),
            Complex::new(-0.5, 0.0),
        ];
        let mut output = vec![];
        rescale_energy(&mut output, 3, &input);
        assert_eq!(output.len(), 3);
        assert_approx(output[0], 0.0);
        assert_approx(output[1], 5.0);
        assert_approx(output[2], 0.25);
    }
    #[test]
    fn test_rescale_energy_smaller() {
        let input = vec![
            Complex::new(1.0, 0.0),
            Complex::new(2.0, 0.0),
            Complex::new(3.0, 0.0),
            Complex::new(4.0, 0.0),
        ];
        let mut output = vec![];
        rescale_energy(&mut output, 3, &input);
        assert_eq!(output.len(), 3);
        assert_approx(output[0], 2.3333333333333);
        assert_approx(output[1], 8.6666666666667);
        assert_approx(output[2], 19.0);
    }
    #[test]
    fn test_rescale_energy_larger() {
        let input = vec![
            Complex::new(1.0, 0.0),
            Complex::new(2.0, 0.0),
            Complex::new(3.0, 0.0),
        ];
        let mut output = vec![];
        rescale_energy(&mut output, 4, &input);
        assert_eq!(output.len(), 4);
        assert_approx(output[0], 0.75);
        assert_approx(output[1], 2.25);
        assert_approx(output[2], 4.25);
        assert_approx(output[3], 6.75);
    }
}
