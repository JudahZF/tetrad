# Bluetooth

The Wi-Fi chip of the Quad Cortex (a Microchip WILC3000) also has a Bluetooth Low Energy (BLE)
radio. Stock CorOS does not use it. Tetrad uses it for BLE MIDI: a phone, a tablet or a computer
can send MIDI to the unit and get the unit's MIDI, with no cable. The Tetrad app (in work) uses
the same link to do what the Remote page does.

- [Turn on Bluetooth](#turn-on-bluetooth)
- [Pair a phone or computer](#pair-a-phone-or-computer)
- [BLE MIDI](#ble-midi)
- [The Tetrad app link](#the-tetrad-app-link)
- [Limits](#limits)
- [Problems](#problems)
- [How it works](#how-it-works)

## Turn on Bluetooth

1. Open **Settings → Tetrad → Bluetooth**.
2. Turn on **Bluetooth**. The first line shows the unit's Bluetooth name, for example
   `Quad Cortex 1FEB`. The four characters at the end come from the unit's Bluetooth address, so
   two units have different names.
3. **Host mode** (on by default) lets a phone or computer connect to the unit. Turn it off when
   you do not want any phone or computer to connect.
4. **MIDI over Bluetooth** (on by default) turns BLE MIDI on or off in both directions.

The switches stay as you set them after a restart (`/media/p4/tetrad/bluetooth.json`). Bluetooth
is off until you turn it on.

## Pair a phone or computer

A phone or computer pairs once. After that it connects again without a new pairing.

- **New devices can pair only while the Bluetooth page is open on the unit.** On other screens
  the unit refuses a new pairing and shows "To pair a new device, open Settings › Tetrad ›
  Bluetooth". A device that is already paired connects at any time.
- The pairing has no code to type ("Just Works"). Tetrad uses LE Secure Connections when the
  other device has it (all current phones and computers do), so someone who only listens to the
  radio cannot read the keys. A pairing without a code cannot stop a device that pretends to be
  the other side while you pair: pair at home, not at a venue.
- BLE MIDI needs the pairing: the unit takes and sends MIDI only on an encrypted link.

How to connect:

| Device | Steps |
| --- | --- |
| Mac | Open **Audio MIDI Setup**, then **Window → Show MIDI Studio**. Click the Bluetooth icon (Bluetooth Configuration), then **Connect** next to the unit. |
| iPhone, iPad | In a music app, open its Bluetooth MIDI page (many apps have one, for example in their MIDI settings) and tap the unit. |
| Android | In a MIDI app, open its list of Bluetooth MIDI devices and tap the unit. |
| Windows 11 | Pair the unit in **Settings → Bluetooth & devices → Add device**. Your music program must support BLE MIDI. |
| Linux | `bluetoothctl`, then `pair <address>` and `connect <address>`. BlueZ with MIDI support makes an ALSA MIDI port. |

The Bluetooth page lists the paired devices. **FORGET** removes a pairing from the unit; remove it
on the phone or computer too, or that device cannot connect until it pairs again.

## BLE MIDI

- **In:** notes, CCs, program changes, pitch bend and SysEx from a BLE host act as the same
  message on the MIDI IN jack: the unit's MIDI channel applies, and a channel that an extension
  reserved (for example MIDI Mapper's) goes to that extension. The unit takes MIDI only while a
  preset is loaded, as for DIN.
- **Out:** every message that the unit sends to MIDI OUT (Preset MIDI Out, scene and footswitch
  messages, messages from extensions) also goes to the BLE host, when the host listens.
- **Not MIDI clock:** the unit does not send MIDI clock or other real-time messages over
  Bluetooth, and it ignores the ones it gets. BLE sends data at fixed intervals of several
  milliseconds, which makes a clock jitter. Use DIN or USB for clock.
- **Latency:** after a host connects, Tetrad asks for a connection interval of 15 to 30 ms. The
  host decides; most computers and phones take it.

## The Tetrad app link

The unit has a GATT service for the Tetrad app: the **Tetrad Link**. Over it the app does what the
Remote page does: add IRs, send and delete extension files (NAM models), use the tabs of
extensions, and download the support file. It also sends messages to extensions with the `ble`
capability and gets the events they push ([extensions.md](extensions.md#answer-the-tetrad-app)).

**The PIN.** While Bluetooth is on, the Host mode line of the Bluetooth page shows the **App
PIN** (4 digits, new at each start of Bluetooth). The app asks for it once per pairing: the unit
then remembers that this phone gave it. FORGET removes the pairing and that permission. Each
wrong PIN makes the next try wait 1 second; after 10 wrong PINs the unit makes a new PIN.

**For app authors.**

| | |
| --- | --- |
| Service | `5162a6fe-ad05-405f-b22e-6e4047a5b524` |
| Request characteristic | `…b525`: write without response (or write), app → unit; needs an encrypted link |
| Reply characteristic | `…b526`: notify, unit → app; needs an encrypted link |
| Frame | One flags byte (1 START, 2 END), then a piece of a message. A message runs from a START frame to an END frame; a frame is at most the ATT MTU less 3 bytes. Ask for a large MTU (the unit takes 247). |
| Message | The format of the USB link ([usb.md](usb.md#messages)): the length of a JSON header (u32, little-endian), the JSON header, then a binary body. At most 64 KiB from the app. |

Each request has an `id` (a number) and an `op`; its reply has the same `id`, and an `error`
field when it was refused. The unit answers one request at a time, in order.

| `op` | Fields | Body | Reply |
| --- | --- | --- | --- |
| `hello` | | | `tetrad` (version), `protocol` (1), `name`, `authorized` |
| `auth` | `pin` | | `authorized: true`, or an error |
| `status` | | | `folder`: the IR saving destination |
| `ir-upload` | `name` | the IR as the Remote page sends it (mono, 48 kHz, 32-bit float WAV) | `name`, `folder` |
| `files` | | | `extensions`, as `GET /api/files` ([remote.md](remote.md#extension-files)) |
| `file-chunk` | `extension`, `name`, `offset`, `last` | the next bytes of the file | `received`, or `name` and `size` after the last piece |
| `file-delete` | `extension`, `name` | | `name` |
| `tabs` | | | `tabs`: the extension tabs (`id`, `title`) |
| `tab-page` | `extension` | | `html` |
| `tab-call` | `extension`, `message` (JSON) | | `result`: the extension's answer |
| `support` | | | `name`; the body is the support file (zip) |
| `extension` | `extension`, `message` (JSON) | | `value`: the answer of the extension's `ctx.ble.onMessage` |

Every op except `hello` and `auth` needs the PIN first. An extension pushes an event with
`scope.ble.notify(value)`; it arrives as a message with no `id`:
`{"event": "extension", "extension": "<id>", "value": …}`.

At 115200 baud the link carries about 8 KB/s: an IR takes a second, a 3 MiB NAM model several
minutes. The Remote page over Wi-Fi is faster for large files.

Messages to the app wait in a queue of 256 KiB. When it is full, a message drops: a request
gets no reply, and an extension's event is lost (`notify` still resolved `true`). A message
goes only to the connection it was made for: when that connection ends, its messages drop.

## Limits

- One phone or computer at a time. While one is connected, the unit does not advertise.
- The radio talks to the unit at 115200 baud: about 8 KB/s. This is enough for MIDI.
- Wi-Fi and Bluetooth share one radio, on 2.4 GHz. The chip shares the air time: a large Wi-Fi
  transfer delays BLE messages, and with a weak Wi-Fi signal (many repeats) a link can drop. On
  the test unit, links dropped within seconds at a Wi-Fi signal of -68 dBm. Tetrad asks each host
  for a 6 s supervision timeout so that a link survives short gaps, but the host decides.

## Problems

| The page or the unit says | What to do |
| --- | --- |
| Restarting | The radio stopped answering. Tetrad starts it again (up to three times). |
| Stopped: … Turn Bluetooth off and on. | Turn Bluetooth off and on. If it stays stopped, restart the unit. |
| To pair a new device, open Settings › Tetrad › Bluetooth | Open the Bluetooth page on the unit, then pair again. |
| The Mac or phone does not list the unit | Check that Bluetooth and Host mode are on, and that no other phone or computer is connected. |
| A device that was paired does not connect | The unit or the device forgot the pairing. Remove the pairing on the device, then pair again with the Bluetooth page open. |

## How it works

For developers.

- **The controller.** The WILC3000's BLE controller is Bluetooth 4.0, LE only. Its HCI runs on
  `/dev/ttySC2` at 115200 8N1 with no flow control; faster rates gave a hardware error. The
  `wilc-spi` driver loads the BLE firmware (`/lib/firmware/mchp/wilc3000_ble_firmware.bin`)
  when Tetrad writes `BT_POWER_UP`, `BT_DOWNLOAD_FW` and `BT_FW_CHIP_WAKEUP` to `/dev/wilc_bt`.
  Read Local Version tells Tetrad the state: manufacturer 0x0060 is the boot ROM, 0x0013 the BLE
  firmware. A new load starts a controller that stopped answering. Tetrad never writes
  `BT_POWER_DOWN`: with Wi-Fi up, it left the BT core silent until a reboot.
- **The controller does one role at a time.** With a phone connected, it refuses to advertise
  for a second connection (LE Supported States, checked on a unit).
- **The host stack** is the `tetrad-ble` crate, Tetrad's own: HCI, L2CAP, ATT and a GATT
  server, SMP (Just Works with LE Secure Connections or legacy pairing) and the BLE MIDI packet
  format. It does no I/O. `tetrad-sys` `ble` runs it on its own thread with the UART;
  `tetrad-app` `device/bluetooth.rs` is the device side.
- **The GATT table:** GAP, GATT (with Service Changed), the BLE MIDI service
  (`03B80E5A-EDE8-4B33-A751-6CE34EC4C700`, characteristic `7772E5DB-3868-4112-A1A9-F2669D106BF3`)
  and the Tetrad Link. The MIDI and Link characteristics need an encrypted link. A paired host
  that saw another table gets a Service Changed indication when it comes back.
- **Files:** `/media/p4/tetrad/bluetooth.json` holds the switches
  (`{"enabled":true,"host_mode":true,"midi":true}`). `/media/p4/tetrad/bluetooth-bonds.json`
  holds the pairings, with their keys. The support file never takes the keys: its
  `bluetooth.txt` has the switches, the state and the names of the paired devices.
- **A check without Tetrad:** `cargo run -p tetrad-sys --example ble_peripheral`, built for ARM,
  runs the stack alone on a unit as a BLE MIDI device that answers each message (stop Tetrad's
  Bluetooth first).
