# Tetrad app

The phone app for a Quad Cortex that runs Tetrad. Built with Expo (SDK 58, Expo Router,
TypeScript). It does what the Remote page does, and it edits the preset: the Grid, blocks,
parameters and scenes.

| Tab | What it does |
| --- | --- |
| Rig | The loaded preset, its scenes and the 4 × 8 Grid. Tap a block to select it and again to open it, drag to move, hold to copy, tap an empty slot to add. The block editor has every parameter as a knob, values and bypass per scene, replace and remove. |
| Library | IRs (WAV or AIFF, converted on the phone to the unit's 48 kHz mono IR, as the Remote page does), the IR library (folders, rename, delete, saving destination), and the files of extensions such as NAM. |
| Control | The Custom footswitch mode (four pages, ALL PRESETS and THIS PRESET, as the unit's editor draws them) and the mappings of the preset and of the unit, with LEARN. |
| Unit | The theme (the app takes the unit's theme), backup and restore, extensions, notices, the support file, and the pages of extensions. |

The icons are the unit's own: `scripts/icons.mjs` renders them from `crates/tetrad-assets`.

## How it reaches the unit

| | Bluetooth (Tetrad Link, experimental) | Wi-Fi (Remote) | Demo |
| --- | --- | --- | --- |
| Everything of the Remote page: IRs, IR library, extension files and pages, support file, footswitches, mappings, backup, extensions, themes, notices | yes | yes | yes |
| Messages to extensions with the `ble` capability | yes | no | yes |
| Preset and Grid editing | from Link protocol 2 ([docs/preset-protocol.md](docs/preset-protocol.md)) | no | yes |

- **Bluetooth (experimental)**: the Tetrad Link of docs/bluetooth.md (PR #44, not merged yet).
  It is not tested on a unit: the app's Link code passes its tests against fake ports only. The
  app finds units by the BLE MIDI service that they advertise, and asks once for the App PIN of
  the unit's Bluetooth page.
  It sends the Remote API requests through the Link's `remote` op, so the Wi-Fi client
  (`RemoteClient`) and the Link client share one code path. The Link carries about 8 KB/s, so
  large files are faster over Wi-Fi.
- **Wi-Fi**: the Remote API ([docs/remote-api.md](docs/remote-api.md)). Type the address that
  Settings › Tetrad › Remote shows, or paste the link of its QR code. Tested on a unit: every
  request of the app, and its screens on the unit's data.
- **Demo**: a unit in the app, with every feature. Use it to try the app with no Quad Cortex.

A screen that needs a feature the connection does not have says so instead of failing.

## Run it

```sh
pnpm install
pnpm web             # the browser: the demo unit (Wi-Fi needs a same-origin proxy to the unit's /api; no Bluetooth)
pnpm android         # a development build on a phone or emulator (Bluetooth needs a phone)
pnpm ios
```

Bluetooth (`react-native-ble-plx`) is native code, so it does not run in Expo Go: use a
development build (`pnpm android`, `pnpm ios`, or `npx eas-cli build --profile development`).

## Check it

```sh
pnpm typecheck
pnpm lint
pnpm test            # the codec, clients, IR conversion, preset edits and edit queue
```

The tests run the Link codec against the unit's frame rules, the clients against fake ports
and a fake `fetch`, and the demo unit. None of them need a unit.

## Layout

| Path | |
| --- | --- |
| `src/app/` | Routes (Expo Router): Connect, the four tabs, and the sheets |
| `src/lib/link/` | Tetrad Link: frames, message format, request/reply session, the BLE port |
| `src/lib/unit/` | One `UnitClient` interface; the Link, Remote and demo clients; the unit's types |
| `src/lib/preset/` | The preset model, its edits (`applyEdit`) and the edit queue |
| `src/lib/ir/` | WAV/AIFF decoding, the mono mix, resampling and the unit's IR WAV |
| `src/state/` | The connected unit, saved units (secure store), toasts, loading |
| `src/ui/` | The kit (cards, rows, tabs, switches), Grid, knobs, footswitch tiles, icons |
| `scripts/icons.mjs` | Renders the unit's icons into `assets/icons/` and `src/ui/icon-assets.ts` (needs cargo) |
| `scripts/app-icons.sh` | App icon and splash from `docs/brand/` (needs rsvg-convert) |
