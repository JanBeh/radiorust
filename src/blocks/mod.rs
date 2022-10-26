//! Signal processing blocks that can be [connected] with each other
//!
//! This module and its submodules contain signal processing blocks, which will
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
//! There is no data structure describing the graph of connected blocks.
//! Instead, any [`Producer<T>`] can be connected with any [`Consumer<T>`], see
//! [`Producer::connect_to_consumer`] or [`Consumer::connect_to_producer`].
//!
//! Blocks will usually [`spawn`] a [task], and thus require an active
//! [`tokio::runtime::Runtime`] while being created. The spawned task will
//! usually keep working even if a block gets dropped as long as there is a
//! connected [`Producer`] and a connected [`Consumer`]. Thus creating circles
//! must be avoided. [I/O blocks] are an exception to this rule: when they are
//! dropped, they will stop working.
//!
//! [connected]: crate::flow
//! [produce]: crate::flow::Producer
//! [consume]: crate::flow::Consumer
//! [`Samples`]: crate::samples::Samples
//! [`chunk`]: crate::samples::Samples::chunk
//! [`sample_rate`]: crate::samples::Samples::sample_rate
//! [`Complex<Flt>`]: crate.numbers::Complex
//! [`Float`]: crate::numbers::Float
//! [`Producer<T>`]: crate::flow::Producer
//! [`Consumer<T>`]: crate::flow::Consumer
//! [`Producer::connect_to_consumer`]: crate::flow::Producer::connect_to_consumer
//! [`Consumer::connect_to_producer`]: crate::flow::Consumer::connect_to_producer
//! [`spawn`]: tokio::task::spawn
//! [task]: tokio::task
//! [I/O blocks]: crate::blocks::io

pub mod analysis;
pub mod buffering;
pub mod chunks;
pub mod filters;
pub mod io;
pub mod modulation;
pub mod morse;
pub mod resampling;
pub mod transform;

/// Re-export of basic blocks
pub mod prelude {
    pub use super::analysis::Fourier;
    pub use super::buffering::Buffer;
    pub use super::chunks::{Overlapper, Rechunker};
    pub use super::filters::Filter;
    pub use super::resampling::{Downsampler, Upsampler};
    pub use super::transform::{FreqShifter, Function};
}

pub use self::prelude::*;

use crate::flow::*;

use tokio::task::spawn;

/// Block which performs no operation on the received data and simply sends it
/// out unchanged
///
/// This block mostly serves documentation purposes but may also be used to
/// (re-)connect multiple [`Receiver`]s at once.
/// Note that most blocks don't work with `T` but [`Samples<T>`] or
/// `Samples<Complex<Flt>>`.
///
/// [`Samples<T>`]: crate::samples::Samples
pub struct Nop<T> {
    receiver_connector: ReceiverConnector<T>,
    sender_connector: SenderConnector<T>,
}

impl<T> Consumer<T> for Nop<T> {
    fn receiver_connector(&self) -> &ReceiverConnector<T> {
        &self.receiver_connector
    }
}

impl<T> Producer<T> for Nop<T> {
    fn sender_connector(&self) -> &SenderConnector<T> {
        &self.sender_connector
    }
}

impl<T> Nop<T>
where
    T: Clone + Send + 'static,
{
    /// Creates a block which does nothing but pass data through
    pub fn new() -> Self {
        let (mut receiver, receiver_connector) = new_receiver::<T>();
        let (sender, sender_connector) = new_sender::<T>();
        spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(msg) => {
                        if let Err(_) = sender.send(msg).await {
                            return;
                        }
                    }
                    Err(err) => {
                        if let Err(_) = sender.forward_error(err).await {
                            return;
                        }
                        if err == RecvError::Closed {
                            return;
                        }
                    }
                }
            }
        });
        Self {
            receiver_connector,
            sender_connector,
        }
    }
}

#[cfg(test)]
mod tests {}
