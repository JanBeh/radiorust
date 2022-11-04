//! Asynchronous broadcast channel with backpressure
//!
//! A channel can be created by calling the [`channel`] function, which returns
//! a [`Sender`] and an [`Enlister`]. To create a [`Receiver`], use
//! [`Enlister::subscribe`].
//!
//! The channel has a fixed capacity of `1`.

use parking_lot::{Mutex, MutexGuard};
use tokio::sync::Notify;

use std::error::Error;
use std::fmt;
use std::sync::Arc;

/// Error returned by [`Sender::send`] if there are no [`Enlister`]s or
/// [`Receiver`]s
pub struct SendError<T>(
    /// The value that could not be sent
    pub T,
);

impl<T> fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "sending to broadcast channel failed (no enlisters or receivers)"
        )
    }
}

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SendError")
    }
}

impl<T> Error for SendError<T> {}

/// Error returned by [`Sender::reserve`] if there are no [`Enlister`]s or
/// [`Receiver`]s
#[derive(Debug)]
pub struct RsrvError;

impl fmt::Display for RsrvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "preparing sending to broadcast channel failed (no enlisters or receivers)"
        )
    }
}

impl Error for RsrvError {}

/// Error returned by [`Receiver::recv`] if there are no senders
#[derive(Debug)]
pub struct RecvError;

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "receiving from broadcast channel failed (no senders)")
    }
}

impl Error for RecvError {}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
enum Slot {
    A,
    B,
}

impl Slot {
    const fn new() -> Self {
        Slot::A
    }
    fn change(self) -> Self {
        match self {
            Slot::A => Slot::B,
            Slot::B => Slot::A,
        }
    }
}

#[derive(Debug)]
struct Synced<T> {
    data: Option<T>,
    slot: Slot,
    sndr_count: usize,
    elst_count: usize,
    rcvr_count: usize,
    unseen: usize,
}

#[derive(Debug)]
struct Shared<T> {
    synced: Mutex<Synced<T>>,
    notify_sndr: Notify,
    notify_rcvr: Notify,
}

/// Sender for broadcast channel with backpressure
#[derive(Debug)]
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

/// Handle allowing subscription to [`Sender`]
#[derive(Debug)]
pub struct Enlister<T> {
    shared: Arc<Shared<T>>,
}

/// Guarantee to send one value from [`Sender`] to [`Receiver`]s immediately
///
/// This type is `!Send`. If you require this to be [`Send`], use the
/// `send_reservation` feature.
#[derive(Debug)]
pub struct Reservation<'a, T> {
    shared: &'a Shared<T>,
    synced: MutexGuard<'a, Synced<T>>,
}

/// Receiver for broadcast channel with backpressure
#[derive(Debug)]
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
    slot: Slot,
}

impl<T> Shared<T> {
    fn subscribe(self: &Arc<Self>) -> Receiver<T> {
        let mut synced = self.synced.lock();
        synced.rcvr_count = synced.rcvr_count.checked_add(1).unwrap();
        let slot = synced.slot;
        self.notify_sndr.notify_waiters();
        drop(synced);
        Receiver {
            shared: self.clone(),
            slot,
        }
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        let mut synced = self.shared.synced.lock();
        synced.sndr_count = synced.sndr_count.checked_add(1).unwrap();
        drop(synced);
        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<T> Clone for Enlister<T> {
    fn clone(&self) -> Self {
        let mut synced = self.shared.synced.lock();
        synced.elst_count = synced.elst_count.checked_add(1).unwrap();
        drop(synced);
        Self {
            shared: self.shared.clone(),
        }
    }
}

impl<T> Clone for Receiver<T> {
    fn clone(&self) -> Self {
        self.shared.subscribe()
    }
}

impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        let mut synced = self.shared.synced.lock();
        synced.sndr_count -= 1;
        if synced.sndr_count == 0 {
            self.shared.notify_rcvr.notify_waiters();
        }
    }
}

impl<T> Drop for Enlister<T> {
    fn drop(&mut self) {
        let mut synced = self.shared.synced.lock();
        synced.elst_count -= 1;
        if synced.elst_count == 0 && synced.rcvr_count == 0 {
            self.shared.notify_sndr.notify_waiters();
        }
    }
}

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut synced = self.shared.synced.lock();
        synced.rcvr_count -= 1;
        let mut notify = synced.rcvr_count == 0 && synced.elst_count == 0;
        if self.slot != synced.slot {
            synced.unseen -= 1;
            if synced.unseen == 0 {
                notify = true;
            }
        }
        if notify {
            self.shared.notify_sndr.notify_waiters();
        }
    }
}

/// Create a new broadcast channel by returning a [`Sender`] and [`Enlister`]
pub fn channel<T>() -> (Sender<T>, Enlister<T>) {
    let shared1 = Arc::new(Shared {
        synced: Mutex::new(Synced {
            data: None,
            slot: Slot::new(),
            sndr_count: 1,
            elst_count: 1,
            rcvr_count: 0,
            unseen: 0,
        }),
        notify_sndr: Notify::new(),
        notify_rcvr: Notify::new(),
    });
    let shared2 = shared1.clone();
    (Sender { shared: shared1 }, Enlister { shared: shared2 })
}

impl<T> Sender<T> {
    /// Wait until ready to send
    ///
    /// The returned [`Reservation`] handle may be used to send a value
    /// immediately (through [`Reservation::send`], which is not `async`).
    pub async fn reserve(&self) -> Result<Reservation<'_, T>, RsrvError> {
        let synced = loop {
            {
                let synced = self.shared.synced.lock();
                if synced.unseen == 0 && synced.rcvr_count > 0 {
                    break synced;
                }
                if synced.elst_count == 0 && synced.rcvr_count == 0 {
                    return Err(RsrvError);
                }
                self.shared.notify_sndr.notified()
            }
            .await;
        };
        Ok(Reservation {
            shared: &self.shared,
            synced,
        })
    }
    /// Check if ready to send
    ///
    /// The returned [`Reservation`] handle may be used to send a value
    /// immediately (through [`Reservation::send`], which is not `async`).
    ///
    /// This method returns `Ok(None)` if it's not possible to send a value
    /// immediately.
    pub fn try_reserve(&self) -> Result<Option<Reservation<'_, T>>, RsrvError> {
        let synced = self.shared.synced.lock();
        if synced.unseen == 0 && synced.rcvr_count > 0 {
            Ok(Some(Reservation {
                shared: &self.shared,
                synced,
            }))
        } else if synced.elst_count == 0 && synced.rcvr_count == 0 {
            Err(RsrvError)
        } else {
            Ok(None)
        }
    }
    /// Send a value
    ///
    /// This method waits when there are no receivers or some receivers have
    /// not received the previous value yet.
    pub async fn send(&self, value: T) -> Result<(), SendError<T>> {
        match self.reserve().await {
            Ok(reservation) => {
                reservation.send(value);
                Ok(())
            }
            Err(RsrvError) => Err(SendError(value)),
        }
    }
}

impl<T> Reservation<'_, T> {
    /// Send a value
    pub fn send(mut self, value: T) {
        self.synced.slot = self.synced.slot.change();
        self.synced.data = Some(value);
        self.synced.unseen = self.synced.rcvr_count;
        self.shared.notify_rcvr.notify_waiters();
    }
}

impl<T> Enlister<T> {
    /// Create a new [`Receiver`] connected with the associated [`Sender`]
    pub fn subscribe(&self) -> Receiver<T> {
        self.shared.subscribe()
    }
}

impl<T> Receiver<T>
where
    T: Clone,
{
    /// Receive a value
    ///
    /// This method waits when there is no value to receive but returns `None`
    /// when all [`Sender`]s have been dropped.
    pub async fn recv(&mut self) -> Result<T, RecvError> {
        let mut synced = loop {
            {
                let synced = self.shared.synced.lock();
                if synced.slot != self.slot {
                    break synced;
                }
                if synced.sndr_count == 0 {
                    return Err(RecvError);
                }
                self.shared.notify_rcvr.notified()
            }
            .await;
        };
        self.slot = synced.slot;
        synced.unseen -= 1;
        Ok(if synced.unseen == 0 {
            self.shared.notify_sndr.notify_waiters();
            synced.data.take().unwrap()
        } else {
            synced.data.as_ref().unwrap().clone()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[cfg(feature = "send_reservation")]
    async fn test_reservation_sendable() {
        let (sender, enlister) = channel::<i32>();
        let _receiver = enlister.subscribe();
        fn takes_send<T: Send>(_: T) {}
        takes_send(sender.reserve().await.unwrap());
    }
    #[tokio::test]
    async fn test_broadcast() {
        let (sender, enlister) = channel::<i32>();
        let mut rcvr1 = enlister.subscribe();
        let mut rcvr2 = enlister.subscribe();
        let mut rcvr3 = rcvr2.clone();
        drop(enlister);
        let (_, vec1, vec2, vec3) = tokio::join!(
            async move {
                sender.send(1).await.unwrap();
                sender.send(5).await.unwrap();
                sender.send(3).await.unwrap();
            },
            async move {
                let mut vec = Vec::new();
                vec.push(rcvr1.recv().await.unwrap());
                vec.push(rcvr1.recv().await.unwrap());
                vec.push(rcvr1.recv().await.unwrap());
                vec
            },
            async move {
                let mut vec = Vec::new();
                vec.push(rcvr2.recv().await.unwrap());
                vec.push(rcvr2.recv().await.unwrap());
                vec.push(rcvr2.recv().await.unwrap());
                vec
            },
            async move {
                let mut vec = Vec::new();
                vec.push(rcvr3.recv().await.unwrap());
                vec.push(rcvr3.recv().await.unwrap());
                vec.push(rcvr3.recv().await.unwrap());
                vec
            },
        );
        assert_eq!(vec1, vec![1, 5, 3]);
        assert_eq!(vec2, vec![1, 5, 3]);
        assert_eq!(vec3, vec![1, 5, 3]);
    }
}
