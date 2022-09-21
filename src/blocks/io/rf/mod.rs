//! Blocks interfacing RF hardware
//!
//! Use feature "`soapysdr`" for SoapySDR support.

#[cfg(feature = "soapysdr")]
mod soapy;

#[cfg(feature = "soapysdr")]
pub use soapy::*;

#[cfg(test)]
mod tests {}
