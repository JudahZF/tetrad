//! MIDI controller: DIN and USB MIDI receive/transmit, assignments, clock and
//! program-change policy. Transport is injected; this crate does no device I/O.
#![forbid(unsafe_code)]

pub mod clock;
pub mod controller;
pub mod dispatch;
pub mod message;
pub mod output;
pub mod parser;
pub mod preset_out;
pub mod settings;
pub mod usb;

pub use clock::{TempoContext, TempoMode, TempoTracker};
pub use controller::{MidiController, MidiPort};
pub use dispatch::{
    BlockMidi, ExpressionPedal, JackMidiMode, LooperButton, LooperControl, MidiAction,
    SetlistSelect,
};
pub use message::{Channel, MidiMessage, Realtime, U7};
pub use output::{DspCommand, MidiOutput, SettingReport};
pub use parser::{MidiParser, ParseError, ParseEvent};
pub use preset_out::{
    GeneralMidiMessage, MidiMessageInfo, MidiOutKind, MidiOutSlot, MidiOutSource, PresetMidiFields,
    PresetMidiOut, RemoteMidiError, SlotError, ToggleState,
};
pub use settings::{ChannelSetting, ClockOut, MidiSettings};
pub use usb::UsbMidiEvent;
