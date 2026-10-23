# Architecture

Tetrad is one Rust workspace. The device program is `tetrad` (crate `tetrad-app`). It runs on the
unit's ARM CPU in place of stock ZenUI. It uses the stock Linux kernel and drivers, and it boots
the stock SHARC DSP firmware that is on the unit.

## Rules for the code

- **Stock data stays on the unit.** Tetrad reads the stock ZenUI binary, the SHARC firmware, the
  model list, factory data and device keys from the unit at run time. The repository holds no
  stock bytes. Tests use synthetic data. Tests that need stock files are `#[ignore]`.
- **Injected I/O.** Most crates do no I/O. They take traits or closures for files, devices,
  sockets, clocks and commands. `tetrad-sys` holds the Linux backends. Each crate has fakes for its
  tests.
- **Reducers and effects.** The device state has one owner. Events go in, effects come out, and
  the platform executes the effects.
- **Lossless data.** Presets, settings and Cortex Control payloads keep unknown fields and the
  original bytes. A no-op save gives the same bytes.
- **Marks in comments.** **(C)** is confirmed behaviour, **(I)** is inferred and **(O)** is an
  owned choice that differs from stock or fills a gap. Code addresses such as `0x5239d0` point
  into the stock CorOS 4.1.0 binaries, for reference. They are not code.
- `unsafe` code is forbidden in every crate except `tetrad-sys`.

## Crate map

```text
                                 tetrad-app (tetrad)
  ┌───────────────┬──────────────────┬────────┴───────┬────────────────┬─────────────────┐
  UI              DSP                Control          MIDI             Services          Lifecycle
  tetrad-ui       tetrad-dsp-host    tetrad-control   tetrad-midi      tetrad-services   tetrad-watchdog
  tetrad-assets   tetrad-loader                                        tetrad-settings   tetrad-runtime
                                                                       tetrad-remote
  tetrad-theme    tetrad-engine
                  tetrad-dsp-protocol
  ──────────────────────────── data and rules ────────────────────────────────────────────────
  tetrad-preset  tetrad-catalog  tetrad-core  tetrad-library  tetrad-storage  tetrad-media
  tetrad-capture  tetrad-firmware
  ──────────────────────────── hardware ──────────────────────────────────────────────────────
  tetrad-hal (requests and records)       tetrad-sys (Linux backends)
  ──────────────────────────── offline and owned DSP ─────────────────────────────────────────
  tetrad-cli   tetrad-sharc   tetrad-blocks
```

| Crate | Role |
| --- | --- |
| `tetrad-app` | `tetrad`: the device program (`src/device/`), the boot selector and offline tools |
| `tetrad-ui` | Screens, widgets, dialogs and view models of the touch UI |
| `tetrad-assets` | Fixed data colours (block categories, scene swatches), icons and the IBM Plex Sans font |
| `tetrad-theme` | Themes: the palette of UI colours, the stock palette, built-in themes, and the checked schema of theme files and extension themes |
| `tetrad-dsp-host` | Shared-memory ring transport, the DSP session, model blob placement, per-model block drivers |
| `tetrad-loader` | SHARC boot: LDR parsing and the boot plans of both DSP chips |
| `tetrad-engine` | Preset-to-DSP compiler: routing, core assignment, run lists, latency alignment |
| `tetrad-dsp-protocol` | DSP message codecs |
| `tetrad-control` | Cortex Control link: report framing, payload codecs, the dispatcher |
| `tetrad-midi` | MIDI receive and transmit, assignments, clock, program changes |
| `tetrad-extensions` | Extension runner (QuickJS), host protocol, manifests, packages (with DSP block graphs), signed catalogs, the on-device store, the TypeScript SDK and its block builder |
| `tetrad-extension-cli` | `tetrad-ext`: scaffold, build, package, sign and stage extensions |
| `tetrad-services` | Wi-Fi, DHCP, time, internet checks, updater and cloud clients |
| `tetrad-remote` | Remote: the LAN web page and HTTP server that upload IRs |
| `tetrad-settings` | Global settings in the stock Qt INI format |
| `tetrad-watchdog` | Start-up, supervision and power-action rules |
| `tetrad-runtime` | Supervisor and worker models, owned DSP resident models |
| `tetrad-preset` | Lossless preset reader, editor, migrations and stock save layout |
| `tetrad-catalog` | Model and parameter metadata |
| `tetrad-core` | Value conversion, scenes, stomps, LEDs, the preset lifecycle |
| `tetrad-library` | The stock library on the unit, and an owned SQLite library |
| `tetrad-storage` | SQLite access and the stock media files |
| `tetrad-media` | IRs, cab blocks, the factory IR catalog, metronome sounds |
| `tetrad-capture` | Neural Capture flows and numeric references |
| `tetrad-firmware` | Known CorOS builds and their per-build addresses |
| `tetrad-hal` | Hardware requests and records of the stock HAL |
| `tetrad-sys` | Framebuffer, evdev, ioctls, UART, UIO, `/dev/mem`, power actions |
| `tetrad-cli` | `tetrad-cli`: offline inspection and export of local files |
| `tetrad-sharc` | Owned SHARC ISA48 emitter, linker and simulator |
| `tetrad-blocks` | Owned SHARC DSP blocks |

`qc-format` and `qc-asm` come from the `qcdsp` submodule.

## Data flow of one boot

1. **Launcher.** The boot hook runs `tetrad-launch.sh`. It checks the armed binary, the start counter
   and the factory files, shows the boot selector, and runs `tetrad run`
   ([launcher.md](launcher.md)).
2. **Firmware check.** `tetrad run` first names its process `ZenUI`: ZenWatchdog restarts the
   unit after about 3 s without one, and the hash takes a few seconds. `tetrad bringup` and
   `tetrad select` also take the name first. `tetrad-firmware` hashes `/opt/neuraldsp/ZenUI` and
   `/usr/bin/loadsharc_SC589`. A build without a complete profile exits 4 before anything opens.
   Else it selects the profile for the process.
3. **Platform.** `tetrad-app` `device/linux.rs` opens the nodes through `tetrad-sys`: the framebuffer,
   touch, the HAL nodes, `/dev/mem`, UIO and the MIDI UART.
4. **DSP boot.** `tetrad-loader` reads the SHARC firmware files on the unit and boots SOC1 cores 1
   and 2, then the SOC2 slave. `tetrad-dsp-host` runs the stock handshake over the shared-memory
   rings and pulls the boot keys from the DSP.
5. **Data.** `tetrad-library`, `tetrad-storage` and `tetrad-media` open the stock library, `zenui.db`,
   `factory.db` and the IR catalog. `tetrad-settings` reads the stock INI files. Boot migrations run
   as stock does.
6. **Preset deploy.** `tetrad-preset` reads the last preset. `tetrad-engine` compiles it with the
   metadata from `tetrad-catalog` into a deployment. `tetrad-dsp-host` deploys it in the stock push
   order: shared blob loads, routers, splitter and mixer, block drivers, lane I/O, the sidechain
   step, then core enable. Block drivers read their stock tables from the ZenUI binary on the
   unit, at the addresses of the selected profile.
7. **Run.** The device loop takes events: touch, knobs, footswitches, expression pedals, DSP
   responses, Cortex Control reports (through the DSP HID transport and `tetrad-control`), MIDI
   (`tetrad-midi`), service results (`tetrad-services`, on two worker threads) and timers. One reducer
   updates the state and returns effects: DSP messages, UI frames (`tetrad-ui`, drawn to the
   framebuffer), LED writes, file writes, service jobs.
8. **Power action.** On restart or power off, `tetrad-watchdog` gives the stock teardown order.
   `tetrad` exits 0.

## DSP blocks from extensions

A package can bring DSP blocks as qcdsp graphs (owned). `tetrad-app` `device/community.rs`
compiles each one with `qcdsp::device::build` against the stock Volume blob and its per-core
records, which `Dsp::volume_template` reads from the unit's own firmware files. The block gets
a local model ID at the top of its category band (`tetrad_catalog::community`) and a spec that
the engine, the picker and the editor use like a stock spec. At deploy, `Dsp::core_loads`
sends the generated blob for each core through the stock `BlobLoad` path. A preset write
(`preset::exported`) puts each block's stock placeholder in its slot and a record in field 7002
(`tetrad_preset::community`); `preset::prepare` restores it. When the set of blocks changes,
the loaded preset leaves with the old catalog and is recalled with the new one.

## Wi-Fi recovery

Stock only reports a lost internet. Tetrad also recovers the link, because a Wi-Fi link once
stayed associated but passed no traffic until a restart. The internet probe (a TCP connect to
8.8.8.8:53 or 8.8.4.4:53 every 5 s) starts the recovery when it finds the internet lost on a
link that had it. Then, 60 s apart:

1. a new DHCP run (`udhcpc`); a failure keeps the association;
2. `REASSOCIATE` to the supplicant;
3. `REASSOCIATE` and the interface reset (`ifconfig wlan0 down`, then `up`); this step repeats
   every 10 min.

A connected probe ends the recovery, and so do a Wi-Fi connect, disconnect or forget and a
disabled connection check. No step runs during a CorOS install or without a saved network.
Each step writes one log line with the wlan0 packet and error counters. The DHCP log line has
the lease time.

## Offline tools

- `tetrad-cli` inspects and checks local files: presets, the model catalog, blobs, LDR files, DSP
  packets, Cortex Control reports, captures, IRs and the owned library. It never touches a
  device. `tetrad-cli capabilities` prints `docs/capabilities.toml`: what Tetrad does and how far
  each part is tested.
- `tetrad render`, `replay` and the other offline commands drive the UI and the library on the
  host.

## Add a CorOS build

A new CorOS build is data in `tetrad-firmware` only. The drivers do not change.

1. Hash the build's `ZenUI` and `loadsharc_SC589` with `sha256sum`.
2. Add a variant to `Firmware` and to `Firmware::ALL`, `identity`, `profile` and `Display`.
3. Add `src/v<x_y_z>.rs` like `v4_1_1.rs`, with `IDENTITY` and a `DRAFT`, and add its `static`
   profile in `lib.rs`.
4. Fill each slot. For the ZenUI addresses, find each 4.1.0 read site in the new ZenUI and write
   `Relocation` entries.
5. Run `cargo test -p tetrad-firmware -p tetrad-dsp-host`. The tests check that the relocation
   table is sorted and that every read site has an address in each complete build.
6. Add the per-site results as `data/v<x_y_z>_sites.tsv` and a test like
   `v4_1_1_sites_match_the_re_map`.
7. On a host with the new firmware files, select the new build, read each ZenUI payload and load
   the driver tables.

A build with an open slot stays a draft, and `tetrad run` refuses it with exit 4.
