use crate::blocks;
use crate::flow::*;
use num::Complex;

pub struct SimpleSdr {
    pub sdr: blocks::io::rf::Sdr,
    pub freq_shifter: blocks::FreqShifter<f32>,
    pub downsample1: blocks::Downsampler<f32>,
    pub filter1: blocks::filters::Filter<f32>,
    pub demodulator: blocks::modulation::FmDemod<f32>,
    pub filter2: blocks::filters::Filter<f32>,
    pub downsample2: blocks::Downsampler<f32>,
    pub playback: blocks::io::audio::Audio,
}

impl SimpleSdr {
    pub fn new() -> Self {
        let sdr = blocks::io::rf::Sdr::for_receiving(1024000.0, 100e6, 1024000.0);

        let freq_shifter = blocks::FreqShifter::<f32>::with_shift(0.3e6);
        freq_shifter.connect_to_producer(&sdr);

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
        let writer = blocks::io::raw_out::ContinuousF32BeWriter::with_path("output.raw");
        writer.connect_to_producer(&downsample2);
        std::mem::forget(writer); // quick and dirty
        */

        let playback = blocks::io::audio::Audio::for_playback(48000.0, 2 * 4096);
        playback.connect_to_producer(&downsample2);

        SimpleSdr {
            sdr,
            freq_shifter,
            downsample1,
            filter1,
            demodulator,
            filter2,
            downsample2,
            playback,
        }
    }
}
