//! Blocks interfacing RF hardware
//!
//! Use feature "`soapysdr`" for SoapySDR support.

#[cfg(feature = "soapysdr")]
pub mod soapysdr;

#[cfg(test)]
mod tests {}
