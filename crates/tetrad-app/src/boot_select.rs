//! `tetrad select`: the boot selector (docs/launcher.md). When Tetrad is armed, the boot launcher
//! (`scripts/launcher/tetrad-launch.sh`) runs it before either UI. It shows [`tetrad_ui::BootMenu`]
//! for [`COUNTDOWN`]: a touch on a button picks that UI at once, else the default starts. The exit
//! status names the choice and the launcher starts that UI. A touched choice is saved in the choice
//! file ([`CHOICE_FILE`]) and is the next default.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::time::Duration;

pub use tetrad_ui::BootChoice;

use crate::device::platform::TouchSample;

/// Exit status of `tetrad select` for each choice: `TD_SELECT_STOCK` and `TD_SELECT_TETRAD` in
/// tetrad-launch.sh (a host test compares them). Owned: neither is a status that a crash, a kill,
/// a clap error or a missing binary gives (0–2, 101, 126, 127, 128 + signal), so the launcher
/// reads every other status as a failure and starts the saved choice.
pub const STOCK_EXIT: u8 = 10;
pub const TETRAD_EXIT: u8 = 11;
/// From the first frame to the start of the default. The launcher kills the selector after
/// `TD_SELECT_LIMIT` seconds (a host test keeps a margin).
pub const COUNTDOWN: Duration = Duration::from_secs(5);
/// The saved choice (Tetrad-owned, p4). One line: `tetrad` or `coros`. The launcher reads the
/// same words when the selector fails.
pub const CHOICE_FILE: &str = "/media/p4/tetrad/launcher/boot-choice";
const VERSION: &str = concat!("Tetrad ", env!("CARGO_PKG_VERSION"));

pub fn exit_code(choice: BootChoice) -> u8 {
    match choice {
        BootChoice::Stock => STOCK_EXIT,
        BootChoice::Tetrad => TETRAD_EXIT,
    }
}

fn word(choice: BootChoice) -> &'static str {
    match choice {
        BootChoice::Stock => "coros",
        BootChoice::Tetrad => "tetrad",
    }
}

/// The saved choice. No file (the first boot), an unreadable file or an unknown word: Tetrad.
pub fn read_choice(path: &Path) -> BootChoice {
    match fs::read_to_string(path).as_deref().map(str::trim) {
        Ok("coros") => BootChoice::Stock,
        _ => BootChoice::Tetrad,
    }
}

/// Replaces the choice file atomically: a temp file beside it, `fsync`, `rename`, `fsync` of
/// the folder. After a power cut the file holds the old or the new choice.
pub fn write_choice(path: &Path, choice: BootChoice) -> io::Result<()> {
    let temp = path.with_extension("tmp");
    let mut file = File::create(&temp)?;
    writeln!(file, "{}", word(choice))?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    let folder = path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty());
    File::open(folder.unwrap_or(Path::new(".")))?.sync_all()
}

/// The countdown and the touch rule, without a clock or a device.
#[derive(Debug, Clone)]
pub struct Selector {
    default: BootChoice,
    started: Duration,
    /// The last sample was pressed: a choice needs a new press.
    down: bool,
}

impl Selector {
    pub fn new(default: BootChoice, now: Duration) -> Self {
        Self {
            default,
            started: now,
            down: false,
        }
    }

    /// Whole seconds until the default starts, rounded up; 0 when it is due.
    pub fn seconds_left(&self, now: Duration) -> u32 {
        let left = COUNTDOWN.saturating_sub(now.saturating_sub(self.started));
        left.as_millis().div_ceil(1000) as u32
    }

    /// The default once the countdown is over.
    pub fn due(&self, now: Duration) -> Option<BootChoice> {
        (self.seconds_left(now) == 0).then_some(self.default)
    }

    /// A press (a pressed sample after a released one) on a button picks its UI.
    pub fn touch(&mut self, sample: TouchSample) -> Option<BootChoice> {
        let press = sample.pressed && !self.down;
        self.down = sample.pressed;
        press
            .then(|| BootChoice::at(i32::from(sample.x), i32::from(sample.y)))
            .flatten()
    }

    pub fn frame(&self, now: Duration) -> tetrad_ui::Frame {
        tetrad_ui::BootMenu {
            default: self.default,
            seconds_left: self.seconds_left(now),
            version: VERSION,
        }
        .render()
    }
}

/// `tetrad select --file FILE`. Renames itself `ZenUI` first (ZenWatchdog, o-lifecycle §2),
/// then shows the selector over the boot splash and puts the splash pixels back before it
/// returns, so each UI takes over the screen it expects. Without a framebuffer or a touch
/// screen, or after a read or write failure, it returns the error and the launcher starts the
/// saved choice. A failed save of a touched choice is only logged: the choice still starts.
#[cfg(target_os = "linux")]
pub fn run(file: &Path) -> io::Result<BootChoice> {
    use crate::device::linux::{LinuxRoot, STOCK_COMM, Touch};
    use tetrad_sys::display::Framebuffer;

    fs::write("/proc/self/comm", STOCK_COMM)?;
    let default = read_choice(file);
    let mut framebuffer = Framebuffer::open(Path::new(tetrad_sys::paths::FRAMEBUFFER))?;
    let mut notes = Vec::new();
    let mut touch = Touch::open(&LinuxRoot::new("/"), &mut notes)?;
    let splash = framebuffer.save()?;
    let chosen = choose(&mut framebuffer, &mut touch, default);
    let restored = framebuffer.restore(&splash);
    let choice = chosen?;
    restored?;
    if choice != default
        && let Err(error) = write_choice(file, choice)
    {
        notes.push(format!("{}: {error}; the default stays", file.display()));
    }
    let mut stderr = io::stderr().lock();
    for note in notes {
        let _ = writeln!(stderr, "tetrad select: {note}");
    }
    Ok(choice)
}

/// The selector loop: the whole frame once per countdown second (the splash's QWS server may
/// have painted over it), touch every [`POLL`].
#[cfg(target_os = "linux")]
fn choose(
    framebuffer: &mut tetrad_sys::display::Framebuffer,
    touch: &mut crate::device::linux::Touch,
    default: BootChoice,
) -> io::Result<BootChoice> {
    use crate::device::platform::Input;
    /// Owned: touch poll interval.
    const POLL: Duration = Duration::from_millis(20);

    let start = std::time::Instant::now();
    let mut selector = Selector::new(default, Duration::ZERO);
    let mut inputs = Vec::new();
    // A touch from before the selector showed picks nothing.
    touch.poll(&mut inputs)?;
    inputs.clear();
    let mut shown = None;
    loop {
        let now = start.elapsed();
        if let Some(choice) = selector.due(now) {
            return Ok(choice);
        }
        let left = selector.seconds_left(now);
        if shown != Some(left) {
            framebuffer.forget_shown();
            let frame = selector.frame(now);
            framebuffer.present_rgb8(frame.width(), frame.height(), frame.rgb())?;
            shown = Some(left);
        }
        touch.poll(&mut inputs)?;
        for input in inputs.drain(..) {
            if let Input::Touch(sample) = input
                && let Some(choice) = selector.touch(sample)
            {
                return Ok(choice);
            }
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(choice: Option<BootChoice>, pressed: bool) -> TouchSample {
        let (x, y) = choice.map_or((400, 440), |choice| choice.button().center());
        TouchSample {
            x: x as u16,
            y: y as u16,
            pressed,
        }
    }

    /// The default starts after the countdown; the count shown rounds up.
    #[test]
    fn the_countdown_ends_in_the_default() {
        let start = Duration::from_millis(700);
        let selector = Selector::new(BootChoice::Stock, start);
        let at = |ms| start + Duration::from_millis(ms);
        assert_eq!(selector.seconds_left(at(0)), 5);
        assert_eq!(selector.seconds_left(at(1)), 5);
        assert_eq!(selector.seconds_left(at(1000)), 4);
        assert_eq!(selector.seconds_left(at(4999)), 1);
        assert_eq!(selector.due(at(4999)), None);
        assert_eq!(selector.due(at(5000)), Some(BootChoice::Stock));
    }

    /// Only a press picks: on a button its UI, off the buttons nothing; a finger that slides
    /// onto a button or stays down picks nothing until it lifts and presses again.
    #[test]
    fn a_press_on_a_button_picks_it() {
        let mut selector = Selector::new(BootChoice::Tetrad, Duration::ZERO);
        assert_eq!(selector.touch(sample(None, true)), None);
        assert_eq!(selector.touch(sample(Some(BootChoice::Stock), true)), None);
        assert_eq!(selector.touch(sample(Some(BootChoice::Stock), false)), None);
        assert_eq!(
            selector.touch(sample(Some(BootChoice::Stock), true)),
            Some(BootChoice::Stock)
        );
        let mut selector = Selector::new(BootChoice::Stock, Duration::ZERO);
        assert_eq!(
            selector.touch(sample(Some(BootChoice::Tetrad), true)),
            Some(BootChoice::Tetrad)
        );
    }

    /// The choice file round-trips both choices, is replaced without a temp file left over,
    /// and reads as Tetrad when it is missing or holds an unknown word. Each choice has its
    /// own exit status, and neither is 0 or 1.
    #[test]
    fn the_choice_file_and_exit_codes() {
        let dir = std::env::temp_dir().join(format!("tetrad-boot-choice-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("boot-choice");
        assert_eq!(read_choice(&file), BootChoice::Tetrad);
        write_choice(&file, BootChoice::Stock).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "coros\n");
        assert_eq!(read_choice(&file), BootChoice::Stock);
        write_choice(&file, BootChoice::Tetrad).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "tetrad\n");
        assert_eq!(read_choice(&file), BootChoice::Tetrad);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::write(&file, "CorOS 4.1.0\n").unwrap();
        assert_eq!(read_choice(&file), BootChoice::Tetrad);
        fs::remove_dir_all(&dir).unwrap();
        let codes = BootChoice::ALL.map(exit_code);
        assert_ne!(codes[0], codes[1]);
        assert!(
            codes
                .iter()
                .all(|&code| code > 2 && code < 126 && code != 101)
        );
    }
}
