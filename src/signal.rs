//! [`Signal`] type containing [sample data] and [events], and handling of
//! those events
//!
//! [sample data]: Signal::Samples
//! [events]: Signal::Event

use crate::bufferpool::Chunk;
use crate::flow::Message;

use tokio::sync::mpsc;

use std::any::Any;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, Weak};

/// Types that can be used for [`Signal::Event`]
pub trait Event: Any + Debug + Send + Sync {
    /// True if [`Signal::Samples`] data before and after the event is not
    /// seamlessly connected
    fn is_interrupt(&self) -> bool {
        false
    }
    /// True if flushing of previously sent [`Signal::Samples`] data is desired
    fn is_flush(&self) -> bool {
        false
    }
    /// Returns dynamic reference, suitable for type checking and downcasting
    fn as_any(&self) -> &(dyn Any + Send + Sync);
}

/// Unit struct indicating a disconnection of [connected] blocks
///
/// [connected]: crate::flow
#[derive(Clone, Debug)]
pub struct Disconnection;

impl Event for Disconnection {
    fn is_interrupt(&self) -> bool {
        true
    }
    fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
        self
    }
}

type BoxedCallback = Box<dyn FnMut(&Arc<dyn Event>) + Send>;

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
                callbacks
                    .lock()
                    .unwrap()
                    .callbacks
                    .retain(|x| x.id != self.id);
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
    pub fn register<F: FnMut(&Arc<dyn Event>) + Send + 'static>(
        &self,
        func: F,
    ) -> EventHandlerGuard {
        let mut registry = self.0.lock().unwrap();
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
    /// Invoke all event handlers
    pub fn invoke(&self, event: &Arc<dyn Event>) {
        for IdentifiedCallback { callback, .. } in self.0.lock().unwrap().callbacks.iter_mut() {
            callback(event);
        }
    }
}

/// Implemented by [signal processing blocks] which support [event] handling
///
/// [signal processing blocks]: crate::blocks
/// [event]: Signal::Event
pub trait EventHandling {
    /// Register event handler
    fn on_event<F: FnMut(&Arc<dyn Event>) + Send + 'static>(&self, func: F) -> EventHandlerGuard;
    /// Wait for closure to return true on event
    fn wait_for_event<F>(&self, mut func: F) -> Pin<Box<dyn Future<Output = ()> + Send>>
    where
        F: FnMut(&Arc<dyn Event>) -> bool + Send + 'static,
    {
        let (waiter_tx, mut waiter_rx) = mpsc::unbounded_channel::<()>();
        let handle = self.on_event(move |event| {
            if func(event) {
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
    /// Special event (may be created with [`Signal::new_event`])
    Event(
        /// Dynamic [`Event`]
        Arc<dyn Event>,
    ),
}

impl<T> Signal<T> {
    /// Create new [`Signal::Event`]
    pub fn new_event<E: Event>(event: E) -> Self {
        Self::Event(Arc::new(event))
    }
    /// Is message a special event?
    pub fn is_event(&self) -> bool {
        match self {
            Signal::Samples { .. } => false,
            Signal::Event(_) => true,
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
        Some(Signal::new_event(Disconnection))
    }
}
