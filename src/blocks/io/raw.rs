//! Raw I/Q data input and output

use crate::bufferpool::*;
use crate::flow::*;
use crate::numbers::*;
use crate::samples::*;

use tokio::sync::oneshot;
use tokio::task::{spawn, JoinHandle};
use tokio::time::{interval, MissedTickBehavior};

use std::fs::File;
use std::future::{ready, Future};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::time::Duration;

/// Block invoking a callback to produce [`Chunk<T>`]s at a specified rate
///
/// The type argument `E` indicates a possible reported error type by the
/// callback.
pub struct ClosureSource<T, E> {
    sender: Sender<Samples<T>>,
    abort: oneshot::Sender<()>,
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
        let (abort_send, mut abort_recv) = oneshot::channel::<()>();
        let join_handle = spawn(async move {
            let mut clock = interval(Duration::from_secs_f64(sample_rate / chunk_len as f64));
            clock.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                match abort_recv.try_recv() {
                    Ok(()) => unreachable!(),
                    Err(oneshot::error::TryRecvError::Empty) => (),
                    Err(oneshot::error::TryRecvError::Closed) => return Ok(()),
                }
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
            abort: abort_send,
            join_handle,
        }
    }
    /// Wait for stream to finish
    pub async fn wait(self) -> Result<(), E> {
        self.join_handle.await.expect("task panicked")
    }
    /// Stop operation
    pub async fn stop(self) -> Result<(), E> {
        drop(self.abort);
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

/// Error returned by [`ContinuousClosureSink::wait`] and
/// [`ContinuousClosureSink::stop`]
#[derive(Debug)]
pub enum ContinuousClosureSinkError<E> {
    /// A buffer underrun occurred
    Underrun,
    /// An error was reported by the processing closure
    Report(E),
}

enum ContinuousClosureSinkStatus<E> {
    RecvError(RecvError),
    Report(E),
}

/// Block invoking a callback for every received [`Chunk<T>`] and failing on
/// buffer underrun
///
/// The type argument `E` indicates a possible reported error type by the
/// callback.
///
/// [`Chunk<T>`]: crate::bufferpool::Chunk
pub struct ContinuousClosureSink<T, E> {
    receiver: Receiver<Samples<T>>,
    join_handle: JoinHandle<ContinuousClosureSinkStatus<E>>,
}

impl<T, E> Consumer<Samples<T>> for ContinuousClosureSink<T, E>
where
    T: Clone,
{
    fn receiver(&self) -> &Receiver<Samples<T>> {
        &self.receiver
    }
}

impl<T, E> ContinuousClosureSink<T, E>
where
    T: Clone + Send + Sync + 'static,
    E: Send + 'static,
{
    /// Create block which invokes the `process` closure for each received
    /// [`Chunk<T>`]
    ///
    /// [`Chunk<T>`]: crate::bufferpool::Chunk
    pub fn new<F>(mut process: F) -> Self
    where
        F: FnMut(&[T]) -> Result<(), E> + Send + 'static,
    {
        Self::new_async(move |arg| ready(process(arg)))
    }
    /// Create block which invokes the `process` closure for each received
    /// [`Chunk<T>`] and `await`s its result
    ///
    /// This function is the same as [`ContinuousClosureSink::new`] but accepts
    /// an asynchronously working closure.
    ///
    /// [`Chunk<T>`]: crate::bufferpool::Chunk
    pub fn new_async<F, R>(mut process: F) -> Self
    where
        F: FnMut(&[T]) -> R + Send + 'static,
        R: Future<Output = Result<(), E>> + Send,
    {
        let receiver = Receiver::<Samples<T>>::new();
        let mut input = receiver.stream();
        let join_handle = spawn(async move {
            loop {
                match input.recv().await {
                    Ok(Samples {
                        sample_rate: _,
                        chunk,
                    }) => match process(&chunk).await {
                        Ok(()) => (),
                        Err(err) => return ContinuousClosureSinkStatus::Report(err),
                    },
                    Err(err) => return ContinuousClosureSinkStatus::RecvError(err),
                }
            }
        });
        Self {
            receiver,
            join_handle,
        }
    }
    /// Wait for stream to finish
    pub async fn wait(self) -> Result<(), ContinuousClosureSinkError<E>> {
        match self.join_handle.await {
            Ok(ContinuousClosureSinkStatus::RecvError(RecvError::Finished)) => Ok(()),
            Ok(ContinuousClosureSinkStatus::RecvError(_)) => {
                Err(ContinuousClosureSinkError::Underrun)
            }
            Ok(ContinuousClosureSinkStatus::Report(err)) => {
                Err(ContinuousClosureSinkError::Report(err))
            }
            Err(_) => panic!("task panicked"),
        }
    }
    /// Stop operation
    pub async fn stop(self) -> Result<(), ContinuousClosureSinkError<E>> {
        drop(self.receiver);
        match self.join_handle.await {
            Ok(ContinuousClosureSinkStatus::RecvError(RecvError::Closed)) => Ok(()),
            Ok(ContinuousClosureSinkStatus::RecvError(RecvError::Finished)) => Ok(()),
            Ok(ContinuousClosureSinkStatus::RecvError(_)) => {
                Err(ContinuousClosureSinkError::Underrun)
            }
            Ok(ContinuousClosureSinkStatus::Report(err)) => {
                Err(ContinuousClosureSinkError::Report(err))
            }
            Err(_) => panic!("task panicked"),
        }
    }
}

/// Error returned by [`ContinuousF32BeWriter::wait`] and
/// [`ContinuousF32BeWriter::stop`]
#[derive(Debug)]
pub enum ContinuousWriterError {
    /// A buffer underrun occurred
    Underrun,
    /// An error was reported by the processing closure
    Io(io::Error),
}

/// Block which writes single precision float samples in big endianess to a
/// [writer][Write], failing on buffer underrun
pub struct ContinuousF32BeWriter {
    inner: ContinuousClosureSink<Complex<f32>, io::Error>,
}

impl Consumer<Samples<Complex<f32>>> for ContinuousF32BeWriter {
    fn receiver(&self) -> &Receiver<Samples<Complex<f32>>> {
        &self.inner.receiver
    }
}

impl ContinuousF32BeWriter {
    /// Create `ContinuousF32BeWriter`, which writes all samples to the given
    /// `writer`
    pub fn new<W>(mut writer: W) -> Self
    where
        W: Write + Send + 'static,
    {
        let inner = ContinuousClosureSink::new(move |chunk: &[Complex<f32>]| {
            for sample in chunk.iter() {
                writer.write_all(&sample.re.to_be_bytes())?;
                writer.write_all(&sample.im.to_be_bytes())?;
            }
            Ok(())
        });
        ContinuousF32BeWriter { inner }
    }
    /// Create `ContinuousF32BeWriter`, which writes all samples to a file
    pub fn with_path<P: AsRef<Path>>(path: P) -> Self {
        Self::new(BufWriter::new(
            File::create(path).expect("could not open file for sink"),
        ))
    }
    fn map_err(err: ContinuousClosureSinkError<io::Error>) -> ContinuousWriterError {
        match err {
            ContinuousClosureSinkError::Underrun => ContinuousWriterError::Underrun,
            ContinuousClosureSinkError::Report(rpt) => ContinuousWriterError::Io(rpt),
        }
    }
    /// Wait for stream to finish
    pub async fn wait(self) -> Result<(), ContinuousWriterError> {
        self.inner.wait().await.map_err(Self::map_err)
    }
    /// Stop operation
    pub async fn stop(self) -> Result<(), ContinuousWriterError> {
        self.inner.stop().await.map_err(Self::map_err)
    }
}

#[cfg(test)]
mod tests {}
