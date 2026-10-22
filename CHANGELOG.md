# Changelog

All notable changes to Tetrad are in this file. The version numbers follow
[Semantic Versioning](https://semver.org/).

## Unreleased

### Added

- DSP blocks in extensions (experimental): a package can bring qcdsp graphs. Tetrad validates
  each graph, compiles it on the unit against the stock Volume framework of the unit's own
  firmware and deploys it like a stock model, on any DSP core. The blocks show in the block
  list of their category and on the extension's page with their CPU. A package never brings DSP
  machine code (docs/extensions.md, "DSP blocks").
- Presets with DSP blocks: a save writes the package's stock placeholder in the slot (default:
  Gain at 0 dB) and a record in the Tetrad field 7002. Stock CorOS loads and plays the
  placeholder; Tetrad puts the DSP block back when it is installed.
- Extension API 2: `blocks` in the manifest, the `blocks` capability, `ctx.blocks.provide` and
  `ctx.blocks.builder()` to build a block from an asset file, package format 2 (up to 4 MiB),
  a 32 MiB JavaScript heap. `tetrad-ext init --blocks`, `check` (block CPU and memory) and
  `preview` (run a block on a WAV file).
- Neural Amp Modeler 0.1.0: plays NAM LSTM models from `/media/p4/tetrad/assets/tetrad.nam/`
  as Guitar Amplifier blocks. Its graph matches NeuralAmpModelerCore within 2e-3.
- qcdsp IR 2: `div`, `recip`, `tanh`, `sigmoid` and mono blocks.

### Changed

- qcdsp's CPU estimate counts instructions (one per cycle) instead of 16-bit parcels; the
  catalogue figure still doubles it.

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
