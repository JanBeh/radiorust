//! Software defined radio
//!
//! # Getting started
//!
//! For getting started, have a look at the [`blocks`] module for a selection
//! of ready-to-use signal processing blocks and see the "Hello World" example
//! below.
//!
//! # Hello World example
//!
//! The following example requires the `cpal` feature to be enabled.
//!
//! ```
//! use radiorust::prelude::*;
//!
//! #[tokio::main]
//! async fn main() {
//! # #[cfg(any())]
//! # #[cfg(feature = "cpal")]
//! # {
//!     let morse_keyer = blocks::morse::Keyer::with_message(
//!         4096,
//!         48000.0,
//!         blocks::morse::Speed::from_paris_wpm(16.0),
//!         "<CT> Hello World <AR>",
//!     ).unwrap();
//!     let audio_modulator = blocks::FreqShifter::with_shift(700.0);
//!     audio_modulator.feed_from(&morse_keyer);
//!     let playback = blocks::io::audio::cpal::AudioPlayer::new(48000.0, None).unwrap();
//!     playback.feed_from(&audio_modulator);
//!     playback.wait_for_event(|event| {
//!         event.as_any().is::<blocks::morse::events::EndOfMessages>()
//!     }).await;
//! # }
//! }
//! ```

#![warn(missing_docs)]

pub mod blocks;
pub mod bufferpool;
pub mod flow;
pub mod math;
pub mod metering;
pub mod numbers;
pub mod prelude;
pub mod signal;
pub mod sync;
pub mod windowing;

#[cfg(test)]
mod tests {
    const PRECISION: f64 = 1e-10;
    pub(crate) fn assert_approx(a: f64, b: f64) {
        if !((a - b).abs() <= PRECISION || (a / b).ln().abs() <= PRECISION) {
            panic!("{a} and {b} are not approximately equal");
        }
    }
}
