//! Raw I/Q data output

use crate::blocks::Samples;
use crate::flow::*;

use num::Complex;
use tokio::task::{spawn, JoinHandle};

use std::fs::File;
use std::future::{ready, Future};
use std::io::{self, BufWriter, Write};
use std::path::Path;

/// Error returned by [`ContinuousClosure::wait`] and [`ContinuousClosure::stop`]
pub enum ContinuousClosureError<E> {
    /// A buffer underrun occurred
    Underrun,
    /// An error was reported by the processing closure
    Report(E),
}

enum ContinuousClosureStatus<E> {
    RecvError(RecvError),
    Report(E),
}

/// Block invoking an async callback for every received [`Chunk<T>`] and
/// failing on buffer underrun
///
/// The type argument `E` indicates a possible reported error type by the
/// callback.
///
/// [`Chunk<T>`]: crate::bufferpool::Chunk
pub struct ContinuousClosure<T, E> {
    receiver: Receiver<Samples<T>>,
    join_handle: JoinHandle<ContinuousClosureStatus<E>>,
}

impl<T, E> Consumer<Samples<T>> for ContinuousClosure<T, E>
where
    T: Clone,
{
    fn receiver(&self) -> &Receiver<Samples<T>> {
        &self.receiver
    }
}

impl<T, E> ContinuousClosure<T, E>
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
    /// This function is the same as [`ContinuousClosure::new`] but accepts an
    /// asynchronously working closure.
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
                        Err(err) => return ContinuousClosureStatus::Report(err),
                    },
                    Err(err) => return ContinuousClosureStatus::RecvError(err),
                }
            }
        });
        Self {
            receiver,
            join_handle,
        }
    }
    /// Wait for stream to finish
    pub async fn wait(self) -> Result<(), ContinuousClosureError<E>> {
        match self.join_handle.await {
            Ok(ContinuousClosureStatus::RecvError(RecvError::Finished)) => Ok(()),
            Ok(ContinuousClosureStatus::RecvError(_)) => Err(ContinuousClosureError::Underrun),
            Ok(ContinuousClosureStatus::Report(err)) => Err(ContinuousClosureError::Report(err)),
            Err(_) => panic!("task panicked"),
        }
    }
    /// Stop operation
    pub async fn stop(self) -> Result<(), ContinuousClosureError<E>> {
        drop(self.receiver);
        match self.join_handle.await {
            Ok(ContinuousClosureStatus::RecvError(RecvError::Closed)) => Ok(()),
            Ok(ContinuousClosureStatus::RecvError(RecvError::Finished)) => Ok(()),
            Ok(ContinuousClosureStatus::RecvError(_)) => Err(ContinuousClosureError::Underrun),
            Ok(ContinuousClosureStatus::Report(err)) => Err(ContinuousClosureError::Report(err)),
            Err(_) => panic!("task panicked"),
        }
    }
}

/// Error returned by [`ContinuousF32BeWriter::wait`] and
/// [`ContinuousF32BeWriter::stop`]
pub enum ContinuousWriterError {
    /// A buffer underrun occurred
    Underrun,
    /// An error was reported by the processing closure
    Io(io::Error),
}

/// Block which writes single precision float samples in big endianess to a
/// [writer][Write], failing on buffer underrun
pub struct ContinuousF32BeWriter {
    inner: ContinuousClosure<Complex<f32>, io::Error>,
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
        let inner = ContinuousClosure::new(move |chunk: &[Complex<f32>]| {
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
    fn map_err(err: ContinuousClosureError<io::Error>) -> ContinuousWriterError {
        match err {
            ContinuousClosureError::Underrun => ContinuousWriterError::Underrun,
            ContinuousClosureError::Report(rpt) => ContinuousWriterError::Io(rpt),
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
