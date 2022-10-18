//! Data flow between [blocks]
//!
//! # Example
//!
//! The following toy example passes a `String` from a [`Producer`] to a
//! [`Consumer`]. For radio applications, you will usually pass [`Samples`]
//! instead.
//!
//! [`Samples`]: crate::samples::Samples
//!
//! ```
//! # tokio::runtime::Runtime::new().unwrap().block_on(async move {
//! use radiorust::flow::*;
//!
//! struct MySource {
//!     sender: Sender<String>,
//!     /* extra fields can go here */
//! }
//! impl Producer<String> for MySource {
//!     fn sender_connector(&self) -> SenderConnector<'_, String> {
//!         self.sender.connector()
//!     }
//! }
//!
//! struct MySink {
//!     receiver: Receiver<String>,
//!     /* extra fields can go here */
//! }
//! impl Consumer<String> for MySink {
//!     fn receiver_connector(&self) -> ReceiverConnector<'_, String> {
//!         self.receiver.connector()
//!     }
//! }
//!
//! let src = MySource { sender: Sender::new() };
//! let dst = MySink { receiver: Receiver::new() };
//!
//! dst.connect_to_producer(&src);
//! let mut stream: ReceiverStream<String> = dst.receiver.stream();
//! src.sender.send("Hello World!".to_string()).await;
//! assert_eq!(stream.recv().await.unwrap(), "Hello World!".to_string());
//! # });
//! ```
//!
//! [blocks]: crate::blocks

use crate::sync::broadcast_bp;

use tokio::select;
use tokio::sync::watch;
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

/// Error value returned by [`ReceiverStream::recv`]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecvError {
    /// Some values may have been lost or the data stream is interrupted;
    /// more/new data may be received in the future.
    Reset,
    /// The data stream has been completed; more/new data may be received in
    /// the future.
    Finished,
    /// The [`Receiver`] has been dropped and no more data can be received
    Closed,
}

/// Sender that can be dynamically connected to a [`Receiver`]
///
/// To connect a [`Sender`] to a [`Receiver`], a [`SenderConnector`] handle
/// must be obtained by calling [`Sender::connector`]. The `SenderConnector`
/// handle can then be passed to [`ReceiverConnector::connect`].
///
/// To send data to the connected `Receiver`s, use [`Sender::send`]. Call
/// [`Sender::reset`] to indicate missing data.
pub struct Sender<T> {
    inner: broadcast_bp::Sender<Message<T>>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Sender {
            inner: self.inner.clone(),
        }
    }
}

/// Temporary handle to connect a [`Sender`] to a [`Receiver`]
///
/// A `SenderConnector` is obtained by invoking the [`Sender::connector`]
/// method and can be passed to the [`ReceiverConnector::connect`] method.
pub struct SenderConnector<'a, T> {
    sender: &'a Sender<T>,
}

impl<T> Clone for SenderConnector<'_, T> {
    fn clone(&self) -> Self {
        SenderConnector {
            sender: self.sender,
        }
    }
}

impl<T> Copy for SenderConnector<'_, T> {}

impl<T> Sender<T>
where
    T: Clone,
{
    /// Create a new `Sender`
    pub fn new() -> Self {
        Self {
            inner: broadcast_bp::Sender::new(),
        }
    }
    /// Send data to all [`Receiver`]s which have been [connected]
    ///
    /// [connected]: ReceiverConnector::connect
    pub async fn send(&self, value: T) {
        self.inner.send(Message::Value(value)).await;
    }
    /// Notify all [`Receiver`]s that some data is missing or that the data
    /// stream has been restarted
    pub async fn reset(&self) {
        self.inner.send(Message::Reset).await;
    }
    /// Notify all [`Receiver`]s that the data stream has been completed
    pub async fn finish(&self) {
        self.inner.send(Message::Finished).await;
    }
    /// Propagate a [`RecvError`] to all [`Receiver`]s
    ///
    /// [`RecvError::Closed`] is mapped to [`RecvError::Reset`] because a
    /// `Receiver` may be reconnected with another [`Sender`] later.
    pub async fn forward_error(&self, error: RecvError) {
        self.inner
            .send(match error {
                RecvError::Reset => Message::Reset,
                RecvError::Finished => Message::Finished,
                RecvError::Closed => Message::Reset,
            })
            .await;
    }
    /// Obtain a [`SenderConnector`], which can be used to [connect] a
    /// [`Receiver`] to this `Sender`
    ///
    /// [connect]: ReceiverConnector::connect
    pub fn connector(&self) -> SenderConnector<'_, T> {
        SenderConnector { sender: self }
    }
}

/// Receiver that can be dynamically connected to a [`Sender`]
///
/// To connect a [`Receiver`] to a [`Sender`], obtain a [`ReceiverConnector`]
/// and [`SenderConnector`] handle and use the [`ReceiverConnector::connect`]
/// method.
///
/// To retrieve data from any connected `Sender`, a [`ReceiverStream`] must be
/// obtained by calling the [`Receiver::stream`] method.
pub struct Receiver<T> {
    watch: watch::Sender<Option<broadcast_bp::Subscriber<Message<T>>>>,
}

/// Temporary handle to connect a [`Receiver`] to a [`Sender`]
///
/// A `ReceiverConnector` is obtained by invoking the [`Receiver::connector`]
/// method.
pub struct ReceiverConnector<'a, T> {
    receiver: &'a Receiver<T>,
}

impl<T> Clone for ReceiverConnector<'_, T> {
    fn clone(&self) -> Self {
        ReceiverConnector {
            receiver: self.receiver,
        }
    }
}

impl<T> Copy for ReceiverConnector<'_, T> {}

/// Handle that allows retrieving data from a [`Receiver`]
///
/// A [`ReceiverStream`] is obtained by calling the [`Receiver::stream`]
/// method. Mutable access to the `ReceiverStream` is then required to invoke
/// one of its receive methods.
pub struct ReceiverStream<T> {
    watch: watch::Receiver<Option<broadcast_bp::Subscriber<Message<T>>>>,
    inner: Option<broadcast_bp::Receiver<Message<T>>>,
    continuity: bool,
}

impl<T> Receiver<T>
where
    T: Clone,
{
    /// Create a new `Receiver` which isn't [connected] with any [`Sender`] yet
    ///
    /// [connected]: ReceiverConnector::connect
    pub fn new() -> Self {
        Self {
            watch: watch::channel(None).0,
        }
    }
    /// Create a `Receiver` and [connect] it to a [`Sender`]
    ///
    /// [connect]: ReceiverConnector::connect
    pub fn with_sender(sender: &Sender<T>) -> Self {
        let this = Self::new();
        this.connector().connect(sender.connector());
        this
    }
    /// Obtain a [`ReceiverConnector`], which can be used to [connect] a
    /// [`Sender`] to this `Receiver`
    ///
    /// [connect]: ReceiverConnector::connect
    pub fn connector(&self) -> ReceiverConnector<'_, T> {
        ReceiverConnector { receiver: self }
    }
    /// Retrieve [`ReceiverStream`] handle which can be used to [receive] data
    ///
    /// [receive]: ReceiverStream::recv
    pub fn stream(&self) -> ReceiverStream<T> {
        let mut watch = self.watch.subscribe();
        let inner = watch.borrow_and_update().as_ref().map(|x| x.subscribe());
        ReceiverStream {
            watch,
            inner,
            continuity: false,
        }
    }
}

impl<T> ReceiverConnector<'_, T>
where
    T: Clone,
{
    /// Connect this `Receiver` to a [`Sender`]
    ///
    /// Any previously connected `Sender` is automatically disconnected.
    pub fn connect(&self, connector: SenderConnector<T>) {
        self.receiver
            .watch
            .send_replace(Some(connector.sender.inner.subscriber()));
    }
    /// Disconnect this `Receiver` from any [`Sender`] if connected
    pub fn disconnect(&self) {
        self.receiver.watch.send_replace(None);
    }
}

impl<T> ReceiverStream<T>
where
    T: Clone,
{
    /// Receive next `T`
    ///
    /// If a message was lost, [`RecvError::Reset`] is returned.
    /// If there is no connected [`Sender`] anymore and if the originating
    /// [`Receiver`] has been dropped, [`RecvError::Closed`] is returned.
    pub async fn recv(&mut self) -> Result<T, RecvError> {
        let change = |this: &mut Self| {
            let was_connected = this.inner.is_some();
            this.inner = this
                .watch
                .borrow_and_update()
                .as_ref()
                .map(|x| x.subscribe());
            if was_connected && this.continuity {
                this.continuity = false;
                Err(RecvError::Reset)
            } else {
                Ok(())
            }
        };
        let mut unchangeable = false;
        loop {
            if let Some(inner) = self.inner.as_mut() {
                select! {
                    result = async {
                        if unchangeable {
                            pending::<()>().await;
                        }
                        self.watch.changed().await
                    } => {
                        match result {
                            Ok(()) => change(self)?,
                            Err(_) => unchangeable = true,
                        }
                    }
                    message_opt = inner.recv() => {
                        match message_opt {
                            Some(Message::Value(value)) => {
                                self.continuity = true;
                                return Ok(value);
                            }
                            Some(Message::Reset) => {
                                self.continuity = false;
                                return Err(RecvError::Reset);
                            }
                            Some(Message::Finished) => {
                                self.continuity = false;
                                return Err(RecvError::Finished);
                            }
                            None => self.inner = None,
                        }
                    }
                }
            } else {
                let result = self.watch.changed().await;
                match result {
                    Ok(()) => change(self)?,
                    Err(_) => {
                        if self.continuity {
                            self.continuity = false;
                            return Err(RecvError::Reset);
                        } else {
                            return Err(RecvError::Closed);
                        }
                    }
                }
            }
        }
    }
}

/// Type which contains a [`Sender`] and can be connected to a [`Consumer`]
///
/// This trait is implemented for `Sender` but may also be implemented for more
/// complex types (e.g. structs which contain a `Sender`).
pub trait Producer<T>
where
    T: Clone,
{
    /// Obtain inner [`Sender`]'s [`SenderConnector`]
    fn sender_connector(&self) -> SenderConnector<'_, T>;
    /// Connect `Producer` to [`Consumer`]
    fn connect_to_consumer<C: Consumer<T>>(&self, consumer: &C) {
        consumer
            .receiver_connector()
            .connect(self.sender_connector());
    }
}

impl<T> Producer<T> for Sender<T>
where
    T: Clone,
{
    fn sender_connector(&self) -> SenderConnector<'_, T> {
        Sender::connector(self)
    }
}

/// Type which contains a [`Receiver`] and can be connected to a [`Producer`]
///
/// This trait is implemented for `Receiver` but may also be implemented for
/// more complex types (e.g. structs which contain a `Receiver`).
pub trait Consumer<T>
where
    T: Clone,
{
    /// Obtain inner [`Receiver`]'s [`ReceiverConnector`]
    fn receiver_connector(&self) -> ReceiverConnector<'_, T>;
    /// Connect `Consumer` to [`Producer`]
    fn connect_to_producer<P: Producer<T>>(&self, producer: &P) {
        self.receiver_connector()
            .connect(producer.sender_connector());
    }
    /// Disconnect `Consumer` from any connected [`Producer`] if connected
    fn disconnect(&self) {
        self.receiver_connector().disconnect();
    }
}

impl<T> Consumer<T> for Receiver<T>
where
    T: Clone,
{
    fn receiver_connector(&self) -> ReceiverConnector<'_, T> {
        Receiver::connector(self)
    }
}

/// Data which corresponds to a duration
pub trait Temporal {
    /// Duration in seconds
    fn duration(&self) -> f64;
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
pub struct Buffer<T> {
    receiver: Receiver<T>,
    sender: Sender<T>,
}

impl<T> Consumer<T> for Buffer<T>
where
    T: Clone,
{
    fn receiver_connector(&self) -> ReceiverConnector<T> {
        self.receiver.connector()
    }
}

impl<T> Producer<T> for Buffer<T>
where
    T: Clone,
{
    fn sender_connector(&self) -> SenderConnector<T> {
        self.sender.connector()
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
    /// If buffered data is older than `max_age` seconds, it will be discarded.
    pub fn new(initial_capacity: f64, min_capacity: f64, max_capacity: f64, max_age: f64) -> Self {
        let receiver = Receiver::<T>::new();
        let sender = Sender::<T>::new();
        let mut input = receiver.stream();
        let output = sender.clone();
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
                    Fill(T),
                    Drain(broadcast_bp::Reservation<'a, T>),
                    Close,
                }
                match select! {
                    action = async {
                        if closed || !(queue.duration() <= max_capacity) {
                            pending::<()>().await;
                        }
                        match input.recv().await {
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
                        Action::Drain(output.inner.reserve().await)
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
                                reservation.take().unwrap().send(Message::Reset);
                                reset_sent = true;
                            }
                        }
                        if let Some(reservation) = reservation {
                            if let Some(message) = queue.pop() {
                                reservation.send(message);
                                reset_sent = false;
                            } else {
                                underrun = true;
                            }
                        }
                    }
                    Action::Close => closed = true,
                }
            }
        });
        Self { receiver, sender }
    }
}
