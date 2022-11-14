use radiorust::prelude::*;

use soapysdr::Direction::Rx;

pub struct SimpleSdr {
    pub device: soapysdr::Device,
    pub sdr_rx: blocks::io::rf::soapysdr::SoapySdrRx,
    pub freq_shifter: blocks::FreqShifter<f32>,
    pub volume: blocks::GainControl<f32>,
    pub playback: blocks::io::audio::cpal::AudioPlayer,
}

impl SimpleSdr {
    pub async fn new(device_options: &str, frequency: f64) -> Self {
        let sample_rate = 1024000.0;
        let bandwidth = 1024000.0;
        let device = soapysdr::Device::new(device_options).unwrap();
        device.set_frequency(Rx, 0, frequency, "").unwrap();
        device.set_sample_rate(Rx, 0, sample_rate).unwrap();
        device.set_bandwidth(Rx, 0, bandwidth).unwrap();
        let rx_stream = device.rx_stream::<Complex<f32>>(&[0]).unwrap();
        let mut sdr_rx = blocks::io::rf::soapysdr::SoapySdrRx::new(rx_stream, sample_rate);
        sdr_rx.activate().await.unwrap();

        let freq_shifter = blocks::FreqShifter::<f32>::with_shift(0.0e6);
        freq_shifter.feed_from(&sdr_rx);

        let downsample1 = blocks::Downsampler::<f32>::new(16384, 384000.0, 200000.0);
        downsample1.feed_from(&freq_shifter);

        let filter1 = blocks::Filter::new(|_, freq| {
            if freq.abs() <= 100000.0 {
                Complex::from(1.0)
            } else {
                Complex::from(0.0)
            }
        });
        filter1.feed_from(&downsample1);

        let demodulator = blocks::modulation::FmDemod::<f32>::new(150000.0);
        demodulator.feed_from(&filter1);

        let filter2 = blocks::filters::Filter::new_rectangular(|bin, freq| {
            if bin.abs() >= 1 && freq.abs() >= 20.0 && freq.abs() <= 16000.0 {
                blocks::filters::deemphasis_factor(50e-6, freq)
            } else {
                Complex::from(0.0)
            }
        });
        filter2.feed_from(&demodulator);

        let downsample2 = blocks::Downsampler::<f32>::new(4096, 48000.0, 2.0 * 20000.0);
        downsample2.feed_from(&filter2);

        let volume = blocks::GainControl::<f32>::new(1.0);
        volume.feed_from(&downsample2);

        let buffer = blocks::Buffer::new(0.0, 0.0, 0.0, 0.01);
        buffer.feed_from(&volume);

        let playback = blocks::io::audio::cpal::AudioPlayer::new(48000.0, None).unwrap();
        playback.feed_from(&buffer);

        SimpleSdr {
            device,
            sdr_rx,
            freq_shifter,
            volume,
            playback,
        }
    }
}
