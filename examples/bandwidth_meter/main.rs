use radiorust::blocks;
use radiorust::flow::*;
use radiorust::metering;
use radiorust::samples::*;
use radiorust::windowing;

use num::Complex;
use soapysdr::Direction::Rx;

use clap::Parser;

use std::collections::VecDeque;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Bandwidth measurement tool",
    long_about = None,
)]
struct Args {
    /// Center frequency in MHz
    #[arg(short = 'f', long)]
    frequency: f64,
    /// Hardware frequency offset in kHz
    #[arg(short = 'o', long, default_value = "200")]
    freq_offset: f64,
    /// Maximum bandwidth in kHz
    #[arg(short = 'm', long, default_value = "60")]
    max_bandwidth: f64,
    /// Quality
    #[arg(short = 'q', long, default_value = "4")]
    quality: u16,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    assert!(args.max_bandwidth > 0.0 && args.max_bandwidth <= 80.0);
    assert!(args.quality > 0);
    let frequency = args.frequency * 1e6;
    let freq_offset = args.freq_offset * 1e3;
    let max_bandwidth = args.max_bandwidth * 1e3;
    let quality: usize = args.quality.try_into().unwrap();
    let hw_frequency = frequency + freq_offset;
    let sample_rate = 1024000.0;
    let bandwidth = 1024000.0;
    let device = soapysdr::Device::new("").unwrap();
    println!("Hardware frequency: {hw_frequency}");
    device.set_frequency(Rx, 0, hw_frequency, "").unwrap();
    device.set_sample_rate(Rx, 0, sample_rate).unwrap();
    device.set_bandwidth(Rx, 0, bandwidth).unwrap();
    let rx_stream = device.rx_stream::<Complex<f32>>(&[0]).unwrap();
    let mut sdr_rx = blocks::io::rf::soapysdr::SoapySdrRx::new(rx_stream, sample_rate);
    sdr_rx.activate().unwrap();
    let freq_shifter = blocks::FreqShifter::<f32>::with_shift(freq_offset);
    println!("Frequency: {}", hw_frequency - freq_offset);
    freq_shifter.connect_to_producer(&sdr_rx);
    let downsampler = blocks::Downsampler::<f32>::new(1024, 102400.0, max_bandwidth);
    downsampler.connect_to_producer(&freq_shifter);
    let filter = blocks::filters::Filter::new(move |_, freq| {
        if freq.abs() <= max_bandwidth / 2.0 {
            Complex::from(1.0)
        } else {
            Complex::from(0.0)
        }
    });
    filter.connect_to_producer(&downsampler);
    let overlapper = blocks::Overlapper::new(quality);
    overlapper.connect_to_producer(&filter);
    let fourier = blocks::Fourier::with_window(windowing::Kaiser::with_null_at_bin(quality as f64));
    fourier.connect_to_producer(&overlapper);
    let receiver = Receiver::<Samples<Complex<f32>>>::new();
    receiver.connect_to_producer(&fourier);
    let mut output = receiver.stream();
    let mut values: VecDeque<f64> = VecDeque::new();
    let mut i = 0;
    loop {
        match output.recv().await {
            Ok(samples) => {
                let bw = metering::bandwidth(0.01, &samples);
                if values.len() >= 100 {
                    values.pop_front();
                }
                values.push_back(bw);
                let mut max_bw = 0.0;
                for &value in values.iter() {
                    if value > max_bw {
                        max_bw = value;
                    }
                }
                i += 1;
                if i >= 50 {
                    println!("Max bandwidth within 1 second {max_bw}");
                    i = 0;
                }
            }
            Err(err) => println!("Error: {err:?}"),
        }
    }
}
