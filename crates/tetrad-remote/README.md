# tetrad-remote

Remote, the web page on the unit's Wi-Fi ([docs/remote.md](../../docs/remote.md)). It manages
IRs, mappings, Custom footswitch layouts, backups, themes, notices and extensions, and it
gives out the support file. Each request that changes the unit goes to a callback.

- `serve_one`: one HTTP/1.1 exchange on any `Read + Write` stream, with size limits.
- `Server`: the server thread on a TCP port. Requests that need the device come out of `drain`
  as a `Call`; `reply` answers them.
- `page.html`: the page. The IR box converts each IR in the browser to the stock format:
  mono, 48 kHz, 32-bit float, the first 1,024 samples.

The crate does no library work. `tetrad-app` (`device/remote.rs`) checks the PIN and runs the
actions.

Part of [Tetrad](../../README.md).
