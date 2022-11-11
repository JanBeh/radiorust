//! External sources and sinks
//!
//! The [`audio`] and [`rf`] modules contain blocks that allow accessing
//! hardware audio or radio interfaces.
//!
//! **Note:** Blocks in this module will stop working when dropped.

pub mod audio;
pub mod rf;
