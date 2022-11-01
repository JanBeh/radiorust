//! External sources and sinks
//!
//! The [`audio`] and [`rf`] modules contain blocks that allow accessing
//! hardware audio or radio interfaces.
//! The [`raw`] module allows reading or writing I/Q data as bytes (e.g.
//! from/to files).
//!
//! **Note:** Blocks in this module will stop working when dropped.

pub mod audio;
pub mod raw;
pub mod rf;
