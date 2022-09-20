use crate::bufferpool::*;
use crate::flow::*;
use crate::flt;
use crate::genfloat::Float;
use crate::math::*;
use crate::samples::Samples;

use num::rational::Ratio;
use num::Complex;
use tokio::sync::{mpsc, watch};
use tokio::task::spawn;

/// Block which performs no operation on the received data and simply sends it
/// out unchanged
///
/// This block mostly serves documentation purposes but may also be used to
/// (re-)connect multiple [`Receiver`]s at once.
/// Note that most blocks don't work with `T` but [`Samples<T>`] or
/// `Samples<Complex<Flt>>`.
pub struct Nop<T> {
    receiver: Receiver<T>,
    sender: Sender<T>,
}

impl<T> Consumer<T> for Nop<T>
where
    T: Clone,
{
    fn receiver(&self) -> &Receiver<T> {
        &self.receiver
    }
}

impl<T> Producer<T> for Nop<T>
where
    T: Clone,
{
    fn connector(&self) -> SenderConnector<T> {
        self.sender.connector()
    }
}

impl<T> Nop<T>
where
    T: Clone + Send + 'static,
{
    /// Creates a block which does nothing but pass data through
    pub fn new() -> Self {
        let receiver = Receiver::<T>::new();
        let sender = Sender::<T>::new();
        let mut input: ReceiverStream<T> = receiver.stream();
        let output: Sender<T> = sender.clone();
        spawn(async move {
            loop {
                match input.recv().await {
                    Ok(msg) => output.send(msg).await,
                    Err(err) => {
                        output.forward_error(err).await;
                        if err == RecvError::Closed {
                            return;
                        }
                    }
                }
            }
        });
        Self { receiver, sender }
    }
}

/// Block which receives [`Samples<T>`] and applies a closure to every sample,
/// e.g. to change amplitude or to apply a constant phase shift, before sending
/// it further.
///
/// # Example
///
/// ```
/// # tokio::runtime::Runtime::new().unwrap().block_on(async move {
/// use radiorust::blocks::Function;
/// use num::Complex;
/// let attenuate_6db = Function::<Complex<f32>>::with_closure(move |x| x / 2.0);
/// # });
/// ```
pub struct Function<T> {
    receiver: Receiver<Samples<T>>,
    sender: Sender<Samples<T>>,
    closure: mpsc::UnboundedSender<Box<dyn Fn(T) -> T + Send + Sync + 'static>>,
}

impl<T> Consumer<Samples<T>> for Function<T>
where
    T: Clone,
{
    fn receiver(&self) -> &Receiver<Samples<T>> {
        &self.receiver
    }
}

impl<T> Producer<Samples<T>> for Function<T>
where
    T: Clone,
{
    fn connector(&self) -> SenderConnector<Samples<T>> {
        self.sender.connector()
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
        let receiver = Receiver::<Samples<T>>::new();
        let sender = Sender::<Samples<T>>::new();
        let (closure_send, mut closure_recv) = mpsc::unbounded_channel();
        closure_send.send(closure).ok();
        let mut input = receiver.stream();
        let output = sender.clone();
        spawn(async move {
            let mut buf_pool = ChunkBufPool::new();
            let mut closure_opt: Option<Box<dyn Fn(T) -> T + Send + Sync + 'static>> = None;
            loop {
                match input.recv().await {
                    Ok(Samples {
                        sample_rate,
                        chunk: input_chunk,
                    }) => {
                        loop {
                            match closure_recv.try_recv() {
                                Ok(f) => closure_opt = Some(f),
                                Err(mpsc::error::TryRecvError::Empty) => break,
                                Err(mpsc::error::TryRecvError::Disconnected) => return,
                            }
                        }
                        let closure = closure_opt.as_ref().unwrap();
                        let mut output_chunk = buf_pool.get_with_capacity(input_chunk.len());
                        for sample in input_chunk.iter() {
                            output_chunk.push(closure(sample.clone()));
                        }
                        output
                            .send(Samples {
                                sample_rate: sample_rate,
                                chunk: output_chunk.finalize(),
                            })
                            .await;
                    }
                    Err(err) => {
                        output.forward_error(err).await;
                        if err == RecvError::Closed {
                            return;
                        }
                    }
                }
            }
        });
        Self {
            receiver,
            sender,
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

/// Block that receives [`Samples`] with arbitrary chunk lengths and produces
/// `Samples<T>` with fixed chunk length
///
/// This block may be needed when a certain chunk length is required, e.g. for
/// the [`filters::Filter`] block, unless the chunk length can be adjusted
/// otherwise, e.g. due to an existing [`Downsampler`] or [`Upsampler`].
///
/// [`filters::Filter`]: crate::blocks::filters::Filter
pub struct Rechunker<T> {
    receiver: Receiver<Samples<T>>,
    sender: Sender<Samples<T>>,
    output_chunk_len: watch::Sender<usize>,
}

impl<T> Consumer<Samples<T>> for Rechunker<T>
where
    T: Clone,
{
    fn receiver(&self) -> &Receiver<Samples<T>> {
        &self.receiver
    }
}

impl<T> Producer<Samples<T>> for Rechunker<T>
where
    T: Clone,
{
    fn connector(&self) -> SenderConnector<Samples<T>> {
        self.sender.connector()
    }
}

impl<T> Rechunker<T>
where
    T: Clone + Send + Sync + 'static,
{
    /// Create new `Rechunker` block with given output chunk length
    pub fn new(mut output_chunk_len: usize) -> Self {
        assert!(output_chunk_len > 0, "chunk length must be positive");
        let receiver = Receiver::<Samples<T>>::new();
        let sender = Sender::<Samples<T>>::new();
        let (output_chunk_len_send, mut output_chunk_len_recv) = watch::channel(output_chunk_len);
        let mut input = receiver.stream();
        let output = sender.clone();
        let mut buf_pool = ChunkBufPool::<T>::new();
        let mut samples_opt: Option<Samples<T>> = None;
        let mut patchwork_opt: Option<(f64, ChunkBuf<T>)> = None;
        spawn(async move {
            loop {
                let mut samples = match samples_opt {
                    Some(x) => x,
                    None => loop {
                        match input.recv().await {
                            Ok(samples) => {
                                if let Some((sample_rate, _)) = patchwork_opt {
                                    if sample_rate != samples.sample_rate {
                                        patchwork_opt = None;
                                        output.reset().await;
                                    }
                                }
                                break samples;
                            }
                            Err(err) => {
                                output.forward_error(err).await;
                                if err == RecvError::Closed {
                                    return;
                                }
                                patchwork_opt = None;
                            }
                        }
                    },
                };
                match output_chunk_len_recv.has_changed() {
                    Ok(false) => (),
                    Ok(true) => {
                        output_chunk_len = output_chunk_len_recv.borrow_and_update().clone()
                    }
                    Err(_) => return,
                }
                if let Some((sample_rate, mut patchwork_chunk)) = patchwork_opt {
                    if patchwork_chunk.len() > output_chunk_len {
                        let mut chunk = patchwork_chunk.finalize();
                        while chunk.len() > output_chunk_len {
                            output
                                .send(Samples {
                                    sample_rate: samples.sample_rate,
                                    chunk: chunk.separate_beginning(output_chunk_len),
                                })
                                .await;
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
                        output
                            .send(Samples {
                                sample_rate,
                                chunk: patchwork_chunk.finalize(),
                            })
                            .await;
                        samples_opt = None;
                        patchwork_opt = None;
                    } else if samples.chunk.len() >= missing {
                        patchwork_chunk.extend_from_slice(&samples.chunk[0..missing]);
                        output
                            .send(Samples {
                                sample_rate,
                                chunk: patchwork_chunk.finalize(),
                            })
                            .await;
                        samples.chunk.discard_beginning(missing);
                        samples_opt = Some(samples);
                        patchwork_opt = None;
                    } else {
                        unreachable!();
                    }
                } else {
                    while samples.chunk.len() > output_chunk_len {
                        output
                            .send(Samples {
                                sample_rate: samples.sample_rate,
                                chunk: samples.chunk.separate_beginning(output_chunk_len),
                            })
                            .await;
                    }
                    if samples.chunk.len() == output_chunk_len {
                        output.send(samples).await;
                        samples_opt = None;
                        patchwork_opt = None;
                    } else {
                        patchwork_opt = Some((samples.sample_rate, buf_pool.get()));
                        samples_opt = Some(samples);
                    }
                }
            }
        });
        Rechunker {
            receiver,
            sender,
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

/// Complex oscillator and mixer, which shifts all frequencies in an I/Q stream
pub struct FreqShifter<Flt> {
    receiver: Receiver<Samples<Complex<Flt>>>,
    sender: Sender<Samples<Complex<Flt>>>,
    precision: f64,
    shift: watch::Sender<f64>,
}

impl<Flt> Consumer<Samples<Complex<Flt>>> for FreqShifter<Flt>
where
    Flt: Clone,
{
    fn receiver(&self) -> &Receiver<Samples<Complex<Flt>>> {
        &self.receiver
    }
}

impl<Flt> Producer<Samples<Complex<Flt>>> for FreqShifter<Flt>
where
    Flt: Clone,
{
    fn connector(&self) -> SenderConnector<Samples<Complex<Flt>>> {
        self.sender.connector()
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
        let receiver = Receiver::<Samples<Complex<Flt>>>::new();
        let sender = Sender::<Samples<Complex<Flt>>>::new();
        let (shift_send, mut shift_recv) = watch::channel(shift);
        let mut input = receiver.stream();
        let output = sender.clone();
        spawn(async move {
            let mut phase_vec: Vec<Complex<Flt>> = Vec::new();
            let mut phase_idx: usize = 0;
            let mut prev_sample_rate: Option<f64> = None;
            let mut buf_pool = ChunkBufPool::<Complex<Flt>>::new();
            loop {
                match input.recv().await {
                    Ok(Samples {
                        sample_rate,
                        chunk: input_chunk,
                    }) => {
                        let recalculate: bool = match shift_recv.has_changed() {
                            Ok(false) => Some(sample_rate) != prev_sample_rate,
                            Ok(true) => true,
                            Err(_) => return,
                        };
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
                        output
                            .send(Samples {
                                sample_rate: sample_rate,
                                chunk: output_chunk.finalize(),
                            })
                            .await;
                    }
                    Err(err) => {
                        output.forward_error(err).await;
                        if err == RecvError::Closed {
                            return;
                        }
                    }
                }
            }
        });
        FreqShifter {
            receiver,
            sender,
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

/// Reduce sample rate
pub struct Downsampler<Flt> {
    receiver: Receiver<Samples<Complex<Flt>>>,
    sender: Sender<Samples<Complex<Flt>>>,
}

impl<Flt> Consumer<Samples<Complex<Flt>>> for Downsampler<Flt>
where
    Flt: Clone,
{
    fn receiver(&self) -> &Receiver<Samples<Complex<Flt>>> {
        &self.receiver
    }
}

impl<Flt> Producer<Samples<Complex<Flt>>> for Downsampler<Flt>
where
    Flt: Clone,
{
    fn connector(&self) -> SenderConnector<Samples<Complex<Flt>>> {
        self.sender.connector()
    }
}

impl<Flt> Downsampler<Flt>
where
    Flt: Float,
{
    /// Create new `Downsampler` block
    ///
    /// This call corresponds to [`Downsampler::with_quality`] with a `quality`
    /// value of `3.0`.
    ///
    /// Connected [`Producer`]s must emit [`Samples`] with a [`sample_rate`]
    /// equal to or higher than `output_rate`; otherwise a panic occurs.
    ///
    /// Aliasing is suppressed for frequencies lower than `bandwidth`.
    ///
    /// [`sample_rate`]: Samples::sample_rate
    pub fn new(output_chunk_len: usize, output_rate: f64, bandwidth: f64) -> Self {
        Self::with_quality(output_chunk_len, output_rate, bandwidth, 3.0)
    }
    /// Create new `Downsampler` block with `quality` setting
    ///
    /// Same as [`Downsampler::new`], but allows to specify a `quality`
    /// setting, which must be equal to or greater than `1.0`.
    pub fn with_quality(
        output_chunk_len: usize,
        output_rate: f64,
        bandwidth: f64,
        quality: f64,
    ) -> Self {
        assert!(output_rate >= 0.0, "output sample rate must be positive");
        assert!(bandwidth >= 0.0, "bandwidth must be positive");
        assert!(
            bandwidth < output_rate,
            "bandwidth must be smaller than output sample rate"
        );
        let receiver = Receiver::<Samples<Complex<Flt>>>::new();
        let sender = Sender::<Samples<Complex<Flt>>>::new();
        let mut input = receiver.stream();
        let output = sender.clone();
        let mut buf_pool = ChunkBufPool::<Complex<Flt>>::new();
        let mut output_chunk = buf_pool.get_with_capacity(output_chunk_len);
        spawn(async move {
            let margin = (output_rate - bandwidth) / 2.0;
            let mut prev_input_rate: Option<f64> = None;
            let mut ir: Vec<Flt> = Default::default();
            let mut ringbuf: Vec<Complex<Flt>> = Default::default();
            let mut ringbuf_pos: usize = Default::default();
            let mut pos: f64 = Default::default();
            loop {
                match input.recv().await {
                    Ok(Samples {
                        sample_rate: input_rate,
                        chunk: input_chunk,
                    }) => {
                        if Some(input_rate) != prev_input_rate {
                            prev_input_rate = Some(input_rate);
                            assert!(input_rate >= 0.0, "input sample rate must be positive");
                            assert!(
                                input_rate >= output_rate,
                                "input sample rate must be greater than or equal to output sample rate"
                            );
                            let ir_len: usize = (input_rate / margin * quality).ceil() as usize;
                            assert!(ir_len > 0);
                            let ir_len_flt = ir_len as f64;
                            let window =
                                kaiser_fn_with_null_at_bin(ir_len_flt * margin / input_rate);
                            let mut ir_buf: Vec<f64> = Vec::with_capacity(ir_len);
                            let mut energy = 0.0;
                            for i in 0..ir_len {
                                let x = (i as f64 + 0.5) - ir_len_flt / 2.0;
                                let y = sinc(x * output_rate / input_rate)
                                    * window(x * 2.0 / ir_len_flt);
                                ir_buf.push(y);
                                energy += y * y;
                            }
                            let scale = energy.sqrt().recip();
                            ir = ir_buf.into_iter().map(|y| flt!(y * scale)).collect();
                            ringbuf = vec![Complex::from(Flt::zero()); ir_len];
                            ringbuf_pos = 0;
                            pos = 0.0;
                        }
                        for &sample in input_chunk.iter() {
                            ringbuf[ringbuf_pos] = sample;
                            ringbuf_pos += 1;
                            if ringbuf_pos == ir.len() {
                                ringbuf_pos = 0;
                            }
                            pos += output_rate;
                            if pos >= input_rate {
                                pos -= input_rate;
                                let mut sum: Complex<Flt> = Complex::from(Flt::zero());
                                let mut ir_iter = ir.iter();
                                let mut next_ir = || *ir_iter.next().unwrap();
                                for i in ringbuf_pos..ir.len() {
                                    sum += ringbuf[i] * next_ir();
                                }
                                for i in 0..ringbuf_pos {
                                    sum += ringbuf[i] * next_ir();
                                }
                                output_chunk.push(sum);
                                if output_chunk.len() >= output_chunk_len {
                                    output
                                        .send(Samples {
                                            sample_rate: output_rate,
                                            chunk: output_chunk.finalize(),
                                        })
                                        .await;
                                    output_chunk = buf_pool.get_with_capacity(output_chunk_len);
                                }
                            }
                        }
                    }
                    Err(err) => {
                        output.forward_error(err).await;
                        if err == RecvError::Closed {
                            return;
                        }
                    }
                }
            }
        });
        Downsampler { receiver, sender }
    }
}

/// Increase sample rate
pub struct Upsampler<Flt> {
    receiver: Receiver<Samples<Complex<Flt>>>,
    sender: Sender<Samples<Complex<Flt>>>,
}

impl<Flt> Consumer<Samples<Complex<Flt>>> for Upsampler<Flt>
where
    Flt: Clone,
{
    fn receiver(&self) -> &Receiver<Samples<Complex<Flt>>> {
        &self.receiver
    }
}

impl<Flt> Producer<Samples<Complex<Flt>>> for Upsampler<Flt>
where
    Flt: Clone,
{
    fn connector(&self) -> SenderConnector<Samples<Complex<Flt>>> {
        self.sender.connector()
    }
}

impl<Flt> Upsampler<Flt>
where
    Flt: Float,
{
    /// Create new `Upsampler` block
    ///
    /// This call corresponds to [`Upsampler::with_quality`] with a `quality`
    /// value of `3.0`.
    ///
    /// Connected [`Producer`]s must emit [`Samples`] with a [`sample_rate`]
    /// equal to or smaller than `output_rate` but larger than `bandwidth`;
    /// otherwise a panic occurs.
    ///
    /// Aliasing is suppressed for frequencies lower than `bandwidth`.
    ///
    /// [`sample_rate`]: Samples::sample_rate
    pub fn new(output_chunk_len: usize, output_rate: f64, bandwidth: f64) -> Self {
        Self::with_quality(output_chunk_len, output_rate, bandwidth, 3.0)
    }
    /// Create new `Upsampler` block with `quality` setting
    ///
    /// Same as [`Upsampler::new`], but allows to specify a `quality` setting,
    /// which must be equal to or greater than `1.0`.
    pub fn with_quality(
        output_chunk_len: usize,
        output_rate: f64,
        bandwidth: f64,
        quality: f64,
    ) -> Self {
        assert!(output_rate >= 0.0, "output sample rate must be positive");
        assert!(bandwidth >= 0.0, "bandwidth must be positive");
        let receiver = Receiver::<Samples<Complex<Flt>>>::new();
        let sender = Sender::<Samples<Complex<Flt>>>::new();
        let mut input = receiver.stream();
        let output = sender.clone();
        let mut buf_pool = ChunkBufPool::<Complex<Flt>>::new();
        let mut output_chunk = buf_pool.get_with_capacity(output_chunk_len);
        spawn(async move {
            let mut prev_input_rate: Option<f64> = None;
            let mut ir: Vec<Flt> = Default::default();
            let mut ringbuf: Vec<Complex<Flt>> = Default::default();
            let mut ringbuf_pos: usize = Default::default();
            let mut pos: f64 = Default::default();
            loop {
                match input.recv().await {
                    Ok(Samples {
                        sample_rate: input_rate,
                        chunk: input_chunk,
                    }) => {
                        if Some(input_rate) != prev_input_rate {
                            prev_input_rate = Some(input_rate);
                            assert!(input_rate >= 0.0, "input sample rate must be positive");
                            assert!(
                                input_rate <= output_rate,
                                "input sample rate must be smaller than or equal to output sample rate"
                            );
                            assert!(
                                bandwidth < input_rate,
                                "bandwidth must be smaller than input sample rate"
                            );
                            let margin = (input_rate - bandwidth) / 2.0;
                            let ir_len: usize = (output_rate / margin * quality).ceil() as usize;
                            assert!(ir_len > 0);
                            let ir_len_flt = ir_len as f64;
                            let window =
                                kaiser_fn_with_null_at_bin(ir_len_flt * margin / output_rate);
                            let mut ir_buf: Vec<f64> = Vec::with_capacity(ir_len);
                            let mut energy = 0.0;
                            for i in 0..ir_len {
                                let x = (i as f64 + 0.5) - ir_len_flt / 2.0;
                                let y = sinc(x * input_rate / output_rate)
                                    * window(x * 2.0 / ir_len_flt);
                                ir_buf.push(y);
                                energy += y * y;
                            }
                            let scale = energy.sqrt().recip();
                            ir = ir_buf.into_iter().map(|y| flt!(y * scale)).collect();
                            ringbuf = vec![Complex::from(Flt::zero()); ir_len];
                            ringbuf_pos = 0;
                            pos = 0.0;
                        }
                        for &sample in input_chunk.iter() {
                            let mut ir_iter = ir.iter();
                            let mut next_ir = || *ir_iter.next().unwrap();
                            for i in ringbuf_pos..ir.len() {
                                ringbuf[i] += sample * next_ir();
                            }
                            for i in 0..ringbuf_pos {
                                ringbuf[i] += sample * next_ir();
                            }
                            while pos < output_rate {
                                output_chunk.push(ringbuf[ringbuf_pos]);
                                ringbuf[ringbuf_pos] = Complex::from(Flt::zero());
                                if output_chunk.len() >= output_chunk_len {
                                    output
                                        .send(Samples {
                                            sample_rate: output_rate,
                                            chunk: output_chunk.finalize(),
                                        })
                                        .await;
                                    output_chunk = buf_pool.get_with_capacity(output_chunk_len);
                                }
                                ringbuf_pos += 1;
                                if ringbuf_pos >= ir.len() {
                                    ringbuf_pos = 0;
                                }
                                pos += input_rate;
                            }
                            pos -= output_rate;
                        }
                    }
                    Err(err) => {
                        output.forward_error(err).await;
                        if err == RecvError::Closed {
                            return;
                        }
                    }
                }
            }
        });
        Upsampler { receiver, sender }
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
        let sender = Sender::new();
        let rechunk = Rechunker::<u8>::new(1024);
        let receiver = Receiver::new();
        sender.connect_to_consumer(&rechunk);
        rechunk.connect_to_consumer(&receiver);
        let mut receiver_stream = receiver.stream();
        assert!(tokio::select!(
            _ = async move {
                loop {
                    sender.send(Samples {
                        chunk: buffer.clone(),
                        sample_rate: 1.0,
                    }).await;
                }
            } => false,
            _ = async move {
                assert_eq!(receiver_stream.recv().await.unwrap().chunk.len(), 1024);
            } => true,
        ));
    }
}
