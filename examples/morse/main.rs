//! Note: This example is very unclean yet and dependent on timing.

use radiorust::flow::*;
use radiorust::numbers::Complex;
use radiorust::*;

use soapysdr::Direction::Tx;

#[tokio::main]
async fn main() {
    let frequency = 433.475e6;
    let sample_rate = 128000.0;
    let audio_mod = blocks::FreqShifter::with_shift(700.0);
    let fm_mod = blocks::modulation::FmMod::<f32>::new(2500.0);
    fm_mod.connect_to_producer(&audio_mod);
    let upsampler = blocks::Upsampler::<f32>::new(32768, sample_rate, 16e3);
    upsampler.connect_to_producer(&fm_mod);
    let device = soapysdr::Device::new("").unwrap();
    device.set_frequency(Tx, 0, frequency, "").unwrap();
    device.set_sample_rate(Tx, 0, sample_rate).unwrap();
    let tx_stream = device.tx_stream::<Complex<f32>>(&[0]).unwrap();
    let sdr_tx = blocks::io::rf::soapysdr::SoapySdrTx::new(tx_stream, sample_rate);
    sdr_tx.connect_to_producer(&upsampler);
    sdr_tx.activate().unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(5000)).await;
    let keyer = blocks::morse::Keyer::<f32>::new(
        4096,
        48000.0,
        blocks::morse::Speed::from_paris_wpm(16.0),
        "HELLO", // put your call-sign here, for example
    )
    .unwrap();
    keyer.connect_to_consumer(&audio_mod);
    tokio::time::sleep(std::time::Duration::from_millis(10000)).await;
    sdr_tx.deactivate().await.unwrap();
}
