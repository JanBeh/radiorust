//! Blocks interfacing RF hardware
//!
//! Use feature "`soapy`" for SoapySDR support.

#[cfg(feature = "soapy")]
mod soapy;

#[cfg(feature = "soapy")]
pub use soapy::*;

#[cfg(test)]
mod tests {}
