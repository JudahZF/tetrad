//! The Quad Cortex hardware abstraction recovered from stock `libzenhal.so`:
//! typed request builders and record decoders, the [`hal::Hal`] trait, a stock
//! implementation over an injected [`hal::Driver`] and a recording fake.
//! No device access: `tetrad-sys` implements `Driver` on Linux.
#![forbid(unsafe_code)]

pub mod analog;
pub mod codec;
pub mod hal;
pub mod input;
pub mod midi;
pub mod node;
pub mod platform;
pub mod requests;
pub mod uart;
pub mod zencoder;
pub mod zenjack;

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecordLengthError {
    pub expected: usize,
    pub actual: usize,
}

impl fmt::Display for RecordLengthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "expected {} record bytes, got {}",
            self.expected, self.actual
        )
    }
}

impl std::error::Error for RecordLengthError {}

/// A gate, not a guessed hardware revision or ioctl endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedContract {
    pub reason: &'static str,
}

/// `analog` alone covers only the integer PGA subset. The full stock analog
/// contract (type, impedance, lifts, headphone, boosts) is `zenjack` + `hal::StockHal`.
pub const ANALOG_CONTROLS: UnsupportedContract = UnsupportedContract {
    reason: "analog exposes only integer PGA gains; use zenjack and hal::StockHal for the full stock analog contract",
};
