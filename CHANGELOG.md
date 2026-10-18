# Changelog

All notable changes to Tetrad are in this file. The version numbers follow
[Semantic Versioning](https://semver.org/).

## Unreleased

### Changed

- `scripts/bringup/tetrad-host.sh`: `TD_SETLIST` selects the setlist that `presets` copies from
  (default "Tetrad Test").
- CI runs rustdoc with `-D warnings` and caches the Cargo build (`Swatinem/rust-cache`).
- README: the corpus tests of the `qcdsp` submodule use the same `QC_CORPUS_DIR` layout. The
  README and docs/install.md link to the GitHub releases.

### Fixed

- `tetrad bringup` takes the process name `ZenUI` before it hashes the stock firmware, as
  `tetrad run` does. Before, the hash took about 6 s on the unit and ZenWatchdog restarted the
  unit during every bring-up stage.
- Rustdoc warnings: links from public docs to private items, and a string that rustdoc read as
  an HTML tag.

## [0.1.0-alpha.1](https://github.com/JudahZF/tetrad/releases/tag/v0.1.0-alpha.1) (2026-10-18)

The first public alpha. Tested on one original (non-ESS) Quad Cortex with CorOS 4.1.0 and 4.1.1.

### Added

- `tetrad run`: the device program. It boots the stock SHARC firmware, deploys presets in the
  stock order and runs the touch UI, footswitches, expression pedals, MIDI, Cortex Control, Wi-Fi
  and cloud services.
- A block driver for every stock model except the Looper. The drivers read their tables from
  the stock files on the unit.
- The boot launcher with a one-line boot hook, a start counter that falls back to stock CorOS
  after three failed starts, and `tetrad-launcher.sh` commands to arm, disarm and remove it.
- The boot selector: CorOS or Tetrad on the screen at each boot, with a saved default.
- On-device CorOS updates that keep the boot hook and the SSH keys (`tetrad-update.sh`), and the
  first-boot rule for a new CorOS root.
- Firmware profiles for CorOS 4.1.0 and 4.1.1.
- Five owned SHARC DSP blocks (experimental, bring-up mode only).
- The `tetrad-cli` command-line tool for offline inspection of local files.
- `scripts/install.sh`: install, update, status, disable, enable and uninstall over root SSH,
  and `scripts/qc-ssh-proxy.py` for the unit's OpenSSH 6.7.
- Host test suite with synthetic data, and the device build with the public toolchain in
  `qc-dev` ([tetrad-toolchain](https://github.com/JudahZF/tetrad-toolchain): Rust, zig and
  cargo-zigbuild, glibc 2.18 symbols).
- GitHub workflows: host checks (`ci.yml`) and the release build (`release.yml`).

### Changed

- The project name is Tetrad. The old working name was zen-rs: the crates are now `tetrad-*`,
  the device binary `tetrad`, the folder on the unit `/media/p4/tetrad` and the launcher
  scripts `tetrad-*.sh`. The UI process keeps the stock name `ZenUI` (see docs/launcher.md).
- `install` and `update` move a zen-rs installation to Tetrad (docs/install.md, "Migrating from
  zen-rs").
- The names that Tetrad writes on the unit: the `zenui.db` table `tetrad_capture_quarantine`,
  the capture copy suffixes `.tetrad-local` and `.tetrad-converted`, and the temporary marker
  `.tetrad-tmp-`. Tetrad renames the zen-rs names at its first start.
