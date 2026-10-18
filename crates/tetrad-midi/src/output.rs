//! Typed outputs of the MIDI controller. The application routes them.

use crate::dispatch::MidiAction;
use crate::settings::ClockOut;
use crate::usb::UsbMidiEvent;

#[derive(Debug, Clone, PartialEq)]
pub enum MidiOutput {
    /// Bytes for the DIN UART (`/dev/ttySC1`, one `write` per message).
    Din(Vec<u8>),
    /// A request for the SHARC.
    Dsp(DspCommand),
    /// Work for the application.
    Action(MidiAction),
}

/// SHARC requests the MIDI controller sends (layouts: `tetrad-dsp-protocol`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DspCommand {
    /// 0x16 `enTempoChange`, integer BPM.
    TempoChange(i32),
    /// 0x18 `enUSBMIDIOut`, immediate delivery.
    UsbMidiOut(UsbMidiEvent),
    /// 0x18 `enUSBMIDIOut`, tagged delivery (selector 1, `0x00876200`): the QC mini's
    /// MIDI out through the SHARC. The word at +4 is MIDI over USB (`usb`); the SHARC
    /// also sends the event over USB when it is set (I).
    PortMidiOut { event: UsbMidiEvent, usb: bool },
    /// 0x22 `enEnableUSBMIDIClock`.
    EnableUsbMidiClock(bool),
    /// 0x23 `enEnableInternalMIDIClock`.
    EnableInternalMidiClock(bool),
    /// 0x24 `enResetInternalMidiClock`; stock only sends 0.
    ResetInternalMidiClock(u32),
    /// 0x25 `enEnableMIDIClockOut` with the `ClockOutEn` value.
    EnableMidiClockOut(ClockOut),
    /// 0x26 `enEnableMIDIClockIn`.
    EnableMidiClockIn(bool),
    /// 0x2d `enMidiClockCountAndTempo`, sent for every in-range DIN tick.
    MidiClockCountAndTempo { tempo: f32, count: u32 },
    /// 0x2e `enMidiClockStart`.
    MidiClockStart,
}

/// MIDI-related fields of the Cortex Control `GeneralSettingsMessage` (9)
/// that stock reports when they change.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SettingReport {
    /// Field 10.
    MidiOverUsb(bool),
    /// Field 11; 17 = OMNI.
    MidiChannel(i32),
    /// Field 12.
    IgnoreDuplicatePc(bool),
    /// Field 19: true while neither DIN nor USB clock is followed.
    InternalMidiClockEnabled(bool),
    /// Field 24.
    MidiClockOut(ClockOut),
    /// Field 34.
    MidiClockIn(bool),
    /// Field 40.
    ExternalMidiClockTempo(f32),
    /// Field 41.
    ExternalMidiClockOutOfRange(bool),
}
