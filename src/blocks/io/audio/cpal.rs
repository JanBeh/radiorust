//! Interface to audio hardware through the [`cpal`] crate

use crate::bufferpool::*;
use crate::flow::*;
use crate::impl_block_trait;
use crate::numbers::*;
use crate::signal::*;

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};

use std::error::Error as StdError;
use std::fmt;

pub use cpal::Device;

#[derive(Debug)]
enum ErrorVariant {
    BuildStreamInvalidArgument(&'static str),
    BuildStreamDriverError(cpal::BuildStreamError),
    PlayStreamDriverError(cpal::PlayStreamError),
    PauseStreamDriverError(cpal::PauseStreamError),
}

/// Error related to audio I/O
#[derive(Debug)]
pub struct Error(ErrorVariant);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ErrorVariant::*;
        match &self.0 {
            BuildStreamInvalidArgument(s) => {
                write!(f, "invalid argument when opening audio device: {s}")
            }
            BuildStreamDriverError(_) => write!(f, "could not open audio device"),
            PlayStreamDriverError(_) => write!(f, "could start audio stream"),
            PauseStreamDriverError(_) => write!(f, "could pause audio stream"),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        use ErrorVariant::*;
        match &self.0 {
            BuildStreamInvalidArgument(_) => None,
            BuildStreamDriverError(inner) => Some(inner),
            PlayStreamDriverError(inner) => Some(inner),
            PauseStreamDriverError(inner) => Some(inner),
        }
    }
}

impl Error {
    fn invalid_argument(msg: &'static str) -> Self {
        Self(ErrorVariant::BuildStreamInvalidArgument(msg))
    }
}

impl From<cpal::BuildStreamError> for Error {
    fn from(inner: cpal::BuildStreamError) -> Self {
        Self(ErrorVariant::BuildStreamDriverError(inner))
    }
}

impl From<cpal::PlayStreamError> for Error {
    fn from(inner: cpal::PlayStreamError) -> Self {
        Self(ErrorVariant::PlayStreamDriverError(inner))
    }
}

impl From<cpal::PauseStreamError> for Error {
    fn from(inner: cpal::PauseStreamError) -> Self {
        Self(ErrorVariant::PauseStreamDriverError(inner))
    }
}

/// Retrieve default device for audio output
///
/// This function panics if there is no audio output device available.
pub fn default_output_device() -> cpal::Device {
    cpal::default_host()
        .default_output_device()
        .expect("no audio output device available")
}

/// Retrieve default device for audio input
///
/// This function panics if there is no audio input device available.
pub fn default_input_device() -> cpal::Device {
    cpal::default_host()
        .default_input_device()
        .expect("no audio input device available")
}

/// Audio player block acting as a [`Consumer`]
pub struct AudioPlayer {
    receiver_connector: ReceiverConnector<Signal<Complex<f32>>>,
    event_handlers: EventHandlers,
    stream: cpal::Stream,
}

impl_block_trait! { Consumer<Signal<Complex<f32>>> for AudioPlayer }
impl_block_trait! { EventHandling for AudioPlayer }

impl AudioPlayer {
    /// Create block for audio playback with given `sample_rate` and optionally
    /// requested `buffer_size`
    pub fn new(sample_rate: f64, buffer_size: Option<usize>) -> Result<Self, Error> {
        Self::with_device(&default_output_device(), sample_rate, buffer_size)
    }
    /// Create block for audio playback with given `sample_rate` and optionally
    /// requested `buffer_size` on given [`cpal::Device`]
    pub fn with_device(
        device: &cpal::Device,
        sample_rate: f64,
        buffer_size: Option<usize>,
    ) -> Result<Self, Error> {
        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(sample_rate.round() as u32),
            buffer_size: match buffer_size {
                None => cpal::BufferSize::Default,
                Some(value) => cpal::BufferSize::Fixed(
                    value
                        .try_into()
                        .or(Err(Error::invalid_argument("invalid buffer size")))?,
                ),
            },
        };
        let rt = tokio::runtime::Handle::current();
        let (mut receiver, receiver_connector) = new_receiver::<Signal<Complex<f32>>>();
        let event_handlers = EventHandlers::new();
        let evhdl_clone = event_handlers.clone();
        let err_fn = move |err| eprintln!("Audio output error: {err}");
        let mut current_chunk_and_pos: Option<(Chunk<Complex<f32>>, usize)> = None;
        let write_audio = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            for sample in data.iter_mut() {
                let (current_chunk, mut current_pos) = match current_chunk_and_pos.take() {
                    Some(x) => x,
                    None => loop {
                        let Ok(signal) = rt.block_on(receiver.recv()) else { return; };
                        match signal {
                            Signal::Samples {
                                sample_rate: rcvd_sample_rate,
                                chunk,
                            } => {
                                assert_eq!(
                                    rcvd_sample_rate, sample_rate,
                                    "audio block received samples with unexpected sample rate"
                                );
                                break (chunk, 0);
                            }
                            Signal::Event { payload, .. } => evhdl_clone.invoke(&payload),
                        }
                    },
                };
                *sample = current_chunk[current_pos].re;
                current_pos += 1;
                if current_pos < current_chunk.len() {
                    current_chunk_and_pos = Some((current_chunk, current_pos))
                }
            }
        };
        let stream = device.build_output_stream(&config, write_audio, err_fn)?;
        stream.play()?;
        Ok(Self {
            receiver_connector,
            event_handlers,
            stream,
        })
    }
    /// Resume playback
    pub fn resume(&self) -> Result<(), Error> {
        self.stream.play()?;
        Ok(())
    }
    /// Pause playback
    pub fn pause(&self) -> Result<(), Error> {
        self.stream.pause()?;
        Ok(())
    }
}

/// Audio recorder block acting as a [`Producer`]
pub struct AudioRecorder {
    sender_connector: SenderConnector<Signal<Complex<f32>>>,
    stream: cpal::Stream,
}

impl Producer<Signal<Complex<f32>>> for AudioRecorder {
    fn sender_connector(&self) -> &SenderConnector<Signal<Complex<f32>>> {
        &self.sender_connector
    }
}

impl AudioRecorder {
    /// Create block for audio recording with given `sample_rate` and
    /// optionally requested `buffer_size`
    pub fn new(sample_rate: f64, buffer_size: Option<usize>) -> Result<Self, Error> {
        Self::with_device(&default_input_device(), sample_rate, buffer_size)
    }
    /// Create block for audio recording with given `sample_rate` and
    /// optionally requested `buffer_size` on given [`cpal::Device`]
    pub fn with_device(
        device: &cpal::Device,
        sample_rate: f64,
        buffer_size: Option<usize>,
    ) -> Result<Self, Error> {
        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(sample_rate.round() as u32),
            buffer_size: match buffer_size {
                None => cpal::BufferSize::Default,
                Some(value) => cpal::BufferSize::Fixed(
                    value
                        .try_into()
                        .or(Err(Error::invalid_argument("invalid buffer size")))?,
                ),
            },
        };
        let rt = tokio::runtime::Handle::current();
        let (sender, sender_connector) = new_sender::<Signal<Complex<f32>>>();
        let err_fn = move |err| eprintln!("Audio input error: {err}");
        let mut buf_pool = ChunkBufPool::<Complex<f32>>::new();
        let read_audio = move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let mut output_chunk = buf_pool.get_with_capacity(data.len());
            for &sample in data.iter() {
                output_chunk.push(Complex::from(sample));
            }
            let Ok(()) = rt.block_on(sender.send(Signal::Samples {
                sample_rate,
                chunk: output_chunk.finalize(),
            }))
            else { return; };
        };
        let stream = device.build_input_stream(&config, read_audio, err_fn)?;
        stream.play()?;
        Ok(Self {
            sender_connector,
            stream,
        })
    }
    /// Resume recording
    pub fn resume(&self) -> Result<(), Error> {
        self.stream.play()?;
        Ok(())
    }
    /// Pause recording
    pub fn pause(&self) -> Result<(), Error> {
        self.stream.pause()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {}
