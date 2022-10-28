//! Window functions

use crate::math::*;

/// Window function
pub trait Window {
    /// Get value at position `x` (where `x` ranges from `-1.0` to `1.0`)
    /// multiplied with unknown constant
    fn relative_value_at(&self, x: f64) -> f64;
}

/// Rectangular window
#[derive(Clone, Debug)]
pub struct Rectangular;

impl Window for Rectangular {
    fn relative_value_at(&self, _: f64) -> f64 {
        1.0
    }
}

/// Kaiser window
#[derive(Clone, Debug)]
pub struct Kaiser {
    beta: f64,
}

impl Window for Kaiser {
    fn relative_value_at(&self, x: f64) -> f64 {
        kaiser_rel_with_beta(self.beta, x)
    }
}

impl Kaiser {
    /// Kaiser window with given `beta` parameter
    pub fn with_beta(beta: f64) -> Self {
        Self { beta }
    }
    /// Kaiser window with given `alpha` parameter
    pub fn with_alpha(alpha: f64) -> Self {
        Self {
            beta: kaiser_alpha_to_beta(alpha),
        }
    }
    /// Kaiser window with first null at `n` bins beside main lobe
    pub fn with_null_at_bin(n: f64) -> Self {
        Self {
            beta: kaiser_null_at_bin_to_beta(n),
        }
    }
}

/// Custom window provided by closure
///
/// For the struct to implement the [`Window`] trait, the closure `F` must be
/// `Fn(f64) -> f64` and accept input and return values according to the
/// [`Window::relative_value_at`] method.
pub struct CustomWindow<F>(pub F);

impl<F> Window for CustomWindow<F>
where
    F: Fn(f64) -> f64,
{
    fn relative_value_at(&self, x: f64) -> f64 {
        (self.0)(x)
    }
}
