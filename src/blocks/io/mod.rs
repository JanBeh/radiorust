//! External sources and sinks
//!
//! The [`audio`] and [`rf`] modules contain blocks that allow accessing
//! hardware audio or radio interfaces. The [`raw_in`] and [`raw_out`] modules
//! allow reading or writing I/Q data as bytes (e.g. from/to files).

pub mod audio;
pub mod raw_in;
pub mod raw_out;
pub mod rf;
