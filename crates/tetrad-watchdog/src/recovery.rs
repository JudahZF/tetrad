//! Stock ZenWatchdog presentation, logging and recovery launch, as typed intents.
//! A [`Delivery`] from the [`Supervisor`](crate::Supervisor) maps to the exact
//! stock side effects; the caller executes them through its own backends.

use std::fmt;

use tetrad_runtime::watchdog::WatchdogEvent;

use crate::{cmdline::Product, supervisor::Delivery};

/// Started by the countdown at zero (`0xcb74`) with `QProcess::start`.
pub const REBOOT_HELPER: &str = "/opt/neuraldsp/zenreboot";
/// Stock probe command (`0xe600`); a Rust target replaces it.
pub const STOCK_PROBE: &str = "pidof ZenUI > /dev/null";

/// Full-screen crash view geometry (main `0xbc50`: fixed 800 px view, scene
/// 800 x 480, black background, frameless window, hidden until armed).
pub const SCREEN_WIDTH: u16 = 800;
pub const SCREEN_HEIGHT: u16 = 480;
/// Text box `(x, y, width, height)` in `CrashGraphicsItem::paint` (`0xc4b8`):
/// centered (`0x84`), white `#FFFFFF`, transparent brush, [`CRASH_FONT`].
/// Drawn only when the text is not empty.
pub const TEXT_BOX: (u16, u16, u16, u16) = (200, 200, 400, 50);

/// Application font set by main `0xbc50` through `0xf254`:
/// `QFont(family, 18, 50, false)`, then `setPixelSize(18)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrashFont {
    pub family: &'static str,
    pub pixel_size: u16,
    /// Qt 4 weight; 50 is `QFont::Normal`.
    pub weight: u16,
    pub italic: bool,
}

pub const CRASH_FONT: CrashFont = CrashFont {
    family: "IBM Plex Sans",
    pixel_size: 18,
    weight: 50,
    italic: false,
};
pub const REBOOTING_TEXT: &str = "Rebooting!";

/// Full-screen crash image. Stock art is a proprietary resource; the UI owner
/// supplies owned art for each ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashImage {
    QuadCortex,
    QuadCortexMini,
}

impl CrashImage {
    pub fn for_product(product: Product) -> Self {
        match product {
            Product::QuadCortex => Self::QuadCortex,
            Product::QuadCortexMini => Self::QuadCortexMini,
        }
    }

    /// Stock Qt resource name (strings `0xf874`, `0xf858`).
    pub fn stock_resource(self) -> &'static str {
        match self {
            Self::QuadCortex => ":resource/crash/qc.png",
            Self::QuadCortexMini => ":resource/crash/atma.png",
        }
    }
}

/// `"%1 encountered an error, rebooting in %2 s"` (string `0xf5a0`).
pub fn countdown_text(product: Product, seconds: u8) -> String {
    format!(
        "{} encountered an error, rebooting in {seconds} s",
        product.display_name()
    )
}

/// Qt message type; the message handler (`0xef04`) tags each line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Warning,
    Critical,
    /// The stock handler calls `abort()` after printing a fatal line.
    Fatal,
}

impl LogLevel {
    fn tag(self) -> char {
        match self {
            Self::Debug => 'D',
            Self::Warning => 'W',
            Self::Critical => 'C',
            Self::Fatal => 'F',
        }
    }
}

/// One stock message. Text is exact, including the trailing space that Qt 4
/// `QDebug` leaves after the last item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogMessage {
    pub level: LogLevel,
    pub text: &'static str,
}

/// Local wall-clock time from `QDateTime::currentDateTime`, injected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogTimestamp {
    pub day: u8,
    pub month: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub millisecond: u16,
}

impl fmt::Display for LogTimestamp {
    /// Qt pattern `dd:MM hh:mm:ss:zzz` (string `0xf7f4`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02}:{:02} {:02}:{:02}:{:02}:{:03}",
            self.day, self.month, self.hour, self.minute, self.second, self.millisecond
        )
    }
}

impl LogMessage {
    const fn new(level: LogLevel, text: &'static str) -> Self {
        Self { level, text }
    }

    /// `"%s [WD] [<tag>]: %s\n"` on stderr; boot pipes stderr into the watchdog log.
    pub fn line(&self, at: LogTimestamp) -> String {
        format!("{at} [WD] [{}]: {}\n", self.level.tag(), self.text)
    }
}

/// Logged by main (`0xbc50`), `start` (`0xdff8`) and the constructor (`0xe250`).
pub const STARTUP_LOG: [LogMessage; 3] = [
    LogMessage::new(LogLevel::Warning, "ZenUI Monitor "),
    LogMessage::new(
        LogLevel::Warning,
        "Watchdog started: \"ZenUI monitoring started without D-BUS\" ",
    ),
    LogMessage::new(LogLevel::Warning, "Watchdog check timer once every 500 ms "),
];

const PROCESS_DIED: LogMessage = LogMessage::new(LogLevel::Critical, "ZenUI process died ");
const STOPPED: LogMessage = LogMessage::new(
    LogLevel::Warning,
    "Watchdog stopped: \"ZenUI process died\" ",
);
const REBOOT_IN: LogMessage =
    LogMessage::new(LogLevel::Critical, "System will reboot in: 5000 ms ");

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryIntent {
    Log(LogMessage),
    /// `QWidget::showFullScreen` on the hidden crash view; no text yet.
    ShowCrashScreen(CrashImage),
    SetCrashText(String),
    /// Start [`REBOOT_HELPER`] without waiting for it. It is not a reboot by itself.
    LaunchRebootHelper,
    /// Owned cancellation only; stock never hides the view once shown.
    HideCrashScreen,
}

/// Stock side effects for one delivery, in stock order.
pub fn recovery_intents(delivery: &Delivery, product: Product) -> Vec<RecoveryIntent> {
    match delivery.event {
        WatchdogEvent::PresenceChecked { .. } => Vec::new(),
        // Check `0xe600`: log, stop (logs again), show, seed 5,000, log, start 1 s timer.
        WatchdogEvent::CountdownStarted { .. } => vec![
            RecoveryIntent::Log(PROCESS_DIED),
            RecoveryIntent::Log(STOPPED),
            RecoveryIntent::ShowCrashScreen(CrashImage::for_product(product)),
            RecoveryIntent::Log(REBOOT_IN),
        ],
        WatchdogEvent::CountdownTick { seconds } => {
            vec![RecoveryIntent::SetCrashText(countdown_text(
                product, seconds,
            ))]
        }
        WatchdogEvent::RebootRequested => vec![
            RecoveryIntent::SetCrashText(REBOOTING_TEXT.to_owned()),
            RecoveryIntent::LaunchRebootHelper,
        ],
        WatchdogEvent::Cancelled => vec![RecoveryIntent::HideCrashScreen],
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroUsize, time::Duration};

    use super::*;
    use crate::{
        Clock, Supervisor,
        process::{ProcessName, ProcessRecord, ProcessSource, ProcessTarget},
    };

    struct At(Duration);
    impl Clock for At {
        fn now(&self) -> Duration {
            self.0
        }
    }
    struct Gone;
    impl ProcessSource for Gone {
        type Error = std::convert::Infallible;
        fn snapshot(&mut self) -> Result<&[ProcessRecord], Self::Error> {
            Ok(&[])
        }
    }

    #[test]
    fn missing_process_yields_stock_screen_log_and_helper_sequence() {
        let target = ProcessTarget::Name(ProcessName::new(b"ZenUI").unwrap());
        let mut supervisor =
            Supervisor::new(Duration::ZERO, target, NonZeroUsize::new(8).unwrap()).unwrap();
        let mut intents = Vec::new();
        for ms in (500..=9_000).step_by(500) {
            if let Some(delivery) = supervisor
                .poll(&At(Duration::from_millis(ms)), &mut Gone)
                .unwrap()
                .copied()
            {
                intents.extend(recovery_intents(&delivery, Product::QuadCortexMini));
                supervisor.acknowledge(delivery.id).unwrap();
            }
        }
        let text = |s: u8| RecoveryIntent::SetCrashText(countdown_text(Product::QuadCortexMini, s));
        assert_eq!(
            intents,
            [
                RecoveryIntent::Log(PROCESS_DIED),
                RecoveryIntent::Log(STOPPED),
                RecoveryIntent::ShowCrashScreen(CrashImage::QuadCortexMini),
                RecoveryIntent::Log(REBOOT_IN),
                text(5),
                text(4),
                text(3),
                text(2),
                text(1),
                RecoveryIntent::SetCrashText("Rebooting!".to_owned()),
                RecoveryIntent::LaunchRebootHelper,
            ]
        );
        assert_eq!(
            countdown_text(Product::QuadCortex, 5),
            "Quad Cortex encountered an error, rebooting in 5 s"
        );
        let at = LogTimestamp {
            day: 3,
            month: 10,
            hour: 7,
            minute: 5,
            second: 9,
            millisecond: 42,
        };
        assert_eq!(
            PROCESS_DIED.line(at),
            "03:10 07:05:09:042 [WD] [C]: ZenUI process died \n"
        );
    }
}
