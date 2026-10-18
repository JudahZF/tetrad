//! Kernel ioctl ABI of the ES9017/ES9826 codec module (`zencodec_es9017_es9826.ko`,
//! `zencodec_ioctl` `.text+0xd00`). Pure values: no syscalls, no board detection.
//!
//! A request built here proves only the kernel ABI. It does not prove that this module is loaded,
//! that the board carries the target ADC, or which jack a control drives. Impedance (codes
//! `00`/`01`) and lift/48 V (`06..09`) drive the same GPIO nibbles; stock libzenhal uses the first
//! only for `_hwType` 1 and the second only for `_hwType` 2.
use crate::requests::{IoctlPayload, IoctlRequest};

/// Misc device name is confirmed; the `/dev` node path assumes devtmpfs.
pub const DEVICE_PATH: &str = "/dev/zencodec_es9017_es9826";

/// Initial getter word. Every getter path stores a zero-extended byte, so the
/// driver never writes this value. If it comes back unchanged, the driver did not answer.
pub const GETTER_SENTINEL: u32 = 0xffff_ffff;

/// The 20 dispatch codes, `0x4004e200 + nr`. Every code is `_IOW(0xe2, nr, int)`,
/// getters included. Use `code as u32` for the request number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum CodecIoctl {
    Inst1ImpedanceSet = 0x4004_e200,
    Inst2ImpedanceSet = 0x4004_e201,
    Inst1GainSet = 0x4004_e202,
    Inst2GainSet = 0x4004_e203,
    Ret1GainSet = 0x4004_e204,
    Ret2GainSet = 0x4004_e205,
    GndLiftXlr1Set = 0x4004_e206,
    GndLiftXlr2Set = 0x4004_e207,
    GndLiftComboSet = 0x4004_e208,
    Phantom48vSet = 0x4004_e209,
    Inst1GainGet = 0x4004_e20a,
    Inst2GainGet = 0x4004_e20b,
    Ret1GainGet = 0x4004_e20c,
    Ret2GainGet = 0x4004_e20d,
    GndLiftXlr1Get = 0x4004_e20e,
    GndLiftXlr2Get = 0x4004_e20f,
    GndLiftComboGet = 0x4004_e210,
    Phantom48vGet = 0x4004_e211,
    Inst1ImpedanceGet = 0x4004_e212,
    Inst2ImpedanceGet = 0x4004_e213,
}

/// ES9826 `adc-id`. The driver answers 0 without action when this ADC is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Adc {
    One,
    Two,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// Reads one word from the caller; only the return code comes back.
    Set,
    /// Ignores the caller's word and overwrites it, but only when the ADC exists.
    Get,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueKind {
    Impedance,
    Gain,
    Flag,
}

impl CodecIoctl {
    pub const ALL: [Self; 20] = [
        Self::Inst1ImpedanceSet,
        Self::Inst2ImpedanceSet,
        Self::Inst1GainSet,
        Self::Inst2GainSet,
        Self::Ret1GainSet,
        Self::Ret2GainSet,
        Self::GndLiftXlr1Set,
        Self::GndLiftXlr2Set,
        Self::GndLiftComboSet,
        Self::Phantom48vSet,
        Self::Inst1GainGet,
        Self::Inst2GainGet,
        Self::Ret1GainGet,
        Self::Ret2GainGet,
        Self::GndLiftXlr1Get,
        Self::GndLiftXlr2Get,
        Self::GndLiftComboGet,
        Self::Phantom48vGet,
        Self::Inst1ImpedanceGet,
        Self::Inst2ImpedanceGet,
    ];

    pub fn direction(self) -> Direction {
        if (self as u32) < Self::Inst1GainGet as u32 {
            Direction::Set
        } else {
            Direction::Get
        }
    }

    /// Note: INST2 impedance is on ADC 2, but INST2 gain is on ADC 1.
    pub fn adc(self) -> Adc {
        use CodecIoctl::*;
        match self {
            Inst1ImpedanceSet | Inst1GainSet | Inst2GainSet | GndLiftXlr1Set | GndLiftXlr2Set
            | Inst1GainGet | Inst2GainGet | GndLiftXlr1Get | GndLiftXlr2Get | Inst1ImpedanceGet => {
                Adc::One
            }
            _ => Adc::Two,
        }
    }

    pub fn kind(self) -> ValueKind {
        use CodecIoctl::*;
        match self {
            Inst1ImpedanceSet | Inst2ImpedanceSet | Inst1ImpedanceGet | Inst2ImpedanceGet => {
                ValueKind::Impedance
            }
            Inst1GainSet | Inst2GainSet | Ret1GainSet | Ret2GainSet | Inst1GainGet
            | Inst2GainGet | Ret1GainGet | Ret2GainGet => ValueKind::Gain,
            _ => ValueKind::Flag,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImpedanceInput {
    Inst1,
    Inst2,
}

/// Kernel channel names. `r0x44[7:4]` for INST1/RET1, `[3:0]` for INST2/RET2.
/// Stock HAL `ret n` swaps between `_hwType` 1 and 2; see `analog`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GainChannel {
    Inst1,
    Inst2,
    Ret1,
    Ret2,
}

/// Kernel boolean controls, all with direct set/get polarity at the ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Flag {
    GndLiftXlr1,
    GndLiftXlr2,
    /// Stored inverted in GPIO mask bit 3; the getter inverts it back.
    GndLiftCombo,
    Phantom48v,
}

impl Flag {
    fn bit(self) -> u8 {
        match self {
            Self::GndLiftXlr1 | Self::Phantom48v => 0x10,
            Self::GndLiftXlr2 | Self::GndLiftCombo => 0x08,
        }
    }
}

/// Setter impedance index 0..=8. Indices 0 and 8 both write GPIO mask 0, so a
/// read-back of 8 is 0: compare `mask()`, not `index()`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Impedance(u8);

impl TryFrom<u8> for Impedance {
    type Error = CodecError;

    /// The driver range-checks only the low byte of the word (`ldrb`, `> 8` is
    /// `-EINVAL`); this type rejects what the driver would silently truncate.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value <= 8 {
            Ok(Self(value))
        } else {
            Err(CodecError::InvalidImpedance(value))
        }
    }
}

impl Impedance {
    pub fn index(self) -> u8 {
        self.0
    }

    /// `instz_lut` (`.rodata+0x258`): index i -> `i << 3`, index 8 -> 0.
    pub fn mask(self) -> GpioMask {
        GpioMask(if self.0 == 8 { 0 } else { self.0 << 3 })
    }
}

/// Kernel `r0x44` nibble 0..=15. This is wider than the stock HAL PGA cap of 10
/// (`analog::HardwareType::maximum_gain`); the HAL cap is policy, not ABI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KernelGain(u8);

impl TryFrom<u8> for KernelGain {
    type Error = CodecError;

    /// The driver keeps only the low nibble of the word's low byte.
    fn try_from(value: u8) -> Result<Self, Self::Error> {
        if value <= 0xf {
            Ok(Self(value))
        } else {
            Err(CodecError::InvalidGain(value))
        }
    }
}

impl KernelGain {
    pub fn value(self) -> u8 {
        self.0
    }
}

/// ES9826 GPIO mask: bits 3..=5 only, one per nibble of `r0x1b`/`r0x1c`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct GpioMask(u8);

impl GpioMask {
    pub fn bits(self) -> u8 {
        self.0
    }

    /// Kernel `gpio_get` (`.text+0x96c`): a bit is set only when its nibble is
    /// exactly 0xF (`r0x1b[7:4]` bit 3, `r0x1c[3:0]` bit 4, `r0x1c[7:4]` bit 5).
    pub fn from_registers(r1b: u8, r1c: u8) -> Self {
        let bit = |nibble: u8, bit: u8| if nibble == 0xf { bit } else { 0 };
        Self(bit(r1b >> 4, 0x08) | bit(r1c & 0xf, 0x10) | bit(r1c >> 4, 0x20))
    }

    /// Kernel `gpio_set` (`.text+0x83c`): new `(r0x1b, r0x1c)`. Keeps `r0x1b[3:0]`
    /// and writes each selected nibble as 0xF (bit set) or 0xE (clear).
    pub fn registers(self, r1b: u8) -> (u8, u8) {
        let nibble = |bit: u8| if self.0 & bit != 0 { 0xf } else { 0xe };
        (
            (nibble(0x08) << 4) | (r1b & 0x0f),
            nibble(0x10) | (nibble(0x20) << 4),
        )
    }

    /// Getter cases `0e..11`.
    pub fn flag(self, flag: Flag) -> bool {
        (self.0 & flag.bit() != 0) != (flag == Flag::GndLiftCombo)
    }

    /// Setter cases `06..09`: change one bit of the current mask, keep the others.
    pub fn with_flag(self, flag: Flag, on: bool) -> Self {
        if on != (flag == Flag::GndLiftCombo) {
            Self(self.0 | flag.bit())
        } else {
            Self(self.0 & !flag.bit())
        }
    }

    /// Kernel `z_get` (`.text+0xa24`): the first LUT index with this mask. Every
    /// bit-3..5 mask matches an index 0..=7, so mask 0 maps to 0, never 8.
    pub fn impedance(self) -> Impedance {
        Impedance(self.0 >> 3)
    }
}

/// A getter target. Each maps to a kernel GET code, never to a setter code
/// (stock `GetXlrOutGroundLiftStatus` issues SET codes `06`/`07` instead).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Query {
    Impedance(ImpedanceInput),
    Gain(GainChannel),
    Flag(Flag),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Setting {
    Impedance(ImpedanceInput, Impedance),
    Gain(GainChannel, KernelGain),
    Flag(Flag, bool),
}

/// A decoded getter word. It is the driver's answer, not proof of hardware state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reading {
    /// Index 0..=7 only. 0 also stands for a written 8.
    Impedance(Impedance),
    Gain(KernelGain),
    Flag(bool),
}

/// A zero setter return. It is not proof of a hardware change: the driver also
/// returns 0 without action when the target ADC is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Acknowledged;

impl Acknowledged {
    /// `raw_return` is the kernel return (negative errno, or a positive helper status).
    pub fn from_return(raw_return: i32) -> Result<Self, CodecError> {
        if raw_return == 0 {
            Ok(Self)
        } else {
            Err(CodecError::Return(raw_return))
        }
    }
}

impl Query {
    /// (setter, getter) code pair for this target.
    fn codes(self) -> (CodecIoctl, CodecIoctl) {
        use CodecIoctl::*;
        match self {
            Self::Impedance(ImpedanceInput::Inst1) => (Inst1ImpedanceSet, Inst1ImpedanceGet),
            Self::Impedance(ImpedanceInput::Inst2) => (Inst2ImpedanceSet, Inst2ImpedanceGet),
            Self::Gain(GainChannel::Inst1) => (Inst1GainSet, Inst1GainGet),
            Self::Gain(GainChannel::Inst2) => (Inst2GainSet, Inst2GainGet),
            Self::Gain(GainChannel::Ret1) => (Ret1GainSet, Ret1GainGet),
            Self::Gain(GainChannel::Ret2) => (Ret2GainSet, Ret2GainGet),
            Self::Flag(Flag::GndLiftXlr1) => (GndLiftXlr1Set, GndLiftXlr1Get),
            Self::Flag(Flag::GndLiftXlr2) => (GndLiftXlr2Set, GndLiftXlr2Get),
            Self::Flag(Flag::GndLiftCombo) => (GndLiftComboSet, GndLiftComboGet),
            Self::Flag(Flag::Phantom48v) => (Phantom48vSet, Phantom48vGet),
        }
    }

    pub fn ioctl(self) -> CodecIoctl {
        self.codes().1
    }

    /// The payload is also the output buffer: a backend passes a writable
    /// four-byte word initialized to these bytes and hands the word back.
    pub fn request(self) -> IoctlRequest {
        IoctlRequest {
            number: self.ioctl() as u32,
            payload: IoctlPayload::Integer(GETTER_SENTINEL.to_le_bytes()),
        }
    }

    /// Decodes the kernel return and the word after the call.
    pub fn decode(self, raw_return: i32, word: [u8; 4]) -> Result<Reading, CodecError> {
        if raw_return != 0 {
            return Err(CodecError::Return(raw_return));
        }
        let word = u32::from_le_bytes(word);
        match (self, word) {
            (_, GETTER_SENTINEL) => Err(CodecError::NoResponse),
            (Self::Impedance(_), 0..=7) => Ok(Reading::Impedance(Impedance(word as u8))),
            (Self::Impedance(_), 8) => Err(CodecError::ImpedanceUnavailable),
            (Self::Gain(_), 0..=0xf) => Ok(Reading::Gain(KernelGain(word as u8))),
            (Self::Gain(_), 0xff) => Err(CodecError::GainReadFailed),
            (Self::Flag(_), 0 | 1) => Ok(Reading::Flag(word == 1)),
            _ => Err(CodecError::UnexpectedWord(word)),
        }
    }
}

impl Setting {
    /// The getter that reads this setting back.
    pub fn target(self) -> Query {
        match self {
            Self::Impedance(input, _) => Query::Impedance(input),
            Self::Gain(channel, _) => Query::Gain(channel),
            Self::Flag(flag, _) => Query::Flag(flag),
        }
    }

    pub fn ioctl(self) -> CodecIoctl {
        self.target().codes().0
    }

    /// One four-byte LE word. Booleans are canonical 0/1.
    pub fn request(self) -> IoctlRequest {
        let word = match self {
            Self::Impedance(_, impedance) => u32::from(impedance.0),
            Self::Gain(_, gain) => u32::from(gain.0),
            Self::Flag(_, on) => u32::from(on),
        };
        IoctlRequest {
            number: self.ioctl() as u32,
            payload: IoctlPayload::Integer(word.to_le_bytes()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CodecError {
    #[error("impedance index {0} is outside 0..=8")]
    InvalidImpedance(u8),
    #[error("kernel gain {0} is outside the 0..=15 nibble")]
    InvalidGain(u8),
    /// Nonzero kernel return: `-EFAULT`/`-EACCES` copy faults, `-EIO` GPIO read,
    /// `-1` register write/readback, `1` register pre-read in a set helper.
    #[error("codec ioctl returned {0}")]
    Return(i32),
    #[error("getter word unchanged: the driver did not answer (target ADC absent)")]
    NoResponse,
    #[error("gain getter reported a register read failure (0xff)")]
    GainReadFailed,
    #[error("impedance getter returned 8: register read failed or no LUT match")]
    ImpedanceUnavailable,
    #[error("getter word {0:#x} is outside the response contract")]
    UnexpectedWord(u32),
}
