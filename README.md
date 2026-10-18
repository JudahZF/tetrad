# Tetrad

Tetrad is an independent, unofficial re-implementation of the Neural DSP Quad Cortex user
interface. It is written in Rust. It runs in place of the stock UI program (ZenUI), which stays
installed on the unit. Tetrad keeps the stock Linux kernel, drivers, bootloader and SHARC DSP
firmware, and it reads the stock data files from your own unit at run time.

Tetrad is not affiliated with, endorsed by or supported by Neural DSP Technologies. "Neural DSP",
"Quad Cortex", "CorOS" and "Cortex Control" are trademarks of their owners. This repository
contains no Neural DSP code, firmware, assets or keys.

## Status

**Alpha.** Tetrad runs as the boot UI on one unit: an original (non-ESS) Quad Cortex with CorOS
4.1.0, and later CorOS 4.1.1 after an on-device update. No other unit, model or CorOS version is
tested. Expect bugs. Keep a backup of your unit.

The first release is
[v0.1.0-alpha.1](https://github.com/JudahZF/tetrad/releases/tag/v0.1.0-alpha.1). All releases
are on [GitHub releases](https://github.com/JudahZF/tetrad/releases). The changes are in
[CHANGELOG.md](CHANGELOG.md).

What ran on that unit:

- Boot of the stock SHARC firmware, preset deploys and audio over USB, with output parity
  checks against stock.
- Scene, stomp and preset changes with audio, the metronome, MIDI over USB.
- Cortex Control: connect, a 10 minute link, scene and preset pushes.
- Wi-Fi, DHCP, NTP, the signed update check, cloud reads and writes.
- Library writes that stock CorOS reads back, backup restore.
- A 61 minute soak with Cortex Control and the USB clock.
- Boots through the boot selector, and the fall-back to CorOS after failed starts.
- A CorOS 4.1.0 to 4.1.1 update from the touchscreen that kept Tetrad.

## Features

- **Boot selector.** At each boot a screen shows two buttons, CorOS and Tetrad, for 5 seconds.
  A touch picks one. Without a touch the saved choice starts. See [docs/launcher.md](docs/launcher.md).
- **Safe boot hook.** One line in one init script starts the launcher. When Tetrad fails to start
  three times in a row, the unit starts stock CorOS until you arm Tetrad again.
- **On-device CorOS updates.** Settings → System → Updates downloads and installs a CorOS update as stock
  does. The install keeps the boot hook and your SSH keys. See [docs/updates.md](docs/updates.md).
- **Firmware profiles.** Tetrad identifies the stock build by file hashes and supports CorOS
  4.1.0 and 4.1.1. On another build it starts stock CorOS.
- **Stock DSP path.** Tetrad boots the stock SHARC firmware and deploys presets in the stock
  order. Every stock model except the Looper has a block driver. The drivers read their tables
  from the stock ZenUI on your unit.
- **Touch UI.** An owned 800×480 UI that follows the stock screens: Grid, Gig and Stomp views,
  the preset and capture browsers, the block editors, settings, dialogs and keypads. It uses the
  stock design tokens and IBM Plex Sans.
- **Performance controls.** Footswitches, stomp and scene modes, scene copy, bank cycling,
  expression pedals, tempo and tap tempo, the tuner, the metronome and the Looper.
- **MIDI.** DIN and USB MIDI in and out, program changes, MIDI clock and Preset MIDI Out.
- **Cortex Control.** The USB link to the desktop app: settings, Global EQ, meters, the file
  browser, model presets, backups, the USB CorOS updater and cloud forwarding.
- **Services.** Wi-Fi, time, internet checks, cloud sign-in, downloads and uploads, local and
  cloud backups.
- **Neural Capture.** The capture screens, the V1 capture flow steps and the Neural Capture 2
  dialog. Capture training on the unit is not tested.
- **Owned DSP blocks (experimental).** `tetrad-blocks` builds five SHARC blocks (Volume, Gain,
  Low-High Cut, Simple Gate, Graphic-9) with no stock code. They run only in bring-up mode,
  on DSP core 0, and only muted runs are tested.

## Risks

Read this before you install Tetrad.

- **It changes the boot.** The installer edits `/etc/init.d/S80drvinit` on the root partition.
  The edit is one line, and `remove-hook` puts back the original file byte for byte.
- **You need root SSH access** to the unit. Tetrad does not give you this access.
- **It can void your warranty.** Neural DSP does not support modified units.
- **It is alpha software.** A bug can stop audio, lose a setting or need a recovery from the SD
  card. Keep a full backup of the SD card.
- **No stock files are included.** Tetrad reads the stock firmware files, data and device keys
  from your unit at run time. It never copies them into this repository.
- **No automatic root fall-back is known.** If a CorOS update writes a bad root partition, only
  the manual steps in [Recovery](#recovery) bring the unit back.

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
tests also need `QC_BLOBDIS` and `SELACHE` (see `qcdsp/README.md`).

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
`qc-dev/check-elf.sh`. See [qc-dev/README.md](qc-dev/README.md) for the details and a build
without Nix.

Releases: a `v*` tag runs `.github/workflows/release.yml`. It builds `tetrad`, checks it and
attaches `tetrad`, `tetrad.sha256`, the release archive `tetrad-<tag>-armv7.tar.gz` and its
`.sha256` to the [GitHub release](https://github.com/JudahZF/tetrad/releases). The archive has `bin/tetrad`,
`launcher/*.sh`, `scripts/install.sh`, `scripts/qc-ssh-proxy.py`, the install, launcher and
update guides in `docs/`, `README.md`, `CHANGELOG.md` and `LICENSE`.

## Install

Download the release archive from [GitHub releases](https://github.com/JudahZF/tetrad/releases), then
follow [docs/install.md](docs/install.md). A unit with zen-rs (the old working name of Tetrad):
see [Migrating from zen-rs](docs/install.md#migrating-from-zen-rs).

## Recovery

Each of these steps puts stock CorOS back. Use the first one that works.

1. **Pick CorOS on the boot selector.** Touch "CorOS" during the 5 second countdown. Tetrad
   saves the choice, so the next boots also start CorOS.
2. **Disarm over SSH.** Run `sh /media/p4/tetrad/launcher/tetrad-launcher.sh disarm` on the unit
   and restart it. The hook stays, but it starts stock ZenUI.
3. **Let the start counter act.** When Tetrad fails before its start-up ends three times in a
   row, the next boot starts stock and stays stock. A power cut during the start-up also counts.
4. **Remove the hook.** Run `tetrad-launcher.sh disarm`, then `tetrad-launcher.sh remove-hook`. The
   init script is then the CorOS original again.
5. **Use the SD card.** On a computer, open partition 4 (ext4) and delete
   `tetrad/launcher/select`. The hook then starts stock ZenUI.
6. **Roll back a CorOS update.** The unit keeps the previous CorOS on the other root partition.
   The boot partition (p3) holds one empty file, `one` or `two`, that selects the root
   partition. Change that file to the other name and restart. This step is not tested on a unit.
   See [docs/updates.md](docs/updates.md#roll-back).

Details: [docs/launcher.md](docs/launcher.md#recovery).

## Known gaps

- Only one unit type is tested: the original Quad Cortex without the ESS codec. The ESS codec
  and the Quad Cortex mini paths exist in code but have no hardware test.
- Only CorOS 4.1.0 and 4.1.1 are supported. On another build Tetrad starts stock CorOS.
- Not tested on the unit: Neural Capture training, loading a user Neural Capture 2 capture,
  capture over Cortex Control, the Global EQ and limiter toggles with audio, DIN MIDI, the panel
  controls one by one, cable pulls and shutdown.
- Owned DSP blocks run only muted and only in bring-up mode.
- Support reports are off: SEND REPORT and the Cortex Control log request are refused.
- Cortex Control gain calibration is refused, because stock writes the U-Boot environment there.
- Tetrad writes only the stock library database schema of CorOS 4.1.x (410.1).
- The boot selector has no settings page. Use `tetrad-launcher.sh chooser on|off`.
- Some behaviour is inferred from analysis, not from stock source. The code marks inferred
  behaviour with **(I)** and owned choices with **(O)**.

## Project layout

| Path | What it is |
| --- | --- |
| `crates/tetrad-app` | `tetrad`: the device program, the boot selector and offline tools |
| `crates/tetrad-ui` | The 800×480 touch UI: screens, widgets, dialogs and view models |
| `crates/tetrad-assets` | Colour tokens, icons and the IBM Plex Sans font |
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
| `scripts/install.sh` | The installer that runs on your computer ([docs/install.md](docs/install.md)) |
| `scripts/qc-ssh-proxy.py` | SSH proxy for the unit's OpenSSH 6.7 (`install.sh --proxy`) |
| `qcdsp/` | Submodule ([qcdsp](https://github.com/JudahZF/qcdsp)): block file formats, the VISA assembler and the qcdsp DSP language |
| `qc-dev/` | Submodule: the public ARM toolchain ([tetrad-toolchain](https://github.com/JudahZF/tetrad-toolchain)) |
| `.github/workflows/` | CI (host checks) and the release build |
| `docs/` | Architecture, launcher, updates and install guides |

See [docs/architecture.md](docs/architecture.md) for the data flow.

## Licence

Tetrad is licensed under the GNU General Public License v3.0 or later. See [LICENSE](LICENSE).

The IBM Plex Sans fonts in `crates/tetrad-assets/fonts/ibm-plex-sans/` keep their own licence, the
SIL Open Font License 1.1. See `OFL.txt` in that folder.
