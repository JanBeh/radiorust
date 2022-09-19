//! Blocks accessing audio interfaces
//!
//! Use feature "`cpal`" for audio interface support.

#[cfg(feature = "cpal")]
mod cpal;

#[cfg(feature = "cpal")]
pub use self::cpal::*;

#[cfg(test)]
mod tests {}
