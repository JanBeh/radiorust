//! Software defined radio
//!
//! **Note:** This crate is in an early alpha stage.
//!
//! For getting started, have a look at the [`blocks`] module.

#![warn(missing_docs)]

mod sync;

pub mod blocks;
pub mod bufferpool;
pub mod flow;
pub mod math;
pub mod numbers;
pub mod samples;
