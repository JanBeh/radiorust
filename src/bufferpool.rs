//! Pools allowing to get buffers that are recycled when dropped
//!
//! See example in [`ChunkBufPool`].

use tokio::sync::mpsc;

use std::mem::take;
use std::ops::{Deref, DerefMut, Range};
use std::sync::Arc;

/// Buffer for reading that gets recycled when dropped
///
/// `Chunk<T>` implements [`Deref`] with [`Target`][Deref::Target] being
/// [`[T]`][prim@slice].
///
/// A `Chunk<T>` can be created by invoking [`ChunkBuf::finalize`].
/// It is read-only with the exception that parts at the beginning may be
/// discarded.
///
/// When dropped, the underlying buffer gets recycled by sending it back to the
/// originating [`ChunkBufPool`] if no other chunks (clones or separated
/// chunks) are left sharing the same internal buffer.
#[derive(Clone, Debug)]
pub struct Chunk<T> {
    buffer: Arc<Vec<T>>,
    range: Range<usize>,
    recycler: mpsc::UnboundedSender<Vec<T>>,
}

impl<T> Chunk<T> {
    fn new(buffer: Vec<T>, recycler: mpsc::UnboundedSender<Vec<T>>) -> Self {
        let len = buffer.len();
        Chunk {
            buffer: Arc::new(buffer),
            range: 0..len,
            recycler,
        }
    }
    /// Discard the first `len` elements of the chunk
    pub fn discard_beginning(&mut self, len: usize) {
        assert!(len <= self.range.end - self.range.start, "length exceeded");
        self.range.start += len;
    }
    /// Split chunk into two parts
    ///
    /// This returns a new [`Chunk<T>`] with size `len` consisting of the first
    /// `len` elements. In the existing chunk, the first `len` elements are
    /// discarded.
    /// No data will be copied for this operation.
    pub fn separate_beginning(&mut self, len: usize) -> Self {
        assert!(len <= self.range.end - self.range.start, "length exceeded");
        let new_range = self.range.start..self.range.start + len;
        self.range.start = new_range.end;
        Chunk {
            buffer: self.buffer.clone(),
            range: new_range,
            recycler: self.recycler.clone(),
        }
    }
}

impl<T> Drop for Chunk<T> {
    fn drop(&mut self) {
        if let Ok(buffer) = Arc::try_unwrap(take(&mut self.buffer)) {
            self.recycler.send(buffer).ok();
        }
    }
}

impl<T> Deref for Chunk<T> {
    type Target = [T];
    fn deref(&self) -> &Self::Target {
        &self.buffer[self.range.clone()]
    }
}

/// Buffer for writing that can be converted into a cheaply cloneable
/// [`Chunk<T>`]
///
/// `ChunkBuf<T>` implements [`Deref`] and [`DerefMut`] with
/// [`Target`][Deref::Target] being [`[T]`][prim@slice].
///
/// A `ChunkBuf<T>` can be obtained by invoking [`ChunkBufPool::get`] and be
/// converted into a [`Chunk<T>`] by calling [`ChunkBuf::finalize`].
#[derive(Debug)]
pub struct ChunkBuf<T> {
    buffer: Vec<T>,
    recycler: Option<mpsc::UnboundedSender<Vec<T>>>,
}

impl<T> ChunkBuf<T> {
    fn new(buffer: Vec<T>, recycler: mpsc::UnboundedSender<Vec<T>>) -> Self {
        ChunkBuf {
            buffer,
            recycler: Some(recycler),
        }
    }
    /// Convert into [`Chunk<T>`]
    pub fn finalize(mut self) -> Chunk<T> {
        Chunk::new(take(&mut self.buffer), self.recycler.take().unwrap())
    }
}

impl<T> Drop for ChunkBuf<T> {
    fn drop(&mut self) {
        if let Some(recycler) = self.recycler.take() {
            recycler.send(take(&mut self.buffer)).ok();
        }
    }
}

impl<T> Deref for ChunkBuf<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Self::Target {
        &self.buffer
    }
}

impl<T> DerefMut for ChunkBuf<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.buffer
    }
}

impl<T> From<ChunkBuf<T>> for Chunk<T> {
    fn from(chunk_buf: ChunkBuf<T>) -> Self {
        chunk_buf.finalize()
    }
}

/// Pool to obtain [`ChunkBuf<T>`]s
///
/// [`ChunkBufPool::get`] will either reuse a previously [recycled buffer] or
/// create a new buffer, returning a [`ChunkBuf<T>`] in either case.
/// When it's known how many elements will be filled into a [`ChunkBuf`], then
/// [`ChunkBufPool::get_with_capacity`] can be used.
///
/// [recycled buffer]: Chunk
///
/// # Example
///
/// ```
/// # use radiorust::bufferpool::ChunkBufPool;
/// let mut buf_pool = ChunkBufPool::<u8>::new();
/// let mut chunk_buf = buf_pool.get();
/// chunk_buf.push(64);
/// chunk_buf.push(65);
/// let chunk = chunk_buf.finalize();
/// let chunk2 = chunk.clone();
/// ```
pub struct ChunkBufPool<T> {
    recycler: mpsc::UnboundedSender<Vec<T>>,
    dispenser: mpsc::UnboundedReceiver<Vec<T>>,
}

impl<T> ChunkBufPool<T> {
    /// Create a new `ChunkBufPool<T>`
    pub fn new() -> Self {
        let (recycler, dispenser) = mpsc::unbounded_channel::<Vec<T>>();
        Self {
            recycler,
            dispenser,
        }
    }
    /// Get a new [`ChunkBuf<T>`]
    pub fn get(&mut self) -> ChunkBuf<T> {
        let buffer = match self.dispenser.try_recv() {
            Ok(mut buffer) => {
                buffer.clear();
                buffer
            }
            Err(_) => Vec::new(),
        };
        ChunkBuf::new(buffer, self.recycler.clone())
    }
    /// Get a new [`ChunkBuf<T>`] with at least the specified `capacity`
    pub fn get_with_capacity(&mut self, capacity: usize) -> ChunkBuf<T> {
        let buffer = match self.dispenser.try_recv() {
            Ok(mut buffer) => {
                buffer.clear();
                buffer
            }
            Err(_) => Vec::with_capacity(capacity),
        };
        ChunkBuf::new(buffer, self.recycler.clone())
    }
}
