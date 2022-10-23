//! Interface to audio hardware through the [`cpal`] crate
//!
//! **Note: The API of this module is highly unstable yet and subject to change.**

use crate::bufferpool::*;
use crate::flow::*;
use crate::numbers::*;
use crate::samples::*;

use cpal::traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _};

/// Audio player
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
    /// Create `AudioPlayer` block for playback only with given `sample_rate`
    /// and desired `buffer_size`
    pub fn new(sample_rate: f64, buffer_size: usize) -> Self {
        let rt = tokio::runtime::Handle::current();
        let sample_rate_int = sample_rate.round() as u32;
        let mut buffer_size: u32 = buffer_size.try_into().unwrap();
        let (mut receiver, receiver_connector) = new_receiver::<Samples<Complex<f32>>>();
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .expect("no output device available");
        let supported_ranges = device
            .supported_output_configs()
            .expect("no supported audio config");
        let range = supported_ranges
            .filter(|range| {
                range.channels() == 1
                    && range.min_sample_rate().0 <= sample_rate_int
                    && range.max_sample_rate().0 >= sample_rate_int
                    && range.sample_format() == cpal::SampleFormat::F32
            })
            .next()
            .expect("no suitable audio config found");
        let supported_config = range.with_sample_rate(cpal::SampleRate(sample_rate_int));
        match supported_config.buffer_size() {
            &cpal::SupportedBufferSize::Range { min, max } => {
                buffer_size = buffer_size.min(max).max(min)
            }
            &cpal::SupportedBufferSize::Unknown => (),
        }
        let config = cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(sample_rate_int),
            buffer_size: cpal::BufferSize::Fixed(buffer_size),
        };
        let err_fn = |err| eprintln!("an error occurred on the output audio stream: {}", err);
        let mut current_chunk_opt: Option<Chunk<Complex<f32>>> = None;
        let mut current_pos: usize = 0;
        let write_audio = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            for sample in data.iter_mut() {
                let current_chunk = if current_chunk_opt.is_some() {
                    current_chunk_opt.take().unwrap()
                } else {
                    loop {
                        match rt.block_on(receiver.recv()) {
                            Ok(Samples {
                                sample_rate: rcvd_sample_rate,
                                chunk,
                            }) => {
                                assert_eq!(
                                    rcvd_sample_rate, sample_rate,
                                    "audio block received samples with unexpected sample rate"
                                );
                                break chunk;
                            }
                            Err(_) => (),
                        }
                    }
                };
                *sample = current_chunk[current_pos].re;
                current_pos += 1;
                if current_pos < current_chunk.len() {
                    current_chunk_opt = Some(current_chunk)
                } else {
                    current_pos = 0;
                    current_chunk_opt = None;
                }
            }
        };
        let stream = device
            .build_output_stream(&config, write_audio, err_fn)
            .unwrap();
        stream.play().unwrap();
        Self {
            receiver_connector,
            stream,
        }
    }
    /// Resume playback
    pub fn resume(&self) {
        self.stream.play().unwrap();
    }
    /// Pause playback
    pub fn pause(&self) {
        self.stream.pause().unwrap();
    }
}

#[cfg(test)]
mod tests {}
