//! Blocks accessing audio interfaces
//!
//! Use feature "`cpal`" for audio interface support.

#[cfg(feature = "cpal")]
pub mod cpal;

#[cfg(test)]
mod tests {}
