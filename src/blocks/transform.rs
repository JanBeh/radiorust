//! Basic transformations

use crate::bufferpool::*;
use crate::flow::*;
use crate::flt;
use crate::numbers::*;
use crate::samples::*;

use num::rational::Ratio;
use tokio::sync::{mpsc, watch};
use tokio::task::spawn;

/// Block which receives [`Samples<T>`] and applies a closure to every sample,
/// e.g. to change amplitude or to apply a constant phase shift, before sending
/// it further.
///
/// # Example
///
/// ```
/// # tokio::runtime::Runtime::new().unwrap().block_on(async move {
/// use radiorust::{blocks::Function, numbers::Complex};
/// let attenuate_6db = Function::<Complex<f32>>::with_closure(move |x| x / 2.0);
/// # });
/// ```
pub struct Function<T> {
    receiver_connector: ReceiverConnector<Samples<T>>,
    sender_connector: SenderConnector<Samples<T>>,
    closure: mpsc::UnboundedSender<Box<dyn Fn(T) -> T + Send + Sync + 'static>>,
}

impl<T> Consumer<Samples<T>> for Function<T> {
    fn receiver_connector(&self) -> &ReceiverConnector<Samples<T>> {
        &self.receiver_connector
    }
}

impl<T> Producer<Samples<T>> for Function<T> {
    fn sender_connector(&self) -> &SenderConnector<Samples<T>> {
        &self.sender_connector
    }
}

impl<T> Function<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Creates a block which doesn't modify the [`Samples`]
    pub fn new() -> Self {
        Self::with_closure(|x| x)
    }
    /// Creates a block which applies the given `closure` to every sample
    pub fn with_closure<F>(closure: F) -> Self
    where
        F: Fn(T) -> T + Send + Sync + 'static,
    {
        Self::new_internal(Box::new(closure))
    }
    fn new_internal(closure: Box<dyn Fn(T) -> T + Send + Sync + 'static>) -> Self {
        let (mut receiver, receiver_connector) = new_receiver::<Samples<T>>();
        let (sender, sender_connector) = new_sender::<Samples<T>>();
        let (closure_send, mut closure_recv) = mpsc::unbounded_channel();
        closure_send
            .send(closure)
            .unwrap_or_else(|_| unreachable!());
        spawn(async move {
            let mut buf_pool = ChunkBufPool::new();
            let mut closure_opt: Option<Box<dyn Fn(T) -> T + Send + Sync + 'static>> = None;
            loop {
                match receiver.recv().await {
                    Ok(Samples {
                        sample_rate,
                        chunk: input_chunk,
                    }) => {
                        loop {
                            match closure_recv.try_recv() {
                                Ok(f) => closure_opt = Some(f),
                                Err(_) => break,
                            }
                        }
                        let closure = closure_opt.as_ref().unwrap();
                        let mut output_chunk = buf_pool.get_with_capacity(input_chunk.len());
                        for sample in input_chunk.iter() {
                            output_chunk.push(closure(sample.clone()));
                        }
                        if let Err(_) = sender
                            .send(Samples {
                                sample_rate: sample_rate,
                                chunk: output_chunk.finalize(),
                            })
                            .await
                        {
                            return;
                        }
                    }
                    Err(err) => {
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
            closure: closure_send,
        }
    }
    /// Modifies the used `closure`, which is applied to every sample
    pub fn set_closure<F>(&self, closure: F)
    where
        F: Fn(T) -> T + Send + Sync + 'static,
    {
        self.closure.send(Box::new(closure)).ok();
    }
}

/// Complex oscillator and mixer, which shifts all frequencies in an I/Q stream
pub struct FreqShifter<Flt> {
    receiver_connector: ReceiverConnector<Samples<Complex<Flt>>>,
    sender_connector: SenderConnector<Samples<Complex<Flt>>>,
    precision: f64,
    shift: watch::Sender<f64>,
}

impl<Flt> Consumer<Samples<Complex<Flt>>> for FreqShifter<Flt> {
    fn receiver_connector(&self) -> &ReceiverConnector<Samples<Complex<Flt>>> {
        &self.receiver_connector
    }
}

impl<Flt> Producer<Samples<Complex<Flt>>> for FreqShifter<Flt> {
    fn sender_connector(&self) -> &SenderConnector<Samples<Complex<Flt>>> {
        &self.sender_connector
    }
}

impl<Flt> FreqShifter<Flt>
where
    Flt: Float,
{
    /// Create new `FreqShifter` block with 1 Hz precision and initial
    /// frequency shift of zero
    pub fn new() -> Self {
        Self::with_precision_and_shift(1.0, 0.0)
    }
    /// Create new `FreqShifter` block with 1 Hz precision and given initial
    /// frequency `shift` in hertz
    pub fn with_shift(shift: f64) -> Self {
        Self::with_precision_and_shift(1.0, shift)
    }
    /// Create new `FreqShifter` block with given `precision` in hertz and an
    /// initial shift of zero
    pub fn with_precision(precision: f64) -> Self {
        Self::with_precision_and_shift(precision, 0.0)
    }
    /// Create new `FreqShifter` block with given `precision` and `shift`, both
    /// in hertz
    pub fn with_precision_and_shift(precision: f64, shift: f64) -> Self {
        let freq_to_ratio = move |sample_rate: f64, frequency: f64| {
            let denom: isize = (sample_rate / precision).round() as isize;
            let numer: isize = (denom as f64 * frequency / sample_rate).round() as isize;
            Ratio::new(numer, denom)
        };
        let (mut receiver, receiver_connector) = new_receiver::<Samples<Complex<Flt>>>();
        let (sender, sender_connector) = new_sender::<Samples<Complex<Flt>>>();
        let (shift_send, mut shift_recv) = watch::channel(shift);
        spawn(async move {
            let mut phase_vec: Vec<Complex<Flt>> = Vec::new();
            let mut phase_idx: usize = 0;
            let mut prev_sample_rate: Option<f64> = None;
            let mut buf_pool = ChunkBufPool::<Complex<Flt>>::new();
            loop {
                match receiver.recv().await {
                    Ok(Samples {
                        sample_rate,
                        chunk: input_chunk,
                    }) => {
                        let recalculate: bool = shift_recv.has_changed().unwrap_or(false)
                            || Some(sample_rate) != prev_sample_rate;
                        prev_sample_rate = Some(sample_rate);
                        if recalculate {
                            let start_phase: Flt = match phase_vec.is_empty() {
                                true => Flt::zero(),
                                false => phase_vec[phase_idx].arg(),
                            };
                            phase_vec.clear();
                            phase_idx = 0;
                            let shift = shift_recv.borrow_and_update().clone();
                            let ratio: Ratio<isize> = freq_to_ratio(sample_rate, shift);
                            let (numer, denom): (isize, isize) = ratio.into();
                            phase_vec.reserve(denom.try_into().unwrap());
                            let mut i: isize = 0;
                            for _ in 0..denom {
                                let (im, re) =
                                    Flt::sin_cos(start_phase + flt!(i) / flt!(denom) * Flt::TAU());
                                phase_vec.push(Complex::new(re, im));
                                i = i.checked_add(numer).unwrap();
                                i %= denom;
                            }
                        }
                        let mut output_chunk = buf_pool.get_with_capacity(input_chunk.len());
                        for sample in input_chunk.iter() {
                            output_chunk.push(sample * phase_vec[phase_idx]);
                            phase_idx += 1;
                            if phase_idx == phase_vec.len() {
                                phase_idx = 0;
                            }
                        }
                        if let Err(_) = sender
                            .send(Samples {
                                sample_rate: sample_rate,
                                chunk: output_chunk.finalize(),
                            })
                            .await
                        {
                            return;
                        }
                    }
                    Err(err) => {
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
            precision,
            shift: shift_send,
        }
    }
    /// Get frequency precision in hertz
    ///
    /// The precision can't be changed once the block has been created.
    /// Create a new block with [`FreqShifter::with_precision`] or
    /// [`FreqShifter::with_precision_and_shift`] if you need to change the
    /// frequency precision.
    pub fn precision(&self) -> f64 {
        self.precision
    }
    /// Get current frequency shift
    pub fn shift(&self) -> f64 {
        self.shift.borrow().clone()
    }
    /// Set frequency shift
    pub fn set_shift(&self, shift: f64) {
        self.shift.send_replace(shift);
    }
    /// Update frequency shift
    pub fn update_shift<F: FnOnce(&mut f64)>(&self, modify: F) {
        self.shift.send_modify(modify);
    }
}

#[cfg(test)]
mod tests {}
