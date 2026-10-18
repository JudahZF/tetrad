# tetrad-control

Cortex Control, the USB HID link to the desktop app.

- `pipeline`: 128-byte report framing, compression and the transform stage.
- `codecs`: typed payload codecs that keep unknown fields and the original wire bytes.
- `link::ControlLink`: the stock receive stages, session rules and per-type receivers, over a
  `device::DeviceModel` that the application implements.
- `block`: the stock block serializer for the resync messages.

The crate does no USB I/O. `tetrad-app` connects it to the DSP HID transport.

Part of [Tetrad](../../README.md).
