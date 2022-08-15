//! Signal processing blocks that can be [connected] with each other
//!
//! This module's submodules contain signal processing blocks, which will
//! [produce] or [consume] data of type `Samples<Complex<Flt>>`.
//!
//! [`Samples`] are [`chunk`]s of data with a specified [`sample_rate`].
//! [`Complex<Flt>`] is a complex number where real and imaginary part are of
//! type `Flt`.
//! Blocks will require that `Flt` implements [`Float`], i.e. `Flt` is either
//! [`f32`] or [`f64`], depending on desired precision.
//!
//! For real valued samples, use `Complex` with an imaginary part of zero.
//! This allows using blocks which are implemented with a complex fourier
//! transform.
//!
//! Blocks will usually [`spawn`] a [task], and thus require an active
//! [`tokio::runtime::Runtime`]. When the block gets dropped, it may stop
//! working.
//!
//! [connected]: crate::flow
//! [produce]: crate::flow::Producer
//! [consume]: crate::flow::Consumer
//! [`chunk`]: Samples::chunk
//! [`sample_rate`]: Samples::sample_rate
//! [`Complex<Flt>`]: num::Complex
//! [`Float`]: crate::genfloat::Float
//! [`spawn`]: tokio::task::spawn
//! [task]: tokio::task

use crate::bufferpool::*;

pub mod convert;
pub mod filters;
pub mod io;
pub mod modulation;

/// A chunk of samples with a specified sample rate
#[derive(Clone, Debug)]
pub struct Samples<T> {
    /// Sample rate
    pub sample_rate: f64,
    /// Sample data
    pub chunk: Chunk<T>,
}
