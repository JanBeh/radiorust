//! Blocks accessing audio interfaces
//!
//! Use feature "`cpal`" for audio interface support.
//!
//! **Note: The API of this module is highly unstable yet and subject to change.**

#[cfg(feature = "cpal")]
mod cpal;

#[cfg(feature = "cpal")]
pub use self::cpal::*;

#[cfg(test)]
mod tests {}
