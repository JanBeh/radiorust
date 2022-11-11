use radiorust::prelude::*;

#[tokio::main]
async fn main() {
    let recorder = blocks::io::audio::cpal::AudioRecorder::new(48000.0, None).unwrap();
    let player = blocks::io::audio::cpal::AudioPlayer::new(48000.0, None).unwrap();
    player.feed_from(&recorder);
    std::future::pending::<()>().await; // TODO: handle panics in threads
}
