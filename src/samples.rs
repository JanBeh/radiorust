//! Sample data type
//!
//! See [`Samples`].

use crate::bufferpool::Chunk;
use crate::flow::Temporal;

/// A chunk of samples with a specified sample rate
///
/// This data type is typically used for `T` in [`Producer<T>`] and
/// [`Consumer<T>`]. Passing this data structure between [blocks] allows each
/// block to be (dynamically) aware of the current sample rate, which
/// simplifies usage.
///
/// [`Producer<T>`]: crate::flow::Producer
/// [`Consumer<T>`]: crate::flow::Consumer
/// [blocks]: crate::blocks
#[derive(Clone, Debug)]
pub struct Samples<T> {
    /// Sample rate
    pub sample_rate: f64,
    /// Sample data
    pub chunk: Chunk<T>,
}

impl<T> Temporal for Samples<T> {
    fn duration(&self) -> f64 {
        self.chunk.len() as f64 / self.sample_rate
    }
}
