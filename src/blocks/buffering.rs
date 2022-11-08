//! Buffering

use crate::flow::*;
use crate::samples::*;

use tokio::select;
use tokio::task::spawn;

use std::collections::VecDeque;
use std::future::pending;
use std::time::Instant;

#[derive(Clone, Debug)]
enum Message<T> {
    Value(T),
    Reset,
    Finished,
}

struct TemporalQueueEntry<T> {
    instant: Instant,
    data: T,
}

/// Queue which tracks the duration and age of stored elements
struct TemporalQueue<T> {
    queue: VecDeque<TemporalQueueEntry<T>>,
    duration: f64,
}

impl<T> TemporalQueue<T>
where
    T: Temporal,
{
    /// Create empty queue
    pub fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            duration: 0.0,
        }
    }
    fn update_duration(&mut self) {
        self.duration = 0.0;
        for entry in self.queue.iter() {
            self.duration += entry.data.duration();
        }
    }
    /// Is queue empty?
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
    /// Append at back of queue
    pub fn push(&mut self, element: T) {
        self.queue.push_back(TemporalQueueEntry {
            instant: Instant::now(),
            data: element,
        });
        self.update_duration();
    }
    /// Pop from front of queue
    pub fn pop(&mut self) -> Option<T> {
        let popped = self.queue.pop_front().map(|entry| entry.data);
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
}

impl<T> Temporal for Message<T>
where
    T: Temporal,
{
    fn duration(&self) -> f64 {
        match self {
            Message::Value(value) => value.duration(),
            Message::Reset => 0.0,
            Message::Finished => 0.0,
        }
    }
}

/// Buffering mechanism for [`Temporal`] data
///
/// This struct is a [`Consumer`] and [`Producer`] which forwards data from a
/// connected [`Producer`] to all connected [`Consumer`]s while performing
/// buffering.
///
/// This block may also be used to "suck" parasitic buffers empty. As each
/// [`flow::Sender`] has a capacity of `1` (see underlying [`broadcast_bp`]
/// channel), a chain of [blocks] may accumulate a significant buffer volume.
/// This may be unwanted.
/// By placing a `Buffer` block near the end of the chain and providing a
/// `max_age` argument equal to or smaller than `max_capacity` to
/// [`Buffer::new`], the buffer block will consume (and discard) data even when
/// its connected consumer isn't fast enough.
///
/// [`flow::Sender`]: crate::flow::Sender
/// [`broadcast_bp`]: crate::sync::broadcast_bp
/// [blocks]: crate::blocks
pub struct Buffer<T> {
    receiver_connector: ReceiverConnector<T>,
    sender_connector: SenderConnector<T>,
}

impl<T> Consumer<T> for Buffer<T> {
    fn receiver_connector(&self) -> &ReceiverConnector<T> {
        &self.receiver_connector
    }
}

impl<T> Producer<T> for Buffer<T> {
    fn sender_connector(&self) -> &SenderConnector<T> {
        &self.sender_connector
    }
}

impl<T> Buffer<T>
where
    T: Clone + Send + Sync + 'static,
    T: Temporal,
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
        let (mut receiver, receiver_connector) = new_receiver::<T>();
        let (sender, sender_connector) = new_sender::<T>();
        spawn(async move {
            let mut initial = true;
            let mut underrun = true;
            let mut closed = false;
            let mut reset_sent = false;
            let mut queue: TemporalQueue<Message<T>> = TemporalQueue::new();
            loop {
                if queue.is_empty() && closed {
                    break;
                }
                enum Action<'a, T> {
                    Fill(Message<T>),
                    Drain(Reservation<'a, T>),
                    Close,
                    Exit,
                }
                match select! {
                    action = async {
                        if closed || !(queue.duration() <= max_capacity) {
                            pending::<()>().await;
                        }
                        match receiver.recv().await {
                            Ok(data) => Action::Fill(Message::Value(data)),
                            Err(err) => match err {
                                RecvError::Reset => Action::Fill(Message::Reset),
                                RecvError::Finished => Action::Fill(Message::Finished),
                                RecvError::Closed => Action::Close,
                            }
                        }
                    } => action,
                    action = async {
                        if underrun {
                            pending::<()>().await;
                        }
                        match sender.reserve().await {
                            Ok(reservation) => Action::Drain(reservation),
                            Err(_) => Action::Exit,
                        }
                    } => action,
                } {
                    Action::Fill(message) => {
                        queue.push(message);
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
                                if queue.len() > 1 && queue.age() > max_age {
                                    while queue.len() > 1 && queue.age() > max_age {
                                        queue.pop();
                                    }
                                    if !reset_sent {
                                        reservation.take().unwrap().reset();
                                        reset_sent = true;
                                    }
                                }
                                if let Some(reservation) = reservation {
                                    let message = queue.pop().unwrap();
                                    match message {
                                        Message::Value(value) => reservation.send(value),
                                        Message::Reset => reservation.reset(),
                                        Message::Finished => reservation.finish(),
                                    };
                                    reset_sent = false;
                                }
                            }
                            Ok(None) => (),
                            Err(_) => closed = true,
                        }
                    }
                    Action::Drain(reservation) => {
                        let mut reservation = Some(reservation);
                        if queue.age() > max_age {
                            while queue.age() > max_age {
                                if queue.pop().is_none() {
                                    break;
                                }
                            }
                            if !reset_sent {
                                reservation.take().unwrap().reset();
                                reset_sent = true;
                            }
                        }
                        if let Some(reservation) = reservation {
                            if let Some(message) = queue.pop() {
                                match message {
                                    Message::Value(value) => reservation.send(value),
                                    Message::Reset => reservation.reset(),
                                    Message::Finished => reservation.finish(),
                                };
                                reset_sent = false;
                            } else {
                                underrun = true;
                            }
                        }
                    }
                    Action::Close => {
                        closed = true;
                        underrun = false;
                    }
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
