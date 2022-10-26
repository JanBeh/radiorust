//! Data flow between [blocks]
//!
//! [blocks]: crate::blocks

use crate::sync::broadcast_bp;

use tokio::select;
use tokio::sync::watch;

use std::error::Error;
use std::fmt;
use std::future::pending;

pub use broadcast_bp::{RsrvError, SendError};

#[derive(Clone, Debug)]
enum Message<T> {
    Value(T),
    Reset,
    Finished,
}

/// Error value returned by [`Receiver::recv`]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecvError {
    /// Some values may have been lost or the data stream is interrupted;
    /// more/new data may be received in the future.
    Reset,
    /// The data stream has been completed; more/new data may be received in
    /// the future. This error is also used by blocks which have no data to
    /// send yet, prior to sending silence.
    Finished,
    /// No more data can be received and the [`ReceiverConnector`] has been
    /// dropped.
    Closed,
}

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecvError::Reset => write!(f, "data stream interrupted"),
            RecvError::Finished => write!(f, "data stream completed"),
            RecvError::Closed => write!(f, "data stream closed"),
        }
    }
}

impl Error for RecvError {}

/// Sender that can be dynamically connected to a [`Receiver`]
///
/// To send data to the connected `Receiver`s, use [`Sender::send`]. Call
/// [`Sender::reset`] to indicate missing data and [`Sender::finish`] to
/// indicate end of stream.
///
/// Connecting the `Sender` to a `Receiver` is done by passing a
/// [`SenderConnector`] reference to [`ReceiverConnector::connect`].
/// The `SenderConnector` is obtained when calling [`new_sender`].
pub struct Sender<T> {
    inner_sender: broadcast_bp::Sender<Message<T>>,
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        Self {
            inner_sender: self.inner_sender.clone(),
        }
    }
}

/// Guarantee to send one value from [`Sender`] to [`Receiver`]s immediately
pub struct Reservation<'a, T> {
    inner_reservation: broadcast_bp::Reservation<'a, Message<T>>,
}

/// Handle to connect a [`Sender`] to a [`Receiver`]
///
/// A `SenderConnector` can be obtained by calling [`new_sender`].
/// A reference to a `SenderConnector` can be passed to
/// [`ReceiverConnector::connect`] to connect the associated `Sender` to the
/// associated `Receiver`.
pub struct SenderConnector<T> {
    inner_enlister: broadcast_bp::Enlister<Message<T>>,
}

impl<T> Clone for SenderConnector<T> {
    fn clone(&self) -> Self {
        Self {
            inner_enlister: self.inner_enlister.clone(),
        }
    }
}

/// Create a [`Sender`] with an associated [`SenderConnector`]
pub fn new_sender<T>() -> (Sender<T>, SenderConnector<T>) {
    let (inner_sender, inner_enlister) = broadcast_bp::channel();
    (Sender { inner_sender }, SenderConnector { inner_enlister })
}

impl<T> Sender<T> {
    /// Wait until ready to send
    ///
    /// The returned [`Reservation`] handle may be used to send a value
    /// immediately (through [`Reservation::send`], which is not `async`).
    pub async fn reserve(&self) -> Result<Reservation<'_, T>, RsrvError> {
        Ok(Reservation {
            inner_reservation: self.inner_sender.reserve().await?,
        })
    }
    /// Send data to all [`Receiver`]s which have been [connected]
    ///
    /// [connected]: ReceiverConnector::connect
    pub async fn send(&self, value: T) -> Result<(), SendError<T>> {
        match self.reserve().await {
            Ok(reservation) => {
                reservation.send(value);
                Ok(())
            }
            Err(RsrvError) => Err(SendError(value)),
        }
    }
    /// Notify all [`Receiver`]s that some data is missing or that the data
    /// stream has been restarted
    pub async fn reset(&self) -> Result<(), SendError<()>> {
        match self.reserve().await {
            Ok(reservation) => {
                reservation.reset();
                Ok(())
            }
            Err(RsrvError) => Err(SendError(())),
        }
    }
    /// Notify all [`Receiver`]s that the data stream has been completed
    pub async fn finish(&self) -> Result<(), SendError<()>> {
        match self.reserve().await {
            Ok(reservation) => {
                reservation.finish();
                Ok(())
            }
            Err(RsrvError) => Err(SendError(())),
        }
    }
    /// Propagate a [`RecvError`] to all [`Receiver`]s
    ///
    /// [`RecvError::Closed`] is mapped to [`RecvError::Reset`] because a
    /// `Receiver` may be reconnected with another [`Sender`] later.
    pub async fn forward_error(&self, error: RecvError) -> Result<(), SendError<()>> {
        match self.reserve().await {
            Ok(reservation) => {
                reservation.forward_error(error);
                Ok(())
            }
            Err(RsrvError) => Err(SendError(())),
        }
    }
}

impl<T> Reservation<'_, T> {
    /// Send data to all [`Receiver`]s which have been [connected]
    ///
    /// [connected]: ReceiverConnector::connect
    pub fn send(self, value: T) {
        self.inner_reservation.send(Message::Value(value));
    }
    /// Notify all [`Receiver`]s that some data is missing or that the data
    /// stream has been restarted
    pub fn reset(self) {
        self.inner_reservation.send(Message::Reset);
    }
    /// Notify all [`Receiver`]s that the data stream has been completed
    pub fn finish(self) {
        self.inner_reservation.send(Message::Finished)
    }
    /// Propagate a [`RecvError`] to all [`Receiver`]s
    ///
    /// [`RecvError::Closed`] is mapped to [`RecvError::Reset`] because a
    /// `Receiver` may be reconnected with another [`Sender`] later.
    pub fn forward_error(self, error: RecvError) {
        self.inner_reservation.send(match error {
            RecvError::Reset => Message::Reset,
            RecvError::Finished => Message::Finished,
            RecvError::Closed => Message::Reset,
        })
    }
}

/// Handle to connect a [`Receiver`] to a [`Sender`]
///
/// A `ReceiverConnector` is either obtained when calling [`new_receiver`] or
/// by calling [`ReceiverConnector::new`].
///
/// Connecting a `Receiver` to a `Sender` is done by passing a
/// [`SenderConnector`] reference to [`ReceiverConnector::connect`].
/// The `SenderConnector` is obtained when calling [`new_sender`].
pub struct ReceiverConnector<T> {
    enlister_tx: watch::Sender<Option<broadcast_bp::Enlister<Message<T>>>>,
}

/// Receiver that can be dynamically connected to a [`Sender`]
///
/// A `Receiver` is either obtained through [`new_receiver`] or by calling
/// [`ReceiverConnector::stream`].
///
/// Receiving data is done by calling [`Receiver::recv`].
///
/// Connecting a `Receiver` to a `Sender` is done by passing a
/// [`SenderConnector`] reference to [`ReceiverConnector::connect`].
/// The `SenderConnector` is obtained when calling [`new_sender`].
pub struct Receiver<T> {
    enlister_rx: watch::Receiver<Option<broadcast_bp::Enlister<Message<T>>>>,
    inner_receiver: Option<broadcast_bp::Receiver<Message<T>>>,
    continuity: bool,
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Self {
            enlister_rx: self.enlister_rx.clone(),
            inner_receiver: self.inner_receiver.clone(),
            continuity: self.continuity,
        }
    }
}

/// Create a [`Receiver`] with an associated [`ReceiverConnector`]
///
/// Alternatively, you can use [`ReceiverConnector::new`] and
/// [`ReceiverConnector::stream`].
pub fn new_receiver<T>() -> (Receiver<T>, ReceiverConnector<T>) {
    let receiver_connector = ReceiverConnector::new();
    let receiver = receiver_connector.stream();
    (receiver, receiver_connector)
}

impl<T> ReceiverConnector<T> {
    /// Create a new `ReceiverConnector` without associated [`Receiver`]s
    pub fn new() -> Self {
        Self {
            enlister_tx: watch::channel(None).0,
        }
    }
    /// Connect associated [`Receiver`]s with a [`Sender`]
    pub fn connect(&self, connector: &SenderConnector<T>) {
        self.enlister_tx
            .send_replace(Some(connector.inner_enlister.clone()));
    }
    /// Disconnect associated [`Receiver`]s from [`Sender`] if connected
    pub fn disconnect(&self) {
        self.enlister_tx.send_replace(None);
    }
    /// Obtain an associated [`Receiver`]
    pub fn stream(&self) -> Receiver<T> {
        let mut enlister_rx = self.enlister_tx.subscribe();
        let inner_receiver = enlister_rx
            .borrow_and_update()
            .as_ref()
            .map(|x| x.subscribe());
        Receiver {
            enlister_rx,
            inner_receiver,
            continuity: false,
        }
    }
}

impl<T> Receiver<T>
where
    T: Clone,
{
    /// Receive data from connected [`Sender`]
    pub async fn recv(&mut self) -> Result<T, RecvError> {
        let change = |this: &mut Self| {
            let was_connected = this.inner_receiver.is_some();
            this.inner_receiver = this
                .enlister_rx
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
            if let Some(inner_receiver) = self.inner_receiver.as_mut() {
                select! {
                    result = async {
                        if unchangeable {
                            pending::<()>().await;
                        }
                        self.enlister_rx.changed().await
                    } => {
                        match result {
                            Ok(()) => change(self)?,
                            Err(_) => unchangeable = true,
                        }
                    }
                    result = inner_receiver.recv() => {
                        match result {
                            Ok(Message::Value(value)) => {
                                self.continuity = true;
                                return Ok(value);
                            }
                            Ok(Message::Reset) => {
                                self.continuity = false;
                                return Err(RecvError::Reset);
                            }
                            Ok(Message::Finished) => {
                                self.continuity = false;
                                return Err(RecvError::Finished);
                            }
                            Err(_) => self.inner_receiver = None,
                        }
                    }
                }
            } else {
                match self.enlister_rx.changed().await {
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

/// Type which contains a [`SenderConnector`] and can be connected to a
/// [`Consumer`]
///
/// This trait is implemented for `SenderConnector` but may also be implemented
/// for structs which contain a `SenderConnector`.
pub trait Producer<T> {
    /// Obtain reference to [`SenderConnector`]
    fn sender_connector(&self) -> &SenderConnector<T>;
    /// Connect `Producer` to [`Consumer`]
    fn connect_to_consumer<C: Consumer<T>>(&self, consumer: &C) {
        consumer
            .receiver_connector()
            .connect(self.sender_connector());
    }
}

impl<T> Producer<T> for SenderConnector<T> {
    fn sender_connector(&self) -> &SenderConnector<T> {
        self
    }
}

/// Type which contains a [`ReceiverConnector`] and can be connected to a
/// [`Producer`]
///
/// This trait is implemented for `ReceiverConnector` but may also be
/// implemented for structs which contain a `ReceiverConnector`.
pub trait Consumer<T> {
    /// Obtain reference to [`ReceiverConnector`]
    fn receiver_connector(&self) -> &ReceiverConnector<T>;
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

impl<T> Consumer<T> for ReceiverConnector<T> {
    fn receiver_connector(&self) -> &ReceiverConnector<T> {
        self
    }
}
