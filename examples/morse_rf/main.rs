//! A2A morse transmitter

use clap::Parser;
use radiorust::prelude::*;
use tokio::sync::watch;
use tokio::task::spawn;

use std::sync::Arc;

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
    /// Gain in dB
    #[arg(short = 'g', long)]
    gain: f64,
}

#[tokio::main]
async fn main() {
    // Parse command line args:
    let args = Args::parse();

    // Prepare `::soapysdr::TxStream`:
    let device = soapysdr::Device::new(args.device_options.as_str()).unwrap();
    device
        .set_gain(soapysdr::Direction::Tx, 0, args.gain)
        .unwrap();
    device
        .set_frequency(soapysdr::Direction::Tx, 0, args.frequency * 1e6, "")
        .unwrap();
    device
        .set_sample_rate(soapysdr::Direction::Tx, 0, 128000.0)
        .unwrap();
    let tx_stream = device.tx_stream::<Complex<f32>>(&[0]).unwrap();

    // Determine optimal chunk size:
    let mtu: usize = tx_stream.mtu().unwrap();

    // Create and connect signal processing blocks:
    let keyer =
        blocks::morse::Keyer::<f32>::new(mtu, 128000.0, blocks::morse::Speed::from_paris_wpm(16.0));
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
    let sdr_tx = Arc::new(blocks::io::rf::soapysdr::SoapySdrTx::new(tx_stream));
    sdr_tx.feed_from(&rf_mod);

    // Track whether a morse transmission is in progress:
    let (busy_send, mut busy_recv1) = watch::channel(false);
    let mut busy_recv2 = busy_recv1.clone();
    sdr_tx
        .on_event(move |event| {
            // NOTE: Event handler is not async and must return quickly
            if event
                .as_any()
                .is::<blocks::morse::events::StartOfMessages>()
            {
                busy_send.send_replace(true);
            }
            if event.as_any().is::<blocks::morse::events::EndOfMessages>() {
                busy_send.send_replace(false);
            }
        })
        .forget();

    // Spawn background task which deactivates transmitter when morse
    // transmission has ended:
    let sdr_tx_clone = sdr_tx.clone();
    spawn(async move {
        loop {
            let Ok(()) = busy_recv1.changed().await else { return; };
            let busy = busy_recv1.borrow_and_update().clone();
            if !busy {
                sdr_tx_clone.deactivate().await.unwrap();
            }
        }
    });

    // Interactively ask for messages to transmit:
    let mut rl = rustyline::Editor::<()>::new().unwrap();
    loop {
        match rl.readline("Enter morse message> ") {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    println!("Empty message, exiting.");
                    break;
                }
                match keyer.send(&line) {
                    Ok(()) => sdr_tx.activate().await.unwrap(),
                    Err(_) => println!("Invalid character!"),
                }
            }
            Err(err) => {
                println!("Error: {err}");
                break;
            }
        }
    }

    // Wait if there is a transmission in progress:
    loop {
        let busy = busy_recv2.borrow_and_update().clone();
        if !busy {
            break;
        }
        busy_recv2.changed().await.unwrap();
    }

    // Deactivate transmitter:
    sdr_tx.deactivate().await.unwrap();
}
