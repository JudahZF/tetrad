//! `[MidiSettings]` of ZenjackConfig.ini. Load `0x0032e7e4`, save
//! `0x0032e024`, defaults reset `0x0033099c`.
//!
//! This module is the only owner of that group. tetrad-midi keeps its own
//! runtime copy and maps it to and from [`MidiSettings`]; it never reads or
//! writes the file.

use crate::Variant;
use crate::typed::{KeyReader, KeyWriter, bounded, stock_enum};

bounded!(
    /// `Channel`: 1..=16, or 17 for OMNI (channel list `0x00416674`, match
    /// test `stored == msg || stored == 0x11` in `0x0032ff40`).
    MidiChannel: i32, 1, 17, default 1
);

impl MidiChannel {
    pub const OMNI: Self = Self(17);

    pub fn is_omni(self) -> bool {
        self == Self::OMNI
    }
}

stock_enum!(
    /// `ClockOutEn`; same numbering as Cortex Control `MidiClockOut`.
    MidiClockOut {
        Off = 0,
        DinOnly = 1,
        UsbOnly = 2,
        UsbAndDin = 3,
    }
);

#[derive(Clone, Debug, PartialEq)]
pub struct MidiSettings {
    /// `LastMidiMessageSource` (byte): source selected in the MIDI message
    /// editor; indexes 0x1E0-byte message tables.
    pub last_message_source: u8,
    pub channel: MidiChannel,
    /// `Passthru`: MIDI Thru.
    pub thru: bool,
    pub over_usb: bool,
    pub ignore_duplicate_pcs: bool,
    pub clock_out: MidiClockOut,
    pub clock_in: bool,
    /// `PCInput` / `PCOutput`: stored and reset only; no reader found.
    pub pc_input: i32,
    pub pc_output: i32,
}

impl Default for MidiSettings {
    fn default() -> Self {
        Self {
            last_message_source: 0,
            channel: MidiChannel::DEFAULT,
            thru: false,
            over_usb: true,
            ignore_duplicate_pcs: false,
            clock_out: MidiClockOut::Off,
            clock_in: false,
            pc_input: 0,
            pc_output: 0,
        }
    }
}

impl MidiSettings {
    pub(crate) fn load(r: &mut KeyReader) -> Self {
        let d = Self::default();
        let channel = r.int("MidiSettings/Channel", d.channel.get());
        let clock_out = r.int("MidiSettings/ClockOutEn", d.clock_out.to_stock());
        Self {
            // Stock keeps the low byte of toInt().
            last_message_source: r.int("MidiSettings/LastMidiMessageSource", 0) as u8,
            channel: r.check("MidiSettings/Channel", MidiChannel::new(channel), d.channel),
            thru: r.bool("MidiSettings/Passthru", d.thru),
            over_usb: r.bool("MidiSettings/OverUSB", d.over_usb),
            ignore_duplicate_pcs: r.bool("MidiSettings/IgnoreDuplicatePCs", d.ignore_duplicate_pcs),
            clock_out: r.check(
                "MidiSettings/ClockOutEn",
                MidiClockOut::from_stock(clock_out),
                d.clock_out,
            ),
            clock_in: r.bool("MidiSettings/ClockInEn", d.clock_in),
            pc_input: r.int("MidiSettings/PCInput", d.pc_input),
            pc_output: r.int("MidiSettings/PCOutput", d.pc_output),
        }
    }

    pub(crate) fn store(&self, old: &Self, w: &mut KeyWriter) {
        w.put(
            "MidiSettings/LastMidiMessageSource",
            &self.last_message_source,
            &old.last_message_source,
            |v| Variant::UInt(u32::from(*v)),
        );
        w.put("MidiSettings/Channel", &self.channel, &old.channel, |v| {
            Variant::Int(v.get())
        });
        w.bool("MidiSettings/Passthru", self.thru, old.thru);
        w.bool("MidiSettings/OverUSB", self.over_usb, old.over_usb);
        w.bool(
            "MidiSettings/IgnoreDuplicatePCs",
            self.ignore_duplicate_pcs,
            old.ignore_duplicate_pcs,
        );
        w.int("MidiSettings/PCInput", self.pc_input, old.pc_input);
        w.int("MidiSettings/PCOutput", self.pc_output, old.pc_output);
        w.put(
            "MidiSettings/ClockOutEn",
            &self.clock_out,
            &old.clock_out,
            |v| Variant::Int(v.to_stock()),
        );
        w.bool("MidiSettings/ClockInEn", self.clock_in, old.clock_in);
    }
}
