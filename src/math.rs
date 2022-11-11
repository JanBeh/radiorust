//! Mathematic functions helpful for signal processing

use crate::numbers::*;

/// Modified Bessel function of the first kind of order zero
#[allow(non_snake_case)]
pub fn bessel_I0<Flt: Float>(x: Flt) -> Flt {
    let base = x * x / flt!(4);
    let mut addend = flt!(1);
    let mut sum = flt!(1);
    for i in 1.. {
        addend *= base / flt!(i * i);
        let old = sum;
        sum += addend;
        if sum == old || !sum.is_finite() {
            break;
        }
    }
    sum
}

/// Return value (multiplied with unknown constant) of Kaiser window with given
/// `beta`
///
/// The argument `x` ranges from `-1.0` to `1.0`.
pub fn kaiser_rel_with_beta<Flt: Float>(beta: Flt, x: Flt) -> Flt {
    bessel_I0(beta * Flt::sqrt(flt!(1) - x * x))
}

/// Convert `alpha` parameter to `beta` parameter of Kaiser window
pub fn kaiser_alpha_to_beta<Flt: Float>(alpha: Flt) -> Flt {
    alpha * Flt::PI()
}

/// `beta` parameter of Kaiser window with first null at `n` bins beside main
/// lobe
pub fn kaiser_null_at_bin_to_beta<Flt: Float>(n: Flt) -> Flt {
    Flt::sqrt(n * n - flt!(1))
}

/// Normalized sinc function *sin(πx) / (πx)*
pub fn sinc<Flt: Float>(x: Flt) -> Flt {
    if x.is_zero() {
        Flt::one()
    } else {
        let t = x * Flt::PI();
        t.sin() / t
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::assert_approx;
    #[test]
    #[allow(non_snake_case)]
    fn test_bessel_I0() {
        assert_eq!(bessel_I0(0.0), 1.0);
        assert_eq!(bessel_I0(f64::NEG_INFINITY), f64::INFINITY);
        assert_eq!(bessel_I0(f64::INFINITY), f64::INFINITY);
        assert!(bessel_I0(f64::NAN).is_nan());
        assert_approx(bessel_I0(0.5), 1.06348337074132);
        assert_approx(bessel_I0(-0.5), 1.06348337074132);
        assert_approx(bessel_I0(1.23), 1.41552757215846);
        assert_approx(bessel_I0(15.8), 736184.938479417);
        assert_approx(bessel_I0(456.0), 2.04094157812291e+196);
        assert_eq!(bessel_I0(1000.0), f64::INFINITY);
        assert_eq!(bessel_I0(-1000.0), f64::INFINITY);
    }
    #[test]
    fn test_sinc() {
        assert_eq!(sinc(0.0), 1.0);
        assert_approx(sinc(0.4), 0.756826728640657);
        assert_approx(sinc(-0.4), 0.756826728640657);
        assert_approx(sinc(1.0), 0.0);
        assert_approx(sinc(-1.0), 0.0);
        assert_approx(sinc(2.0), 0.0);
        assert_approx(sinc(2.6), 0.11643488132933186);
        assert_approx(sinc(-2.6), 0.11643488132933186);
        assert_approx(sinc(5.8), -0.03225825116512552);
        assert_approx(sinc(-5.8), -0.03225825116512552);
        assert_approx(sinc(17.0), 0.0);
        assert_approx(sinc(2345.0), 0.0);
        assert_approx(sinc(-2345.0), 0.0);
    }
}
