use radiorust::prelude::*;

#[tokio::main]
async fn main() {
    let keyer =
        blocks::morse::Keyer::new(4096, 48000.0, blocks::morse::Speed::from_paris_wpm(16.0));
    let volume = blocks::Function::with_closure(|x| 0.5 * x);
    volume.connect_to_producer(&keyer);
    let audio_mod = blocks::FreqShifter::with_shift(700.0);
    audio_mod.connect_to_producer(&volume);
    let filter = blocks::Filter::new(|_, freq| {
        if freq >= 300.0 && freq <= 2700.0 {
            Complex::from(1.0)
        } else {
            Complex::from(0.0)
        }
    });
    filter.connect_to_producer(&audio_mod);
    let playback = blocks::io::audio::cpal::AudioPlayer::new(48000.0, 2 * 4096);
    playback.connect_to_producer(&filter);
    let mut rl = rustyline::Editor::<()>::new().unwrap();
    loop {
        let line = rl.readline("Enter morse message> ").unwrap();
        if keyer.send(&line).is_err() {
            println!("Invalid character!");
        }
    }
}
