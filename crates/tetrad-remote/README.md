# tetrad-remote

Remote, the web page that uploads IRs over Wi-Fi ([docs/remote.md](../../docs/remote.md)).

- `serve_one`: one HTTP/1.1 exchange on any `Read + Write` stream: the page, the PIN check and
  the upload route, with size limits. Each request that needs the device goes to a callback.
- `Server`: the server thread on a TCP port. Requests that need the device come out of `drain`
  as a `Call`; `reply` answers them.
- `page.html`: the page. It converts each IR in the browser (Web Audio) to the stock format,
  mono 48 kHz float32, and keeps the first 1,024 samples.

The crate does no library work. `tetrad-app` (`device/remote.rs`) checks the PIN and imports
the IR.

Part of [Tetrad](../../README.md).
