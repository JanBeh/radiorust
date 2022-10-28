//! Interface to audio hardware through the [`cpal`] crate

use crate::bufferpool::*;
use crate::flow::*;
use crate::numbers::*;
use crate::samples::*;

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};
use tokio::sync::oneshot;

pub use cpal::{Device, Host};

use std::error::Error;

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
    receiver_connector: ReceiverConnector<Samples<Complex<f32>>>,
    stream: cpal::Stream,
    completion: oneshot::Receiver<RecvError>,
}

impl Consumer<Samples<Complex<f32>>> for AudioPlayer {
    fn receiver_connector(&self) -> &ReceiverConnector<Samples<Complex<f32>>> {
        &self.receiver_connector
    }
}

impl AudioPlayer {
    /// Create block for audio playback with given `sample_rate` and optionally
    /// requested `buffer_size`
    pub fn new(sample_rate: f64, buffer_size: Option<usize>) -> Result<Self, Box<dyn Error>> {
        Self::with_device(&default_output_device(), sample_rate, buffer_size)
    }
    /// Create block for audio playback with given `sample_rate` and optionally
    /// requested `buffer_size` on given [`cpal::Device`]
    pub fn with_device(
        device: &cpal::Device,
        sample_rate: f64,
        buffer_size: Option<usize>,
    ) -> Result<Self, Box<dyn Error>> {
        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(sample_rate.round() as u32),
            buffer_size: match buffer_size {
                None => cpal::BufferSize::Default,
                Some(value) => {
                    cpal::BufferSize::Fixed(value.try_into().or(Err("invalid buffer size"))?)
                }
            },
        };
        let rt = tokio::runtime::Handle::current();
        let (mut receiver, receiver_connector) = new_receiver::<Samples<Complex<f32>>>();
        let (completion_send, completion_recv) = oneshot::channel::<RecvError>();
        let mut completion_send = Some(completion_send);
        let err_fn = move |err| eprintln!("Audio output error: {err}");
        let mut current_chunk_and_pos: Option<(Chunk<Complex<f32>>, usize)> = None;
        let write_audio = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            for sample in data.iter_mut() {
                let (current_chunk, mut current_pos) = match current_chunk_and_pos.take() {
                    Some(x) => x,
                    None => loop {
                        match rt.block_on(receiver.recv()) {
                            Ok(Samples {
                                sample_rate: rcvd_sample_rate,
                                chunk,
                            }) => {
                                assert_eq!(
                                    rcvd_sample_rate, sample_rate,
                                    "audio block received samples with unexpected sample rate"
                                );
                                break (chunk, 0);
                            }
                            Err(err) => {
                                if let Some(completion_send) = completion_send.take() {
                                    completion_send.send(err).ok();
                                }
                                if err == RecvError::Closed {
                                    return;
                                }
                            }
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
            stream,
            completion: completion_recv,
        })
    }
    /// Resume playback
    pub fn resume(&self) -> Result<(), Box<dyn Error>> {
        self.stream.play()?;
        Ok(())
    }
    /// Pause playback
    pub fn pause(&self) -> Result<(), Box<dyn Error>> {
        self.stream.pause()?;
        Ok(())
    }
    /// Wait for played back stream to finish
    pub async fn wait(self) -> Result<(), Box<dyn Error>> {
        match self.completion.await? {
            RecvError::Finished => Ok(()),
            err => Err(err.into()),
        }
    }
}

/// Audio recorder block acting as a [`Producer`]
pub struct AudioRecorder {
    sender_connector: SenderConnector<Samples<Complex<f32>>>,
    stream: cpal::Stream,
}

impl Producer<Samples<Complex<f32>>> for AudioRecorder {
    fn sender_connector(&self) -> &SenderConnector<Samples<Complex<f32>>> {
        &self.sender_connector
    }
}

impl AudioRecorder {
    /// Create block for audio recording with given `sample_rate` and
    /// optionally requested `buffer_size`
    pub fn new(sample_rate: f64, buffer_size: Option<usize>) -> Result<Self, Box<dyn Error>> {
        Self::with_device(&default_input_device(), sample_rate, buffer_size)
    }
    /// Create block for audio recording with given `sample_rate` and
    /// optionally requested `buffer_size` on given [`cpal::Device`]
    pub fn with_device(
        device: &cpal::Device,
        sample_rate: f64,
        buffer_size: Option<usize>,
    ) -> Result<Self, Box<dyn Error>> {
        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(sample_rate.round() as u32),
            buffer_size: match buffer_size {
                None => cpal::BufferSize::Default,
                Some(value) => {
                    cpal::BufferSize::Fixed(value.try_into().or(Err("invalid buffer size"))?)
                }
            },
        };
        let rt = tokio::runtime::Handle::current();
        let (sender, sender_connector) = new_sender::<Samples<Complex<f32>>>();
        let err_fn = move |err| eprintln!("Audio input error: {err}");
        let mut buf_pool = ChunkBufPool::<Complex<f32>>::new();
        let read_audio = move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let mut output_chunk = buf_pool.get_with_capacity(data.len());
            for &sample in data.iter() {
                output_chunk.push(Complex::from(sample));
            }
            if let Err(_) = rt.block_on(sender.send(Samples {
                sample_rate,
                chunk: output_chunk.finalize(),
            })) {
                return;
            }
        };
        let stream = device.build_input_stream(&config, read_audio, err_fn)?;
        stream.play()?;
        Ok(Self {
            sender_connector,
            stream,
        })
    }
    /// Resume recording
    pub fn resume(&self) -> Result<(), Box<dyn Error>> {
        self.stream.play()?;
        Ok(())
    }
    /// Pause recording
    pub fn pause(&self) -> Result<(), Box<dyn Error>> {
        self.stream.pause()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {}
