# Tetrad over USB

Tetrad has its own link over the USB cable of the Quad Cortex. A computer can ask Tetrad for
its version, a screenshot and its recent notices, and it can send a Tetrad release to the unit.
The link uses the same USB cable and HID interface as Cortex Control. Both can run at the same
time.

## How it works

The USB port of the Quad Cortex belongs to the stock firmware of the second chip (SOC2). Tetrad
cannot change the USB device: the audio interface, USB MIDI and the HID interface stay as stock
makes them. The HID interface carries 128-byte reports in both directions. The stock firmware
passes every report between the computer and Tetrad unchanged. This was measured on a unit on
2026-10-22.

Tetrad reports start with two zero bytes. A Cortex Control receiver reads them as a report with
no FIRST flag, no LAST flag and no bytes, so it ignores them. Tetrad takes its own reports
before they reach its Cortex Control link.

| Direction | Measured speed |
| --- | --- |
| Unit to computer | about 186 KiB/s |
| Computer to unit | about 24 KiB/s |
| One request and its reply | about 8 ms |

The computer-to-unit direction is slow because the unit stalls the status stage of every HID
SET_REPORT request, so each report is one control transfer. The data still arrives. Cortex
Control gets the same stall.

## The tetrad-usb tool

`tetrad-usb` runs on the computer. Build it from the Tetrad repository:

```sh
cargo build --release -p tetrad-usb --features host
```

It uses [hidapi](https://github.com/libusb/hidapi). On Linux it needs `libudev` (on NixOS:
`nix-shell -p pkg-config udev`), and your user needs read and write access to the
`/dev/hidraw*` node of the unit.

| Command | What it does |
| --- | --- |
| `tetrad-usb hello` | Shows the Tetrad version on the unit. |
| `tetrad-usb screenshot -o screen.png` | Saves the screen of the unit as a PNG. |
| `tetrad-usb log` | Prints the recent notices of the unit. |
| `tetrad-usb install DIR` | Sends a Tetrad release to the unit (see below). |
| `tetrad-usb ext ID JSON` | Sends a JSON message to an extension and prints its reply (see below). |

### Install a release over USB

1. Download the release assets into one folder: `tetrad`, `tetrad-launch.sh`,
   `tetrad-launcher.sh`, `tetrad-update.sh`, `tetrad-update.json` and
   `tetrad-update.json.sig`. For example:
   `gh release download v0.1.0-alpha.9 -R JudahZF/tetrad -D release`.
2. Run `tetrad-usb install release`. The unit checks the signature of `tetrad-update.json`
   with the Tetrad release key first, so only a real Tetrad release is accepted. Then the files
   go to the unit. The binary takes some minutes at about 24 KiB/s.
3. On the unit, open Settings → Tetrad → Tetrad Updates (Settings → System on 0.1.0-alpha.8 and earlier) and touch UPDATE TETRAD. The unit
   checks the size and the SHA-256 value of each file, checks that the scripts parse and that
   the new binary supports the CorOS build, arms it and restarts. These are the same steps as
   an update from GitHub ([updates.md](updates.md#tetrad-updates)).

A build with its own key ([development.md](development.md#your-own-updates)) takes the files that
`scripts/dev-update.sh pack` signs with that key, and a release build takes only the release key.
Over USB, any signed version installs, an older one too. Nothing installs until you touch
UPDATE TETRAD on the unit. During a Tetrad or CorOS install, the unit refuses a USB install.

## Custom features: extensions over USB

An [extension](extensions.md) with the `usb` capability can answer messages from a computer.
This is how you add your own USB features without changing Tetrad:

```ts
ctx.usb.onMessage(async (message, scope) => {
  if (message.cmd === "bypass") await scope.controls.toggleBypass(message.binding);
  return { ok: true };
});
```

```sh
tetrad-usb ext my.extension '{"cmd":"bypass","binding":{…}}'
```

The handler gets the JSON message and the usual scope, and its return value is the reply. A
handler that throws replies with an error. The handler has the extension limits: 1 s, then the
host stops the extension.

## Both at once

Cortex Control and `tetrad-usb` can run at the same time. This was tested on a unit on
2026-10-22: Cortex Control stayed connected through a 12-minute USB install and changed scenes
during it. Tetrad sends its reports only between Cortex Control messages. Run one `tetrad-usb`
at a time: two at once mix their requests.

## Protocol

Other tools (a script, or a web page with WebHID) can use the link. The `tetrad-usb` crate is
the reference.

### Reports

Each report is 128 bytes. On the computer, output reports have report ID 2 and input reports
have report ID 1.

| Bytes | Content |
| --- | --- |
| 0–1 | `00 00` |
| 2–5 | `TTRD` |
| 6 | Protocol version: 1 |
| 7 | Flags: 1 = START, 2 = END |
| 8 | Channel: 0 |
| 9 | Payload length: 0 to 118 |
| 10–127 | Payload |

A message is the payload of the reports from the START report to the END report on one
channel. An empty message is one report with both flags.

### Messages

A message is: the length of a JSON header (u32, little-endian), the JSON header, then a binary
body. Each request has an `id`, and its reply has the same `id`.

| Request | Body | Reply | Reply body |
| --- | --- | --- | --- |
| `{"id":1,"op":"hello"}` | — | `{"id":1,"reply":"hello","tetrad":"0.1.0-alpha.9","protocol":1}` | — |
| `{"id":2,"op":"screenshot"}` | — | `{"id":2,"reply":"screenshot","width":800,"height":480}` | PNG |
| `{"id":3,"op":"log"}` | — | `{"id":3,"reply":"log","lines":12}` | UTF-8 text, one notice per line |
| `{"id":4,"op":"install-begin","manifest":"…","signature":"…"}` | — | `{"id":4,"reply":"install-begin","version":"…","files":[{"path":"bin/tetrad","size":…},…]}` | — |
| `{"id":5,"op":"install-chunk","path":"bin/tetrad","offset":0}` | up to 64 KiB of the file | `{"id":5,"reply":"install-chunk","path":"bin/tetrad","received":65536}` | — |
| `{"id":6,"op":"install-ready"}` | — | `{"id":6,"reply":"install-ready","version":"…"}` | — |
| `{"id":7,"op":"extension","extension":"my.extension","message":…}` | — | `{"id":7,"reply":"extension","value":…}` | — |

`manifest` and `signature` are the exact text of `tetrad-update.json` and
`tetrad-update.json.sig`. The chunks of a file must follow on: `offset` is the `received` value
of the last reply. A refused request gets `{"id":…,"reply":"error","message":"…"}`.
