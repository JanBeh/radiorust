//! Morse code
//!
//! **Note: The API of this module is highly unstable yet and subject to change.**

use crate::bufferpool::*;
use crate::flow::*;
use crate::numbers::*;
use crate::samples::*;

use tokio::task::spawn;
use tokio::time::{interval, MissedTickBehavior};

use std::iter;
use std::time::Duration;

/// Morse speed
#[derive(Clone, Copy, PartialEq, PartialOrd, Debug)]
pub struct Speed {
    dits_per_minute: f64,
}

impl Speed {
    /// Convert characters per minute (using word "PARIS") into `Speed`
    pub fn from_paris_cpm(cpm: f64) -> Self {
        Speed {
            dits_per_minute: 10.0 * cpm,
        }
    }
    /// Convert characters per minute (using word "CODEX") into `Speed`
    pub fn from_codex_cpm(codex_cpm: f64) -> Self {
        Speed {
            dits_per_minute: 12.0 * codex_cpm,
        }
    }
    /// Convert words per minute (using word "PARIS") into `Speed`
    pub fn from_paris_wpm(wpm: f64) -> Self {
        Self::from_paris_cpm(5.0 * wpm)
    }
    /// Convert words per minute (using word "CODEX") into `Speed`
    pub fn from_codex_wpm(codex_wpm: f64) -> Self {
        Self::from_codex_cpm(5.0 * codex_wpm)
    }
    /// Convert dits per minute into `Speed`
    pub fn from_dits_per_minute(dits_per_minute: f64) -> Self {
        Speed { dits_per_minute }
    }
    /// Characters per minute (using word "PARIS")
    pub fn paris_cpm(self) -> f64 {
        self.dits_per_minute / 10.0
    }
    /// Characters per minute (using word "CODEX")
    pub fn codex_cpm(self) -> f64 {
        self.dits_per_minute / 12.0
    }
    /// Words per minute (using word "PARIS")
    pub fn paris_wpm(self) -> f64 {
        self.paris_cpm() / 5.0
    }
    /// Words per minute (using word "CODEX")
    pub fn codex_wpm(self) -> f64 {
        self.codex_cpm() / 5.0
    }
    /// Words per minute (using word "CODEX")
    pub fn dits_per_minute(self) -> f64 {
        self.dits_per_minute
    }
    /// Duration of a single dit in seconds
    ///
    /// Note that this value may be inexact for integer speeds due to floating
    /// point errors.
    pub fn seconds_per_dit(self) -> f64 {
        60.0 / self.dits_per_minute
    }
    /// Sample count for a single dit
    pub fn samples_per_dit(self, sample_rate: f64) -> f64 {
        60.0 * sample_rate / self.dits_per_minute
    }
}

/// Units of which morse code is composed
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Unit {
    /// Dit
    Dit,
    /// Dah (usually three times as long as a dit)
    Dah,
    /// Space between dits and dahs (usually as long as a dit)
    Space,
    /// Space between characters (usually as long as three dits or a dah)
    CharSpace,
    /// Space between words (usually as long as seven dits)
    WordSpace,
}

use Unit::*;

impl Unit {
    /// `true` if carrier must be turned on, otherwise `false`
    pub fn on(self) -> bool {
        match self {
            Dit => true,
            Dah => true,
            Space => false,
            CharSpace => false,
            WordSpace => false,
        }
    }
    /// Relative duration of given `Unit`, where a [`Dit`] is equivalent to
    /// `1.0`
    pub fn relative_duration(self) -> f64 {
        match self {
            Dit => 1.0,
            Dah => 3.0,
            Space => 1.0,
            CharSpace => 3.0,
            WordSpace => 7.0,
        }
    }
    /// Length of `Unit` in samples for given [`Speed`]
    pub fn samples(self, sample_rate: f64, speed: Speed) -> f64 {
        speed.samples_per_dit(sample_rate) * self.relative_duration()
    }
}

/// Encodes a text as sequence of [`Unit`]s
///
/// Returns `None` on invalid or unexpected input.
pub fn encode(text: &str) -> Option<Vec<Unit>> {
    use Space as Sp;
    let mut output: Vec<Unit> = Vec::new();
    let mut prosign = false;
    let mut previous_char: bool = false;
    for c in text.chars().map(|c| c.to_uppercase()).flatten() {
        match c {
            '<' => {
                if prosign {
                    return None;
                }
                if previous_char {
                    previous_char = false;
                    output.push(CharSpace)
                }
                prosign = true;
            }
            '>' => {
                if !prosign || !previous_char {
                    return None;
                }
                prosign = false;
            }
            ' ' => {
                if prosign {
                    return None;
                }
                previous_char = false;
                output.push(WordSpace);
            }
            _ => {
                if previous_char {
                    output.push(if prosign { Space } else { CharSpace });
                }
                previous_char = true;
                output.extend_from_slice(match c {
                    '0' => &[Dah, Sp, Dah, Sp, Dah, Sp, Dah, Sp, Dah],
                    '1' => &[Dit, Sp, Dah, Sp, Dah, Sp, Dah, Sp, Dah],
                    '2' => &[Dit, Sp, Dit, Sp, Dah, Sp, Dah, Sp, Dah],
                    '3' => &[Dit, Sp, Dit, Sp, Dit, Sp, Dah, Sp, Dah],
                    '4' => &[Dit, Sp, Dit, Sp, Dit, Sp, Dit, Sp, Dah],
                    '5' => &[Dit, Sp, Dit, Sp, Dit, Sp, Dit, Sp, Dit],
                    '6' => &[Dah, Sp, Dit, Sp, Dit, Sp, Dit, Sp, Dit],
                    '7' => &[Dah, Sp, Dah, Sp, Dit, Sp, Dit, Sp, Dit],
                    '8' => &[Dah, Sp, Dah, Sp, Dah, Sp, Dit, Sp, Dit],
                    '9' => &[Dah, Sp, Dah, Sp, Dah, Sp, Dah, Sp, Dit],
                    'A' => &[Dit, Sp, Dah],
                    'B' => &[Dah, Sp, Dit, Sp, Dit, Sp, Dit],
                    'C' => &[Dah, Sp, Dit, Sp, Dah, Sp, Dit],
                    'D' => &[Dah, Sp, Dit, Sp, Dit],
                    'E' => &[Dit],
                    'F' => &[Dit, Sp, Dit, Sp, Dah, Sp, Dit],
                    'G' => &[Dah, Sp, Dah, Sp, Dit],
                    'H' => &[Dit, Sp, Dit, Sp, Dit, Sp, Dit],
                    'I' => &[Dit, Sp, Dit],
                    'J' => &[Dit, Sp, Dah, Sp, Dah, Sp, Dah],
                    'K' => &[Dah, Sp, Dit, Sp, Dah],
                    'L' => &[Dit, Sp, Dah, Sp, Dit, Sp, Dit],
                    'M' => &[Dah, Sp, Dah],
                    'N' => &[Dah, Sp, Dit],
                    'O' => &[Dah, Sp, Dah, Sp, Dah],
                    'P' => &[Dit, Sp, Dah, Sp, Dah, Sp, Dit],
                    'Q' => &[Dah, Sp, Dah, Sp, Dit, Sp, Dah],
                    'R' => &[Dit, Sp, Dah, Sp, Dit],
                    'S' => &[Dit, Sp, Dit, Sp, Dit],
                    'T' => &[Dah],
                    'U' => &[Dit, Sp, Dit, Sp, Dah],
                    'V' => &[Dit, Sp, Dit, Sp, Dit, Sp, Dah],
                    'W' => &[Dit, Sp, Dah, Sp, Dah],
                    'X' => &[Dah, Sp, Dit, Sp, Dit, Sp, Dah],
                    'Y' => &[Dah, Sp, Dit, Sp, Dah, Sp, Dah],
                    'Z' => &[Dah, Sp, Dah, Sp, Dit, Sp, Dit],
                    '/' => &[Dah, Sp, Dit, Sp, Dit, Sp, Dah, Sp, Dit],
                    '+' => &[Dit, Sp, Dah, Sp, Dit, Sp, Dah, Sp, Dit],
                    '=' => &[Dah, Sp, Dit, Sp, Dit, Sp, Dit, Sp, Dah],
                    '-' => &[Dah, Sp, Dit, Sp, Dit, Sp, Dit, Sp, Dit, Sp, Dah],
                    '.' => &[Dit, Sp, Dah, Sp, Dit, Sp, Dah, Sp, Dit, Sp, Dah],
                    ',' => &[Dah, Sp, Dah, Sp, Dit, Sp, Dit, Sp, Dah, Sp, Dah],
                    '?' => &[Dit, Sp, Dit, Sp, Dah, Sp, Dah, Sp, Dit, Sp, Dit],
                    '(' => &[Dah, Sp, Dit, Sp, Dah, Sp, Dah, Sp, Dit],
                    ')' => &[Dah, Sp, Dit, Sp, Dah, Sp, Dah, Sp, Dit, Sp, Dah],
                    '"' => &[Dit, Sp, Dah, Sp, Dit, Sp, Dit, Sp, Dah, Sp, Dit],
                    ':' => &[Dah, Sp, Dah, Sp, Dah, Sp, Dit, Sp, Dit, Sp, Dit],
                    ';' => &[Dah, Sp, Dit, Sp, Dah, Sp, Dit, Sp, Dah, Sp, Dit],
                    '&' => &[Dit, Sp, Dah, Sp, Dit, Sp, Dit, Sp, Dit],
                    '\'' => &[Dit, Sp, Dah, Sp, Dah, Sp, Dah, Sp, Dah, Sp, Dit],
                    '!' => &[Dah, Sp, Dit, Sp, Dah, Sp, Dit, Sp, Dah, Sp, Dah],
                    '_' => &[Dit, Sp, Dit, Sp, Dah, Sp, Dah, Sp, Dit, Sp, Dah],
                    '$' => &[Dit, Sp, Dit, Sp, Dit, Sp, Dah, Sp, Dit, Sp, Dit, Sp, Dah],
                    '@' => &[Dit, Sp, Dah, Sp, Dah, Sp, Dit, Sp, Dah, Sp, Dit],
                    _ => return None,
                });
            }
        }
    }
    Some(output)
}

/// Keyer block which generates morse signals
pub struct Keyer<Flt> {
    sender: Sender<Samples<Complex<Flt>>>,
}

impl<Flt> Producer<Samples<Complex<Flt>>> for Keyer<Flt>
where
    Flt: Clone,
{
    fn sender_connector(&self) -> SenderConnector<Samples<Complex<Flt>>> {
        self.sender.connector()
    }
}

impl<Flt> Keyer<Flt>
where
    Flt: Float,
{
    /// Generate new `Keyer` block
    ///
    /// **Note:** Currently the created `Keyer` block sends a message once and
    /// then sends silence. This behavior and this interface is expected to
    /// change in future versions.
    pub fn new(chunk_len: usize, sample_rate: f64, speed: Speed, text: &str) -> Option<Self> {
        let units = match encode(text) {
            Some(units) => units,
            None => return None,
        };
        let unit_iter = iter::once(WordSpace)
            .chain(units.into_iter())
            .chain(iter::once(WordSpace));
        let mut unit_iter = unit_iter.chain(iter::repeat(Space)); // TODO: workaround
        let sender = Sender::<Samples<Complex<Flt>>>::new();
        let output = sender.clone();
        let mut is_on: bool = Default::default();
        let mut remaining_samples: usize = 0;
        let mut buf_pool = ChunkBufPool::<Complex<Flt>>::new();
        spawn(async move {
            let mut clock = interval(Duration::from_secs_f64(chunk_len as f64 / sample_rate));
            clock.set_missed_tick_behavior(MissedTickBehavior::Delay);
            loop {
                let mut output_chunk = buf_pool.get_with_capacity(chunk_len);
                while output_chunk.len() < chunk_len {
                    if remaining_samples == 0 {
                        match unit_iter.next() {
                            None => break,
                            Some(unit) => {
                                remaining_samples =
                                    unit.samples(sample_rate, speed).round() as usize; // TODO
                                is_on = unit.on();
                            }
                        }
                    }
                    output_chunk.push(Complex::from(if is_on { Flt::one() } else { Flt::zero() }));
                    remaining_samples -= 1;
                }
                if output_chunk.is_empty() {
                    break;
                }
                clock.tick().await;
                output
                    .send(Samples {
                        sample_rate,
                        chunk: output_chunk.finalize(),
                    })
                    .await;
            }
            output.finish().await;
        });
        Some(Self { sender })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const PRECISION: f64 = 1e-10;
    fn assert_approx(a: f64, b: f64) {
        assert!((a - b).abs() <= PRECISION || (a / b).ln().abs() <= PRECISION);
    }
    #[test]
    fn test_morse_speed_type() {
        let speed = Speed::from_paris_wpm(16.0);
        assert_approx(speed.paris_wpm(), 16.0);
        assert_approx(speed.codex_wpm(), 13.333333333333);
        assert_approx(Speed::from_codex_wpm(13.333333333333).paris_wpm(), 16.0);
        assert_approx(speed.paris_cpm(), 80.0);
        assert_approx(Speed::from_paris_cpm(80.0).paris_wpm(), 16.0);
        assert_approx(speed.codex_cpm(), 66.666666666667);
        assert_approx(Speed::from_codex_cpm(66.666666666667).paris_wpm(), 16.0);
        assert_approx(speed.dits_per_minute(), 800.0);
        assert_approx(
            Speed::from_dits_per_minute(800.0).paris_wpm(),
            speed.paris_wpm(),
        );
        assert_approx(speed.seconds_per_dit(), 75e-3);
        assert_approx(speed.samples_per_dit(1.0), 75e-3);
        assert_approx(speed.samples_per_dit(48000.0), 3600.0);
        assert_approx(Unit::Dit.samples(48000.0, speed), 3600.0);
        assert_approx(Unit::Dah.samples(48000.0, speed), 10800.0);
        assert_approx(Unit::CharSpace.samples(48000.0, speed), 10800.0);
        assert_approx(Unit::WordSpace.samples(48000.0, speed), 25200.0);
    }
    #[test]
    fn test_encode() {
        use Space as Sp;
        assert_eq!(
            encode("AB C").as_ref().unwrap(),
            &[
                Dit, Sp, Dah, CharSpace, Dah, Sp, Dit, Sp, Dit, Sp, Dit, WordSpace, Dah, Sp, Dit,
                Sp, Dah, Sp, Dit,
            ]
        );
    }
    #[test]
    fn test_encode_prosign() {
        use Space as Sp;
        assert_eq!(
            encode("<TTTTTT>V <CT> X<AR>").as_ref().unwrap(),
            &[
                Dah, Sp, Dah, Sp, Dah, Sp, Dah, Sp, Dah, Sp, Dah, CharSpace, Dit, Sp, Dit, Sp, Dit,
                Sp, Dah, WordSpace, Dah, Sp, Dit, Sp, Dah, Sp, Dit, Sp, Dah, WordSpace, Dah, Sp,
                Dit, Sp, Dit, Sp, Dah, CharSpace, Dit, Sp, Dah, Sp, Dit, Sp, Dah, Sp, Dit,
            ]
        );
    }
}
