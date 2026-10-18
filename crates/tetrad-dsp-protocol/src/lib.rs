//! Bounded offline DSP codecs and logical transport models. No hardware access.
#![forbid(unsafe_code)]

pub mod address;
pub mod controls;
pub mod deployment;
pub mod fragment;
pub mod hid;
pub mod message_type;
pub mod packet;
pub mod requests;
pub mod responses;
pub mod ring;
pub mod synchronization;
pub mod tuner_looper;

pub use message_type::MessageType;

use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Error {
    #[error("unknown DSP message type {0:#04x}; expected 0x00..=0x5b")]
    MessageType(u8),
    #[error("invalid DSP core {0}; expected 0..=3")]
    DspCore(u8),
    #[error("invalid transport core {0}; expected 0..=4")]
    Core(u8),
    #[error("invalid module slot {0}; expected 0..30")]
    Slot(u32),
    #[error("invalid {0}")]
    Invalid(&'static str),
    #[error("{0} exceeds its configured bound")]
    Limit(&'static str),
    #[error("checked arithmetic overflow in {0}")]
    Overflow(&'static str),
    #[error("LoadModel and ReceiveData on core 0/1 use the direct-memory copy")]
    DirectMemoryRequired,
    #[error("ring is full")]
    RingFull,
    #[error("completion token belongs to a different ring")]
    ForeignToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DspCore {
    Core0 = 0,
    Core1 = 1,
    Core2 = 2,
    Core3 = 3,
}

impl TryFrom<u8> for DspCore {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            0 => Ok(Self::Core0),
            1 => Ok(Self::Core1),
            2 => Ok(Self::Core2),
            3 => Ok(Self::Core3),
            _ => Err(Error::DspCore(value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreId {
    Dsp(DspCore),
    Control,
}

impl CoreId {
    pub const fn wire(self) -> u8 {
        match self {
            Self::Dsp(core) => core as u8,
            Self::Control => 4,
        }
    }
    pub const fn fragment_payload_max(self) -> usize {
        match self {
            Self::Dsp(_) => 2032,
            Self::Control => 128,
        }
    }
}

impl TryFrom<u8> for CoreId {
    type Error = Error;
    fn try_from(value: u8) -> Result<Self, Error> {
        match value {
            0..=3 => Ok(Self::Dsp(DspCore::try_from(value)?)),
            4 => Ok(Self::Control),
            _ => Err(Error::Core(value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModuleSlot(u8);

impl ModuleSlot {
    pub const fn index(self) -> u8 {
        self.0
    }
}

impl TryFrom<u32> for ModuleSlot {
    type Error = Error;
    fn try_from(value: u32) -> Result<Self, Error> {
        if value < 30 {
            Ok(Self(value as u8))
        } else {
            Err(Error::Slot(value))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeploymentTarget {
    pub core: DspCore,
    pub slot: ModuleSlot,
}

#[derive(Debug, Clone, Copy)]
pub struct CodecLimits {
    pub max_message_bytes: usize,
    pub max_records: usize,
}

impl Default for CodecLimits {
    fn default() -> Self {
        Self {
            max_message_bytes: 1024 * 1024,
            max_records: 4096,
        }
    }
}

pub(crate) fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[cfg(test)]
mod tests;
