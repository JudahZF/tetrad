# tetrad-hal

The hardware layer of the stock `libzenhal`: typed request builders and record decoders for the
analog front end, the ESS codec, inputs, MIDI, the UART, LEDs and the jack nodes. The `hal::Hal`
trait has a stock implementation over an injected `hal::Driver` and a recording fake. No device
access: `tetrad-sys` implements `Driver` on Linux.

Part of [Tetrad](../../README.md).
