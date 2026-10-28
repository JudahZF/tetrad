# Develop Tetrad

This guide is for people who build, test or change Tetrad. To install Tetrad on a unit, see
[install.md](install.md).

## Build

Requirements: Rust 1.95 or later, and the git submodules.

```sh
git clone --recurse-submodules https://github.com/JudahZF/tetrad.git
cd tetrad
```

The submodules are:

- `qcdsp/` ([JudahZF/qcdsp](https://github.com/JudahZF/qcdsp)): the `qc-format`, `qc-asm` and
  `qcdsp` crates. Cargo uses them as path dependencies.
- `qc-dev/` ([JudahZF/tetrad-toolchain](https://github.com/JudahZF/tetrad-toolchain)): the public ARM toolchain (Nix
  flake and build scripts) for the device build.

There are two builds:

| Build | Command | Output | Use |
| --- | --- | --- | --- |
| Host | `cargo build` | `target/{debug,release}/tetrad`, for your computer's CPU | Tests, the offline tools, `render_owned` |
| Device | `nix develop path:./qc-dev -c ./qc-dev/build-arm.sh . dist` | `dist/tetrad`, 32-bit ARM | The unit: `scripts/install.sh` |

Only the device build runs on the unit. `cargo build --release` gives a host binary
(x86_64 or arm64, larger than the release binary). `scripts/install.sh` refuses it with
"is not an ARM Linux binary". To compare your build with a release, use the device build: it is
the command that the release workflow runs.

### Host build and tests

```sh
cargo build --locked --workspace
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

The default tests use synthetic data only. Tests marked `#[ignore]` read stock files from a local
copy (see [Corpus tests](#corpus-tests)) or are slow timing benches. They do not run by default.

`TD_LAUNCHER_TEST_SH` runs the boot launcher tests with another shell, and `TD_REMOTE_PATH` runs
the remote side of the `scripts/install.sh` tests with other tools: for example the unit's
BusyBox 1.23.1 under `qemu-arm`.

### Corpus tests

The corpus tests read stock files from your own unit. They never copy these files into the
repository. Set `QC_CORPUS_DIR` to a folder with this layout. A test reads only the files that it
needs. Without `QC_CORPUS_DIR` it prints a note and passes.

| Path in `QC_CORPUS_DIR` | Content |
| --- | --- |
| `rootfs/` | A copy of the unit's root file system (CorOS 4.1.0): `opt/neuraldsp/ZenUI`, `opt/neuraldsp/sharc_binaries/`, `etc/init.d/S80drvinit` and so on |
| `rootfs/media/p4/` | A copy of the user data partition, with `qc_sn`, `zenui.db` and the settings files |
| `plain/` | Decrypted stock files: `ModelRepo.xml`, `moduleReport.xml`, `blobs/core1/*.raw`, `ZPUKernel_Core1.ldr`, `ZPUKernel_Core2.ldr` |
| `presets/` | A folder tree of stock `.pb` presets |

```sh
QC_CORPUS_DIR=/path/to/corpus cargo test --locked --workspace -- --include-ignored
```

The `tetrad-blocks` decode tests also need `QC_BLOBDIS`, the path of a `blobdis` binary (not in
this repository).

The corpus tests of the `qcdsp` submodule use the same `QC_CORPUS_DIR` folder. They read the
decrypted files in `plain/`: `blobs/core1/*.raw`, `ModelRepo.xml`, `moduleReport.xml` and, if it
is there, `kernel_call_map.json`. They read the core 2-4 diff files from
`plain/blobs/core{2,3,4}/diffs.blob`, or else from `rootfs/opt/neuraldsp/blobs/`. Some of these
tests also need `QC_BLOBDIS` and `SELACHE` (see [qcdsp/README.md](../qcdsp/README.md)).

```sh
cd qcdsp && QC_CORPUS_DIR=/path/to/corpus cargo test --locked --workspace -- --include-ignored
```

### Device (ARM) build

The device build uses the public toolchain in the `qc-dev` submodule: Rust 1.95.0, zig 0.16.0
and cargo-zigbuild 0.23.4. It needs Nix with flakes, and no files from the unit.

```sh
nix develop path:./qc-dev -c ./qc-dev/build-arm.sh . dist
```

The result is `dist/tetrad` and `dist/tetrad.sha256`. `build-arm.sh` checks the ELF file with
`qc-dev/check-elf.sh`. See [qc-dev/README.md](../qc-dev/README.md) for the details and a build
without Nix (rustup and cargo-zigbuild).

Install it from the checkout. `install.sh` finds `dist/tetrad` and `scripts/launcher/` itself
([install.md](install.md)):

```sh
scripts/install.sh --host <unit> install
```

### Dev build

`--features dev` makes a dev build: the boot selector gets a third button, Tetrad dev, and
`tetrad run --dev` writes loop and frame statistics (`perf`, every 10 s) to the log. The Remote
page also takes a Tetrad update, and gives the log ([remote.md](remote.md#update-tetrad-dev-build)).
A release build has none of this, and the release workflow does not turn the feature on.

`scripts/build-dev.sh` builds it for the unit and makes the update archive:

```sh
nix develop path:./qc-dev -c ./scripts/build-dev.sh --scripts dist-dev
```

`dist-dev/` then has `tetrad`, `tetrad.sha256` and `tetrad-update.tar.gz` (the binary, and with
`--scripts` the launcher scripts of the checkout). `qc-dev/build-arm.sh` has no option for
features (it belongs to the `qc-dev` repository), so the script puts a `cargo-zigbuild`
stand-in in front of it that adds `--features dev`; every other step is the release build. On the
host: `cargo test --locked -p tetrad-app -p tetrad-ui --features dev`.

### Your own updates

The first dev build goes onto the unit once, by hand (`scripts/install.sh update` over SSH, or
the storage card in a computer: copy `bin/tetrad` and the scripts to `tetrad/` on partition 4 and
run `tetrad-launcher.sh arm`, which writes the md5 of the binary into `launcher/select`). After
that, `tetrad-update.tar.gz` goes to the unit over Wi-Fi: with the Remote page
([remote.md](remote.md#update-tetrad-dev-build)), or `scripts/dev-update.sh push`. Then touch
UPDATE TETRAD on the unit.

For scripts, a dev build gives its PIN to a key that you add once with `dev-update.sh
authorize` ([remote.md](remote.md#without-reading-the-pin-a-key)); then `key:KEYFILE` takes the
place of the PIN in `push`, `install`, `log` and `status`.

Over USB, `tetrad-usb install` takes only files that the key built into the unit's Tetrad
signed. A build can have its own key (a private one that only you have):

1. `scripts/dev-update.sh key ~/tetrad-dev.pem` makes the key and prints
   `TETRAD_RELEASE_PUBKEY=<public key>`.
2. Build with that variable set. The build then trusts only your key: it does not accept the
   releases of `JudahZF/tetrad`, and the Tetrad Updates page refuses them. A build without the
   variable (every release) is unchanged.
3. `scripts/dev-update.sh pack DIR ~/tetrad-dev.pem` adds the launcher scripts and writes the
   signed `tetrad-update.json` and `tetrad-update.json.sig`. `tetrad-usb install DIR` sends
   the six files ([usb.md](usb.md#install-a-release-over-usb)).

Tests: `cargo test --locked -p tetrad-app --test dev_update_script` runs the scripts (key, pack,
archive, and the dev build with a stand-in for `build-arm.sh`) and checks a packed update with
the code of the unit.

### Releases

A `v*` tag runs `.github/workflows/release.yml`. It builds `tetrad`, checks it and
attaches `tetrad`, `tetrad.sha256`, the release archive `tetrad-<tag>-armv7.tar.gz` and its
`.sha256` to the [GitHub release](https://github.com/JudahZF/tetrad/releases). The archive has `bin/tetrad`,
`launcher/*.sh`, `scripts/install.sh`, `scripts/qc-ssh-proxy.py`, the install, launcher and
update guides in `docs/`, the README images in `docs/images/`, `README.md`, `CHANGELOG.md` and
`LICENSE`.

### Extensions

The host tests need no Node.js: they use the packaged extensions in
`extensions/repository/packages/`. `tests/nam.rs` in `tetrad-extensions` runs the Neural Amp
Modeler package on a synthetic LSTM model and compares it with a port of NeuralAmpModelerCore.
With `QC_CORPUS_DIR` set, `tetrad-app` also links DSP blocks against the stock Volume blob
(`device::community` tests) and qcdsp's machine tests in `tetrad-sharc` run every example. To change an extension's TypeScript, install Node.js and npm,
then use `tetrad-ext build`; [extensions.md](extensions.md#write-an-extension) has the steps, and
`extensions/repository/README.md` the release steps of the default repository.

## README images

The images in `docs/images/` come from the owned UI with synthetic data. No stock file is used.
Make them again after a UI change:

```sh
cargo run --locked -p tetrad-ui --example render_owned -- readme docs/images
```

`render_owned` with one view name writes that view as PPM to stdout, for a quick look at one
screen. `render_owned themes <dir>` writes a few views in every built-in theme as PNG.

Screens take their colours from `l.palette` (a `tetrad_theme::Palette`), never from constants,
so every theme reaches them. A stock shade that is not a palette role stays a constant with its
stock value and is drawn as `p.tone(CONST)`: unchanged in Stock, mapped onto the theme
elsewhere. Block colours, scene swatches and the tuner green are data and stay fixed.

## What ran on the test unit

The unit is an original (non-ESS) Quad Cortex with CorOS 4.1.0, and later CorOS 4.1.1 after an
on-device update.

- Boot of the stock SHARC firmware, preset deploys and audio over USB, with output parity
  checks against stock.
- Scene, stomp and preset changes with audio, the metronome, MIDI over USB.
- Cortex Control: connect, a 10 minute link, scene and preset pushes.
- Wi-Fi, DHCP, NTP, the signed update check, cloud reads and writes.
- Library writes that stock CorOS reads back, backup restore.
- A 61 minute soak with Cortex Control and the USB clock.
- Boots through the boot selector, and the fall-back to CorOS after failed starts.
- A CorOS 4.1.0 to 4.1.1 update from the touchscreen that kept Tetrad.

## Project layout

| Path | What it is |
| --- | --- |
| `crates/tetrad-app` | `tetrad`: the device program, the boot selector and offline tools |
| `crates/tetrad-ui` | The 800×480 touch UI: screens, widgets, dialogs and view models |
| `crates/tetrad-assets` | Fixed data colours, icons and the IBM Plex Sans font |
| `crates/tetrad-theme` | Themes: the UI palette, built-in themes and the theme file schema |
| `crates/tetrad-control` | Cortex Control: USB HID reports, payload codecs and the dispatcher |
| `crates/tetrad-dsp-host` | ARM side of the DSP link: ring transport, session, model deploy, block drivers |
| `crates/tetrad-dsp-protocol` | DSP request and response codecs |
| `crates/tetrad-loader` | SHARC boot: LDR parsing and boot plans for both DSP chips |
| `crates/tetrad-engine` | Preset-to-DSP compiler: routing, core assignment, run lists |
| `crates/tetrad-preset` | Lossless preset reader and editor |
| `crates/tetrad-catalog` | Model and parameter metadata from the stock model list |
| `crates/tetrad-core` | Shared rules: value conversion, scenes, stomps, LEDs, lifecycle |
| `crates/tetrad-library` | The stock preset library on the unit, and an owned SQLite library |
| `crates/tetrad-storage` | SQLite access for the stock and owned databases |
| `crates/tetrad-media` | IRs, cab blocks, the factory IR catalog and metronome sounds |
| `crates/tetrad-capture` | Neural Capture V1 and V2 flows and numeric references |
| `crates/tetrad-midi` | MIDI receive and transmit rules, clock and program changes |
| `crates/tetrad-services` | Wi-Fi, DHCP, time, updater and cloud clients |
| `crates/tetrad-settings` | Global settings in the stock file format |
| `crates/tetrad-firmware` | Known CorOS builds and their per-build addresses |
| `crates/tetrad-hal` | Hardware requests and records of the stock HAL |
| `crates/tetrad-sys` | Linux backends: framebuffer, input, ioctls, UIO, `/dev/mem` |
| `crates/tetrad-runtime` | Supervisor, worker and owned DSP resident models |
| `crates/tetrad-watchdog` | Start-up, supervision and power-action rules |
| `crates/tetrad-sharc` | Owned SHARC ISA48 code emitter and simulator |
| `crates/tetrad-blocks` | Owned SHARC DSP blocks |
| `crates/tetrad-cli` | `tetrad-cli`: offline inspection and export of local files |
| `scripts/launcher/` | Boot launcher, operator commands and the CorOS update wrapper (run on the unit) |
| `scripts/bringup/` | Developer tools for staged test runs on a unit |
| `scripts/install.sh` | The installer that runs on your computer ([install.md](install.md)) |
| `scripts/qc-ssh-proxy.py` | SSH proxy for the unit's OpenSSH 6.7 (`install.sh --proxy`) |
| `qcdsp/` | Submodule ([qcdsp](https://github.com/JudahZF/qcdsp)): block file formats, the VISA assembler and the qcdsp DSP language |
| `qc-dev/` | Submodule: the public ARM toolchain ([tetrad-toolchain](https://github.com/JudahZF/tetrad-toolchain)) |
| `.github/workflows/` | CI (host checks) and the release build |
| `docs/` | Architecture, launcher, updates, install and development guides |
| `docs/images/` | The README images (see [README images](#readme-images)) |

See [architecture.md](architecture.md) for the data flow.

## Owned DSP blocks (experimental)

`tetrad-blocks` builds five SHARC blocks (Volume, Gain, Low-High Cut, Simple Gate, Graphic-9)
with no stock code. They run only in bring-up mode, on DSP core 0, and only muted runs are
tested.

Some behaviour is inferred from analysis, not from stock source. The code marks inferred
behaviour with **(I)** and owned choices with **(O)**.
