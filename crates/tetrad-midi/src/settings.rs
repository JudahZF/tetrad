//! Global MIDI settings as a typed value. Stock keeps them in the `[MidiSettings]`
//! group of `ZenjackConfig.ini` (load `0x0032e7e4`, save `0x0032e024`). tetrad-settings
//! is the only owner of that file. It maps its MIDI group into [`MidiSettings`].

use crate::message::Channel;

/// Receive channel. Stock stores 1..=16, or 17 for OMNI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChannelSetting {
    Channel(Channel),
    Omni,
}

impl ChannelSetting {
    pub const OMNI_RAW: i32 = 17;

    /// Stock remote setter `0x004163c4` accepts only 1..=17.
    pub fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            Self::OMNI_RAW => Some(Self::Omni),
            _ => Channel::from_number(u32::try_from(raw).ok()?).map(Self::Channel),
        }
    }

    pub fn raw(self) -> i32 {
        match self {
            Self::Channel(channel) => i32::from(channel.number()),
            Self::Omni => Self::OMNI_RAW,
        }
    }

    /// CC and PC filter of `onMessageReceived` (`0x0032ff40`).
    pub fn accepts(self, channel: Channel) -> bool {
        match self {
            Self::Channel(own) => own == channel,
            Self::Omni => true,
        }
    }
}

/// `MidiClockOut` / `ClockOutEn` (combo "OFF", DIN, "USB", "USB+DIN").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ClockOut {
    #[default]
    Off,
    Din,
    Usb,
    Both,
}

/// Plain typed value. All fields are public. Start from [`Default`] and set fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MidiSettings {
    /// Last source shown on the Preset MIDI Out page (0..=9).
    pub last_message_source: u8,
    pub channel: ChannelSetting,
    /// The OUT jack works as THRU (hardware mux in the zenjack driver).
    pub thru: bool,
    pub over_usb: bool,
    pub clock_out: ClockOut,
    pub ignore_duplicate_pc: bool,
    pub clock_in: bool,
    /// Stored and saved; stock has no reader for these two.
    pub pc_input: i32,
    pub pc_output: i32,
}

impl Default for MidiSettings {
    /// Reset-to-defaults `0x0033099c`: channel 1, MIDI over USB on, the
    /// rest off.
    fn default() -> Self {
        Self {
            last_message_source: 0,
            channel: ChannelSetting::Channel(Channel::from_index(0).expect("channel 1")),
            thru: false,
            over_usb: true,
            clock_out: ClockOut::Off,
            ignore_duplicate_pc: false,
            clock_in: false,
            pc_input: 0,
            pc_output: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stock channel range: 1..=16 and OMNI (17). Zero and 18 are rejected.
    #[test]
    fn channel_raw_range() {
        assert_eq!(ChannelSetting::from_raw(17), Some(ChannelSetting::Omni));
        assert_eq!(ChannelSetting::from_raw(0), None);
        assert_eq!(ChannelSetting::from_raw(18), None);
        for raw in 1..=16 {
            assert_eq!(
                ChannelSetting::from_raw(raw).map(ChannelSetting::raw),
                Some(raw)
            );
        }
    }
}
