use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Brightness(u8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidBrightness(pub i32);

impl fmt::Display for InvalidBrightness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "brightness {} is outside 0..=100", self.0)
    }
}
impl std::error::Error for InvalidBrightness {}

impl TryFrom<i32> for Brightness {
    type Error = InvalidBrightness;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        if (0..=100).contains(&value) {
            Ok(Self(value as u8))
        } else {
            Err(InvalidBrightness(value))
        }
    }
}

impl Brightness {
    pub fn percent(self) -> u8 {
        self.0
    }
}

/// The ioctl argument. Except `Null`, every variant is one in/out buffer whose
/// length equals the `_IOC` size field of the request number (ARM 32-bit layout).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoctlPayload {
    /// A NULL pointer, not a pointer to an empty Rust slice.
    Null,
    Integer([u8; 4]),
    ElementLed([u8; 5]),
    /// `__tempo_cfg`, request `0x4001e101`.
    Byte([u8; 1]),
    /// Board unique ID and jack status blocks (`0x4010....`).
    Block([u8; 16]),
}

impl IoctlPayload {
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Null => None,
            Self::Integer(b) => Some(b),
            Self::ElementLed(b) => Some(b),
            Self::Byte(b) => Some(b),
            Self::Block(b) => Some(b),
        }
    }

    /// The buffer a backend passes to the kernel; the kernel may overwrite it.
    pub fn bytes_mut(&mut self) -> Option<&mut [u8]> {
        match self {
            Self::Null => None,
            Self::Integer(b) => Some(b),
            Self::ElementLed(b) => Some(b),
            Self::Byte(b) => Some(b),
            Self::Block(b) => Some(b),
        }
    }

    /// The first word as a signed LE integer, for `Integer` payloads.
    pub fn word(&self) -> Option<i32> {
        match self {
            Self::Integer(b) => Some(i32::from_le_bytes(*b)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoctlRequest {
    pub number: u32,
    pub payload: IoctlPayload,
}

impl IoctlRequest {
    /// One four-byte LE word, the argument shape of most libzenhal requests.
    pub fn word(number: u32, value: i32) -> Self {
        Self {
            number,
            payload: IoctlPayload::Integer(value.to_le_bytes()),
        }
    }

    /// `_IOC_SIZE` of the request number. Legacy numbers without `_IOC`
    /// direction bits (display `0xe203..0xe205`) report 0.
    pub fn encoded_size(&self) -> usize {
        ((self.number >> 16) & 0x3fff) as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayCommand {
    Brightness(Brightness),
    Enable,
    Disable,
    Reset,
}

impl DisplayCommand {
    pub fn request(self) -> IoctlRequest {
        let (number, payload) = match self {
            Self::Brightness(b) => (
                0x4004_e206,
                IoctlPayload::Integer(i32::from(b.0).to_le_bytes()),
            ),
            Self::Enable => (0xe203, IoctlPayload::Null),
            Self::Disable => (0xe204, IoctlPayload::Null),
            Self::Reset => (0xe205, IoctlPayload::Null),
        };
        IoctlRequest { number, payload }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ElementLed {
    pub index: u8,
    pub red: u8,
    pub green: u8,
    pub blue: u8,
    /// Brightness. ZenUI computes it (0x00668e98); libzenhal passes it through.
    pub opaque: u8,
}

impl ElementLed {
    pub fn request(self) -> IoctlRequest {
        IoctlRequest {
            number: 0x4005_e101,
            payload: IoctlPayload::ElementLed([
                self.index,
                self.red,
                self.green,
                self.blue,
                self.opaque,
            ]),
        }
    }
}

/// Future syscall backends must perform the blocking zero-length read even
/// though normal `Read::read(&mut [])` implementations may return immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverloadOperation {
    BlockingZeroLengthRead,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_checks_range_and_keeps_null_distinct() {
        for value in [-1, 101, i32::MIN, i32::MAX] {
            assert!(Brightness::try_from(value).is_err());
        }
        for value in [0, 100] {
            let b = Brightness::try_from(value).unwrap();
            assert_eq!(i32::from(b.percent()), value);
            let r = DisplayCommand::Brightness(b).request();
            assert_eq!(r.number, 0x4004_e206);
            assert_eq!(r.payload.bytes(), Some(value.to_le_bytes().as_slice()));
        }
        for (cmd, number) in [
            (DisplayCommand::Enable, 0xe203),
            (DisplayCommand::Disable, 0xe204),
            (DisplayCommand::Reset, 0xe205),
        ] {
            let r = cmd.request();
            assert_eq!(r.number, number);
            assert_eq!(r.payload, IoctlPayload::Null);
            assert_eq!(r.payload.bytes(), None);
        }
    }

    #[test]
    fn led_payload_is_exactly_five_bytes_with_opaque_last_byte() {
        let r = ElementLed {
            index: 255,
            red: 1,
            green: 2,
            blue: 3,
            opaque: 0xa5,
        }
        .request();
        assert_eq!(r.number, 0x4005_e101);
        assert_eq!(r.payload.bytes(), Some([255, 1, 2, 3, 0xa5].as_slice()));
    }
}
