use clap::Parser;
use radiorust::prelude::*;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "Wide FM radio playback",
    long_about = None,
)]
struct Args {
    /// Device options (e.g. "driver=rtlsdr")
    #[arg(short = 'd', long, default_value = "")]
    device_options: String,
    /// Frequency in MHz
    #[arg(short = 'f', long)]
    frequency: f64,
    /// Callsign
    #[arg(short = 'c', long, default_value = "")]
    callsign: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let keyer = blocks::morse::Keyer::<f32>::with_message(
        65536,
        128000.0,
        blocks::morse::Speed::from_paris_wpm(16.0),
        &format!("VVV <CT> DE {} <AR>", args.callsign),
    )
    .unwrap();
    let limiter = blocks::filters::SlewRateLimiter::new(100.0);
    limiter.feed_from(&keyer);
    let filter = blocks::Filter::new(|_, freq| {
        if freq.abs() <= 100.0 {
            Complex::from(1.0)
        } else {
            Complex::from(0.0)
        }
    });
    filter.feed_from(&limiter);
    let audio_mod = blocks::FreqShifter::with_shift(700.0);
    audio_mod.feed_from(&filter);
    let rf_mod = blocks::modulation::FmMod::new(2500.0);
    rf_mod.feed_from(&audio_mod);
    let device = soapysdr::Device::new(args.device_options.as_str()).unwrap();
    device.set_gain(soapysdr::Direction::Tx, 0, 64.0).unwrap();
    device
        .set_frequency(soapysdr::Direction::Tx, 0, args.frequency * 1e6, "")
        .unwrap();
    device
        .set_sample_rate(soapysdr::Direction::Tx, 0, 128000.0)
        .unwrap();
    let tx_stream = device.tx_stream::<Complex<f32>>(&[0]).unwrap();
    let mut sdr_tx = blocks::io::rf::soapysdr::SoapySdrTx::new(tx_stream);
    sdr_tx.feed_from(&rf_mod);
    tokio::time::sleep(std::time::Duration::from_millis(5000)).await;
    sdr_tx.activate().await.unwrap();
    sdr_tx
        .wait_for_event(|payload| payload.is::<blocks::morse::events::EndOfMessages>())
        .await;
    sdr_tx.deactivate().await.unwrap();
}
