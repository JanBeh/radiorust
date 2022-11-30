//! Reorganizing [`Chunk`]s
//!
//! [`Chunks`]: crate::bufferpool::Chunk

use crate::bufferpool::*;
use crate::flow::*;
use crate::impl_block_trait;
use crate::signal::*;

use tokio::sync::watch;
use tokio::task::spawn;

use std::collections::VecDeque;

/// Types used for [`Signal::Event`]
pub mod events {
    use super::*;
    /// Sent as [`Signal::Event`] when some data was lost
    #[derive(Clone, Debug)]
    pub struct SamplesLost;
    impl Event for SamplesLost {
        fn is_interrupt(&self) -> bool {
            true
        }
        fn as_any(&self) -> &(dyn std::any::Any + Send + Sync) {
            self
        }
    }
}
use events::*;

/// Block that receives [`Signal`] messages with arbitrary chunk lengths and
/// produces fixed chunk lengths
///
/// This block may be needed when a certain chunk length is required, e.g. for
/// the [`Filter`] block, unless the chunk length can be adjusted
/// otherwise, e.g. due to an existing [`Downsampler`] or [`Upsampler`].
///
/// [`Filter`]: crate::blocks::filters::Filter
/// [`Downsampler`]: crate::blocks::resampling::Downsampler
/// [`Upsampler`]: crate::blocks::resampling::Upsampler
pub struct Rechunker<T> {
    receiver_connector: ReceiverConnector<Signal<T>>,
    sender_connector: SenderConnector<Signal<T>>,
    output_chunk_len: watch::Sender<usize>,
}

impl_block_trait! { <T> Consumer<Signal<T>> for Rechunker<T> }
impl_block_trait! { <T> Producer<Signal<T>> for Rechunker<T> }

impl<T> Rechunker<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Create new `Rechunker` block with given output chunk length
    pub fn new(mut output_chunk_len: usize) -> Self {
        assert!(output_chunk_len > 0, "chunk length must be positive");
        let (mut receiver, receiver_connector) = new_receiver::<Signal<T>>();
        let (sender, sender_connector) = new_sender::<Signal<T>>();
        let (output_chunk_len_send, mut output_chunk_len_recv) = watch::channel(output_chunk_len);
        spawn(async move {
            let mut buf_pool = ChunkBufPool::<T>::new();
            let mut input_opt: Option<(f64, Chunk<T>)> = None;
            let mut patchwork_opt: Option<(f64, ChunkBuf<T>)> = None;
            loop {
                let (sample_rate, mut chunk) = match input_opt {
                    Some(x) => x,
                    None => loop {
                        let Ok(signal) = receiver.recv().await else { return; };
                        match signal {
                            Signal::Samples { sample_rate, chunk } => {
                                if let Some((patchwork_sample_rate, _)) = patchwork_opt {
                                    if sample_rate != patchwork_sample_rate {
                                        patchwork_opt = None;
                                        let Ok(()) = sender.send(Signal::new_event(
                                            SamplesLost,
                                        )).await
                                        else { return; };
                                    }
                                }
                                break (sample_rate, chunk);
                            }
                            event @ Signal::Event { .. } => {
                                if patchwork_opt.is_some() {
                                    let Ok(()) = sender.send(Signal::new_event(
                                        SamplesLost,
                                    )).await
                                    else { return; };
                                    patchwork_opt = None;
                                }
                                let Ok(()) = sender.send(event).await else { return; };
                            }
                        }
                    },
                };
                if output_chunk_len_recv.has_changed().unwrap_or(false) {
                    output_chunk_len = output_chunk_len_recv.borrow_and_update().clone();
                }
                if let Some((sample_rate, mut patchwork_chunk)) = patchwork_opt {
                    if patchwork_chunk.len() > output_chunk_len {
                        let mut chunk = patchwork_chunk.finalize();
                        while chunk.len() > output_chunk_len {
                            let Ok(()) = sender.send(Signal::Samples {
                                sample_rate,
                                chunk: chunk.separate_beginning(output_chunk_len),
                            }).await
                            else { return; };
                        }
                        patchwork_chunk = buf_pool.get();
                        patchwork_chunk.extend_from_slice(&chunk);
                    }
                    let missing = output_chunk_len - patchwork_chunk.len();
                    if chunk.len() < missing {
                        patchwork_chunk.extend_from_slice(&chunk);
                        input_opt = None;
                        patchwork_opt = Some((sample_rate, patchwork_chunk));
                    } else if chunk.len() == missing {
                        patchwork_chunk.extend_from_slice(&chunk);
                        let Ok(()) = sender.send(Signal::Samples {
                            sample_rate,
                            chunk: patchwork_chunk.finalize(),
                        }).await
                        else { return; };
                        input_opt = None;
                        patchwork_opt = None;
                    } else if chunk.len() >= missing {
                        patchwork_chunk.extend_from_slice(&chunk[0..missing]);
                        let Ok(()) = sender.send(Signal::Samples {
                            sample_rate,
                            chunk: patchwork_chunk.finalize(),
                        }).await
                        else { return; };
                        chunk.discard_beginning(missing);
                        input_opt = Some((sample_rate, chunk));
                        patchwork_opt = None;
                    } else {
                        unreachable!();
                    }
                } else {
                    while chunk.len() > output_chunk_len {
                        let Ok(()) = sender.send(Signal::Samples {
                            sample_rate,
                            chunk: chunk.separate_beginning(output_chunk_len),
                        }).await
                        else { return; };
                    }
                    if chunk.len() == output_chunk_len {
                        let Ok(()) = sender.send(Signal::Samples {
                            sample_rate,
                            chunk,
                        }).await
                        else { return; };
                        input_opt = None;
                        patchwork_opt = None;
                    } else {
                        patchwork_opt = Some((sample_rate, buf_pool.get()));
                        input_opt = Some((sample_rate, chunk));
                    }
                }
            }
        });
        Self {
            receiver_connector,
            sender_connector,
            output_chunk_len: output_chunk_len_send,
        }
    }
    /// Get output chunk length
    pub fn output_chunk_len(&self) -> usize {
        self.output_chunk_len.borrow().clone()
    }
    /// Set output chunk length
    pub fn set_output_chunk_len(&self, output_chunk_len: usize) {
        assert!(output_chunk_len > 0, "chunk length must be positive");
        self.output_chunk_len.send_replace(output_chunk_len);
    }
}

/// Block that concatenates successive chunks to produce overlapping chunks
pub struct Overlapper<T> {
    receiver_connector: ReceiverConnector<Signal<T>>,
    sender_connector: SenderConnector<Signal<T>>,
}

impl_block_trait! { <T> Consumer<Signal<T>> for Overlapper<T> }
impl_block_trait! { <T> Producer<Signal<T>> for Overlapper<T> }

impl<T> Overlapper<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Create new block with given number of chunks to be concatenated for
    /// overlapping
    pub fn new(chunk_count: usize) -> Self {
        assert!(chunk_count > 0, "chunk count must be positive");
        let (mut receiver, receiver_connector) = new_receiver::<Signal<T>>();
        let (sender, sender_connector) = new_sender::<Signal<T>>();
        spawn(async move {
            let mut buf_pool = ChunkBufPool::<T>::new();
            let mut history: VecDeque<(f64, Chunk<T>)> = VecDeque::with_capacity(chunk_count);
            loop {
                let Ok(signal) = receiver.recv().await else { return; };
                match signal {
                    Signal::Samples { sample_rate, chunk } => {
                        history.push_back((sample_rate, chunk));
                        if history.len() >= chunk_count {
                            let mut sample_count: usize = 0;
                            let mut avg_sample_rate: f64 = 0.0;
                            for (sample_rate, chunk) in history.iter() {
                                sample_count += chunk.len();
                                avg_sample_rate += sample_rate * chunk.len() as f64;
                            }
                            avg_sample_rate /= sample_count as f64;
                            let mut output_chunk = buf_pool.get_with_capacity(sample_count);
                            for (_, chunk) in history.iter() {
                                output_chunk.extend_from_slice(chunk);
                            }
                            let Ok(()) = sender.send(Signal::Samples {
                                sample_rate: avg_sample_rate,
                                chunk: output_chunk.finalize(),
                            }).await
                            else { return; };
                            history.pop_front();
                        }
                    }
                    event @ Signal::Event { .. } => {
                        history = VecDeque::new();
                        let Ok(()) = sender.send(Signal::new_event(
                            SamplesLost,
                        )).await
                        else { return; };
                        let Ok(()) = sender.send(event).await else { return; };
                    }
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
mod tests {
    use super::*;
    #[tokio::test]
    async fn test_rechunker() {
        let chunk = Chunk::from(vec![0; 4096]);
        let (sender, sender_connector) = new_sender();
        let rechunk = Rechunker::<u8>::new(1024);
        let (mut receiver, receiver_connector) = new_receiver();
        sender_connector.feed_into(&rechunk);
        rechunk.feed_into(&receiver_connector);
        assert!(tokio::select!(
            _ = async move {
                loop {
                    sender.send(Signal::Samples {
                        chunk: chunk.clone(),
                        sample_rate: 1.0,
                    }).await.unwrap();
                }
            } => false,
            _ = async move {
                match receiver.recv().await.unwrap() {
                    Signal::Samples { chunk, .. } => assert_eq!(chunk.len(), 1024),
                    _ => panic!(),
                }
            } => true,
        ));
    }
}
