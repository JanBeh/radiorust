//! Generic floats and complex numbers
//!
//! This module re-exports [`num::Complex`] as [`Complex`] and provides a
//! [`Float`] trait, which is implemented by [`f32`] and [`f64`].

use rustfft::FftNum;

use std::marker::{Send, Sync};

pub use num::Complex;

/// Trait implemented for [`f32`] and [`f64`]
///
/// This trait is used as bound on functions which support single and double
/// precision calculations.
/// It should not be relied upon that this trait is implemented for other types
/// than [`f32`] or [`f64`], as this can change with minor/patch version bumps.
///
/// See [`flt!`] for an example on how to write functions working with generic
/// floats (i.e. `f32` or `f64`, depending on the caller's choice).
///
/// [`flt!`]: crate::flt
pub trait Float
where
    Self: 'static + Send + Sync,
    Self: num::traits::Float,
    Self: num::traits::FloatConst,
    Self: num::traits::NumAssignOps,
    Self: FftNum,
{
}
impl<T> Float for T
where
    T: 'static + Send + Sync,
    T: num::traits::Float,
    T: num::traits::FloatConst,
    T: num::traits::NumAssignOps,
    T: FftNum,
{
}

/// Macro to convert number into a generic [`Float`] type, which must be in
/// scope as "`Flt`"
///
/// # Example
///
/// ```
/// use radiorust::{flt, numbers::Float};
///
/// fn generic_double<Flt: Float>(arg: Flt) -> Flt {
///     arg * flt!(2)
/// }
/// ```
#[macro_export]
macro_rules! flt {
    ($x:expr) => {
        Flt::from($x).expect("could not convert number into float")
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_generic_float_f32() {
        fn inner<Flt: Float>(x: Flt) -> Flt {
            Flt::from(2.0).unwrap() * x
        }
        assert_eq!(inner(3.5f32), 7.0f32);
    }
    #[test]
    fn test_generic_float_f64() {
        fn inner<Flt: Float>(x: Flt) -> Flt {
            Flt::from(2.0).unwrap() * x
        }
        assert_eq!(inner(3.5f64), 7.0f64);
    }
}
