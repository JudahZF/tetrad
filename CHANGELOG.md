# Changelog

All notable changes to Tetrad are in this file. The version numbers follow
[Semantic Versioning](https://semver.org/).

## Unreleased

### Added

- Neural Amp Modeler A2 0.1.0: a new extension that plays NAM A2 models from
  `/media/p4/tetrad/assets/tetrad.nam-a2/` as Guitar Amplifier blocks ("NAM A2 …"). An A2 file
  holds two sizes; it plays A2-Lite (an estimated 29% of a DSP core, 112 KB of DSP memory).
  A2-Full needs more DSP memory and CPU than one core has. Its blocks match NeuralAmpModelerCore
  v0.6.0 within 2e-6 of the peak.
- qcdsp networks: a leaky ReLU activation (`{ leakyRelu: slope }` in the extension SDK, slope
  0 to 1).
- Extension API 5: the leaky ReLU activation. Hosts before API 5 do not offer API 5 packages, so
  they keep Neural Amp Modeler 0.3.1 and do not see Neural Amp Modeler A2.
- Block list: an Extensions row after the stock categories lists every block from an
  extension, with its category. The row shows when an extension block is installed. The blocks
  also stay in the list of their own category.
- Support file: the Remote page has a Download button for a `.zip` with versions, licence
  states, the loaded preset's blocks, extensions, recent notices and the last 5 boot logs, for
  bug reports. It holds no passwords, tokens or presets. Settings → Support → Send Report shows
  how to get it ([docs/support.md](docs/support.md)).
- Notices in the boot log start with the time of day (UTC) once the clock is set.
- Settings has a Tetrad group. An extension that is on and has settings adds a row with its
  name there; the row opens the extension's settings.

### Changed

- Tetrad Updates, Appearance, Extensions and Remote moved from Settings → System to the new
  Settings → Tetrad group.
- Settings → Support → Send Report shows the Remote switch, address and PIN to get the support
  file. Before, SEND REPORT showed "Sending report..." and then was refused.
- Neural Amp Modeler is now Neural Amp Modeler A1 (0.4.0), with blocks named "NAM A1 …". It
  reads per-layer kernel sizes and activations, LeakyReLU and convolutional heads, and points
  A2 files to Neural Amp Modeler A2.
- Neural Amp Modeler A1 0.5.0 and Neural Amp Modeler A2 0.2.0: each block has the controls of
  the NAM plugin: Input (0 to 10), Bass, Mid and Treble (the plugin's tone stack, 5 is flat)
  and Output (-40 to +40 dB). Before, a block had only Input and Output (0 to 4). A preset
  saved with an older version loads wrong values for these knobs: set the knobs of its NAM
  blocks again (#21, thanks Rick Peters).
- The default extension repository has Neural Amp Modeler A1 0.5.0 and A2 0.2.0 (API 5, tag
  `extensions-2026-10-27`). A1 0.4.0 and A2 0.1.0 were not published.

## [0.1.0-alpha.8](https://github.com/JudahZF/tetrad/releases/tag/v0.1.0-alpha.8) (2026-10-25)

### Fixed

- The touch screen did nothing on units with the ILI2130 touch controller, while the
  footswitches and knobs worked. This controller sends only positions, with no touch, pressure
  or tracking id events, so every touch read as a lift. Tetrad now reads such a frame with a
  position as a press and an empty frame as the release (#16, thanks Rick Peters).

## [0.1.0-alpha.7](https://github.com/JudahZF/tetrad/releases/tag/v0.1.0-alpha.7) (2026-10-24)

### Added

- Presets: the preset ⋮ menu has Copy, and Copy and Move open one page with every setlist you
  can write and all slots of the chosen one. An empty slot takes the preset at once; an
  occupied slot asks to OVERWRITE it (or SWAP, for a move inside the same setlist). Move
  before only changed the slot inside the open setlist.
- Mapping modes: each mapping chooses how its trigger's value drives the action. A parameter
  can be Set, Toggle (between two values), Momentary (held, then back to the value from before),
  Step (up or down by an amount, stop or wrap), Follow (a CC sets the value) or Relative (an
  endless encoder steps it). Bypass, the tuner and Gig View can toggle, switch while held, be
  set on or off, or follow a CC. Values show in the parameter's own units (docs/mappings.md).
- CC window: every CC mapping has a Low and a High (0–127). A press mode presses when the CC
  enters the window and releases when it leaves; Follow maps the window to its Min and Max.
  Several rows on one CC with their own windows map CC values to exact values.
- Pickup for Follow: the parameter waits until the knob reaches its value, then follows.
- The Values page of a CC mapping shows the window, the map and the CC's last value.
- Footswitch releases, note-offs and extension releases (`pressed: false`) reach Mappings.
  MIDI Mapper 0.3.0 sends note-offs as releases.
- Extension API 3: trigger firings can carry `pressed: false` (a release). Builds before API 3
  do not install API 3 packages, so they keep MIDI Mapper 0.2.0.
- Custom footswitch mode (docs/footswitch-modes.md): an extension can add a fourth footswitch
  mode with 1 to 4 pages of ten switches (A–D and Down over E–H and Tap). Each switch runs one
  action: a Mappings system action, a page change, a block bypass (toggle or momentary) or
  parameter, Load preset, its own Send MIDI messages, or a command of a running extension.
  Tetrad runs the switches, paints their LEDs and shows the page in Gig View; an editor (Gig
  View → EDIT) sets the actions, labels and colours. In the mode, Bank Up goes to the next mode
  (a hold opens the tuner). The mode joins the end of the mode cycle, comes back after a
  restart and is saved in `/media/p4/tetrad/footswitch-modes/` and
  `/media/p4/tetrad/footswitch-mode.json`. COPY, MOVE and COPY PAGE copy or swap switches and
  pages; Preset → Choose a preset picks any preset of a setlist; THIS PRESET gives a preset its
  own layout, saved in the preset (field 7003) and played while it is loaded.
- Extension API 4: `footswitchMode` in the manifest (data only, no capability). Hosts before API
  4 do not offer such packages.
- The Custom Footswitch Mode extension (`extensions/custom-footswitch`, 0.1.0): a 4-page Custom
  mode, no code.
- Mappings: the SYSTEM actions Mode → Custom mode, the Custom mode's pages, and a press of each
  of its switches, so MIDI controls the mode.

### Changed

- A CC mapped to a scene, preset, mode or tap tempo runs once when it enters its window, not on
  every message of 64 or more.

## [0.1.0-alpha.6](https://github.com/JudahZF/tetrad/releases/tag/v0.1.0-alpha.6) (2026-10-24)

### Added

- Sweep Synth extension (`tetrad.sweep-synth` 0.1.0): two mono DSP blocks in the Synth list,
  Sweep Synth and Bass Sweep Synth, at about half the CPU of the stereo synth. DSP Examples
  0.3.0 drops its synth, so a unit with both extensions does not show it twice.

### Fixed

- Analog inputs were silent when the unit booted straight into Tetrad: the tuner showed no
  signal and nothing played. Stock ZenUI loads the codec kernel module itself
  (`zencodec_cs4385_cs5364`, or `zencodec_es9017_es9826` on QC-007 and the mini), and that module
  sets up the ADC and DAC. Tetrad now loads it at start. Earlier hardware tests passed only because
  stock had booted first and loaded it.

## [0.1.0-alpha.5](https://github.com/JudahZF/tetrad/releases/tag/v0.1.0-alpha.5) (2026-10-23)

### Added

- Settings → System → Extensions warns when one extension is on from two repositories: its
  Available row says "Also installed from …", the install message asks to keep one on, and both
  Installed rows say "Also on from …" until one is off. Two installations add the same blocks
  and themes twice.
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
- Neural Amp Modeler 0.3.1: plays NAM LSTM and WaveNet models from
  `/media/p4/tetrad/assets/tetrad.nam/` as Guitar Amplifier blocks, including the NAM trainer's
  nano, feather and lite sizes (on the test unit below 25%, about 48% and 75% of a DSP core). Its
  blocks match NeuralAmpModelerCore (within 2e-3 of the peak). The trainer's standard size needs
  more DSP memory than one core has and is refused.
- qcdsp IR 3 networks: channel buffers with gain, dilated convolution, tanh/hardtanh/relu, add and
  copy, before the graph (`dsp.network()` in the extension SDK). The DSP runs them over each
  32-sample call in SIMD kernels: two output channels per instruction, weights in the PM heap,
  convolution history in circular rings. On the unit a lite WaveNet matched its host reference
  within 1e-5.
- DSP Examples 0.2.0: eight qcdsp example blocks (gain, two drives, tremolo, slapback, a 187 ms
  echo from the DSP heap, envelope filter, micro synth) to try DSP blocks on a unit.
- DSP blocks larger than 32 KB get their memory from the DSP core's heap when they start
  (qcdsp arenas): the DM heap when the block fits it, else the SD heap (DDR). The catalogue
  charges the exact heap figures, and the CPU estimate charges the measured cost of DDR access.
- Tested on a unit over USB audio: the blocks match their host references, a preset with a DSP
  block loads in stock CorOS with its placeholder and comes back in Tetrad, and 100 redeploys of
  an arena block leave the heap unchanged. A 512 KB SD arena block starts on cores 2, 3 and 4
  and matches its host reference.
- qcdsp IR 2: `div`, `recip`, `tanh`, `sigmoid` and mono blocks.
- Tetrad over USB (docs/usb.md): the `tetrad-usb` tool shows the Tetrad version, saves a
  screenshot, prints the recent notices and sends a signed Tetrad release to the unit over the
  USB cable. UPDATE TETRAD on the unit then installs it, also without the internet. The link
  shares the HID interface with Cortex Control; both run at the same time.
- Extensions: the `usb` capability and `ctx.usb.onMessage` answer JSON messages from a computer
  (`tetrad-usb ext`), so an extension can add its own USB features.
- USB MIDI sends and receives every MIDI kind, SysEx included. Before, only CC, PC and the clock
  went out over USB.

### Changed

- qcdsp's CPU estimate counts instructions (one per cycle) instead of 16-bit parcels; the
  catalogue figure still doubles it.

### Fixed

- DSP Diagnostics: the heap cell (`DM:a/PM:b/SD:c`) was cut off. The DM, PM and SD heaps now
  have one row each.

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
