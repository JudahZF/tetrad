//! MIDI 1.0 message model and the byte encoder used for DIN output.

use std::fmt;

/// A seven-bit MIDI data value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct U7(u8);

impl U7 {
    pub const MIN: Self = Self(0);
    pub const MAX: Self = Self(127);

    pub const fn new(value: u8) -> Option<Self> {
        if value <= 127 {
            Some(Self(value))
        } else {
            None
        }
    }

    /// Keep the low seven bits, as a UART data byte does.
    pub const fn masked(value: u32) -> Self {
        Self((value & 0x7f) as u8)
    }

    pub const fn get(self) -> u8 {
        self.0
    }
}

impl fmt::Display for U7 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// A MIDI channel. The stock UI and settings count channels from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Channel(u8);

impl Channel {
    /// From a zero-based index, 0..=15.
    pub const fn from_index(index: u8) -> Option<Self> {
        if index < 16 { Some(Self(index)) } else { None }
    }

    /// From a one-based channel number, 1..=16.
    pub const fn from_number(number: u32) -> Option<Self> {
        if number >= 1 && number <= 16 {
            Some(Self((number - 1) as u8))
        } else {
            None
        }
    }

    pub const fn index(self) -> u8 {
        self.0
    }

    pub const fn number(self) -> u8 {
        self.0 + 1
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.number().fmt(f)
    }
}

/// System real-time messages. They may appear between any two bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Realtime {
    TimingClock,
    Start,
    Continue,
    Stop,
    ActiveSensing,
    Reset,
}

impl Realtime {
    pub const fn from_byte(byte: u8) -> Option<Self> {
        Some(match byte {
            0xf8 => Self::TimingClock,
            0xfa => Self::Start,
            0xfb => Self::Continue,
            0xfc => Self::Stop,
            0xfe => Self::ActiveSensing,
            0xff => Self::Reset,
            _ => return None,
        })
    }

    pub const fn byte(self) -> u8 {
        match self {
            Self::TimingClock => 0xf8,
            Self::Start => 0xfa,
            Self::Continue => 0xfb,
            Self::Stop => 0xfc,
            Self::ActiveSensing => 0xfe,
            Self::Reset => 0xff,
        }
    }
}

/// One complete MIDI message.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MidiMessage {
    NoteOff {
        channel: Channel,
        key: U7,
        velocity: U7,
    },
    NoteOn {
        channel: Channel,
        key: U7,
        velocity: U7,
    },
    PolyPressure {
        channel: Channel,
        key: U7,
        pressure: U7,
    },
    ControlChange {
        channel: Channel,
        controller: U7,
        value: U7,
    },
    ProgramChange {
        channel: Channel,
        program: U7,
    },
    ChannelPressure {
        channel: Channel,
        pressure: U7,
    },
    /// 14-bit value, 0..=0x3fff; 0x2000 is centre.
    PitchBend {
        channel: Channel,
        value: u16,
    },
    TimeCodeQuarterFrame(U7),
    /// 14-bit MIDI beat count.
    SongPosition(u16),
    SongSelect(U7),
    TuneRequest,
    /// Data bytes without the F0/F7 delimiters.
    SysEx(Vec<u8>),
    Realtime(Realtime),
}

impl MidiMessage {
    pub const fn channel(&self) -> Option<Channel> {
        match self {
            Self::NoteOff { channel, .. }
            | Self::NoteOn { channel, .. }
            | Self::PolyPressure { channel, .. }
            | Self::ControlChange { channel, .. }
            | Self::ProgramChange { channel, .. }
            | Self::ChannelPressure { channel, .. }
            | Self::PitchBend { channel, .. } => Some(*channel),
            _ => None,
        }
    }

    /// Append the complete wire form (always with a status byte; no running
    /// status on output). SysEx data bytes above 0x7f are masked.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        let channel_status = |base: u8, channel: &Channel| base | channel.index();
        let split14 = |value: u16| [(value & 0x7f) as u8, ((value >> 7) & 0x7f) as u8];
        match self {
            Self::NoteOff {
                channel,
                key,
                velocity,
            } => {
                out.extend([channel_status(0x80, channel), key.get(), velocity.get()]);
            }
            Self::NoteOn {
                channel,
                key,
                velocity,
            } => {
                out.extend([channel_status(0x90, channel), key.get(), velocity.get()]);
            }
            Self::PolyPressure {
                channel,
                key,
                pressure,
            } => {
                out.extend([channel_status(0xa0, channel), key.get(), pressure.get()]);
            }
            Self::ControlChange {
                channel,
                controller,
                value,
            } => {
                out.extend([channel_status(0xb0, channel), controller.get(), value.get()]);
            }
            Self::ProgramChange { channel, program } => {
                out.extend([channel_status(0xc0, channel), program.get()]);
            }
            Self::ChannelPressure { channel, pressure } => {
                out.extend([channel_status(0xd0, channel), pressure.get()]);
            }
            Self::PitchBend { channel, value } => {
                out.push(channel_status(0xe0, channel));
                out.extend(split14(*value));
            }
            Self::TimeCodeQuarterFrame(value) => out.extend([0xf1, value.get()]),
            Self::SongPosition(beats) => {
                out.push(0xf2);
                out.extend(split14(*beats));
            }
            Self::SongSelect(song) => out.extend([0xf3, song.get()]),
            Self::TuneRequest => out.push(0xf6),
            Self::SysEx(data) => {
                out.push(0xf0);
                out.extend(data.iter().map(|byte| byte & 0x7f));
                out.push(0xf7);
            }
            Self::Realtime(realtime) => out.push(realtime.byte()),
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(3);
        self.encode_into(&mut out);
        out
    }
}
