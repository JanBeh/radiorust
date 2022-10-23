//! Raw I/Q data input and output

use crate::bufferpool::*;
use crate::flow::*;
use crate::numbers::*;
use crate::samples::*;

use tokio::select;
use tokio::sync::watch;
use tokio::task::{spawn, JoinHandle};

use std::fs::File;
use std::future::{ready, Future};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::path::Path;

/// Block acting as [`Producer`], which uses a callback as source
///
/// The type argument `E` indicates a possible reported error type by the
/// callback.
pub struct ClosureSource<T, E> {
    sender_connector: SenderConnector<T>,
    abort: watch::Sender<()>,
    join_handle: JoinHandle<Result<(), E>>,
}

impl<T, E> Producer<T> for ClosureSource<T, E> {
    fn sender_connector(&self) -> &SenderConnector<T> {
        &self.sender_connector
    }
}

impl<T, E> ClosureSource<T, E>
where
    T: Clone + Send + Sync + 'static,
    E: Send + 'static,
{
    /// Create block which invokes the `retrieve` closure to produce values to
    /// send
    pub fn new<F>(mut retrieve: F) -> Self
    where
        F: FnMut() -> Result<Option<T>, E> + Send + 'static,
    {
        Self::new_async(move || ready(retrieve()))
    }
    /// Create block which invokes the `retrieve` closure and `await`s its
    /// result to produce values to send
    pub fn new_async<F, R>(mut retrieve: F) -> Self
    where
        F: FnMut() -> R + Send + 'static,
        R: Future<Output = Result<Option<T>, E>> + Send,
    {
        let (sender, sender_connector) = new_sender::<T>();
        let (abort_send, mut abort_recv) = watch::channel::<()>(());
        let join_handle = spawn(async move {
            loop {
                select! {
                    _ = abort_recv.changed() => return Ok(()),
                    result = retrieve() => {
                        match result {
                            Ok(Some(data)) => if let Err(_) = sender.send(data).await {
                                return Ok(());
                            },
                            Ok(None) => {
                                if let Err(_) = sender.finish().await { return Ok(()); }
                                return Ok(());
                            }
                            Err(err) => {
                                if let Err(_) = sender.reset().await { return Ok(()); }
                                return Err(err);
                            }
                        }
                    }
                }
            }
        });
        Self {
            sender_connector,
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

/// Block acting as [`Producer`], which reads [`f32`]s in big endianess from a
/// [reader][Read]
pub struct F32BeReader {
    inner: ClosureSource<Samples<Complex<f32>>, io::Error>,
}

impl Producer<Samples<Complex<f32>>> for F32BeReader {
    fn sender_connector(&self) -> &SenderConnector<Samples<Complex<f32>>> {
        self.inner.sender_connector()
    }
}

impl F32BeReader {
    /// Create `F32BeReader`, which reads samples from the given `writer`
    pub fn new<R>(chunk_len: usize, sample_rate: f64, mut reader: R) -> Self
    where
        R: Read + Send + 'static,
    {
        let mut buf_pool = ChunkBufPool::<Complex<f32>>::new();
        let mut bytes: Vec<u8> = vec![0u8; chunk_len.checked_mul(8).unwrap()];
        let inner = ClosureSource::<Samples<Complex<f32>>, io::Error>::new(move || {
            let mut chunk_buf = buf_pool.get_with_capacity(chunk_len);
            match reader.read_exact(&mut bytes) {
                Ok(()) => {
                    for sample_bytes in bytes.chunks_exact_mut(8) {
                        let re = f32::from_be_bytes(sample_bytes[0..4].try_into().unwrap());
                        let im = f32::from_be_bytes(sample_bytes[4..8].try_into().unwrap());
                        chunk_buf.push(Complex::new(re, im));
                    }
                    Ok(Some(Samples {
                        sample_rate,
                        chunk: chunk_buf.finalize(),
                    }))
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
    /// The stream has been reset (see [`RecvError::Reset`])
    Reset,
    /// An error was reported by the processing closure
    Report(E),
}

enum ContinuousClosureSinkStatus<E> {
    RecvError(RecvError),
    Report(E),
}

/// Block acting as [`Consumer`], which uses a callback as sink and aborts when
/// stream is reset
///
/// The type argument `E` indicates a possible reported error type by the
/// callback.
pub struct ContinuousClosureSink<T, E> {
    receiver_connector: ReceiverConnector<T>,
    join_handle: JoinHandle<ContinuousClosureSinkStatus<E>>,
}

impl<T, E> Consumer<T> for ContinuousClosureSink<T, E>
where
    T: Clone,
{
    fn receiver_connector(&self) -> &ReceiverConnector<T> {
        &self.receiver_connector
    }
}

impl<T, E> ContinuousClosureSink<T, E>
where
    T: Clone + Send + Sync + 'static,
    E: Send + 'static,
{
    /// Create block which invokes the `process` closure for received data
    pub fn new<F>(mut process: F) -> Self
    where
        F: FnMut(T) -> Result<(), E> + Send + 'static,
    {
        Self::new_async(move |arg| ready(process(arg)))
    }
    /// Create block which invokes the `process` closure for received data and
    /// `await`s its result
    ///
    /// This function is the same as [`ContinuousClosureSink::new`] but accepts
    /// an asynchronously working closure.
    pub fn new_async<F, R>(mut process: F) -> Self
    where
        F: FnMut(T) -> R + Send + 'static,
        R: Future<Output = Result<(), E>> + Send,
    {
        let (mut receiver, receiver_connector) = new_receiver::<T>();
        let join_handle = spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(data) => match process(data).await {
                        Ok(()) => (),
                        Err(err) => return ContinuousClosureSinkStatus::Report(err),
                    },
                    Err(err) => return ContinuousClosureSinkStatus::RecvError(err),
                }
            }
        });
        Self {
            receiver_connector,
            join_handle,
        }
    }
    /// Wait for stream to finish
    pub async fn wait(self) -> Result<(), ContinuousClosureSinkError<E>> {
        match self.join_handle.await {
            Ok(ContinuousClosureSinkStatus::RecvError(RecvError::Finished)) => Ok(()),
            Ok(ContinuousClosureSinkStatus::RecvError(_)) => Err(ContinuousClosureSinkError::Reset),
            Ok(ContinuousClosureSinkStatus::Report(err)) => {
                Err(ContinuousClosureSinkError::Report(err))
            }
            Err(_) => panic!("task panicked"),
        }
    }
    /// Stop operation
    pub async fn stop(self) -> Result<(), ContinuousClosureSinkError<E>> {
        self.receiver_connector.disconnect();
        drop(self.receiver_connector);
        match self.join_handle.await {
            Ok(ContinuousClosureSinkStatus::RecvError(RecvError::Closed)) => Ok(()),
            Ok(ContinuousClosureSinkStatus::RecvError(RecvError::Finished)) => Ok(()),
            Ok(ContinuousClosureSinkStatus::RecvError(_)) => Err(ContinuousClosureSinkError::Reset),
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
    /// The stream has been reset (see [`RecvError::Reset`])
    Reset,
    /// An error was reported by the processing closure
    Io(io::Error),
}

/// Block acting as [`Consumer`], which writes [`f32`]s in big endianess to a
/// [writer][Write] and aborts when stream is reset
pub struct ContinuousF32BeWriter {
    inner: ContinuousClosureSink<Samples<Complex<f32>>, io::Error>,
}

impl Consumer<Samples<Complex<f32>>> for ContinuousF32BeWriter {
    fn receiver_connector(&self) -> &ReceiverConnector<Samples<Complex<f32>>> {
        self.inner.receiver_connector()
    }
}

impl ContinuousF32BeWriter {
    /// Create `ContinuousF32BeWriter`, which writes all samples to the given
    /// `writer`
    pub fn new<W>(mut writer: W) -> Self
    where
        W: Write + Send + 'static,
    {
        let inner = ContinuousClosureSink::new(move |samples: Samples<Complex<f32>>| {
            for sample in samples.chunk.iter() {
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
            ContinuousClosureSinkError::Reset => ContinuousWriterError::Reset,
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
mod tests {
    use super::*;
    #[tokio::test]
    async fn test_closure_source_stop() {
        let mut values = vec![1, 2, 3].into_iter();
        let source = ClosureSource::<i32, ()>::new_async(move || {
            let value = values.next();
            async move {
                match value {
                    Some(value) => Ok(Some(value)),
                    None => std::future::pending().await,
                }
            }
        });
        let (mut receiver, receiver_connector) = new_receiver::<i32>();
        receiver_connector.connect_to_producer(&source);
        drop(receiver_connector);
        assert_eq!(receiver.recv().await, Ok(1));
        assert_eq!(receiver.recv().await, Ok(2));
        assert_eq!(receiver.recv().await, Ok(3));
        // optional:
        // assert_eq!(source.stop().await, Ok(()));
        drop(source);
        assert_eq!(receiver.recv().await, Err(RecvError::Reset));
        assert_eq!(receiver.recv().await, Err(RecvError::Closed));
        assert_eq!(receiver.recv().await, Err(RecvError::Closed));
    }
}
