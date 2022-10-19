use radiorust::blocks;
use radiorust::flow::*;
use radiorust::metering;
use radiorust::samples::*;

use num::Complex;
use soapysdr::Direction::Rx;

use clap::Parser;

use std::collections::VecDeque;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long)]
    frequency: f64,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let frequency = args.frequency * 1e6;
    let freq_offset = 200e3;
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
    let downsampler = blocks::Downsampler::<f32>::new(2048, 102400.0, 60e3);
    downsampler.connect_to_producer(&freq_shifter);
    let filter = blocks::filters::Filter::new(|_, freq| {
        if freq.abs() <= 30e3 {
            Complex::from(1.0)
        } else {
            Complex::from(0.0)
        }
    });
    filter.connect_to_producer(&downsampler);
    let fourier = blocks::Fourier::new();
    fourier.connect_to_producer(&filter);
    let receiver = Receiver::<Samples<Complex<f32>>>::new();
    receiver.connect_to_producer(&fourier);
    let mut output = receiver.stream();
    let mut values: VecDeque<f64> = VecDeque::new();
    let mut i = 0;
    loop {
        match output.recv().await {
            Ok(samples) => {
                let bw = metering::bandwidth(0.01, &samples);
                if values.len() >= 50 {
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
                if i >= 25 {
                    println!("Max bandwidth within 1 second {max_bw}");
                    i = 0;
                }
            }
            Err(err) => println!("Error: {err:?}"),
        }
    }
}
