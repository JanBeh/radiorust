use clap::Parser;
use radiorust::prelude::*;

use std::sync::{Arc, Condvar, Mutex};

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
    let args = Args::parse();
    let busy_send = Arc::new((Mutex::new(false), Condvar::new()));
    let busy_recv1 = busy_send.clone();
    let busy_recv2 = busy_send.clone();
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
    let mtu: usize = tx_stream.mtu().unwrap();
    let keyer = blocks::morse::Keyer::<f32>::with_message(
        mtu,
        128000.0,
        blocks::morse::Speed::from_paris_wpm(16.0),
        "VVV",
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
    let sdr_tx = Arc::new(blocks::io::rf::soapysdr::SoapySdrTx::new(tx_stream));
    // Track whether a transmission is in progress:
    sdr_tx
        .on_event(move |payload| {
            let (busy_lock, busy_cvar) = &*busy_send;
            if payload.is::<blocks::morse::events::StartOfMessages>() {
                let mut busy = busy_lock.lock().unwrap();
                *busy = true;
                busy_cvar.notify_all();
            }
            if payload.is::<blocks::morse::events::EndOfMessages>() {
                let mut busy = busy_lock.lock().unwrap();
                *busy = false;
                busy_cvar.notify_all();
            }
        })
        .forget();
    sdr_tx.feed_from(&rf_mod);
    tokio::time::sleep(std::time::Duration::from_millis(2000)).await;
    sdr_tx.activate().await.unwrap();
    let sdr_tx_clone = sdr_tx.clone();
    let rt = tokio::runtime::Handle::current();
    std::thread::spawn(move || {
        let (busy_lock, busy_cvar) = &*busy_recv1;
        let mut busy = busy_lock.lock().unwrap();
        loop {
            if !*busy {
                rt.block_on(sdr_tx_clone.deactivate()).unwrap();
            }
            busy = busy_cvar.wait(busy).unwrap();
        }
    });
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
    {
        let (busy_lock, busy_cvar) = &*busy_recv2;
        let mut busy = busy_lock.lock().unwrap();
        while *busy {
            busy = busy_cvar.wait(busy).unwrap();
        }
    }
    sdr_tx.deactivate().await.unwrap();
}
