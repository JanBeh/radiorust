//! Raw I/Q data input

use crate::bufferpool::*;
use crate::flow::*;
use crate::samples::Samples;

use num::Complex;
use tokio::task::{spawn, JoinHandle};
use tokio::time::{interval, MissedTickBehavior};

use std::fs::File;
use std::future::{ready, Future};
use std::io::{self, BufReader, Read};
use std::path::Path;
use std::time::Duration;

/// Block invoking a callback to produce [`Chunk<T>`]s at a specified rate
///
/// The type argument `E` indicates a possible reported error type by the
/// callback.
pub struct ClosureSource<T, E> {
    sender: Sender<Samples<T>>,
    join_handle: JoinHandle<Result<(), E>>,
}

impl<T, E> Producer<Samples<T>> for ClosureSource<T, E>
where
    T: Clone,
{
    fn connector(&self) -> SenderConnector<'_, Samples<T>> {
        self.sender.connector()
    }
}

impl<T, E> ClosureSource<T, E>
where
    T: Clone + Send + Sync + 'static,
    E: Send + 'static,
{
    /// Create block which invokes the `retrieve` closure to produce
    /// [`Chunk<T>`]s
    ///
    /// The closure will be called at a speed such that the given `sample_rate`
    /// is reached. The returned [`Chunk`]s must match the specified
    /// `chunk_len`, or the background task panics.
    pub fn new<F>(chunk_len: usize, sample_rate: f64, mut retrieve: F) -> Self
    where
        F: FnMut() -> Result<Option<Chunk<T>>, E> + Send + 'static,
    {
        Self::new_async(chunk_len, sample_rate, move || ready(retrieve()))
    }
    /// Create block which invokes the `retrieve` closure and `await`s its
    /// result to produce [`Chunk<T>`]s
    ///
    /// This function is the same as [`ClosureSource::new`] but accepts an
    /// asynchronously working closure.
    pub fn new_async<F, R>(chunk_len: usize, sample_rate: f64, mut retrieve: F) -> Self
    where
        F: FnMut() -> R + Send + 'static,
        R: Future<Output = Result<Option<Chunk<T>>, E>> + Send,
    {
        let sender = Sender::<Samples<T>>::new();
        let output = sender.clone();
        let join_handle = spawn(async move {
            let mut clock = interval(Duration::from_secs_f64(sample_rate / chunk_len as f64));
            clock.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                match retrieve().await {
                    Ok(Some(chunk)) => {
                        assert_eq!(chunk.len(), chunk_len);
                        output.send(Samples { sample_rate, chunk }).await;
                        clock.tick().await;
                    }
                    Ok(None) => {
                        output.finish().await;
                        return Ok(());
                    }
                    Err(err) => {
                        output.reset().await;
                        return Err(err);
                    }
                }
            }
        });
        Self {
            sender,
            join_handle,
        }
    }
    /// Wait for stream to finish
    pub async fn wait(self) -> Result<(), E> {
        self.join_handle.await.expect("task panicked")
    }
    /// Stop operation
    pub async fn stop(self) -> Result<(), E> {
        // TODO: force stopping
        self.join_handle.await.expect("task panicked")
    }
}

/// Block which reads single precision float samples in big endianess from a
/// [reader][Read] at a specified speed
pub struct F32BeReader {
    inner: ClosureSource<Complex<f32>, io::Error>,
}

impl Producer<Samples<Complex<f32>>> for F32BeReader {
    fn connector(&self) -> SenderConnector<'_, Samples<Complex<f32>>> {
        self.inner.connector()
    }
}

impl F32BeReader {
    /// Create `F32BeReader`, which reads samples from the given `writer` at
    /// the specified `sample_rate`
    pub fn new<R>(chunk_len: usize, sample_rate: f64, mut reader: R) -> Self
    where
        R: Read + Send + 'static,
    {
        let mut buf_pool = ChunkBufPool::<Complex<f32>>::new();
        let mut bytes: Vec<u8> = vec![0u8; chunk_len.checked_mul(8).unwrap()];
        let inner = ClosureSource::new(chunk_len, sample_rate, move || {
            let mut chunk_buf = buf_pool.get_with_capacity(chunk_len);
            match reader.read_exact(&mut bytes) {
                Ok(()) => {
                    for sample_bytes in bytes.chunks_exact_mut(8) {
                        let re = f32::from_be_bytes(sample_bytes[0..4].try_into().unwrap());
                        let im = f32::from_be_bytes(sample_bytes[4..8].try_into().unwrap());
                        chunk_buf.push(Complex::new(re, im));
                    }
                    Ok(Some(chunk_buf.finalize()))
                }
                Err(err) => {
                    if err.kind() == io::ErrorKind::UnexpectedEof {
                        Ok(None)
                    } else {
                        Err(err)
                    }
                }
            }
        });
        F32BeReader { inner }
    }
    /// Create `F32BeReader`, which reads all samples from a file at the
    /// specified `sample_rate`
    pub fn with_path<P: AsRef<Path>>(chunk_len: usize, sample_rate: f64, path: P) -> Self {
        Self::new(
            chunk_len,
            sample_rate,
            BufReader::new(File::create(path).expect("could not open file for source")),
        )
    }
    /// Wait for stream to finish
    pub async fn wait(self) -> Result<(), io::Error> {
        self.inner.wait().await
    }
    /// Stop operation
    pub async fn stop(self) -> Result<(), io::Error> {
        self.inner.stop().await
    }
}

mod tests {}
