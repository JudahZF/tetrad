//! The boot launcher (`scripts/launcher/`, docs/launcher.md) under a temp-dir fake root: which UI
//! each boot starts, the failed-start limit, the firmware hand-over, arming, the boot selector and
//! the S80drvinit hook. Stock ZenUI and tetrad are small shell stand-ins that record their calls.
//! `TD_LAUNCHER_TEST_SH` runs the scripts under another shell (for example the QC's BusyBox ash
//! under qemu-arm); the default is `sh`.
#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

#[cfg(feature = "dev")]
use tetrad_app::boot_select::DEV_EXIT;
use tetrad_app::boot_select::{BootChoice, COUNTDOWN, STOCK_EXIT, TETRAD_EXIT, write_choice};
use tetrad_app::device::boot_selector::NO_CHOOSER_FILE;
use tetrad_app::device::linux::WRONG_FIRMWARE_EXIT;

const SCRIPTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../scripts/launcher");

/// Stock ZenUI: records one `stock` line with the serial path init passes.
const STOCK: &str =
    "#!/bin/sh\necho \"stock QC_SERIAL_NUMBER=$QC_SERIAL_NUMBER\" >>\"$TD_TEST/calls\"\n";
/// tetrad: records its arguments. `run`: `confirm` removes the `--started` file (start-up
/// done); `exit` holds the exit code. `select`: exits the code in `select`, or with `hang`
/// sleeps (as the selector's own process) until the launcher kills it.
const TETRAD: &str = "#!/bin/sh\necho \"tetrad $*\" >>\"$TD_TEST/calls\"\n\
    if [ \"$1\" = select ]; then\n\
    [ -f \"$TD_TEST/hang\" ] && exec sleep 30\nexit $(cat \"$TD_TEST/select\")\nfi\n\
    [ -f \"$TD_TEST/confirm\" ] && rm -f \"$3\"\nexit $(cat \"$TD_TEST/exit\")\n";
/// The launcher's kill limit for the selector in these tests (seconds).
const SELECT_LIMIT: &str = "1";
/// The launcher's `tetrad run --check` on a first boot, as the stand-in records it.
const CHECK_CALL: &str = "tetrad run --check";
/// `/opt/neuraldsp/zenreboot`: records `reboot`.
const REBOOT: &str = "#!/bin/sh\necho reboot >>\"$TD_TEST/calls\"\n";
/// The first-boot files that stock ZenUI makes (`TD_FACTORY`, `TD_FIRST_BOOT_DONE`).
const FIRST_BOOT_FILES: [&str; 4] = [
    "factory.db",
    "factory_ir_entities.pb",
    "searchIndexFactory",
    "captureConversionFinished",
];

struct Fake {
    root: PathBuf,
    /// Control files and the call record of the stand-ins.
    test: PathBuf,
    /// The first-boot watcher's cap (`TD_FIRST_BOOT_CAP`, s); it polls every second.
    first_boot_cap: u32,
}

impl Fake {
    fn new(name: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("tetrad-app-launcher-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let fake = Self {
            root: base.join("root"),
            test: base.join("test"),
            first_boot_cap: 30,
        };
        for dir in [
            "opt/neuraldsp",
            "media/p4/tetrad/bin",
            "media/p4/tetrad/launcher",
            "etc/init.d",
        ] {
            fs::create_dir_all(fake.at(dir)).unwrap();
        }
        fs::create_dir_all(&fake.test).unwrap();
        fake.script("opt/neuraldsp/ZenUI", STOCK);
        fs::write(fake.test.join("reboot"), REBOOT).unwrap();
        fs::set_permissions(fake.test.join("reboot"), fs::Permissions::from_mode(0o755)).unwrap();
        fake.first_boot_files(true);
        fake.script("media/p4/tetrad/bin/tetrad", TETRAD);
        fs::copy(
            Path::new(SCRIPTS).join("tetrad-launch.sh"),
            fake.at("media/p4/tetrad/launcher/tetrad-launch.sh"),
        )
        .unwrap();
        fake.tetrad(0, true);
        fake.select(TETRAD_EXIT);
        // The tests before the boot selector run without it; the selector tests turn it on.
        fs::write(fake.launcher("no-chooser"), "").unwrap();
        fake
    }

    /// The next `tetrad select` exits `code`.
    fn select(&self, code: u8) {
        fs::write(self.test.join("select"), code.to_string()).unwrap();
    }

    /// Stock's first-boot files in /opt/neuraldsp: all there, or none (a new root).
    fn first_boot_files(&self, present: bool) {
        for name in FIRST_BOOT_FILES {
            let path = self.at("opt/neuraldsp").join(name);
            if present {
                fs::write(path, "made by stock").unwrap();
            } else {
                let _ = fs::remove_file(path);
            }
        }
    }

    /// Waits up to `limit` for `call` in the call record (a background watcher writes it).
    fn called_within(&self, call: &str, limit: Duration) -> bool {
        let start = Instant::now();
        loop {
            let calls = fs::read_to_string(self.test.join("calls")).unwrap_or_default();
            if calls.lines().any(|line| line == call) {
                return true;
            }
            if start.elapsed() > limit {
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn at(&self, path: &str) -> PathBuf {
        self.root.join(path)
    }

    fn launcher(&self, file: &str) -> PathBuf {
        self.at("media/p4/tetrad/launcher").join(file)
    }

    fn script(&self, path: &str, text: &str) {
        fs::write(self.at(path), text).unwrap();
        fs::set_permissions(self.at(path), fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The next tetrad run exits `code` and confirms start-up when `confirm`.
    fn tetrad(&self, code: u8, confirm: bool) {
        fs::write(self.test.join("exit"), code.to_string()).unwrap();
        let mark = self.test.join("confirm");
        if confirm {
            fs::write(mark, "").unwrap();
        } else {
            let _ = fs::remove_file(mark);
        }
    }

    fn run(&self, script: &str, args: &[&str]) -> std::process::Output {
        let shell = std::env::var("TD_LAUNCHER_TEST_SH").unwrap_or_else(|_| "sh".into());
        Command::new(shell)
            .arg(Path::new(SCRIPTS).join(script))
            .args(args)
            .env("TD_ROOT", &self.root)
            .env("TD_TEST", &self.test)
            .env("QC_SERIAL_NUMBER", "/media/p4/qc_sn")
            .env("TD_SELECT_LIMIT", SELECT_LIMIT)
            .env("TD_FIRST_BOOT_CAP", self.first_boot_cap.to_string())
            .env("TD_FIRST_BOOT_POLL", "1")
            .env("TD_FIRST_BOOT_SETTLE", "1")
            .env("TD_REBOOT", self.test.join("reboot"))
            .output()
            .unwrap()
    }

    fn ctl(&self, command: &str) -> String {
        let args: Vec<&str> = command.split(' ').collect();
        let output = self.run("tetrad-launcher.sh", &args);
        let text = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{command}: {text}");
        text
    }

    /// One boot: the calls the stand-ins recorded, in order.
    fn boot(&self) -> Vec<String> {
        let _ = fs::remove_file(self.test.join("calls"));
        let output = self.run("tetrad-launch.sh", &[]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        fs::read_to_string(self.test.join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn read(&self, file: &str) -> Option<String> {
        fs::read_to_string(self.launcher(file)).ok()
    }

    fn tetrad_call(&self) -> String {
        let pending = self.launcher("pending");
        format!("tetrad run --started {}", pending.display())
    }

    fn select_call(&self) -> String {
        let choice = self.launcher("boot-choice");
        format!("tetrad select --file {}", choice.display())
    }
}

impl Drop for Fake {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(self.root.parent().unwrap());
    }
}

const STOCK_CALL: &str = "stock QC_SERIAL_NUMBER=/media/p4/qc_sn";

/// Not armed: stock ZenUI with init's environment, tetrad never runs. Armed: tetrad runs,
/// confirms start-up and leaves no unconfirmed start. Disarmed: stock again.
#[test]
fn only_an_armed_launcher_starts_tetrad() {
    let fake = Fake::new("armed");
    assert_eq!(fake.boot(), [STOCK_CALL]);
    assert!(fake.ctl("arm").contains("armed tetrad md5"));
    assert_eq!(fake.boot(), [fake.tetrad_call()]);
    assert_eq!(fake.read("pending"), None);
    fake.ctl("disarm");
    assert_eq!(fake.boot(), [STOCK_CALL]);
}

/// A boot loop ends: three starts that never confirm start-up, then stock and `disabled` on
/// the fourth boot, and on every boot after it until `arm`.
#[test]
fn three_unconfirmed_starts_fall_back_to_stock() {
    let fake = Fake::new("fails");
    fake.ctl("arm");
    fake.tetrad(101, false);
    for start in 1..=3 {
        assert_eq!(fake.boot(), [fake.tetrad_call()]);
        assert_eq!(
            fake.read("pending").as_deref(),
            Some(&*format!("{start}\n"))
        );
    }
    for _ in 0..2 {
        assert_eq!(fake.boot(), [STOCK_CALL]);
    }
    let disabled = fake.read("disabled").unwrap();
    assert!(disabled.contains("3 tetrad starts in a row"), "{disabled}");
    assert!(fake.ctl("status").contains("disabled: 3 tetrad starts"));
    fake.ctl("arm");
    fake.tetrad(0, true);
    assert_eq!(fake.boot(), [fake.tetrad_call()]);
}

/// A confirmed start resets the count: two crashes, one good boot, then a crash counts 1.
#[test]
fn a_confirmed_start_resets_the_count() {
    let fake = Fake::new("reset");
    fake.ctl("arm");
    fake.tetrad(101, false);
    fake.boot();
    fake.boot();
    fake.tetrad(0, true);
    fake.boot();
    assert_eq!(fake.read("pending"), None);
    fake.tetrad(101, false);
    fake.boot();
    assert_eq!(fake.read("pending").as_deref(), Some("1\n"));
}

/// tetrad exit 4 (stock firmware not pinned, nothing opened): stock in the same boot, and
/// stock without a tetrad start on the next boot.
#[test]
fn wrong_firmware_hands_over_to_stock_in_the_same_boot() {
    let fake = Fake::new("firmware");
    fake.ctl("arm");
    fake.tetrad(WRONG_FIRMWARE_EXIT, false);
    assert_eq!(fake.boot(), [fake.tetrad_call(), STOCK_CALL.into()]);
    assert!(
        fake.read("disabled")
            .unwrap()
            .contains("not a supported CorOS build")
    );
    assert_eq!(fake.boot(), [STOCK_CALL]);
    let script = fs::read_to_string(Path::new(SCRIPTS).join("tetrad-launch.sh")).unwrap();
    assert!(script.contains(&format!("\nTD_WRONG_FIRMWARE={WRONG_FIRMWARE_EXIT}\n")));
}

/// A tetrad that is not the armed binary (a new upload without `arm`) never starts, and the
/// boot does not count as a failure.
#[test]
fn an_unselected_binary_starts_stock() {
    let fake = Fake::new("md5");
    fake.ctl("arm");
    fake.script(
        "media/p4/tetrad/bin/tetrad",
        &format!("{TETRAD}# another build\n"),
    );
    assert_eq!(fake.boot(), [STOCK_CALL]);
    assert_eq!(fake.read("pending"), None);
    assert_eq!(fake.read("disabled"), None);
}

/// The boot selector of an armed boot: Tetrad goes on to the Tetrad path (and counts a start),
/// CorOS starts stock without a count. `chooser off` and an unarmed launcher show no
/// selector. The script's exit codes and kill limit agree with `tetrad_app::boot_select`, and
/// `chooser off` reads the file that the Boot Selector switch in Settings writes.
#[test]
fn the_boot_selector_picks_the_ui() {
    let fake = Fake::new("chooser");
    fake.ctl("arm");
    assert!(fake.ctl("chooser on").contains("boot selector on"));
    assert!(
        fake.ctl("status")
            .contains("chooser:  on (saved choice: tetrad)")
    );
    assert_eq!(fake.boot(), [fake.select_call(), fake.tetrad_call()]);
    assert_eq!(fake.read("pending"), None);
    fake.tetrad(101, false);
    fake.select(STOCK_EXIT);
    assert_eq!(fake.boot(), [fake.select_call(), STOCK_CALL.into()]);
    assert_eq!(fake.read("pending"), None);
    fake.select(TETRAD_EXIT);
    assert_eq!(fake.boot(), [fake.select_call(), fake.tetrad_call()]);
    assert_eq!(fake.read("pending").as_deref(), Some("1\n"));
    fake.ctl("chooser off");
    assert!(fake.ctl("status").contains("chooser:  off"));
    assert_eq!(fake.boot(), [fake.tetrad_call()]);
    fake.ctl("chooser on");
    fs::write(fake.at(NO_CHOOSER_FILE.trim_start_matches('/')), "").unwrap();
    assert!(fake.ctl("status").contains("chooser:  off"));
    fake.ctl("chooser on");
    fake.ctl("disarm");
    assert_eq!(fake.boot(), [STOCK_CALL]);
    let script = fs::read_to_string(Path::new(SCRIPTS).join("tetrad-launch.sh")).unwrap();
    assert!(script.contains(&format!("\nTD_SELECT_STOCK={STOCK_EXIT}\n")));
    assert!(script.contains(&format!("\nTD_SELECT_TETRAD={TETRAD_EXIT}\n")));
    #[cfg(feature = "dev")]
    assert!(script.contains(&format!("\nTD_SELECT_DEV={DEV_EXIT}\n")));
    let limit: u64 = script
        .split_once("\nTD_SELECT_LIMIT=${TD_SELECT_LIMIT:-")
        .and_then(|(_, rest)| rest.split_once('}'))
        .and_then(|(limit, _)| limit.parse().ok())
        .expect("TD_SELECT_LIMIT default");
    assert!(limit >= COUNTDOWN.as_secs() + 2, "{limit}");
}

/// A selector that fails (an unknown exit, a crash) or hangs (killed at the limit) gives the
/// saved choice without a screen: the file `write_choice` writes, Tetrad without one.
#[test]
fn a_failed_selector_starts_the_saved_choice() {
    let fake = Fake::new("chooser-fail");
    fake.ctl("arm");
    fake.ctl("chooser on");
    fake.select(1);
    assert_eq!(fake.boot(), [fake.select_call(), fake.tetrad_call()]);
    let file = fake.launcher("boot-choice");
    write_choice(&file, BootChoice::Stock).unwrap();
    assert!(fake.ctl("status").contains("(saved choice: coros)"));
    fake.select(101);
    assert_eq!(fake.boot(), [fake.select_call(), STOCK_CALL.into()]);
    fs::write(fake.test.join("hang"), "").unwrap();
    let start = Instant::now();
    assert_eq!(fake.boot(), [fake.select_call(), STOCK_CALL.into()]);
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "{:?}",
        start.elapsed()
    );
    write_choice(&file, BootChoice::Tetrad).unwrap();
    assert_eq!(fake.boot(), [fake.select_call(), fake.tetrad_call()]);
    let log = fake.read("launch.log").unwrap();
    assert!(
        log.contains("boot selector exit 137; the saved choice is Tetrad"),
        "{log}"
    );
}

/// The first boot of a new root (after a CorOS update) has no factory files: stock starts at
/// once, with no selector and no start count, once `run --check` accepts the build. The
/// watcher restarts the unit only after stock made the files and its marker, and not while a
/// CorOS install runs. The next boot is the normal armed boot.
#[test]
fn the_first_boot_after_an_update_runs_stock_then_restarts() {
    let fake = Fake::new("first-boot");
    fake.ctl("arm");
    fake.ctl("chooser on");
    fake.first_boot_files(false);
    assert_eq!(fake.boot(), [CHECK_CALL, STOCK_CALL]);
    assert_eq!(fake.read("pending"), None);
    let log = fake.read("launch.log").unwrap();
    assert!(
        log.contains("first boot after an update: stock builds the factory files"),
        "{log}"
    );
    let reboot = "reboot";
    assert!(!fake.called_within(reboot, Duration::from_secs(2)));
    let status = fake.at("etc/updater_status");
    fs::write(&status, "UPDATER_STEP_3\n").unwrap();
    fake.first_boot_files(true);
    assert!(!fake.called_within(reboot, Duration::from_secs(4)));
    fs::write(&status, "UPDATER_COMPLETE\n").unwrap();
    assert!(fake.called_within(reboot, Duration::from_secs(20)));
    let log = fake.read("launch.log").unwrap();
    assert!(
        log.contains("first boot done: restart; the next boot starts Tetrad"),
        "{log}"
    );
    assert_eq!(fake.boot(), [fake.select_call(), fake.tetrad_call()]);
}

/// A first boot on a build that Tetrad does not support (`run --check` exit 4) takes the
/// exit-4 rule at once: `disabled`, stock, no watcher. A watcher whose files never come gives
/// up after its cap without a restart.
#[test]
fn the_first_boot_restarts_only_for_a_supported_build_and_within_its_cap() {
    let mut fake = Fake::new("first-boot-cap");
    fake.ctl("arm");
    fake.first_boot_files(false);
    fake.tetrad(WRONG_FIRMWARE_EXIT, false);
    assert_eq!(fake.boot(), [CHECK_CALL, STOCK_CALL]);
    assert!(
        fake.read("disabled")
            .unwrap()
            .contains("not a supported CorOS build")
    );
    fake.ctl("arm");
    fake.tetrad(0, true);
    fake.first_boot_cap = 2;
    assert_eq!(fake.boot(), [CHECK_CALL, STOCK_CALL]);
    let start = Instant::now();
    while !fake.read("launch.log").unwrap().contains("no restart") {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the watcher never gave up"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let log = fake.read("launch.log").unwrap();
    assert!(
        log.contains("first boot: no factory files after 2 s; no restart"),
        "{log}"
    );
    assert!(!fake.called_within("reboot", Duration::from_secs(1)));
}

/// A hook line pair as `td_hook_patch` writes it into S80drvinit (or the stock line), run in
/// a temp cwd with a stand-in `./ZenUI`.
fn run_init_line(fake: &Fake, filter: &str) -> String {
    let shell = std::env::var("TD_LAUNCHER_TEST_SH").unwrap_or_else(|_| "sh".into());
    let ctl = Path::new(SCRIPTS).join("tetrad-launcher.sh");
    let script = format!(
        ". {ctl}; printf '%s\\n' \"$TD_LINE_STOCK\" | {filter} >lines; . ./lines; wait",
        ctl = ctl.display()
    );
    let cwd = fake.at("opt/neuraldsp");
    let output = Command::new(shell)
        .args(["-c", &script])
        .current_dir(&cwd)
        .env("TD_TEST", &fake.test)
        .env("SYSTEM_LOG_FILE", fake.test.join("system.log"))
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    fs::read_to_string(cwd.join("lines")).unwrap()
}

/// The init hook: the patched start keeps the stock environment and starts stock ZenUI when
/// the launcher is not on p4 (this host has no /media/p4); unpatch restores the stock line.
#[test]
fn the_init_hook_falls_back_to_stock_and_reverses() {
    let fake = Fake::new("hook");
    let lines = run_init_line(&fake, "td_hook_patch");
    assert_eq!(lines.lines().count(), 2, "{lines}");
    assert!(lines.contains("TD_UI=./ZenUI; TD_LAUNCH=/media/p4/tetrad/launcher/tetrad-launch.sh"));
    let calls = fs::read_to_string(fake.test.join("calls")).unwrap();
    assert_eq!(calls, format!("{STOCK_CALL}\n"));
    let round = run_init_line(&fake, "td_hook_patch | td_hook_unpatch");
    assert!(round.starts_with("\t\tQWS_MOUSE_PROTO=tslib "), "{round}");
    assert!(round.ends_with(" ./ZenUI 2>&1 | tee -a \"$SYSTEM_LOG_FILE\" &\n"));
}

/// The hook commands change only a known S80drvinit: an unknown file is refused unchanged.
#[test]
fn the_hook_refuses_an_unknown_init_script() {
    let fake = Fake::new("unknown");
    let init = fake.at("etc/init.d/S80drvinit");
    fs::write(&init, "#!/bin/sh\n# not CorOS 4.1.0\n").unwrap();
    for command in ["install-hook", "remove-hook"] {
        let output = fake.run("tetrad-launcher.sh", &[command]);
        assert!(!output.status.success());
        assert_eq!(
            fs::read_to_string(&init).unwrap(),
            "#!/bin/sh\n# not CorOS 4.1.0\n"
        );
    }
    assert!(!fake.at("etc/.S80drvinit.tetrad-new").exists());
}

/// With the CorOS 4.1.0 S80drvinit in `$QC_CORPUS_DIR/rootfs`
/// (docs/development.md, "Corpus tests"): install and remove reach the pinned md5s, and
/// remove gives back the stock bytes. Skipped without it.
#[test]
fn the_hook_round_trips_the_coros_4_1_0_init_script() {
    let Some(dir) = std::env::var_os("QC_CORPUS_DIR") else {
        eprintln!("skipped: set QC_CORPUS_DIR");
        return;
    };
    let bytes = fs::read(Path::new(&dir).join("rootfs/etc/init.d/S80drvinit")).unwrap();
    let fake = Fake::new("coros");
    let init = fake.at("etc/init.d/S80drvinit");
    fs::write(&init, &bytes).unwrap();
    assert!(fake.ctl("status").contains("hook:     stock"));
    fake.ctl("install-hook");
    assert!(fake.ctl("status").contains("hook:     hooked"));
    assert_eq!(
        fs::metadata(&init).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert!(fake.ctl("install-hook").contains("already installed"));
    fake.ctl("remove-hook");
    assert_eq!(fs::read(&init).unwrap(), bytes);
}

/// The Tetrad dev entry of a dev build's selector starts `tetrad run --dev` and counts a start;
/// the next boot with the plain Tetrad entry runs without `--dev`.
#[cfg(feature = "dev")]
#[test]
fn the_dev_boot_entry_starts_tetrad_with_dev() {
    let fake = Fake::new("dev-entry");
    fake.ctl("arm");
    fake.ctl("chooser on");
    fake.tetrad(101, false);
    fake.select(DEV_EXIT);
    assert_eq!(
        fake.boot(),
        [fake.select_call(), format!("{} --dev", fake.tetrad_call())]
    );
    assert_eq!(fake.read("pending").as_deref(), Some("1\n"));
    fake.select(TETRAD_EXIT);
    assert_eq!(fake.boot(), [fake.select_call(), fake.tetrad_call()]);
}
