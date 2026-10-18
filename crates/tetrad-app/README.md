# tetrad-app

The application crate. It builds the `tetrad` binary.

- `tetrad run`: the device program. It boots the stock SHARC firmware, deploys presets and
  runs the UI, the footswitches, MIDI, Cortex Control and the services until a power action.
- `tetrad select`: the boot selector (see [docs/launcher.md](../../docs/launcher.md)).
- `tetrad bringup`: a staged test run on a unit. Every risky option is off by default.
- Offline tools that work on local files only: `render`, `replay`, `library-replay`,
  `scene-edit`, `library-scene-edit`, `tune` and `loop`.

The device code is in `src/device/`. One reducer owns all device state. Each event gives
effects, and the platform (`linux.rs` on the unit, a fake in tests) executes them.

```sh
cargo run --locked -p tetrad-app -- render --output /tmp/frame.ppm
cargo run --locked -p tetrad-app -- run --check      # on the unit: firmware check, opens nothing
```

Part of [Tetrad](../../README.md).
