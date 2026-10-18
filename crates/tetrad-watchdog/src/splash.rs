//! Splash queue contract: `splashqueue` main `0x858c` sends one native ARM `int`
//! to the POSIX queue that `zensplash` receives. Sending is an injected backend.

use std::ffi::OsStr;

use thiserror::Error;

pub const QUEUE_NAME: &str = "/splashqueue";
/// `mq_open` flags `0x41` (`O_WRONLY | O_CREAT`, blocking) and mode `0x1b6`.
pub const QUEUE_MODE: u32 = 0o666;
/// Creation attributes. They do not replace those of an existing queue.
pub const QUEUE_MAX_MESSAGES: u32 = 10;
pub const MESSAGE_SIZE: usize = 4;
pub const MESSAGE_PRIORITY: u32 = 0;

/// One splash command. Every `i32` is representable for evidence replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SplashCommand(i32);

impl SplashCommand {
    /// Sent after progress 20 (`S80drvinit:335`). Inferred: hand the screen to the UI.
    pub const HAND_OFF: Self = Self(1000);
    /// Sent when the splash asked to restart and boot is about to exit (`S80drvinit:356`).
    pub const RESTART_READY: Self = Self(2000);
    /// ZenUI `killSplashScreen` (`0x544860`), one second after progress 100.
    /// This is the stock "application ready" signal; it also ends UI progress.
    pub const DISMISS: Self = Self(0);

    pub const fn new(value: i32) -> Self {
        Self(value)
    }

    pub const fn value(self) -> i32 {
        self.0
    }

    /// Native little-endian ARM `int`, priority [`MESSAGE_PRIORITY`].
    pub const fn to_bytes(self) -> [u8; MESSAGE_SIZE] {
        self.0.to_le_bytes()
    }

    pub const fn from_bytes(bytes: [u8; MESSAGE_SIZE]) -> Self {
        Self(i32::from_le_bytes(bytes))
    }
}

/// Boot script progress (`S80drvinit`), in send order. [`SplashCommand::HAND_OFF`] follows.
pub const BOOT_PROGRESS: [i32; 10] = [2, 4, 6, 8, 10, 12, 14, 16, 18, 20];

/// ZenUI startup progress, sent through `popen("/opt/neuraldsp/splashqueue N")`
/// by `0x545908` only while its splash flag (`Application+0x1bc`) is set:
/// 30, 60, 70 from main (`0x7dd68`); 40, 50 from DSP driver and core load
/// (`0x876dec`); 80, 90 from UI build (`0x5af624`); 100 when the UI goes full
/// screen (`0x545ac0`). Values are confirmed; the 40/50 position is inferred.
pub const UI_PROGRESS: [i32; 8] = [30, 40, 50, 60, 70, 80, 90, 100];

/// Injected queue sender. A send is not receiver acknowledgement. The stock
/// sender blocks while the queue is full; a backend must bound that wait.
pub trait SplashQueue {
    type Error;
    fn send(&mut self, command: SplashCommand) -> Result<(), Self::Error>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("usage: splashqueue <command>")]
pub struct UsageError;

/// `splashqueue` argument handling: exactly one argument, converted with glibc
/// `atoi`. Invalid text becomes 0, as stock does; it is not rejected.
pub fn parse_cli(args: &[&OsStr]) -> Result<SplashCommand, UsageError> {
    match args {
        [arg] => Ok(SplashCommand(atoi(arg.as_encoded_bytes()))),
        _ => Err(UsageError),
    }
}

/// glibc `atoi` on 32-bit ARM: `(int) strtol(text, NULL, 10)` with `long` = 32
/// bits, so overflow clamps to `i32::MIN`/`i32::MAX`.
pub fn atoi(text: &[u8]) -> i32 {
    let text = match text.iter().position(|b| !b" \t\n\x0b\x0c\r".contains(b)) {
        Some(start) => &text[start..],
        None => return 0,
    };
    let (negative, digits) = match text.first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let magnitude = digits
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .fold(0_i64, |value, digit| {
            value
                .saturating_mul(10)
                .saturating_add(i64::from(digit - b'0'))
        });
    let value = if negative { -magnitude } else { magnitude };
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_uses_atoi_semantics_and_little_endian_wire_bytes() {
        let parse = |text: &str| parse_cli(&[OsStr::new(text)]).unwrap();
        assert_eq!(parse("1000"), SplashCommand::HAND_OFF);
        assert_eq!(parse("  +2000x"), SplashCommand::RESTART_READY);
        assert_eq!(parse("abc").value(), 0);
        assert_eq!(parse("-99999999999").value(), i32::MIN);
        assert_eq!(parse("99999999999").value(), i32::MAX);
        assert_eq!(parse_cli(&[]), Err(UsageError));
        assert_eq!(
            parse_cli(&[OsStr::new("2"), OsStr::new("4")]),
            Err(UsageError)
        );
        assert_eq!(SplashCommand::HAND_OFF.to_bytes(), [0xe8, 0x03, 0, 0]);
        assert_eq!(
            SplashCommand::from_bytes([0xfe, 0xff, 0xff, 0xff]).value(),
            -2
        );
    }
}
