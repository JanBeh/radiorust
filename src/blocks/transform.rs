//! Basic transformations

use crate::bufferpool::*;
use crate::flow::*;
use crate::impl_block_trait;
use crate::numbers::*;
use crate::signal::*;

use num::rational::Ratio;
use tokio::sync::{mpsc, watch};
use tokio::task::spawn;

/// Gain control
///
/// Note that while this block works with generic [`Float`]s, the `gain` value
/// passed to [`GainControl::new`] and [`GainControl::set`] is of type [`f64`].
///
/// The `gain` value is multiplied with each sample such that a value of `0.5`
/// corresponds to *−0.6 dB*.
///
/// # Example
///
/// ```
/// # tokio::runtime::Runtime::new().unwrap().block_on(async move {
/// use radiorust::blocks::transform::GainControl;
/// let attenuate_6db = GainControl::<f32>::new(0.5f64);
/// # });
/// ```
pub struct GainControl<Flt> {
    receiver_connector: ReceiverConnector<Signal<Complex<Flt>>>,
    sender_connector: SenderConnector<Signal<Complex<Flt>>>,
    gain: watch::Sender<f64>,
}

impl_block_trait! { <Flt> Consumer<Signal<Complex<Flt>>> for GainControl<Flt> }
impl_block_trait! { <Flt> Producer<Signal<Complex<Flt>>> for GainControl<Flt> }

impl<Flt> GainControl<Flt>
where
    Flt: Float,
{
    /// Creates a block which multiplies each sample with the given `gain`
    pub fn new(gain: f64) -> Self {
        let (mut receiver, receiver_connector) = new_receiver::<Signal<Complex<Flt>>>();
        let (sender, sender_connector) = new_sender::<Signal<Complex<Flt>>>();
        let (gain_send, mut gain_recv) = watch::channel(gain);
        spawn(async move {
            let mut gain = flt!(gain);
            let mut buf_pool = ChunkBufPool::new();
            loop {
                let Ok(signal) = receiver.recv().await else { return; };
                match signal {
                    Signal::Samples {
                        sample_rate,
                        chunk: input_chunk,
                    } => {
                        if gain_recv.has_changed().unwrap_or(false) {
                            gain = flt!(gain_recv.borrow_and_update().clone())
                        }
                        let mut output_chunk = buf_pool.get_with_capacity(input_chunk.len());
                        for sample in input_chunk.iter() {
                            output_chunk.push(sample * gain);
                        }
                        let Ok(()) = sender
                            .send(Signal::Samples {
                                sample_rate,
                                chunk: output_chunk.finalize(),
                            })
                            .await
                        else { return; };
                    }
                    event @ Signal::Event { .. } => {
                        let Ok(()) = sender.send(event).await else { return; };
                    }
                }
            }
        });
        Self {
            receiver_connector,
            sender_connector,
            gain: gain_send,
        }
    }
    /// Get current gain
    pub fn get(&self) -> f64 {
        self.gain.borrow().clone()
    }
    /// Set gain
    pub fn set(&self, gain: f64) {
        self.gain.send_replace(gain);
    }
}

/// Block which receives [`Signal<T>`] and applies a closure to every sample,
/// e.g. to change amplitude or to apply a constant phase shift, before sending
/// it further.
///
/// # Example
///
/// ```
/// # tokio::runtime::Runtime::new().unwrap().block_on(async move {
/// use radiorust::{blocks::MapSample, numbers::Complex};
/// let attenuate_6db = MapSample::<Complex<f32>>::with_closure(move |x| x / 2.0);
/// # });
/// ```
///
/// See also [`GainControl`] for a more direct way to use an attenuator.
pub struct MapSample<T> {
    receiver_connector: ReceiverConnector<Signal<T>>,
    sender_connector: SenderConnector<Signal<T>>,
    closure: mpsc::UnboundedSender<Box<dyn FnMut(T) -> T + Send + 'static>>,
}

impl_block_trait! { <T> Consumer<Signal<T>> for MapSample<T> }
impl_block_trait! { <T> Producer<Signal<T>> for MapSample<T> }

impl<T> MapSample<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Creates a block which doesn't modify the samples
    pub fn new() -> Self {
        Self::with_closure(|x| x)
    }
    /// Creates a block which applies the given `closure` to every sample
    pub fn with_closure<F>(closure: F) -> Self
    where
        F: FnMut(T) -> T + Send + 'static,
    {
        Self::new_internal(Box::new(closure))
    }
    fn new_internal(closure: Box<dyn FnMut(T) -> T + Send + 'static>) -> Self {
        let (mut receiver, receiver_connector) = new_receiver::<Signal<T>>();
        let (sender, sender_connector) = new_sender::<Signal<T>>();
        let (closure_send, mut closure_recv) = mpsc::unbounded_channel();
        closure_send
            .send(closure)
            .unwrap_or_else(|_| unreachable!());
        spawn(async move {
            let mut buf_pool = ChunkBufPool::new();
            let mut closure_opt: Option<Box<dyn FnMut(T) -> T + Send + 'static>> = None;
            loop {
                let Ok(signal) = receiver.recv().await else { return; };
                match signal {
                    Signal::Samples {
                        sample_rate,
                        chunk: input_chunk,
                    } => {
                        loop {
                            match closure_recv.try_recv() {
                                Ok(f) => closure_opt = Some(f),
                                Err(_) => break,
                            }
                        }
                        let closure = closure_opt.as_mut().unwrap();
                        let mut output_chunk = buf_pool.get_with_capacity(input_chunk.len());
                        for sample in input_chunk.iter() {
                            output_chunk.push(closure(sample.clone()));
                        }
                        let Ok(()) = sender
                            .send(Signal::Samples {
                                sample_rate,
                                chunk: output_chunk.finalize(),
                            })
                            .await
                        else { return; };
                    }
                    event @ Signal::Event { .. } => {
                        let Ok(()) = sender.send(event).await else { return; };
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
        F: FnMut(T) -> T + Send + 'static,
    {
        self.closure.send(Box::new(closure)).ok();
    }
}

/// Block which applies a closure to every received [`Message`] (e.g.
/// [`Signal`]) before sending it further.
///
/// Note that this block doesn't allow to specify how [`RecvError`]s are
/// handled. Thus in in many cases, it is better to implement a custom type as
/// [`Consumer`]/[`Producer`] (see [`Nop`] for an example) instead of using
/// this block.
///
/// See also [`MapSample`] for a block which performs the operation on a
/// per-sample basis and may be better suited when wanting to apply a function
/// to each sample.
///
/// [`Nop`]: crate::blocks::Nop
pub struct MapSignal<T> {
    receiver_connector: ReceiverConnector<T>,
    sender_connector: SenderConnector<T>,
    closure: mpsc::UnboundedSender<Box<dyn FnMut(T) -> T + Send + 'static>>,
}

impl_block_trait! { <T> Consumer<T> for MapSignal<T> }
impl_block_trait! { <T> Producer<T> for MapSignal<T> }

impl<T> MapSignal<T>
where
    T: Message + Send + 'static,
{
    /// Creates a block which doesn't modify the received messages (e.g. which
    /// doesn't modify the [`Signal`]s)
    pub fn new() -> Self {
        Self::with_closure(|x| x)
    }
    /// Creates a block which applies the given `closure` to each message (e.g.
    /// to each [`Signal`])
    pub fn with_closure<F>(closure: F) -> Self
    where
        F: FnMut(T) -> T + Send + 'static,
    {
        Self::new_internal(Box::new(closure))
    }
    fn new_internal(closure: Box<dyn FnMut(T) -> T + Send + 'static>) -> Self {
        let (mut receiver, receiver_connector) = new_receiver::<T>();
        let (sender, sender_connector) = new_sender::<T>();
        let (closure_send, mut closure_recv) = mpsc::unbounded_channel();
        closure_send
            .send(closure)
            .unwrap_or_else(|_| unreachable!());
        spawn(async move {
            let mut closure_opt: Option<Box<dyn FnMut(T) -> T + Send + 'static>> = None;
            loop {
                let Ok(signal) = receiver.recv().await else { return; };
                loop {
                    match closure_recv.try_recv() {
                        Ok(f) => closure_opt = Some(f),
                        Err(_) => break,
                    }
                }
                let closure = closure_opt.as_mut().unwrap();
                let signal = closure(signal);
                let Ok(()) = sender.send(signal).await else { return; };
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
        F: FnMut(T) -> T + Send + 'static,
    {
        self.closure.send(Box::new(closure)).ok();
    }
}

/// Complex oscillator and mixer, which shifts all frequencies in an I/Q stream
pub struct FreqShifter<Flt> {
    receiver_connector: ReceiverConnector<Signal<Complex<Flt>>>,
    sender_connector: SenderConnector<Signal<Complex<Flt>>>,
    precision: f64,
    shift: watch::Sender<f64>,
}

impl_block_trait! { <Flt> Consumer<Signal<Complex<Flt>>> for FreqShifter<Flt> }
impl_block_trait! { <Flt> Producer<Signal<Complex<Flt>>> for FreqShifter<Flt> }

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
        let (mut receiver, receiver_connector) = new_receiver::<Signal<Complex<Flt>>>();
        let (sender, sender_connector) = new_sender::<Signal<Complex<Flt>>>();
        let (shift_send, mut shift_recv) = watch::channel(shift);
        spawn(async move {
            let mut phase_vec: Vec<Complex<Flt>> = Vec::new();
            let mut phase_idx: usize = 0;
            let mut prev_sample_rate: Option<f64> = None;
            let mut buf_pool = ChunkBufPool::<Complex<Flt>>::new();
            loop {
                let Ok(signal) = receiver.recv().await else { return; };
                match signal {
                    Signal::Samples {
                        sample_rate,
                        chunk: input_chunk,
                    } => {
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
                        let Ok(()) = sender
                            .send(Signal::Samples {
                                sample_rate,
                                chunk: output_chunk.finalize(),
                            })
                            .await
                        else { return; };
                    }
                    event @ Signal::Event { .. } => {
                        let Ok(()) = sender.send(event).await else { return; };
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
mod tests {
    use super::*;
    #[tokio::test]
    async fn test_gain_control() {
        let (sender, sender_connector) = new_sender::<Signal<Complex<f32>>>();
        let attenuator = GainControl::new(0.25);
        let (mut receiver, receiver_connector) = new_receiver::<Signal<Complex<f32>>>();
        attenuator.feed_from(&sender_connector);
        attenuator.feed_into(&receiver_connector);
        let mut buf_pool = ChunkBufPool::<Complex<f32>>::new();
        let mut chunk_buf = buf_pool.get();
        chunk_buf.push(Complex::new(32.0, -1.0));
        chunk_buf.push(Complex::new(15.0, -2.0));
        let chunk = chunk_buf.finalize();
        sender
            .send(Signal::Samples {
                sample_rate: 48000.0,
                chunk,
            })
            .await
            .unwrap();
        let Signal::Samples { chunk, .. } = receiver.recv().await.unwrap()
        else { panic!(); };
        assert_eq!(chunk[0].re, 8.0);
        assert_eq!(chunk[0].im, -0.25);
        assert_eq!(chunk[1].re, 3.75);
        assert_eq!(chunk[1].im, -0.5);
    }
}
