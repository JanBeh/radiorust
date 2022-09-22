use crate::blocks;
use crate::flow::*;

use num::Complex;
use soapysdr::Direction::Rx;

pub struct SimpleSdr {
    pub device: soapysdr::Device,
    pub sdr_rx: blocks::io::rf::soapysdr::SoapySdrRx,
    pub freq_shifter: blocks::FreqShifter<f32>,
    pub downsample1: blocks::Downsampler<f32>,
    pub filter1: blocks::filters::Filter<f32>,
    pub demodulator: blocks::modulation::FmDemod<f32>,
    pub filter2: blocks::filters::Filter<f32>,
    pub downsample2: blocks::Downsampler<f32>,
    pub volume: blocks::Function<Complex<f32>>,
    pub playback: blocks::io::audio::cpal::Audio,
}

impl SimpleSdr {
    pub fn new() -> Self {
        let frequency = 100e6;
        let sample_rate = 1024000.0;
        let bandwidth = 1024000.0;
        let device = soapysdr::Device::new("").unwrap();
        device.set_frequency(Rx, 0, frequency, "").unwrap();
        device.set_sample_rate(Rx, 0, sample_rate).unwrap();
        device.set_bandwidth(Rx, 0, bandwidth).unwrap();
        let rx_stream = device.rx_stream::<Complex<f32>>(&[0]).unwrap();
        let mut sdr_rx = blocks::io::rf::soapysdr::SoapySdrRx::new(rx_stream, sample_rate);
        sdr_rx.activate().unwrap();

        let freq_shifter = blocks::FreqShifter::<f32>::with_shift(0.3e6);
        freq_shifter.connect_to_producer(&sdr_rx);

        let downsample1 = blocks::Downsampler::<f32>::new(16384, 384000.0, 200000.0);
        downsample1.connect_to_producer(&freq_shifter);

        let filter1 = blocks::filters::Filter::new(|_, freq| {
            if freq.abs() <= 100000.0 {
                Complex::from(1.0)
            } else {
                Complex::from(0.0)
            }
        });
        filter1.connect_to_producer(&downsample1);

        let demodulator = blocks::modulation::FmDemod::<f32>::new(150000.0);
        demodulator.connect_to_producer(&filter1);

        let filter2 = blocks::filters::Filter::new_rectangular(|bin, freq| {
            if bin.abs() >= 1 && freq.abs() >= 20.0 && freq.abs() <= 16000.0 {
                blocks::filters::deemphasis_factor(50e-6, freq)
            } else {
                Complex::from(0.0)
            }
        });
        filter2.connect_to_producer(&demodulator);

        let downsample2 = blocks::Downsampler::<f32>::new(4096, 48000.0, 2.0 * 20000.0);
        downsample2.connect_to_producer(&filter2);

        /*
        let writer = blocks::io::raw::ContinuousF32BeWriter::with_path("output.raw");
        writer.connect_to_producer(&downsample2);
        tokio::spawn(async move {
            writer.wait().await.ok();
        });
        */

        let volume = blocks::Function::<Complex<f32>>::new();
        volume.connect_to_producer(&downsample2);

        let playback = blocks::io::audio::cpal::Audio::for_playback(48000.0, 2 * 4096);
        playback.connect_to_producer(&volume);

        SimpleSdr {
            device,
            sdr_rx,
            freq_shifter,
            downsample1,
            filter1,
            demodulator,
            filter2,
            downsample2,
            volume,
            playback,
        }
    }
}
