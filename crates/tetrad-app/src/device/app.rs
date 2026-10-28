//! Extension programs (`app` in the manifest, API 7), such as a game.
//!
//! A program draws on the framebuffer and reads the footswitch nodes itself, so while it runs
//! Tetrad presents no frames and drops footswitch, knob and TEMPO input. The stomp nodes are
//! evdev nodes (stock `S03splash` links `/dev/zencoder/knob_stompN` to `/dev/input/eventN`), and
//! nobody grabs them, so both readers get every event. Audio, MIDI, the volume knob and the
//! pedals go on. A touch held for [`QUIT_HOLD`] stops the program, as does the power button;
//! when it exits, Tetrad draws the whole screen again.

use std::path::PathBuf;
use std::time::Duration;

/// A touch held this long stops the program. The programs do not read the touch screen.
pub const QUIT_HOLD: Duration = Duration::from_secs(2);
/// How often the device checks whether the program exited and how long the touch is held.
pub const POLL: Duration = Duration::from_millis(100);

/// Everything to start one program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppLaunch {
    /// The extension's name, for notices.
    pub name: String,
    /// The program file in the package's program folder.
    pub program: PathBuf,
    /// `{app}` already replaced with the program folder.
    pub args: Vec<String>,
    /// The working folder: the extension's asset folder, where a game keeps its saves.
    pub dir: PathBuf,
    /// The button label on the extension's page.
    pub label: String,
}

/// A started program.
pub trait AppProcess {
    /// `None` while it runs; how it ended once it has.
    fn exited(&mut self) -> Option<String>;
    /// Ends it. [`Self::exited`] reports the end.
    fn stop(&mut self);
}

/// The program that has the screen.
pub struct Running {
    process: Box<dyn AppProcess>,
    pub name: String,
    /// When the current touch went down.
    touch_down: Option<Duration>,
}

impl Running {
    pub fn new(name: String, process: Box<dyn AppProcess>) -> Self {
        Self {
            process,
            name,
            touch_down: None,
        }
    }

    /// Follows the touch for [`QUIT_HOLD`]: a press starts the hold, a release ends it.
    pub fn touch(&mut self, pressed: bool, now: Duration) {
        match pressed {
            true => self.touch_down = self.touch_down.or(Some(now)),
            false => self.touch_down = None,
        }
    }

    /// One [`POLL`]: stops the program when the touch was held long enough, then reports
    /// how it ended, if it has.
    pub fn poll(&mut self, now: Duration) -> Option<String> {
        if self
            .touch_down
            .is_some_and(|down| now.saturating_sub(down) >= QUIT_HOLD)
        {
            self.process.stop();
        }
        self.process.exited()
    }

    /// A finger is on the screen, such as the one that held to quit.
    pub fn touching(&self) -> bool {
        self.touch_down.is_some()
    }

    pub fn stop(&mut self) {
        self.process.stop();
    }
}
