# Changelog

All notable changes to Tetrad are in this file. The version numbers follow
[Semantic Versioning](https://semver.org/).

## Unreleased

### Added

- Themes (Settings → System → Appearance): pick the UI colours. The built-in themes are Stock
  (the default, pixel for pixel the old look), Stage High Contrast, Slate and Ember. Tetrad
  also reads theme files from `/media/p4/tetrad/themes/`, and extensions can add themes in
  their manifest. Themes are dark only and must pass contrast checks. Block colours, scene
  colours, LEDs, the boot menu and Recovery do not change (docs/themes.md).
- Theme-only extensions: a manifest with `themes` and no capabilities, settings or commands
  packs without a module, and Tetrad runs nothing for it (docs/extensions.md#ship-themes).
- Remote (Settings → System → Remote): a web page on the unit to add IRs from a browser on the
  same Wi-Fi, behind a 4-digit PIN that the page shows. The browser converts each WAV or AIFF to
  the stock IR format (mono, 48 kHz, first 1,024 samples); the unit imports it into the IR
  saving destination as Cortex Control does. The on/off setting is in
  `/media/p4/tetrad/remote.json` (docs/remote.md).
- The application can show a toast (`UiSnapshot::toast`); Remote shows "IR added" with it.
- Tetrad over USB (docs/usb.md): the `tetrad-usb` tool shows the Tetrad version, saves a
  screenshot, prints the recent notices and sends a signed Tetrad release to the unit over the
  USB cable. UPDATE TETRAD on the unit then installs it. The link shares the HID interface with
  Cortex Control.

## [0.1.0-alpha.4](https://github.com/JudahZF/tetrad/releases/tag/v0.1.0-alpha.4) (2026-10-22)

### Added

- Tetrad Updates (Settings → System → Tetrad Updates): CHECK TETRAD finds the newest Tetrad
  release on GitHub, and UPDATE TETRAD downloads it, checks the Ed25519 signature of its
  manifest, the SHA-256 values and the CorOS support, arms it and restarts the unit. CorOS does
  not change. The page follows CorOS Updates in Settings → System (docs/updates.md).
- Releases publish `tetrad-update.json`, its signature (secret `TETRAD_RELEASE_KEY`) and the
  three launcher scripts for Tetrad Updates. The release workflow stops when the tag is not the
  version in `Cargo.toml` or the secret is not the pinned release key.

### Changed

- The stock Updates page is now CorOS Updates, and its buttons are DOWNLOAD COROS and UPDATE
  COROS, so they differ from UPDATE TETRAD.

## [0.1.0-alpha.3](https://github.com/JudahZF/tetrad/releases/tag/v0.1.0-alpha.3) (2026-10-20)

### Added

- Mappings (Grid ⋮ → Mappings, replacing Preset MIDI Out): one page for what footswitches,
  expression pedals, MIDI CCs and notes, preset loads and extensions do. Actions: preset and
  scene steps, tuner, tap tempo, footswitch modes, Gig View, Looper buttons, block bypass and
  parameters, and Send MIDI. THIS PRESET mappings are saved in the preset; GLOBAL mappings in
  `/media/p4/tetrad/mappings.json`. Stomp assignments, expression links and Preset MIDI Out
  lists show as rows and stay stock data. LEARN takes the next free MIDI message
  (docs/mappings.md).
- Extensions (Settings → System → Extensions): TypeScript extensions in a confined runner
  process, installed from Ed25519-signed GitHub repositories with trust on first use. The
  default repository is JudahZF/tetrad-extensions. An SDK (`tetrad.d.ts`) and the `tetrad-ext`
  command build, package, sign and stage extensions (docs/extensions.md).
- MIDI Mapper 0.2.0, the first extension: reserves a second MIDI channel and offers its CCs,
  notes and program changes as Mappings triggers.

### Changed

- README: written for players, with images of the UI. The build, test, corpus and project
  layout sections moved to docs/development.md.
- `render_owned readme <dir>` writes the README images (`docs/images/`) as PNG. The PNG encoder
  moved from `tetrad-app` to `tetrad-ui` (`tetrad_ui::png`).
- The release archive has the README images (`docs/images/`).

### Fixed

- `render_owned`: the `gig`, `gigpreset`, `stomp`, `account`, `picker` and `draft` views rendered
  the Grid or the scene menu. They used controls that do nothing on that screen. The `block`
  view released its press before the long press, and now shows the block menu.

## [0.1.0-alpha.2](https://github.com/JudahZF/tetrad/releases/tag/v0.1.0-alpha.2) (2026-10-18)

### Added

- Wi-Fi recovery: when the internet probe finds the internet lost on a link that had it, Tetrad
  renews the DHCP lease after 60 s, sends `REASSOCIATE` after 120 s, and resets the interface
  after 180 s and then every 10 min, until a probe connects (docs/architecture.md). Before, a
  link that stayed associated but passed no traffic needed a restart.
- The DHCP log line has the lease time.
- The `qcdsp` submodule accepts CorOS 4.1.0 and 4.1.1 for custom blocks (`qc-deploy --pin` now takes
  only the two per-unit kernel hashes).

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
