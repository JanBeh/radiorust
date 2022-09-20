//! Signal processing blocks that can be [connected] with each other
//!
//! This module's submodules contain signal processing blocks, which will
//! [produce] or [consume] data of type `Samples<Complex<Flt>>`, where
//! [`Samples`] are [`chunk`]s of data with a specified [`sample_rate`].
//! [`Complex<Flt>`] is a complex number where real and imaginary part are of
//! type `Flt`.
//! Blocks will require that `Flt` implements [`Float`], i.e. `Flt` is either
//! [`f32`] or [`f64`], depending on desired precision.
//!
//! Note: For real valued samples, use `Complex` with an imaginary part of
//! zero. This allows using blocks which are implemented with a complex fourier
//! transform.
//!
//! Blocks will usually [`spawn`] a [task], and thus require an active
//! [`tokio::runtime::Runtime`]. When the block gets dropped, it may stop
//! working.
//! There is no data structure describing the graph of connected blocks.
//! Instead, any [`Producer<T>`] can be connected with any [`Consumer<T>`], see
//! [`Producer::connect_to_consumer`] or [`Consumer::connect_to_producer`].
//!
//! [connected]: crate::flow
//! [produce]: crate::flow::Producer
//! [consume]: crate::flow::Consumer
//! [`Samples`]: crate::samples::Samples
//! [`chunk`]: crate::samples::Samples::chunk
//! [`sample_rate`]: crate::samples::Samples::sample_rate
//! [`Complex<Flt>`]: num::Complex
//! [`Float`]: crate::genfloat::Float
//! [`spawn`]: tokio::task::spawn
//! [task]: tokio::task
//! [`Producer<T>`]: crate::flow::Producer
//! [`Consumer<T>`]: crate::flow::Consumer
//! [`Producer::connect_to_consumer`]: crate::flow::Producer::connect_to_consumer
//! [`Consumer::connect_to_producer`]: crate::flow::Consumer::connect_to_producer

mod basic;
pub use basic::*;

pub mod filters;
pub mod io;
pub mod modulation;
