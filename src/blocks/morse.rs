//! Morse code
//!
//! **Note: The API of this module is highly unstable yet and subject to change.**

use crate::bufferpool::*;
use crate::flow::*;
use crate::impl_block_trait;
use crate::numbers::*;
use crate::signal::*;

use tokio::sync::{mpsc, watch};
use tokio::task::spawn;

use std::borrow::Cow;
use std::error::Error;
use std::fmt;
use std::sync::Arc;

/// Types used as [`Signal::Event::payload`]
pub mod events {
    /// Sent by [`Keyer`] block as [`Signal::event::payload`] when all morse
    /// messages have been sent.
    ///
    /// [`Keyer`]: super::Keyer
    /// [`Signal::event::payload`]: crate::signal::Signal::Event::payload
    #[derive(Clone, Debug)]
    pub struct EndOfMessages;
}
use events::*;

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
    /// Padding at beginning and end of a message (half a `WordSpace`)
    Padding,
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
            Padding => false,
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
            Padding => 3.5,
        }
    }
    /// Length of `Unit` in samples for given [`Speed`]
    pub fn samples(self, sample_rate: f64, speed: Speed) -> f64 {
        speed.samples_per_dit(sample_rate) * self.relative_duration()
    }
}

/// Error when text cannot be converted to morse code
#[derive(Debug)]
pub struct EncodeError(Cow<'static, str>);

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&*self.0)
    }
}

impl Error for EncodeError {}

/// Encodes a text as sequence of [`Unit`]s
///
/// Returns `None` on invalid or unexpected input.
pub fn encode(text: &str) -> Result<Vec<Unit>, EncodeError> {
    use Space as Sp;
    let mut output: Vec<Unit> = Vec::new();
    output.push(Padding);
    let mut prosign = false;
    let mut previous_char: bool = false;
    for c in text.chars().map(|c| c.to_uppercase()).flatten() {
        match c {
            '<' => {
                if prosign {
                    return Err(EncodeError(Cow::Borrowed("double opening bracket")));
                }
                if previous_char {
                    previous_char = false;
                    output.push(CharSpace)
                }
                prosign = true;
            }
            '>' => {
                if !prosign || !previous_char {
                    return Err(EncodeError(Cow::Borrowed("unexpected closing bracket")));
                }
                prosign = false;
            }
            ' ' => {
                if prosign {
                    return Err(EncodeError(Cow::Borrowed("space in prosign")));
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
                    _ => {
                        return Err(EncodeError(match c {
                            c if !c.is_ascii() => Cow::Borrowed("unsupported non-ASCII character"),
                            c if c.is_ascii_control() => {
                                Cow::Borrowed("unsupporded ASCII control character")
                            }
                            _ => Cow::Owned(format!("unsupported character \"{c}\"")),
                        }));
                    }
                });
            }
        }
    }
    output.push(Padding);
    Ok(output)
}

/// Keyer block which generates morse signals
///
/// If not dropped, the keyer will send silence unless a message has been
/// queued using [`Keyer::send`]. On drop, message queue is emptied before
/// sending is stopped. After all messages have been completed, a
/// [`Signal::Event`] with [`EndOfMessages`] as [`payload`] is sent.
///
/// [`payload`]: Signal::Event::payload
pub struct Keyer<Flt> {
    sender_connector: SenderConnector<Signal<Complex<Flt>>>,
    speed: watch::Sender<Speed>,
    messages: mpsc::UnboundedSender<Vec<Unit>>,
}

impl_block_trait! { <Flt> Producer<Signal<Complex<Flt>>> for Keyer<Flt> }

impl<Flt> Keyer<Flt>
where
    Flt: Float,
{
    /// Generate new `Keyer` block without initial message
    ///
    /// [I/O blocks]: crate::blocks::io
    pub fn new(chunk_len: usize, sample_rate: f64, speed: Speed) -> Self {
        Self::new_internal(chunk_len, sample_rate, speed, None)
    }
    /// Generate new `Keyer` block with initial message
    pub fn with_message(
        chunk_len: usize,
        sample_rate: f64,
        speed: Speed,
        message: &str,
    ) -> Result<Self, EncodeError> {
        Ok(Self::new_internal(
            chunk_len,
            sample_rate,
            speed,
            Some(encode(message)?),
        ))
    }
    fn new_internal(
        chunk_len: usize,
        sample_rate: f64,
        speed: Speed,
        message: Option<Vec<Unit>>,
    ) -> Self {
        let (sender, sender_connector) = new_sender::<Signal<Complex<Flt>>>();
        let (speed_send, mut speed_recv) = watch::channel(speed);
        let (messages_send, mut messages_recv) = mpsc::unbounded_channel::<Vec<Unit>>();
        if let Some(message) = message {
            messages_send.send(message).unwrap();
        }
        spawn(async move {
            let mut speed = speed;
            let mut is_on: bool = Default::default();
            let mut remaining_samples: usize = 0;
            let mut buf_pool = ChunkBufPool::<Complex<Flt>>::new();
            let mut empty_chunk_buf = buf_pool.get_with_capacity(chunk_len);
            for _ in 0..chunk_len {
                empty_chunk_buf.push(Complex::from(Flt::zero()));
            }
            let empty_chunk = empty_chunk_buf.finalize();
            let mut idle = false;
            let mut output_chunk = buf_pool.get_with_capacity(chunk_len);
            loop {
                match messages_recv.try_recv() {
                    Ok(units) => {
                        idle = false;
                        let mut unit_iter = units.into_iter();
                        loop {
                            if remaining_samples == 0 {
                                match unit_iter.next() {
                                    None => break,
                                    Some(unit) => {
                                        if speed_recv.has_changed().unwrap_or(false) {
                                            speed = speed_recv.borrow_and_update().clone();
                                        }
                                        remaining_samples =
                                            unit.samples(sample_rate, speed).round() as usize;
                                        is_on = unit.on();
                                    }
                                }
                            }
                            output_chunk.push(Complex::from(if is_on {
                                Flt::one()
                            } else {
                                Flt::zero()
                            }));
                            if output_chunk.len() >= chunk_len {
                                let Ok(()) = sender.send(Signal::Samples {
                                    sample_rate,
                                    chunk: output_chunk.finalize(),
                                }).await
                                else { return; };
                                output_chunk = buf_pool.get_with_capacity(chunk_len);
                            }
                            remaining_samples -= 1;
                        }
                    }
                    Err(mpsc::error::TryRecvError::Empty) => {
                        if !output_chunk.is_empty() {
                            while output_chunk.len() < chunk_len {
                                output_chunk.push(Complex::from(Flt::zero()));
                            }
                            let Ok(()) = sender.send(Signal::Samples {
                                sample_rate,
                                chunk: output_chunk.finalize(),
                            }).await
                            else { return; };
                            output_chunk = buf_pool.get_with_capacity(chunk_len);
                        }
                        if idle {
                            let Ok(()) = sender.send(Signal::Samples {
                                sample_rate,
                                chunk: empty_chunk.clone(),
                            }).await
                            else { return; };
                        } else {
                            let Ok(()) = sender.send(Signal::Event {
                                interrupt: false,
                                payload: Arc::new(EndOfMessages),
                            }).await
                            else { return; };
                            idle = true;
                        }
                    }
                    Err(mpsc::error::TryRecvError::Disconnected) => return,
                }
            }
        });
        Self {
            sender_connector,
            speed: speed_send,
            messages: messages_send,
        }
    }
    /// Send text as morse code
    pub fn send(&self, text: &str) -> Result<(), EncodeError> {
        self.messages.send(encode(text)?).unwrap();
        Ok(())
    }
    /// Set morse speed
    pub fn set_speed(&self, speed: Speed) {
        self.speed.send_replace(speed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::assert_approx;
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
                Padding, Dit, Sp, Dah, CharSpace, Dah, Sp, Dit, Sp, Dit, Sp, Dit, WordSpace, Dah,
                Sp, Dit, Sp, Dah, Sp, Dit, Padding,
            ]
        );
    }
    #[test]
    fn test_encode_prosign() {
        use Space as Sp;
        assert_eq!(
            encode("<TTTTTT>V <CT> X<AR>").as_ref().unwrap(),
            &[
                Padding, Dah, Sp, Dah, Sp, Dah, Sp, Dah, Sp, Dah, Sp, Dah, CharSpace, Dit, Sp, Dit,
                Sp, Dit, Sp, Dah, WordSpace, Dah, Sp, Dit, Sp, Dah, Sp, Dit, Sp, Dah, WordSpace,
                Dah, Sp, Dit, Sp, Dit, Sp, Dah, CharSpace, Dit, Sp, Dah, Sp, Dit, Sp, Dah, Sp, Dit,
                Padding,
            ]
        );
    }
}
