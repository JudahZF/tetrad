//! Device nodes that stock libzenhal opens, with their ARM open flags.
//! Paths: libzenhal `.rodata` 0xf334..0xfa48.

/// ARM 32-bit `open` flag values (`asm-generic/fcntl.h`, ARM `asm/fcntl.h`). They differ from x86.
pub mod flags {
    pub const O_RDONLY: u32 = 0;
    pub const O_RDWR: u32 = 0o2;
    pub const O_NOCTTY: u32 = 0o400;
    pub const O_NONBLOCK: u32 = 0o4000;
    /// `__O_SYNC | O_DSYNC`.
    pub const O_SYNC: u32 = 0o4010000;
}

/// Zero-based footswitch/encoder element, `element_idx_t` 0..=10 (`knob_stomp1..11`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Element(u8);

impl Element {
    pub const COUNT: u8 = 11;

    pub fn all() -> impl Iterator<Item = Self> {
        (0..Self::COUNT).map(Self)
    }

    pub fn index(self) -> u8 {
        self.0
    }
}

impl TryFrom<u8> for Element {
    type Error = InvalidElement;

    /// Stock rejects index > 10 (`SetLedColor` VA 0x66d4, `GetKnobStompEvent` VA 0x72a8).
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value < Self::COUNT {
            Ok(Self(value))
        } else {
            Err(InvalidElement(value))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("element index {0} is outside 0..=10")]
pub struct InvalidElement(pub u8);

/// `expression_nbr_t` 0/1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Pedal {
    One,
    Two,
}

/// The two overload (clip) indicator nodes `soundcard_over1/2`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum OverloadChannel {
    One,
    Two,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Node {
    /// `/dev/zencoder/fd`: LED, volume, version and ID ioctls.
    Zencoder,
    /// `/dev/zencoder/knob_stompN`, evdev records.
    KnobStomp(Element),
    /// `/dev/zencoder/volume`, evdev records.
    Volume,
    /// `/dev/zencoder/tempo`, evdev records.
    Tempo,
    /// `/dev/zencoder/tempoctrl`: tempo and tempo LED ioctls.
    TempoControl,
    /// `/dev/zendisplay/fd`: backlight control, not the framebuffer.
    Display,
    /// `/dev/touchinfo`: touch controller identity ioctl.
    TouchInfo,
    /// `/dev/zjack/soundcard`: zenjack misc ioctls.
    Soundcard,
    /// `/dev/zjack/soundcard_express1/2`, evdev records.
    Expression(Pedal),
    /// `/dev/zjack/soundcard_over1/2`: blocking zero-length read, no `poll`.
    Overload(OverloadChannel),
    /// `/dev/zjack/soundcard_input`: jack detection, one record per positive poll.
    JackInput,
    /// `/dev/zjack/soundcard_power_button`: one record per positive poll.
    PowerButton,
    /// `/dev/zencodec_es9017_es9826` (`CodecOpen` VA 0x98b8).
    Codec,
    /// `/dev/ttySC1`, the MIDI UART (`CMIDI::Open` VA 0xd644).
    MidiUart,
}

const KNOB_STOMP_PATHS: [&str; Element::COUNT as usize] = [
    "/dev/zencoder/knob_stomp1",
    "/dev/zencoder/knob_stomp2",
    "/dev/zencoder/knob_stomp3",
    "/dev/zencoder/knob_stomp4",
    "/dev/zencoder/knob_stomp5",
    "/dev/zencoder/knob_stomp6",
    "/dev/zencoder/knob_stomp7",
    "/dev/zencoder/knob_stomp8",
    "/dev/zencoder/knob_stomp9",
    "/dev/zencoder/knob_stomp10",
    "/dev/zencoder/knob_stomp11",
];

impl Node {
    pub fn path(self) -> &'static str {
        match self {
            Self::Zencoder => "/dev/zencoder/fd",
            Self::KnobStomp(element) => KNOB_STOMP_PATHS[usize::from(element.0)],
            Self::Volume => "/dev/zencoder/volume",
            Self::Tempo => "/dev/zencoder/tempo",
            Self::TempoControl => "/dev/zencoder/tempoctrl",
            Self::Display => "/dev/zendisplay/fd",
            Self::TouchInfo => "/dev/touchinfo",
            Self::Soundcard => "/dev/zjack/soundcard",
            Self::Expression(Pedal::One) => "/dev/zjack/soundcard_express1",
            Self::Expression(Pedal::Two) => "/dev/zjack/soundcard_express2",
            Self::Overload(OverloadChannel::One) => "/dev/zjack/soundcard_over1",
            Self::Overload(OverloadChannel::Two) => "/dev/zjack/soundcard_over2",
            Self::JackInput => "/dev/zjack/soundcard_input",
            Self::PowerButton => "/dev/zjack/soundcard_power_button",
            Self::Codec => "/dev/zencodec_es9017_es9826",
            Self::MidiUart => "/dev/ttySC1",
        }
    }

    /// Stock opens every node `O_RDONLY | O_NONBLOCK` (0x800), ioctl nodes included,
    /// and the MIDI UART `0x101102`. Stock never passes `O_CLOEXEC`.
    pub fn open_flags(self) -> u32 {
        match self {
            Self::MidiUart => flags::O_RDWR | flags::O_NOCTTY | flags::O_SYNC,
            _ => flags::O_RDONLY | flags::O_NONBLOCK,
        }
    }

    /// Every node stock opens at start-up, in `CZencoder::Open`, `CZendisplay::Open`,
    /// `CZenjack::Open`, `CodecOpen` and `CMIDI::Open` order.
    pub fn all() -> impl Iterator<Item = Self> {
        [Self::Zencoder]
            .into_iter()
            .chain(Element::all().map(Self::KnobStomp))
            .chain([
                Self::Volume,
                Self::Tempo,
                Self::TempoControl,
                Self::Display,
                Self::TouchInfo,
                Self::Soundcard,
                Self::Expression(Pedal::One),
                Self::Expression(Pedal::Two),
                Self::Overload(OverloadChannel::One),
                Self::Overload(OverloadChannel::Two),
                Self::JackInput,
                Self::PowerButton,
                Self::Codec,
                Self::MidiUart,
            ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_and_flags_match_stock_open_calls() {
        assert_eq!(Node::all().count(), 26);
        assert_eq!(
            Node::KnobStomp(Element::try_from(10).unwrap()).path(),
            "/dev/zencoder/knob_stomp11"
        );
        assert_eq!(Element::try_from(11), Err(InvalidElement(11)));
        assert_eq!(Node::Volume.open_flags(), 0x800);
        assert_eq!(Node::MidiUart.open_flags(), 0x0010_1102);
    }
}
