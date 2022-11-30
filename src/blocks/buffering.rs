//! Buffering

use crate::flow::*;
use crate::impl_block_trait;
use crate::signal::*;

use tokio::select;
use tokio::task::spawn;

use std::collections::VecDeque;
use std::future::pending;
use std::time::Instant;

const QUEUE_MAX_EVENTS: usize = 256;

/// Types used for [`Signal::Event`]
pub mod events {
    use super::*;
    /// Sent by [`Buffer`] block when some samples have been dropped
    #[derive(Clone, Debug)]
    pub struct BufferOverflow;
    impl Event for BufferOverflow {
        fn is_interrupt(&self) -> bool {
            true
        }
        fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
            self
        }
    }
}
use events::*;

struct TemporalQueueEntry<T> {
    instant: Instant,
    signal: Signal<T>,
}

/// Queue which tracks the duration and age of stored elements
struct TemporalQueue<T> {
    queue: VecDeque<TemporalQueueEntry<T>>,
    duration: f64,
    event_count: usize,
}

impl<T> TemporalQueue<T> {
    /// Create empty queue
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            duration: 0.0,
            event_count: 0,
        }
    }
    fn update_duration(&mut self) {
        self.duration = 0.0;
        for entry in self.queue.iter() {
            self.duration += entry.signal.duration();
        }
    }
    /// Is queue empty?
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
    /// Append at back of queue
    pub fn push(&mut self, signal: Signal<T>) {
        let is_event = signal.is_event();
        self.queue.push_back(TemporalQueueEntry {
            instant: Instant::now(),
            signal,
        });
        if is_event {
            self.event_count += 1;
        }
        self.update_duration();
    }
    /// Pop from front of queue
    pub fn pop(&mut self) -> Option<Signal<T>> {
        let popped = self.queue.pop_front().map(|entry| entry.signal);
        if let Some(signal) = &popped {
            if signal.is_event() {
                self.event_count -= 1;
            }
        }
        self.update_duration();
        popped
    }
    /// Duration in seconds
    pub fn duration(&self) -> f64 {
        self.duration
    }
    /// Age of oldest entry (`0.0` if empty)
    pub fn age(&self) -> f64 {
        self.queue
            .front()
            .map(|entry| entry.instant.elapsed().as_secs_f64())
            .unwrap_or(0.0)
    }
    /// Number of entries
    pub fn len(&self) -> usize {
        self.queue.len()
    }
    /// Number of [`Signal::Event`] entries
    pub fn event_count(&self) -> usize {
        self.event_count
    }
    /// Check if next entry is a [`Signal::Event`]
    ///
    /// Panics if queue is empty.
    pub fn leading_event(&self) -> bool {
        self.queue[0].signal.is_event()
    }
}

/// Buffer management
///
/// This struct is a [`Consumer`] and [`Producer`] which forwards data from a
/// connected [`Producer`] to all connected [`Consumer`]s while performing
/// buffering.
///
/// This block may also be used to "suck" parasitic buffers empty. As each
/// [`flow::Sender`] has a capacity of `1` (see underlying [`broadcast_bp`]
/// channel), a chain of [signal processing blocks] may accumulate a
/// significant buffer volume. This may be unwanted.
/// By placing a `Buffer` block near the end of the chain and providing a
/// `max_age` argument equal to or smaller than `max_capacity` to
/// [`Buffer::new`], the buffer block will consume (and discard) data even when
/// its connected consumer isn't fast enough.
///
/// [`flow::Sender`]: crate::flow::Sender
/// [`broadcast_bp`]: crate::sync::broadcast_bp
/// [signal processing blocks]: crate::blocks
pub struct Buffer<T> {
    receiver_connector: ReceiverConnector<Signal<T>>,
    sender_connector: SenderConnector<Signal<T>>,
}

impl_block_trait! { <T> Consumer<Signal<T>> for Buffer<T> }
impl_block_trait! { <T> Producer<Signal<T>> for Buffer<T> }

impl<T> Buffer<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Create new [`Buffer`]
    ///
    /// The buffer will start with buffering `initial_capacity` seconds of
    /// data before beginning to send out received data.
    /// When empty, the buffer will buffer data corresponding to a duration of
    /// at least `min_capacity` seconds before sending out data again.
    /// It will suspend receiving when holding strictly more than
    /// `max_capacity` seconds of data.
    /// If buffered data is held longer than `max_age` seconds, it will be
    /// discarded.
    pub fn new(initial_capacity: f64, min_capacity: f64, max_capacity: f64, max_age: f64) -> Self {
        let (mut receiver, receiver_connector) = new_receiver::<Signal<T>>();
        let (sender, sender_connector) = new_sender::<Signal<T>>();
        spawn(async move {
            let mut initial = true;
            let mut underrun = true;
            let mut shutdown = false;
            let mut marked_missing = false;
            let mut queue = TemporalQueue::<T>::new();
            loop {
                if queue.is_empty() && shutdown {
                    break;
                }
                enum Action<'a, T> {
                    Fill(Signal<T>),
                    Drain(Reservation<'a, Signal<T>>),
                    Close,
                    Exit,
                }
                match select! {
                    action = async {
                        if shutdown || !(queue.duration() <= max_capacity && queue.event_count() < QUEUE_MAX_EVENTS) {
                            pending::<()>().await;
                        }
                        match receiver.recv().await {
                            Ok(signal) => Action::Fill(signal),
                            Err(_) => Action::Close,
                        }
                    } => action,
                    action = async {
                        if underrun && !shutdown {
                            pending::<()>().await;
                        }
                        match sender.reserve().await {
                            Ok(reservation) => Action::Drain(reservation),
                            Err(_) => Action::Exit,
                        }
                    } => action,
                } {
                    Action::Fill(signal) => {
                        queue.push(signal);
                        if initial {
                            if queue.duration() >= initial_capacity {
                                underrun = false;
                                initial = false;
                            }
                        } else {
                            if queue.duration() >= min_capacity {
                                underrun = false;
                            }
                        }
                        match sender.try_reserve() {
                            Ok(Some(reservation)) => {
                                let mut reservation = Some(reservation);
                                if queue.len() > 1
                                    && queue.age() > max_age
                                    && !queue.leading_event()
                                {
                                    while queue.len() > 1 && queue.age() > max_age {
                                        queue.pop();
                                    }
                                    if !marked_missing {
                                        reservation
                                            .take()
                                            .unwrap()
                                            .send(Signal::new_event(BufferOverflow));
                                        marked_missing = true;
                                    }
                                }
                                if let Some(reservation) = reservation {
                                    reservation.send(queue.pop().unwrap());
                                    marked_missing = false;
                                }
                            }
                            Ok(None) => (),
                            Err(_) => shutdown = true,
                        }
                    }
                    Action::Drain(reservation) => {
                        let mut reservation = Some(reservation);
                        if queue.age() > max_age && !queue.leading_event() {
                            while queue.age() > max_age {
                                if queue.pop().is_none() {
                                    break;
                                }
                            }
                            if !marked_missing {
                                reservation
                                    .take()
                                    .unwrap()
                                    .send(Signal::new_event(BufferOverflow));
                                marked_missing = true;
                            }
                        }
                        if let Some(reservation) = reservation {
                            if let Some(signal) = queue.pop() {
                                reservation.send(signal);
                                marked_missing = false;
                            } else {
                                underrun = true;
                            }
                        }
                    }
                    Action::Close => shutdown = true,
                    Action::Exit => return,
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
