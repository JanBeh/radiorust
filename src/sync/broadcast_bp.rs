//! Asynchronous broadcast channel with backpressure
//!
//! A channel can be created by calling [`Sender::new`] and then creating one
//! or more [`Receiver`]s with [`Sender::subscribe`]. The channel has a fixed
//! capacity of `1`.

use parking_lot::{Mutex, MutexGuard};
use tokio::sync::Notify;

use std::sync::Arc;

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

struct Synced<T> {
    data: Option<T>,
    slot: Slot,
    sndr_count: usize,
    rcvr_count: usize,
    unseen: usize,
}

struct Shared<T> {
    synced: Mutex<Synced<T>>,
    notify_sndr: Notify,
    notify_rcvr: Notify,
}

/// Sender for broadcast channel with backpressure
pub struct Sender<T> {
    shared: Arc<Shared<T>>,
}

/// Handle allowing subscription to [`Sender`]
pub struct Subscriber<T> {
    shared: Arc<Shared<T>>,
}

/// Guarantee to send one value to [`Sender`] immediately
pub struct Reservation<'a, T> {
    shared: &'a Shared<T>,
    synced: MutexGuard<'a, Synced<T>>,
}

/// Receiver for broadcast channel with backpressure
pub struct Receiver<T> {
    shared: Arc<Shared<T>>,
    slot: Slot,
}

impl<T> Shared<T> {
    fn subscribe(self: &Arc<Self>) -> Receiver<T> {
        let shared = self.clone();
        let mut synced = shared.synced.lock();
        synced.rcvr_count = synced.rcvr_count.checked_add(1).unwrap();
        let slot = synced.slot;
        shared.notify_sndr.notify_waiters();
        drop(synced);
        Receiver { shared, slot }
    }
}

impl<T> Clone for Sender<T> {
    fn clone(&self) -> Self {
        let shared = self.shared.clone();
        let mut synced = shared.synced.lock();
        synced.sndr_count = synced.sndr_count.checked_add(1).unwrap();
        drop(synced);
        Self { shared }
    }
}

impl<T> Clone for Subscriber<T> {
    fn clone(&self) -> Self {
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

impl<T> Drop for Receiver<T> {
    fn drop(&mut self) {
        let mut synced = self.shared.synced.lock();
        synced.rcvr_count -= 1;
        if self.slot != synced.slot {
            synced.unseen -= 1;
            if synced.unseen == 0 {
                self.shared.notify_sndr.notify_waiters();
            }
        }
    }
}

impl<T> Sender<T>
where
    T: Clone,
{
    /// Create a new `Sender`
    pub fn new() -> Self {
        Sender {
            shared: Arc::new(Shared {
                synced: Mutex::new(Synced {
                    data: None,
                    slot: Slot::new(),
                    sndr_count: 1,
                    rcvr_count: 0,
                    unseen: 0,
                }),
                notify_sndr: Notify::new(),
                notify_rcvr: Notify::new(),
            }),
        }
    }
    /// Wait until ready to send
    ///
    /// The returned [`Reservation`] handle may be used to send a value
    /// immediately (through [`Reservation::send`], which is not `async`).
    pub async fn reserve(&self) -> Reservation<'_, T> {
        let synced = loop {
            {
                let synced = self.shared.synced.lock();
                if synced.unseen == 0 && synced.rcvr_count > 0 {
                    break synced;
                }
                self.shared.notify_sndr.notified()
            }
            .await;
        };
        Reservation {
            shared: &self.shared,
            synced,
        }
    }
    /// Send a value
    ///
    /// This method waits when there are no receivers or some receivers have
    /// not received the previous value yet.
    pub async fn send(&self, value: T) {
        self.reserve().await.send(value)
    }
    /// Create a new [`Receiver`] connected with the `Sender`
    pub fn subscribe(&self) -> Receiver<T> {
        self.shared.subscribe()
    }
    /// Create a new [`Subscriber`] allowing creation of [`Receiver`]s
    /// connected with the `Sender`
    pub fn subscriber(&self) -> Subscriber<T> {
        Subscriber {
            shared: self.shared.clone(),
        }
    }
}

impl<T> Reservation<'_, T>
where
    T: Clone,
{
    /// Send a value
    pub fn send(mut self, value: T) {
        self.synced.slot = self.synced.slot.change();
        self.synced.data = Some(value);
        self.synced.unseen = self.synced.rcvr_count;
        self.shared.notify_rcvr.notify_waiters();
    }
}

impl<T> Subscriber<T>
where
    T: Clone,
{
    /// Create a new [`Receiver`] connected with the originating [`Sender`]
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
    pub async fn recv(&mut self) -> Option<T> {
        let mut synced = loop {
            {
                let synced = self.shared.synced.lock();
                if synced.slot != self.slot {
                    break synced;
                }
                if synced.sndr_count == 0 {
                    return None;
                }
                self.shared.notify_rcvr.notified()
            }
            .await;
        };
        self.slot = synced.slot;
        synced.unseen -= 1;
        Some(if synced.unseen == 0 {
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
    async fn test_broadcast() {
        let sender = Sender::<i32>::new();
        let mut rcvr1 = sender.subscribe();
        let mut rcvr2 = sender.subscribe();
        let mut rcvr3 = rcvr2.clone();
        let (_, vec1, vec2, vec3) = tokio::join!(
            async move {
                sender.send(1).await;
                sender.send(5).await;
                sender.send(3).await;
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
