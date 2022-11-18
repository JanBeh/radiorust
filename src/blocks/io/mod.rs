//! External sources and sinks (plus [`Silence`] and [`Blackhole`])
//!
//! The [`audio`] and [`rf`] modules contain blocks that allow accessing
//! hardware audio or radio interfaces.
//!
//! **Note:** Blocks in this module will stop working when dropped.

pub mod audio;
pub mod rf;

use crate::bufferpool::*;
use crate::flow::*;
use crate::impl_block_trait;
use crate::signal::*;

use num::Zero;
use tokio::select;
use tokio::sync::watch;
use tokio::task::spawn;

/// [`Producer`] which sends silence with adjustable chunk size and sample rate
pub struct Silence<T> {
    sender_connector: SenderConnector<Signal<T>>,
    chunk_size: watch::Sender<usize>,
    sample_rate: watch::Sender<f64>,
}

impl_block_trait! { <T> Producer<Signal<T>> for Silence<T> }

impl<T> Silence<T>
where
    T: Clone + Send + Sync + Zero + 'static,
{
    /// Creates a block which sends silence with the given `chunk_size` and
    /// `sample_rate`
    pub fn new(mut chunk_size: usize, mut sample_rate: f64) -> Self {
        let (sender, sender_connector) = new_sender::<Signal<T>>();
        let (chunk_size_send, mut chunk_size_recv) = watch::channel(chunk_size);
        let (sample_rate_send, mut sample_rate_recv) = watch::channel(sample_rate);
        spawn(async move {
            let mut buf_pool = ChunkBufPool::new();
            let mut chunk_buf = buf_pool.get_with_capacity(chunk_size);
            chunk_buf.resize(chunk_size, T::zero());
            let mut chunk = chunk_buf.finalize();
            loop {
                match chunk_size_recv.has_changed() {
                    Ok(false) => (),
                    Ok(true) => {
                        chunk_size = chunk_size_recv.borrow_and_update().clone();
                        if chunk.len() != chunk_size {
                            let mut chunk_buf = buf_pool.get_with_capacity(chunk_size);
                            chunk_buf.resize(chunk_size, T::zero());
                            chunk = chunk_buf.finalize();
                        }
                    }
                    Err(_) => return,
                }
                match sample_rate_recv.has_changed() {
                    Ok(false) => (),
                    Ok(true) => sample_rate = sample_rate_recv.borrow_and_update().clone(),
                    Err(_) => return,
                }
                let Ok(()) = sender.send(Signal::Samples {
                    sample_rate,
                    chunk: chunk.clone()
                }).await
                else { return; };
            }
        });
        Self {
            sender_connector,
            chunk_size: chunk_size_send,
            sample_rate: sample_rate_send,
        }
    }
    /// Get chunk size
    pub fn chunk_size(&self) -> usize {
        self.chunk_size.borrow().clone()
    }
    /// Set chunk size
    pub fn set_chunk_size(&self, chunk_size: usize) {
        self.chunk_size.send_replace(chunk_size);
    }
    /// Get sample rate in samples per second
    pub fn sample_rate(&self) -> f64 {
        self.sample_rate.borrow().clone()
    }
    /// Set sample rate in samples per second
    pub fn set_sample_rate(&self, sample_rate: f64) {
        self.sample_rate.send_replace(sample_rate);
    }
}

/// [`Consumer`] which ignores all received [`Signal::Samples`] (but allows
/// [`EventHandling`])
pub struct Blackhole<T> {
    receiver_connector: ReceiverConnector<Signal<T>>,
    event_handlers: EventHandlers,
    _drop_watch: watch::Sender<()>,
}

impl_block_trait! { <T> Consumer<Signal<T>> for Blackhole<T> }
impl_block_trait! { <T> EventHandling for Blackhole<T> }

impl<T> Blackhole<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Create new `Blackhole`
    pub fn new() -> Self {
        let (mut receiver, receiver_connector) = new_receiver::<Signal<T>>();
        let (drop_watch_send, mut drop_watch_recv) = watch::channel(());
        let event_handlers = EventHandlers::new();
        let evhdl_clone = event_handlers.clone();
        spawn(async move {
            loop {
                select! {
                    result = drop_watch_recv.changed() => match result {
                        Err(_) => return,
                        _ => (),
                    },
                    result = receiver.recv() => match result {
                        Ok(Signal::Samples { .. }) => (),
                        Ok(Signal::Event { payload, .. }) => evhdl_clone.invoke(&payload),
                        Err(_) => return,
                    },
                }
            }
        });
        Self {
            receiver_connector,
            event_handlers,
            _drop_watch: drop_watch_send,
        }
    }
}
