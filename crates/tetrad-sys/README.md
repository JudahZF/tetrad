# tetrad-sys

Linux device backends: `/dev/mem` windows, UIO, `/dev/corectrl`, the misc ioctl nodes, the MIDI
UART, evdev input, the framebuffer, GPIO, the wpa_supplicant socket and power actions. This is
the only crate that allows `unsafe` code. Each `unsafe` block has a `SAFETY:` comment.

Nothing here opens a device by itself: a caller must name a node and call `open`. Target: 32-bit
ARM Linux 4.0 with glibc 2.18. The host tests use pipes, socket pairs, FIFOs and ptys only.

Part of [Tetrad](../../README.md).
