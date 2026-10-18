//! `scripts/install.sh` (docs/install.md) against a fake unit in a temp dir: install, update,
//! uninstall, and the move of a zen-rs installation (`/media/p4/zenui-rs`, the old working name)
//! to `/media/p4/tetrad`. A fake `ssh` runs each remote command with the unit's absolute paths
//! moved under the fake root, and a stand-in answers `tetrad run --check`.
//!
//! S80drvinit: the CorOS file at `$QC_CORPUS_DIR/rootfs/etc/init.d/S80drvinit` when it is there
//! (then the md5 pins in the scripts must match it), else a synthetic file with the stock UI
//! line and pins made for it. `TD_REMOTE_PATH` (a directory of BusyBox applet wrappers, for
//! example the unit's BusyBox 1.23.1 under qemu-arm) runs the remote commands with those tools.
#![cfg(target_os = "linux")]

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use md5::{Digest, Md5};

const REPO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
/// `zr-launcher.sh` of zen-rs 0.1.0, the launcher that a zen-rs unit has.
const OLD_LAUNCHER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/zen-rs/zr-launcher.sh"
);
const OLD_HOOKED_PIN: &str = "d6bafded21b2192dc18927f575fbb742";

/// Runs the remote command on the fake unit: the unit's paths go under `TD_ROOT`, and the
/// uploaded ARM binary is the `tetrad` stand-in. `ssh -O exit` (end of the shared
/// connection) does nothing.
const SSH: &str = r##"#!/bin/sh
for a in "$@"; do [ "$a" = -O ] && exit 0; cmd=$a; done
cmd=$(printf '%s\n' "$cmd" | sed \
    -e 's#\(^\|[^A-Za-z0-9_.-]\)\(/media/p4\|/etc/init\.d\|/opt/neuraldsp\|/proc/mounts\)#\1'"$TD_ROOT"'\2#g' \
    -e 's#[^ ]*/bin/tetrad\.part run#'"$TD_TEST"'/tetrad run#g')
export PATH="$TD_TEST/unit:${TD_REMOTE_PATH:+$TD_REMOTE_PATH:}$PATH"
exec sh -c "$cmd"
"##;

fn md5(bytes: &[u8]) -> String {
    Md5::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The output of `command` after sourcing `launcher`, with `input` on stdin (host `sh`).
fn sourced(launcher: &Path, command: &str, input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(format!(". \"$1\"; {command}"))
        .args([Path::new("sh"), launcher])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{command}: {output:?}");
    output.stdout
}

/// Replaces the 32-hex value of each `NAME=` line in `text`.
fn pin(text: &str, pins: &[(&str, &str)]) -> String {
    text.lines()
        .map(|line| {
            match pins
                .iter()
                .find(|(name, _)| line.starts_with(&format!("{name}=")))
            {
                Some((name, md5)) => format!("{name}={md5}{}", &line[name.len() + 33..]),
                None => line.to_owned(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

struct Unit {
    base: PathBuf,
    stock: Vec<u8>,
    hooked_md5: String,
    old_hooked: Vec<u8>,
}

impl Unit {
    fn new(name: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("tetrad-install-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let launchers = Path::new(REPO).join("scripts/launcher");
        let new_launcher = launchers.join("tetrad-launcher.sh");
        let corpus = std::env::var_os("QC_CORPUS_DIR")
            .map(|dir| PathBuf::from(dir).join("rootfs/etc/init.d/S80drvinit"));
        let real = corpus.and_then(|path| fs::read(path).ok());
        let stock = real.clone().unwrap_or_else(|| {
            let line = sourced(&new_launcher, "printf '%s\\n' \"$TD_LINE_STOCK\"", b"");
            [
                b"#!/bin/sh\n# synthetic S80drvinit\n".as_slice(),
                &line,
                b"exit 0\n",
            ]
            .concat()
        });
        let hooked = sourced(&new_launcher, "td_hook_patch", &stock);
        let old_hooked = sourced(Path::new(OLD_LAUNCHER), "zr_hook_patch", &stock);
        let (stock_md5, hooked_md5) = (md5(&stock), md5(&hooked));
        if real.is_some() {
            let text = fs::read_to_string(&new_launcher).unwrap();
            assert!(text.contains(&format!("\nTD_INIT_STOCK_MD5={stock_md5}\n")));
            assert!(text.contains(&format!("\nTD_INIT_HOOKED_MD5={hooked_md5}\n")));
            assert_eq!(md5(&old_hooked), OLD_HOOKED_PIN);
        }

        // The computer side: a release layout (scripts/install.sh, launcher/, bin/tetrad).
        let release = base.join("release");
        fs::create_dir_all(release.join("scripts")).unwrap();
        fs::create_dir_all(release.join("launcher")).unwrap();
        let install = fs::read_to_string(Path::new(REPO).join("scripts/install.sh")).unwrap();
        assert!(install.contains(&format!("\nOLD_HOOKED_MD5={OLD_HOOKED_PIN}\n")));
        let install = pin(&install, &[("OLD_HOOKED_MD5", &md5(&old_hooked))]);
        fs::write(release.join("scripts/install.sh"), install).unwrap();
        for script in ["tetrad-launch.sh", "tetrad-launcher.sh", "tetrad-update.sh"] {
            let text = fs::read_to_string(launchers.join(script)).unwrap();
            let text = pin(
                &text,
                &[
                    ("TD_INIT_STOCK_MD5", &stock_md5),
                    ("TD_INIT_HOOKED_MD5", &hooked_md5),
                ],
            );
            fs::write(release.join("launcher").join(script), text).unwrap();
        }

        let unit = Self {
            base,
            stock,
            hooked_md5,
            old_hooked,
        };
        unit.binary(b"build 1");
        unit.script("test/bin/ssh", SSH);
        unit.script("test/unit/id", "#!/bin/sh\necho 0\n");
        unit.script("test/unit/pidof", "#!/bin/sh\nexit 1\n");
        unit.script("test/unit/usleep", "#!/bin/sh\n");
        unit.script(
            "test/tetrad",
            "#!/bin/sh\necho \"tetrad $*\" >>\"$TD_TEST/calls\"\n",
        );
        unit.script("root/opt/neuraldsp/ZenUI", "#!/bin/sh\n");
        unit.script(
            "root/opt/neuraldsp/zenreboot",
            "#!/bin/sh\necho reboot >>\"$TD_TEST/calls\"\n",
        );
        fs::create_dir_all(unit.at("root/media/p4")).unwrap();
        unit.write(
            "root/proc/mounts",
            &format!(
                "/dev/mmcblk0p4 {} ext4 rw 0 0\n",
                unit.at("root/media/p4").display()
            ),
        );
        unit.write_bytes("root/etc/init.d/S80drvinit", &unit.stock);
        unit
    }

    fn at(&self, path: &str) -> PathBuf {
        self.base.join(path)
    }

    fn write_bytes(&self, path: &str, bytes: &[u8]) {
        let path = self.at(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }

    fn write(&self, path: &str, text: &str) {
        self.write_bytes(path, text.as_bytes());
    }

    fn read(&self, path: &str) -> Option<String> {
        fs::read_to_string(self.at(path)).ok()
    }

    fn script(&self, path: &str, text: &str) {
        self.write(path, text);
        fs::set_permissions(self.at(path), fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The release's `bin/tetrad`: an ARM ELF header (install.sh checks it) and `tag`.
    fn binary(&self, tag: &[u8]) -> String {
        let mut bytes = b"\x7fELF\x01\x01\x01".to_vec();
        bytes.resize(18, 0);
        bytes.extend_from_slice(&[0x28, 0x00]);
        bytes.extend_from_slice(tag);
        self.write_bytes("release/bin/tetrad", &bytes);
        md5(&bytes)
    }

    fn init_md5(&self) -> String {
        md5(&fs::read(self.at("root/etc/init.d/S80drvinit")).unwrap())
    }

    fn install_sh(&self, args: &[&str]) -> Output {
        let _ = fs::remove_file(self.at("test/calls"));
        let path = format!(
            "{}:{}",
            self.at("test/bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = Command::new("sh")
            .arg(self.at("release/scripts/install.sh"))
            .args(["--host", "qc"])
            .args(args)
            .env("PATH", path)
            .env("TD_ROOT", self.at("root"))
            .env("ZR_ROOT", self.at("root"))
            .env("TD_TEST", self.at("test"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "install.sh {args:?}:\n{text}");
        output
    }

    fn calls(&self) -> String {
        self.read("test/calls").unwrap_or_default()
    }

    /// A unit with zen-rs armed: its folder with launcher state, and its hook in S80drvinit.
    fn zen_rs(&self) {
        let old = "root/media/p4/zenui-rs";
        self.script(&format!("{old}/bin/zenui-rs"), "old binary");
        self.write(&format!("{old}/bin/zr-stage.sh"), "#!/bin/sh\n");
        for script in ["zr-launch.sh", "zr-update.sh"] {
            self.write(&format!("{old}/launcher/{script}"), "#!/bin/sh\n");
        }
        let launcher = fs::read_to_string(OLD_LAUNCHER).unwrap();
        let launcher = pin(
            &launcher,
            &[
                ("ZR_INIT_STOCK_MD5", &md5(&self.stock)),
                ("ZR_INIT_HOOKED_MD5", &md5(&self.old_hooked)),
            ],
        );
        self.write(&format!("{old}/launcher/zr-launcher.sh"), &launcher);
        for (file, text) in [
            ("select", "0123456789abcdef0123456789abcdef\n"),
            ("pending", "1\n"),
            ("boot-choice", "coros\n"),
            ("no-chooser", "turned off by zr-launcher.sh\n"),
            ("launch.log", "2026-10-16 12:00:00 zenui-rs exit 0\n"),
        ] {
            self.write(&format!("{old}/launcher/{file}"), text);
        }
        self.write(&format!("{old}/logs/run-1/summary.log"), "a bring-up run\n");
        self.write_bytes("root/etc/init.d/S80drvinit", &self.old_hooked);
    }
}

impl Drop for Unit {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

const D: &str = "root/media/p4/tetrad";

/// A stock unit: install uploads, checks (`run --check`), hooks, arms and restarts; update
/// arms the new binary and keeps the hook; uninstall --yes gives back the stock S80drvinit
/// byte for byte and deletes the folder.
#[test]
fn install_update_and_uninstall_on_a_stock_unit() {
    let unit = Unit::new("stock");
    unit.install_sh(&["install"]);
    assert_eq!(unit.init_md5(), unit.hooked_md5);
    let first = md5(&fs::read(unit.at("release/bin/tetrad")).unwrap());
    assert_eq!(
        unit.read(&format!("{D}/launcher/select")).unwrap().trim(),
        first
    );
    for script in ["tetrad-launch.sh", "tetrad-launcher.sh", "tetrad-update.sh"] {
        assert!(
            unit.at(D).join("launcher").join(script).is_file(),
            "{script}"
        );
    }
    assert_eq!(unit.calls(), "tetrad run --check\nreboot\n");

    let second = unit.binary(b"build 2");
    unit.install_sh(&["update"]);
    assert_eq!(
        unit.read(&format!("{D}/launcher/select")).unwrap().trim(),
        second
    );
    assert_eq!(unit.init_md5(), unit.hooked_md5);

    unit.install_sh(&["--yes", "uninstall"]);
    assert_eq!(
        fs::read(unit.at("root/etc/init.d/S80drvinit")).unwrap(),
        unit.stock
    );
    assert!(!unit.at(D).exists());
}

/// A zen-rs unit: install and update both remove the zen-rs hook with the zen-rs launcher,
/// move the folder (logs, boot choice, selector state and launch.log stay; the old binary,
/// scripts, `select` and `pending` go), then upload, hook and arm Tetrad.
#[test]
fn install_and_update_move_a_zen_rs_unit_to_tetrad() {
    for command in ["install", "update"] {
        let unit = Unit::new(command);
        unit.zen_rs();
        let output = unit.install_sh(&[command]);
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(text.contains("S80drvinit md5"), "{text}");
        assert!(!unit.at("root/media/p4/zenui-rs").exists());
        assert_eq!(unit.init_md5(), unit.hooked_md5, "{command}");
        let binary = md5(&fs::read(unit.at("release/bin/tetrad")).unwrap());
        assert_eq!(
            unit.read(&format!("{D}/launcher/select")).unwrap().trim(),
            binary
        );
        assert_eq!(
            unit.read(&format!("{D}/launcher/boot-choice")).unwrap(),
            "coros\n"
        );
        assert!(unit.at(D).join("launcher/no-chooser").exists());
        assert!(
            unit.read(&format!("{D}/launcher/launch.log"))
                .unwrap()
                .starts_with("2026-10-16")
        );
        assert!(unit.at(D).join("logs/run-1/summary.log").exists());
        for gone in [
            "bin/zenui-rs",
            "bin/zr-stage.sh",
            "launcher/zr-launch.sh",
            "launcher/zr-launcher.sh",
            "launcher/zr-update.sh",
            "launcher/pending",
        ] {
            assert!(!unit.at(D).join(gone).exists(), "{command}: {gone}");
        }
        assert_eq!(unit.calls(), "tetrad run --check\nreboot\n");
    }
}
