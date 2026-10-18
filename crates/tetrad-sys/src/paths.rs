//! Stock device node paths. Constants only; nothing here opens a node. Knob/stomp nodes: `input::KnobStompIndex::node_path`. Codec:
//! `tetrad_hal::codec::DEVICE_PATH`.

/// zencoder misc control (`CZencoder::Open` VA 0x173a8): element LEDs `0x4005e101`, volume `0x4004e102`,
/// firmware `0x4004e103/5`, board revision `0x4004e108`, unique ID `0x4010e109`.
pub const ZENCODER_CONTROL: &str = "/dev/zencoder/fd";
/// Tempo LED `0x4005e106`, default tempo LED `0x4004e107` (NULL), tempo `0x4001e101` (1 byte).
pub const ZENCODER_TEMPO_CONTROL: &str = "/dev/zencoder/tempoctrl";
/// evdev record nodes for `input::RecordNode`.
pub const ZENCODER_VOLUME: &str = "/dev/zencoder/volume";
pub const ZENCODER_TEMPO: &str = "/dev/zencoder/tempo";

/// Display control (`CZendisplay::Open` VA 0x18a38), a symlink to [`ZENDISPLAY_DEVICE`].
pub const ZENDISPLAY_CONTROL: &str = "/dev/zendisplay/fd";
/// `brightnessctrl` (VA 0x84d4) opens this node directly with `O_RDWR`.
pub const ZENDISPLAY_DEVICE: &str = "/dev/zendisplay-dev";
/// `_IOR('a', 0x62, int)` = `0x80046162`, read once at display open; values 0/1.
pub const TOUCH_INFO: &str = "/dev/touchinfo";
/// `TSLIB_TSDEVICE` in the init scripts: an evdev node for `input::RecordNode`.
pub const TOUCH_EVENTS: &str = "/dev/input/event0";
/// Pixel sink of `zendisplay_test` (VA 0x8904).
pub const FRAMEBUFFER: &str = "/dev/fb0";

/// zenjack misc control (`CZenjack::Open` VA 0x1d1a4): analog `0x4004e000..0x4004e037`, status `0x4010e024`.
pub const ZJACK_SOUNDCARD: &str = "/dev/zjack/soundcard";
/// evdev expression pedals 1 and 2.
pub const ZJACK_EXPRESSION: [&str; 2] = [
    "/dev/zjack/soundcard_express1",
    "/dev/zjack/soundcard_express2",
];
/// `jack::OverloadNode`.
pub const ZJACK_OVERLOAD: [&str; 2] = ["/dev/zjack/soundcard_over1", "/dev/zjack/soundcard_over2"];
/// `jack::MiscRecordNode` with `MiscRecordDriver::InputDetector`.
pub const ZJACK_INPUT: &str = "/dev/zjack/soundcard_input";
/// `jack::MiscRecordNode` with `MiscRecordDriver::Capsense`.
pub const ZJACK_POWER_BUTTON: &str = "/dev/zjack/soundcard_power_button";

/// MIDI DIN UART (`CMIDI::Open` VA 0x1d644), `serial::MidiUart`. ZenUI opens it only when 0x0049c14c passes
/// (`tetrad_hal::platform::Platform::uses_midi_uart`; not on the mini).
pub const MIDI_UART: &str = "/dev/ttySC1";

/// Physical memory (`mem::PhysicalWindow`), opened `O_RDWR | O_SYNC` (ZenUI VA 0x54df98).
pub const DEV_MEM: &str = "/dev/mem";
/// SHARC core control (`corectrl::CoreControlNode`), opened `O_RDWR` (ZenUI VA 0x8a8420).
pub const CORECTRL: &str = "/dev/corectrl";
/// SHARC notification lines (`uio::UioDevice`), opened `O_RDWR` (VA 0x8a778c, 0x8a7bbc). `uio2` (DIN clock out)
/// is opened only on a full-size QC with GPIO 97 high.
pub const UIO: [&str; 3] = ["/dev/uio0", "/dev/uio1", "/dev/uio2"];
/// Sysfs GPIO root (`gpio::SysfsGpio`).
pub const SYSFS_GPIO: &str = "/sys/class/gpio";
