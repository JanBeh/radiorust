//! Mathematic functions helpful for signal processing

use crate::flt;
use crate::genfloat::Float;

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

/// Return value of Kaiser window with given `beta` multiplied with unknown
/// constant
pub fn relative_kaiser_beta<Flt: Float>(beta: Flt, x: Flt) -> Flt {
    bessel_I0(beta * Flt::sqrt(flt!(1) - x * x))
}

/// Returns a closure that calculates the Kaiser window for a given `beta`
pub fn kaiser_fn_with_beta<Flt: Float>(beta: Flt) -> impl Fn(Flt) -> Flt {
    let scale = bessel_I0(beta).recip();
    move |x| relative_kaiser_beta(beta, x) * scale
}

/// Returns a closure that calculates the Kaiser window for a given `alpha`
pub fn kaiser_fn_with_alpha<Flt: Float>(alpha: Flt) -> impl Fn(Flt) -> Flt {
    kaiser_fn_with_beta(alpha * Flt::PI())
}

/// Returns a closure that calculates the Kaiser window with first null at `n`
/// bins beside main lobe
pub fn kaiser_fn_with_null_at_bin<Flt: Float>(n: Flt) -> impl Fn(Flt) -> Flt {
    kaiser_fn_with_alpha(Flt::sqrt(n * n - flt!(1)))
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
    const PRECISION: f64 = 1e-10;
    fn assert_approx(a: f64, b: f64) {
        assert!((a - b).abs() <= PRECISION || (a / b).ln().abs() <= PRECISION);
    }
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
