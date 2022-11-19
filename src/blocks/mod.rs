//! Signal processing blocks that can be [connected] with each other
//!
//! # Overview
//!
//! This module and its submodules contain signal processing blocks, which will
//! [produce] or [consume] data of type `Signal<Complex<Flt>>`, where
//! [`Signal`] is an `enum` (which implements the [`Message`] trait) that can
//! either be
//!
//! * [`Signal::Samples`], which contains a [`chunk`] of sample data with a
//!   specified [`sample_rate`] or
//! * [`Signal::Event`], which may indicate [interruption] of the sample data
//!   and carries an arbitrary [`payload`].
//!
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
//! [`Producer::feed_into`] or [`Consumer::feed_from`].
//!
//! # Background tasks
//!
//! Blocks will usually [`spawn`] a [task], and thus require an active
//! [`tokio::runtime::Runtime`] while being created. The spawned task will
//! usually keep working even if a block gets dropped as long as there is a
//! connected [`Producer`] and a connected [`Consumer`]. Thus creating circles
//! must be avoided. **Note:** [I/O blocks] are an exception to this rule: when
//! they are dropped, they will stop working.
//!
//! # Buffering and Congestion
//!
//! Refer to the [`flow`] module and the [`Buffer`] block for further
//! information on buffering and congestion.
//!
//! # How to implement your own block(s)
//!
//! To implement your own blocks, have a look at the [`flow`] module as well as
//! the source code of the [`Nop`] and [`NopSignal`] blocks.
//!
//! [connected]: crate::flow
//! [produce]: crate::flow::Producer
//! [consume]: crate::flow::Consumer
//! [`Samples`]: crate::signal::Samples
//! [`chunk`]: crate::signal::Signal::Samples::chunk
//! [`sample_rate`]: crate::signal::Signal::Samples::sample_rate
//! [interruption]: crate::signal::Signal::Event::interrupt
//! [`payload`]: crate::signal::Signal::Event::payload
//! [`Complex<Flt>`]: crate.numbers::Complex
//! [`Float`]: crate::numbers::Float
//! [`Producer<T>`]: crate::flow::Producer
//! [`Consumer<T>`]: crate::flow::Consumer
//! [`Producer::feed_into`]: crate::flow::Producer::feed_into
//! [`Consumer::feed_from`]: crate::flow::Consumer::feed_from
//! [`spawn`]: tokio::task::spawn
//! [task]: tokio::task
//! [I/O blocks]: crate::blocks::io
//! [`flow`]: crate::flow

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
    pub use super::io::{Blackhole, Silence};
    pub use super::resampling::{Downsampler, Upsampler};
    pub use super::transform::{FreqShifter, GainControl, MapSample};
}

pub use self::prelude::*;

use crate::flow::*;
use crate::signal::*;

use tokio::task::spawn;

#[macro_export]
/// Implement [`Consumer`], [`Producer`], and/or [`EventHandling`] for a
/// [signal processing block]
///
/// Synopsis:
/// * `impl_block_trait! { <Flt> Consumer<Signal<Complex<Flt>>> for SomeBlock<Flt> }`
/// * `impl_block_trait! { <Flt> Producer<Signal<Complex<Flt>>> for SomeBlock<Flt> }`
/// * `impl_block_trait! { <Flt> EventHandling for SomeBlock<Flt> }`
/// * `impl_block_trait! { <Flt> all<Signal<Complex<Flt>>> for SomeBlock<Flt> }`
///
/// This presumes that the block has one/all of the following private fields:
///
/// * `receiver_connector: ReceiverConnector<Signal<Complex<Flt>>>`
/// * `sender_connector: SenderConnector<Signal<Complex<Flt>>>`
/// * `event_handlers: EventHandlers`
///
/// See source code of [`NopSignal`] for an example.
///
/// [signal processing block]: crate::blocks
macro_rules! impl_block_trait {
    { $(<$t:ident>)? Consumer<$signal:path> for $type:path } => {
        impl$(<$t>)? $crate::flow::Consumer<$signal> for $type {
            fn receiver_connector(&self) -> &$crate::flow::ReceiverConnector<$signal> {
                &self.receiver_connector
            }
        }
    };
    { $(<$t:ident>)? Producer<$signal:path> for $type:path } => {
        impl$(<$t>)? $crate::flow::Producer<$signal> for $type {
            fn sender_connector(&self) -> &$crate::flow::SenderConnector<$signal> {
                &self.sender_connector
            }
        }
    };
    { $(<$t:ident>)? EventHandling for $type:path } => {
        impl$(<$t>)? $crate::signal::EventHandling for $type {
            fn on_event<F>(&self, func: F) -> $crate::signal::EventHandlerGuard
            where
                F:
                    ::std::ops::FnMut(&::std::sync::Arc<
                        dyn ::std::any::Any + ::std::marker::Send + ::std::marker::Sync
                    >) + ::std::marker::Send + 'static
            {
                $crate::signal::EventHandlers::register(&self.event_handlers, func)
            }
        }
    };
    { $(<$t:ident>)? all<$signal:path> for $type:path } => {
        impl_block_trait! { $(<$t>)? Consumer<$signal> for $type }
        impl_block_trait! { $(<$t>)? Producer<$signal> for $type }
        impl_block_trait! { $(<$t>)? EventHandling for $type }
    };
}

/// Block which performs no operation on the received data and simply sends it
/// out unchanged
///
/// This block mostly serves documentation purposes but may also be used to
/// (re-)connect multiple [`Receiver`]s at once.
///
/// Note that most blocks don't work with `T` but [`Signal<T>`] or
/// `Signal<Complex<Flt>>`.
/// See [`NopSignal<T>`] for an example which does the same as `Nop` but
/// forwards the more concrete `Signal<T>` instead of just `T` and contains
/// some boilerplate code in its implementation.
pub struct Nop<T> {
    receiver_connector: ReceiverConnector<T>,
    sender_connector: SenderConnector<T>,
}

impl_block_trait! { <T> Consumer<T> for Nop<T> }
impl_block_trait! { <T> Producer<T> for Nop<T> }

impl<T> Nop<T>
where
    T: Message + Send + 'static,
{
    /// Creates a block which does nothing but pass data through
    pub fn new() -> Self {
        let (mut receiver, receiver_connector) = new_receiver::<T>();
        let (sender, sender_connector) = new_sender::<T>();
        spawn(async move {
            loop {
                let Ok(msg) = receiver.recv().await else { return; };
                let Ok(()) = sender.send(msg).await else { return; };
            }
        });
        Self {
            receiver_connector,
            sender_connector,
        }
    }
}

/// Same as [`Nop`] but concrete implementation for passing [`Signal<T>`]
/// (implementing [`EventHandling`])
///
/// This block mostly serves documentation purposes but may also be used to
/// (re-)connect multiple [`Receiver`]s at once, and, additionally to [`Nop`],
/// handle [`Signal::Event`]s by registering event handlers with
/// [`EventHandling::on_event`].
pub struct NopSignal<T> {
    receiver_connector: ReceiverConnector<Signal<T>>,
    sender_connector: SenderConnector<Signal<T>>,
    event_handlers: EventHandlers,
}

impl_block_trait! { <T> all<Signal<T>> for NopSignal<T> }

impl<T> NopSignal<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Creates a block which does nothing but pass data through
    pub fn new() -> Self {
        let (mut receiver, receiver_connector) = new_receiver::<Signal<T>>();
        let (sender, sender_connector) = new_sender::<Signal<T>>();
        let event_handlers = EventHandlers::new();
        let evhdl_clone = event_handlers.clone();
        spawn(async move {
            loop {
                let Ok(signal) = receiver.recv().await else { return; };
                match signal {
                    Signal::Samples {
                        sample_rate,
                        chunk: input_chunk,
                    } => {
                        let output_chunk = input_chunk; // no operation here
                        let Ok(()) = sender
                            .send(Signal::Samples { sample_rate, chunk: output_chunk })
                            .await
                        else { return; };
                    }
                    Signal::Event { interrupt, payload } => {
                        if interrupt {
                            // reset state here (but nothing to do)
                        }
                        evhdl_clone.invoke(&payload);
                        let Ok(()) = sender
                            .send(Signal::Event { interrupt, payload })
                            .await
                        else { return; };
                    }
                }
            }
        });
        Self {
            receiver_connector,
            sender_connector,
            event_handlers,
        }
    }
}

#[cfg(test)]
mod tests {}
