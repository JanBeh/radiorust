//! [`Signal`] type containing [sample data] and [events], and handling of
//! those events
//!
//! [sample data]: Signal::Samples
//! [events]: Signal::Event

use crate::bufferpool::Chunk;
use crate::flow::{Disconnection, Message};

use parking_lot::Mutex;
use tokio::sync::mpsc;

use std::any::Any;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Weak};

type BoxedCallback = Box<dyn FnMut(&Arc<dyn Any + Send + Sync>) + Send>;

struct IdentifiedCallback {
    callback: BoxedCallback,
    id: u64,
}

#[derive(Default)]
struct CallbackRegistry {
    callbacks: Vec<IdentifiedCallback>,
    next_id: u64,
}

/// Synchronized list of callbacks for handling [`Signal::Event`]s
#[derive(Clone, Default)]
pub struct EventHandlers(Arc<Mutex<CallbackRegistry>>);

impl EventHandlers {
    /// Empty list of event handlers
    pub fn new() -> Self {
        Default::default()
    }
}

/// Guard which unregisters an event handler when dropped
///
/// This guard is returned by [`EventHandlers::register`] and
/// [`EventHandling::on_event`]. Use the [`forget`] method to consume the guard
/// without unregistering the handler.
///
/// [`forget`]: EventHandlerGuard::forget
#[must_use]
pub struct EventHandlerGuard {
    callbacks: Weak<Mutex<CallbackRegistry>>,
    id: u64,
    auto: bool,
}

impl Drop for EventHandlerGuard {
    fn drop(&mut self) {
        if self.auto {
            if let Some(callbacks) = Weak::upgrade(&self.callbacks) {
                callbacks.lock().callbacks.retain(|x| x.id != self.id);
            }
        }
    }
}

impl EventHandlerGuard {
    /// Unregister event handler (same as dropping)
    pub fn unregister(self) {}
    /// Consume guard without unregistering event handler
    pub fn forget(mut self) {
        self.auto = false;
    }
}

impl EventHandlers {
    /// Register event handler
    pub fn register<F: FnMut(&Arc<dyn Any + Send + Sync>) + Send + 'static>(
        &self,
        func: F,
    ) -> EventHandlerGuard {
        let mut registry = self.0.lock();
        let boxed_callback: BoxedCallback = Box::new(func);
        let id = registry.next_id;
        registry.next_id += 1;
        registry.callbacks.push(IdentifiedCallback {
            callback: boxed_callback,
            id,
        });
        drop(registry);
        EventHandlerGuard {
            callbacks: Arc::downgrade(&self.0),
            id,
            auto: true,
        }
    }
    /// Invoke all event handlers for given [`payload`]
    ///
    /// [`payload`]: `Signal::Event::payload
    pub fn invoke(&self, payload: &Arc<dyn Any + Send + Sync>) {
        for IdentifiedCallback { callback, .. } in self.0.lock().callbacks.iter_mut() {
            callback(&payload);
        }
    }
}

/// Implemented by [signal processing blocks] which support [event] handling
///
/// [signal processing blocks]: crate::blocks
/// [event]: Signal::Event
pub trait EventHandling {
    /// Register event handler
    fn on_event<F: FnMut(&Arc<dyn Any + Send + Sync>) + Send + 'static>(
        &self,
        func: F,
    ) -> EventHandlerGuard;
    /// Wait for closure to return true on event
    fn wait_for_event<F>(&self, mut func: F) -> Pin<Box<dyn Future<Output = ()> + Send>>
    where
        F: FnMut(&Arc<dyn Any + Send + Sync>) -> bool + Send + 'static,
    {
        let (waiter_tx, mut waiter_rx) = mpsc::unbounded_channel::<()>();
        let handle = self.on_event(move |payload| {
            if func(payload) {
                waiter_tx.send(()).ok();
            }
        });
        Box::pin(async move {
            waiter_rx.recv().await.unwrap();
            handle.unregister();
        })
    }
}

/// [`Message`] used by [signal processing blocks], which may contain
/// [sample data] or special [events]
///
/// [signal processing blocks]: crate::blocks
/// [sample data]: Signal::Samples
/// [events]: Signal::Event
#[derive(Clone, Debug)]
pub enum Signal<T> {
    /// Normal data
    Samples {
        /// Sample rate
        sample_rate: f64,
        /// Sample data
        chunk: Chunk<T>,
    },
    /// Special event
    Event {
        /// Event indicates that previous [`Samples`] are not connected to
        /// following `Samples`
        ///
        /// [`Samples`]: Signal::Samples
        interrupt: bool,
        /// Dynamic payload
        payload: Arc<dyn Any + Send + Sync>,
    },
}

impl<T> Signal<T> {
    /// Is message a special event?
    pub fn is_event(&self) -> bool {
        match self {
            Signal::Samples { .. } => false,
            Signal::Event { .. } => true,
        }
    }
    /// Duration in seconds (or `0.0` for [events])
    ///
    /// [events]: Signal::Event
    pub fn duration(&self) -> f64 {
        match self {
            Signal::Samples { sample_rate, chunk } => chunk.len() as f64 / sample_rate,
            _ => 0.0,
        }
    }
}

impl<T> Message for Signal<T>
where
    T: Clone,
{
    fn disconnection() -> Option<Self> {
        Some(Signal::Event {
            interrupt: true,
            payload: Arc::new(Disconnection),
        })
    }
}
