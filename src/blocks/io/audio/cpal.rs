//! Interface to audio hardware through the [`cpal`] crate

use crate::bufferpool::*;
use crate::flow::*;
use crate::numbers::*;
use crate::samples::*;

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};

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
        .default_output_device()
        .expect("no audio input device available")
}

/// Audio player block acting as a [`Consumer`]
pub struct AudioPlayer {
    receiver_connector: ReceiverConnector<Samples<Complex<f32>>>,
    stream: cpal::Stream,
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
                            Err(RecvError::Closed) => return,
                            _ => (),
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
}

#[cfg(test)]
mod tests {}
