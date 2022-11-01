//! Reorganizing [`Chunk`]s
//!
//! [`Chunks`]: crate::bufferpool::Chunk

use crate::bufferpool::*;
use crate::flow::*;
use crate::samples::*;

use tokio::sync::watch;
use tokio::task::spawn;

use std::collections::VecDeque;

/// Block that receives [`Samples`] with arbitrary chunk lengths and produces
/// `Samples<T>` with fixed chunk length
///
/// This block may be needed when a certain chunk length is required, e.g. for
/// the [`Filter`] block, unless the chunk length can be adjusted
/// otherwise, e.g. due to an existing [`Downsampler`] or [`Upsampler`].
///
/// [`Filter`]: crate::blocks::filters::Filter
/// [`Downsampler`]: crate::blocks::resampling::Downsampler
/// [`Upsampler`]: crate::blocks::resampling::Upsampler
pub struct Rechunker<T> {
    receiver_connector: ReceiverConnector<Samples<T>>,
    sender_connector: SenderConnector<Samples<T>>,
    output_chunk_len: watch::Sender<usize>,
}

impl<T> Consumer<Samples<T>> for Rechunker<T> {
    fn receiver_connector(&self) -> &ReceiverConnector<Samples<T>> {
        &self.receiver_connector
    }
}

impl<T> Producer<Samples<T>> for Rechunker<T> {
    fn sender_connector(&self) -> &SenderConnector<Samples<T>> {
        &self.sender_connector
    }
}

impl<T> Rechunker<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Create new `Rechunker` block with given output chunk length
    pub fn new(mut output_chunk_len: usize) -> Self {
        assert!(output_chunk_len > 0, "chunk length must be positive");
        let (mut receiver, receiver_connector) = new_receiver::<Samples<T>>();
        let (sender, sender_connector) = new_sender::<Samples<T>>();
        let (output_chunk_len_send, mut output_chunk_len_recv) = watch::channel(output_chunk_len);
        spawn(async move {
            let mut buf_pool = ChunkBufPool::<T>::new();
            let mut samples_opt: Option<Samples<T>> = None;
            let mut patchwork_opt: Option<(f64, ChunkBuf<T>)> = None;
            loop {
                let mut samples = match samples_opt {
                    Some(x) => x,
                    None => loop {
                        match receiver.recv().await {
                            Ok(samples) => {
                                if let Some((sample_rate, _)) = patchwork_opt {
                                    if sample_rate != samples.sample_rate {
                                        patchwork_opt = None;
                                        if let Err(_) = sender.reset().await {
                                            return;
                                        }
                                    }
                                }
                                break samples;
                            }
                            Err(err) => {
                                if let Err(_) = sender.forward_error(err).await {
                                    return;
                                }
                                if err == RecvError::Closed {
                                    return;
                                }
                                patchwork_opt = None;
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
                            if let Err(_) = sender
                                .send(Samples {
                                    sample_rate: samples.sample_rate,
                                    chunk: chunk.separate_beginning(output_chunk_len),
                                })
                                .await
                            {
                                return;
                            }
                        }
                        patchwork_chunk = buf_pool.get();
                        patchwork_chunk.extend_from_slice(&chunk);
                    }
                    let missing = output_chunk_len - patchwork_chunk.len();
                    if samples.chunk.len() < missing {
                        patchwork_chunk.extend_from_slice(&samples.chunk);
                        samples_opt = None;
                        patchwork_opt = Some((sample_rate, patchwork_chunk));
                    } else if samples.chunk.len() == missing {
                        patchwork_chunk.extend_from_slice(&samples.chunk);
                        if let Err(_) = sender
                            .send(Samples {
                                sample_rate,
                                chunk: patchwork_chunk.finalize(),
                            })
                            .await
                        {
                            return;
                        }
                        samples_opt = None;
                        patchwork_opt = None;
                    } else if samples.chunk.len() >= missing {
                        patchwork_chunk.extend_from_slice(&samples.chunk[0..missing]);
                        if let Err(_) = sender
                            .send(Samples {
                                sample_rate,
                                chunk: patchwork_chunk.finalize(),
                            })
                            .await
                        {
                            return;
                        }
                        samples.chunk.discard_beginning(missing);
                        samples_opt = Some(samples);
                        patchwork_opt = None;
                    } else {
                        unreachable!();
                    }
                } else {
                    while samples.chunk.len() > output_chunk_len {
                        if let Err(_) = sender
                            .send(Samples {
                                sample_rate: samples.sample_rate,
                                chunk: samples.chunk.separate_beginning(output_chunk_len),
                            })
                            .await
                        {
                            return;
                        }
                    }
                    if samples.chunk.len() == output_chunk_len {
                        if let Err(_) = sender.send(samples).await {
                            return;
                        }
                        samples_opt = None;
                        patchwork_opt = None;
                    } else {
                        patchwork_opt = Some((samples.sample_rate, buf_pool.get()));
                        samples_opt = Some(samples);
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
    receiver_connector: ReceiverConnector<Samples<T>>,
    sender_connector: SenderConnector<Samples<T>>,
}

impl<T> Consumer<Samples<T>> for Overlapper<T> {
    fn receiver_connector(&self) -> &ReceiverConnector<Samples<T>> {
        &self.receiver_connector
    }
}

impl<T> Producer<Samples<T>> for Overlapper<T> {
    fn sender_connector(&self) -> &SenderConnector<Samples<T>> {
        &self.sender_connector
    }
}

impl<T> Overlapper<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Create new block with given number of chunks to be concatenated for
    /// overlapping
    pub fn new(chunk_count: usize) -> Self {
        assert!(chunk_count > 0, "chunk count must be positive");
        let (mut receiver, receiver_connector) = new_receiver::<Samples<T>>();
        let (sender, sender_connector) = new_sender::<Samples<T>>();
        spawn(async move {
            let mut buf_pool = ChunkBufPool::<T>::new();
            let mut history: VecDeque<Samples<T>> = VecDeque::with_capacity(chunk_count);
            loop {
                match receiver.recv().await {
                    Ok(samples) => {
                        history.push_back(samples);
                        if history.len() >= chunk_count {
                            let mut sample_count: usize = 0;
                            let mut sample_rate: f64 = 0.0;
                            for samples in history.iter() {
                                sample_count += samples.chunk.len();
                                sample_rate += samples.sample_rate * samples.chunk.len() as f64;
                            }
                            sample_rate /= sample_count as f64;
                            let mut output_chunk = buf_pool.get_with_capacity(sample_count);
                            for samples in history.iter() {
                                output_chunk.extend_from_slice(&samples.chunk);
                            }
                            if let Err(_) = sender
                                .send(Samples {
                                    sample_rate,
                                    chunk: output_chunk.finalize(),
                                })
                                .await
                            {
                                return;
                            }
                            history.pop_front();
                        }
                    }
                    Err(err) => {
                        history = VecDeque::new();
                        if let Err(_) = sender.forward_error(err).await {
                            return;
                        }
                        if err == RecvError::Closed {
                            return;
                        }
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
        let mut buf_pool = ChunkBufPool::<u8>::new();
        let mut buffer = buf_pool.get();
        buffer.extend_from_slice(&vec![0; 4096]);
        let buffer = buffer.finalize();
        let (sender, sender_connector) = new_sender();
        let rechunk = Rechunker::<u8>::new(1024);
        let (mut receiver, receiver_connector) = new_receiver();
        sender_connector.feed_into(&rechunk);
        rechunk.feed_into(&receiver_connector);
        assert!(tokio::select!(
            _ = async move {
                loop {
                    sender.send(Samples {
                        chunk: buffer.clone(),
                        sample_rate: 1.0,
                    }).await.unwrap();
                }
            } => false,
            _ = async move {
                assert_eq!(receiver.recv().await.unwrap().chunk.len(), 1024);
            } => true,
        ));
    }
}
