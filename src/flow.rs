//! Data flow between [signal processing blocks]
//!
//! [signal processing blocks]: crate::blocks
//!
//! # Overview
//!
//! This module provides an extension to the [`broadcast_bp`] channel.
//! Like `broadcast_bp`, a type argument `T` is used to select the type of the
//! passed values. However, only types which implement the [`Message`] trait
//! can be used by this module.
//!
//! Opposed to `broadcast_bp`, this module allows [`Receiver`]s to be
//! (re-)connected to different [`Sender`]s after creation and use.
//! For that, each `Sender` has an associated [`SenderConnector`] and each
//! `Receiver` has an associated [`ReceiverConnector`]. The connectors provide
//! methods to (re-)connect their associated `Sender`s and `Receiver`s.
//!
//! Moreover, two traits [`Producer`] and [`Consumer`] are provided, which
//! describe data types that produce or consume data using a background task
//! and which contain a `SenderConnector` or `ReceiverConnector`, respectively,
//! such that it's possible to connect `Producer`s and `Consumer`s with each
//! other.
//!
//! Upon disconnection, a special value is optionally inserted into the stream
//! of received values. This value is determined by the
//! [`Message::disconnection`] method.
//!
//! # Implementing a `Producer` or `Consumer`
//!
//! Upon creation, `Producer`s use the [`new_sender`] function to create a pair
//! consisting of a `Sender` and a `SenderConnector`. The `Sender` is passed to
//! a background task while the `SenderConnector` is stored and accessible
//! through the [`Producer::sender_connector`] method.
//!
//! `Consumers` use the [`new_receiver`] function upon creation to create a
//! pair of a `Receiver` and a `ReceiverConnector`. The `Receiver` is passed to
//! a background task while the `ReceiverConnector` is stored and accessible
//! through the [`Consumer::receiver_connector`] method.
//!
//! Refer to the source code of the [`Nop`] block for an example.
//!
//! [`Nop`]: crate::blocks::Nop
//!
//! # Buffering and Congestion
//!
//! Connecting a `Producer` to more than one `Consumer` at the same time will
//! stall all involved blocks if one of the `Consumer`s is stalled; i.e. all
//! `Consumer`s must process the data in order for the `Producer` to be able to
//! send further data.
//!
//! There is a buffer capacity of `1` for each `Sender`/`Producer`. Because of
//! this, longer chains may lead to a significant buffer volume.
//!
//! The [`blocks`] module uses the [`Buffer`] block for tweaking buffering
//! behavior, including dropping data in case of congestion and countermeasures
//! against latency.
//!
//! [`blocks`]: crate::blocks
//! [`Buffer`]: crate::blocks::buffering::Buffer

use crate::sync::broadcast_bp;

use tokio::select;
use tokio::sync::watch;

use std::future::pending;

pub use crate::sync::broadcast_bp::{
    channel as new_sender, Enlister as SenderConnector, RecvError, Reservation, RsrvError,
    SendError, Sender,
};

/// Types that can be used as message from [`Sender`] to [`Receiver`]
pub trait Message: Sized + Clone {
    /// Return message that indicates disconnection or `None` if not
    /// supported
    fn disconnection() -> Option<Self>;
}

/// Wrapper implementing [`Message`], which doesn't provide a value that
/// indicates disconnection
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct SimpleMessage<T>(T);

impl<T> Message for SimpleMessage<T>
where
    T: Clone,
{
    fn disconnection() -> Option<Self> {
        None
    }
}

/// Unit struct indicating a disconnection
///
/// This type is not used by this module but may be used when implementing more
/// complex [`Message`] types.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Disconnection;

/// Handle to connect a [`Receiver`] to a [`Sender`]
///
/// A `ReceiverConnector` is either obtained when calling [`new_receiver`] or
/// by calling [`ReceiverConnector::new`].
///
/// Connecting a `Receiver` to a `Sender` is done by passing a
/// [`SenderConnector`] reference to [`ReceiverConnector::connect`].
/// The `SenderConnector` is obtained when calling [`new_sender`].
#[derive(Debug)]
pub struct ReceiverConnector<T> {
    enlister_tx: watch::Sender<Option<broadcast_bp::Enlister<T>>>,
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
#[derive(Debug)]
pub struct Receiver<T> {
    enlister_rx: watch::Receiver<Option<broadcast_bp::Enlister<T>>>,
    inner_receiver: Option<broadcast_bp::Receiver<T>>,
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        Self {
            enlister_rx: self.enlister_rx.clone(),
            inner_receiver: self.inner_receiver.clone(),
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
        self.enlister_tx.send_replace(Some(connector.clone()));
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
        }
    }
}

impl<T> Receiver<T>
where
    T: Message,
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
            if was_connected {
                Message::disconnection()
            } else {
                None
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
                            Ok(()) => if let Some(message) = change(self) {
                                return Ok(message);
                            },
                            Err(_) => unchangeable = true,
                        }
                    }
                    result = inner_receiver.recv() => {
                        match result {
                            Ok(message) => return Ok(message),
                            Err(_) => self.inner_receiver = None,
                        }
                    }
                }
            } else {
                match self.enlister_rx.changed().await {
                    Ok(()) => {
                        if let Some(message) = change(self) {
                            return Ok(message);
                        }
                    }
                    Err(_) => return Err(RecvError),
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
    fn feed_into<C: Consumer<T>>(&self, consumer: &C) {
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
    fn feed_from<P: Producer<T>>(&self, producer: &P) {
        self.receiver_connector()
            .connect(producer.sender_connector());
    }
    /// Disconnect `Consumer` from any connected [`Producer`] if connected
    fn feed_from_none(&self) {
        self.receiver_connector().disconnect();
    }
}

impl<T> Consumer<T> for ReceiverConnector<T> {
    fn receiver_connector(&self) -> &ReceiverConnector<T> {
        self
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    #[cfg(feature = "send_reservation")]
    async fn test_reservation_sendable() {
        use super::*;
        use tokio::task::spawn;
        let (sender, sender_connector) = new_sender::<SimpleMessage<i32>>();
        let (mut receiver, receiver_connector) = new_receiver::<SimpleMessage<i32>>();
        sender_connector.feed_into(&receiver_connector);
        // `broadcast_bp::Receiver` may not have been created yet, so we need
        // this to avoid deadlocking:
        spawn(async move {
            receiver.recv().await.ok();
        });
        fn takes_send<T: Send>(_: T) {}
        takes_send(sender.reserve().await.unwrap());
    }
}
