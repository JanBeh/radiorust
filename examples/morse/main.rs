use radiorust::prelude::*;

#[tokio::main]
async fn main() {
    let keyer = blocks::morse::Keyer::with_message(
        4096,
        48000.0,
        blocks::morse::Speed::from_paris_wpm(16.0),
        "VVV",
    )
    .unwrap();
    let limiter = blocks::filters::SlewRateLimiter::new(100.0);
    limiter.connect_to_producer(&keyer);
    let filter = blocks::Filter::new(|_, freq| {
        if freq.abs() <= 100.0 {
            Complex::from(1.0)
        } else {
            Complex::from(0.0)
        }
    });
    filter.connect_to_producer(&limiter);
    let volume = blocks::GainControl::new(0.5);
    volume.connect_to_producer(&filter);
    let audio_mod = blocks::FreqShifter::with_shift(700.0);
    audio_mod.connect_to_producer(&volume);
    let playback = blocks::io::audio::cpal::AudioPlayer::new(48000.0, None).unwrap();
    playback.connect_to_producer(&audio_mod);
    /*
    let writer = blocks::io::raw::ContinuousF32BeWriter::new(std::io::BufWriter::new(
        std::fs::File::create("output.raw").unwrap(),
    ));
    writer.connect_to_producer(&audio_mod);
    */
    let mut rl = rustyline::Editor::<()>::new().unwrap();
    loop {
        match rl.readline("Enter morse message> ") {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    println!("Empty message, exiting.");
                    break;
                }
                if keyer.send(&line).is_err() {
                    println!("Invalid character!");
                }
            }
            Err(err) => {
                println!("Error: {err}");
                break;
            }
        }
    }
}
