//! Data flow between [blocks]
//!
//! [blocks]: crate::blocks

use tokio::sync::{broadcast, watch};
use tokio::task::yield_now;

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
/// handle can then be passed to [`Receiver::connect`].
///
/// To send data to the connected `Receiver`s, use [`Sender::send`]. Call
/// [`Sender::reset`] to indicate missing data.
pub struct Sender<T> {
    inner: broadcast::Sender<Message<T>>,
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
/// method and can be passed to the [`Receiver::connect`] method.
pub struct SenderConnector<'a, T> {
    inner: &'a broadcast::Sender<Message<T>>,
}

impl<T> Clone for SenderConnector<'_, T> {
    fn clone(&self) -> Self {
        SenderConnector { inner: self.inner }
    }
}

impl<T> Copy for SenderConnector<'_, T> {}

impl<T> Sender<T>
where
    T: Clone,
{
    /// Create a new `Sender` with a default capacity of 8
    pub fn new() -> Self {
        Self::with_capacity(8)
    }
    /// Create a new `Sender` and manually specify its `capacity`
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            inner: broadcast::channel(capacity).0,
        }
    }
    async fn inner_send(&self, message: Message<T>) {
        self.inner.send(message).ok();
        yield_now().await;
    }
    /// Send data to all [`Receiver`] which have been [connected]
    ///
    /// [connected]: Receiver::connect
    pub async fn send(&self, value: T) {
        self.inner_send(Message::Value(value)).await;
    }
    /// Notify all [`Receiver`]s that some data is missing or that the data
    /// stream has been restarted
    pub async fn reset(&self) {
        self.inner_send(Message::Reset).await;
    }
    /// Notify all [`Receiver`]s that the data stream has been completed
    pub async fn finish(&self) {
        self.inner_send(Message::Finished).await;
    }
    /// Propagate a [`RecvError`] to all [`Receiver`]s
    ///
    /// [`RecvError::Closed`] is mapped to [`RecvError::Reset`] because a
    /// `Receiver` may be reconnected with another [`Sender`] later.
    pub async fn forward_error(&self, error: RecvError) {
        self.inner_send(match error {
            RecvError::Reset => Message::Reset,
            RecvError::Finished => Message::Finished,
            RecvError::Closed => Message::Reset,
        })
        .await;
    }
    /// Obtain a [`SenderConnector`], which can be used to [connect] a
    /// [`Receiver`] to this `Sender`
    ///
    /// [connect]: Receiver::connect
    pub fn connector(&self) -> SenderConnector<'_, T> {
        SenderConnector { inner: &self.inner }
    }
}

/// Receiver that can be dynamically connected to a [`Sender`]
///
/// To connect a [`Receiver`] to a [`Sender`], a [`SenderConnector`] handle
/// must be obtained by calling [`Sender::connector`]. The `SenderConnector`
/// handle can then be passed to [`Receiver::connect`].
///
/// To retrieve data from any connected `Sender`, a [`ReceiverStream`] must be
/// obtained by calling the [`Receiver::stream`] method.
pub struct Receiver<T> {
    watch: watch::Sender<Option<broadcast::Sender<Message<T>>>>,
}

/// Handle that allows retrieving data from a [`Receiver`]
///
/// A [`ReceiverStream`] is obtained by calling the [`Receiver::stream`]
/// method. Mutable access to the `ReceiverStream` is then required to invoke
/// one of its receive methods.
pub struct ReceiverStream<T> {
    watch: watch::Receiver<Option<broadcast::Sender<Message<T>>>>,
    inner: Option<broadcast::Receiver<Message<T>>>,
}

impl<T> Receiver<T>
where
    T: Clone,
{
    /// Create a new `Receiver` which isn't [connected] with any [`Sender`] yet
    ///
    /// [connected]: Receiver::connect
    pub fn new() -> Self {
        Self {
            watch: watch::channel(None).0,
        }
    }
    /// Create a `Receiver` and [connect] it to a [`Sender`]
    ///
    /// [connect]: Receiver::connect
    pub fn with_sender(sender: &Sender<T>) -> Self {
        let this = Self::new();
        this.connect(sender.connector());
        this
    }
    /// Connect this `Receiver` to a [`Sender`]
    ///
    /// Any previously connected `Sender` is automatically disconnected.
    pub fn connect(&self, connector: SenderConnector<T>) {
        self.watch.send_replace(Some(connector.inner.clone()));
    }
    /// Disconnect this `Receiver` from any [`Sender`] if connected
    pub fn disconnect(&self) {
        self.watch.send_replace(None);
    }
    /// Retrieve [`ReceiverStream`] handle which can be used to [receive] data
    /// as long as the [`Receiver`] isn't dropped
    ///
    /// [receive]: ReceiverStream::recv
    pub fn stream(&self) -> ReceiverStream<T> {
        let mut watch = self.watch.subscribe();
        let inner = watch.borrow_and_update().as_ref().map(|x| x.subscribe());
        ReceiverStream { watch, inner }
    }
}

impl<T> ReceiverStream<T>
where
    T: Clone,
{
    /// Receive next `T`
    ///
    /// If a message was lost, [`RecvError::Reset`] is returned.
    /// If the originating [`Receiver`] has been dropped, [`RecvError::Closed`]
    /// is returned.
    ///
    /// For low-latency retrieval, use [`ReceiverStream::recv_lowlat`]
    /// instead.
    pub async fn recv(&mut self) -> Result<T, RecvError> {
        match self.watch.has_changed() {
            Ok(false) => (),
            Ok(true) => {
                let was_connected = self.inner.is_some();
                self.inner = self
                    .watch
                    .borrow_and_update()
                    .as_ref()
                    .map(|x| x.subscribe());
                if was_connected {
                    return Err(RecvError::Reset);
                }
            }
            Err(_) => return Err(RecvError::Closed),
        }
        loop {
            if let Some(inner) = self.inner.as_mut() {
                match inner.recv().await {
                    Ok(Message::Value(value)) => return Ok(value),
                    Ok(Message::Reset) => return Err(RecvError::Reset),
                    Ok(Message::Finished) => return Err(RecvError::Finished),
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        *inner = inner.resubscribe();
                        return Err(RecvError::Reset);
                    }
                    Err(broadcast::error::RecvError::Closed) => (),
                }
            }
            match self.watch.changed().await {
                Ok(_) => {
                    let was_connected = self.inner.is_some();
                    self.inner = self
                        .watch
                        .borrow_and_update()
                        .as_ref()
                        .map(|x| x.subscribe());
                    if was_connected {
                        return Err(RecvError::Reset);
                    }
                }
                Err(_) => return Err(RecvError::Closed),
            }
        }
    }
    /// Receive next `T` with low latency
    ///
    /// Similar to [`ReceiverStream::recv`], but keeps number of buffered chunks
    /// smaller than `capacity` to avoid latency.
    pub async fn recv_lowlat(&mut self, capacity: usize) -> Result<T, RecvError> {
        if let Some(inner) = self.inner.as_mut() {
            let waste = usize::saturating_sub(inner.len(), capacity);
            if waste > 0 {
                for _ in 0..waste {
                    match inner.try_recv() {
                        Ok(Message::Value(_)) => (),
                        Ok(Message::Reset) => (),
                        Ok(Message::Finished) => (),
                        Err(broadcast::error::TryRecvError::Empty) => break,
                        Err(broadcast::error::TryRecvError::Lagged(_)) => {
                            *inner = inner.resubscribe();
                            break;
                        }
                        Err(broadcast::error::TryRecvError::Closed) => (),
                    }
                }
                return Err(RecvError::Reset);
            }
        }
        self.recv().await
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
    /// Obtain inner [`Sender`]s [`SenderConnector`]
    fn connector(&self) -> SenderConnector<'_, T>;
    /// Connect `Producer` to [`Consumer`]
    fn connect_to_consumer<C: Consumer<T>>(&self, consumer: &C) {
        consumer.receiver().connect(self.connector());
    }
}

impl<T> Producer<T> for Sender<T>
where
    T: Clone,
{
    fn connector(&self) -> SenderConnector<'_, T> {
        Sender::connector(&self)
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
    /// Obtain reference to inner [`Receiver`]
    fn receiver(&self) -> &Receiver<T>;
    /// Connect `Consumer` to [`Producer`]
    fn connect_to_producer<P: Producer<T>>(&self, producer: &P) {
        self.receiver().connect(producer.connector());
    }
    /// Disconnect `Consumer` from any connected [`Producer`] if connected
    fn disconnect(&self) {
        self.receiver().disconnect();
    }
}

impl<T> Consumer<T> for Receiver<T>
where
    T: Clone,
{
    fn receiver(&self) -> &Receiver<T> {
        self
    }
}
