//! Per-preset MIDI output ("Preset MIDI Out", `zenUI::MidiMatrixDialog`):
//! messages sent on preset load, on stomp footswitch presses and on expression
//! pedal movement.

use crate::message::{Channel, MidiMessage, Realtime, U7};

/// Slots per list: on-load list and each footswitch/expression source.
pub const SLOTS: usize = 12;
/// Sources 0..=7 are stomp footswitches A..H, 8 and 9 are the expression pedals.
pub const SOURCES: usize = 10;

/// Stock `MidiMessageInfo.type` values (selector "None", "CC", "CC Toggle", "PC").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MidiOutKind {
    #[default]
    None,
    /// param1 = CC number, param2 = value.
    ControlChange,
    /// param1 = CC number, param2 = "MIN VALUE", param3 = "MAX VALUE".
    ControlToggle,
    /// param1 = bank CC#0 value, param2 = bank CC#32 value, param3 = program;
    /// `u32::MAX` leaves a part out.
    ProgramChange,
    /// Internal kind 4: one DIN timing-clock byte. The editor never offers it.
    Clock,
    /// Stored but unknown; stock sends nothing for it.
    Unknown(u8),
}

impl MidiOutKind {
    /// Stock stores `(char)type`, so only the low byte counts.
    pub const fn from_raw(raw: u32) -> Self {
        match raw as u8 {
            0 => Self::None,
            1 => Self::ControlChange,
            2 => Self::ControlToggle,
            3 => Self::ProgramChange,
            4 => Self::Clock,
            other => Self::Unknown(other),
        }
    }

    pub const fn raw(self) -> u8 {
        match self {
            Self::None => 0,
            Self::ControlChange => 1,
            Self::ControlToggle => 2,
            Self::ProgramChange => 3,
            Self::Clock => 4,
            Self::Unknown(raw) => raw,
        }
    }
}

/// Preset.proto `MidiMessageInfo` (fields 1..=5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct MidiMessageInfo {
    pub message_type: u32,
    pub channel: u32,
    pub param1: u32,
    pub param2: u32,
    pub param3: u32,
}

/// One stored message. Values stay raw, as in the 40-byte stock entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct MidiOutSlot {
    pub kind: MidiOutKind,
    /// One-based channel, 1..=16.
    pub channel: u32,
    pub param1: u32,
    pub param2: u32,
    pub param3: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SlotError {
    #[error("channel {0} is outside 1..=16")]
    Channel(u32),
    #[error("data value {0} is outside 0..=127")]
    Value(u32),
}

fn data(value: u32) -> Result<U7, SlotError> {
    u8::try_from(value)
        .ok()
        .and_then(U7::new)
        .ok_or(SlotError::Value(value))
}

impl MidiOutSlot {
    pub const fn from_info(info: MidiMessageInfo) -> Self {
        Self {
            kind: MidiOutKind::from_raw(info.message_type),
            channel: info.channel,
            param1: info.param1,
            param2: info.param2,
            param3: info.param3,
        }
    }

    pub const fn to_info(self) -> MidiMessageInfo {
        MidiMessageInfo {
            message_type: self.kind.raw() as u32,
            channel: self.channel,
            param1: self.param1,
            param2: self.param2,
            param3: self.param3,
        }
    }

    pub const fn is_empty(&self) -> bool {
        matches!(self.kind, MidiOutKind::None)
    }

    /// Messages for one send, in wire order (`0x00331340`). `toggle_low`
    /// picks param2 ("MIN VALUE") for a CC Toggle; otherwise param3 is sent.
    /// Stock does not range-check stored values; this refuses them instead of
    /// emitting malformed bytes.
    pub fn messages(&self, toggle_low: bool) -> Result<Vec<MidiMessage>, SlotError> {
        let channel = || Channel::from_number(self.channel).ok_or(SlotError::Channel(self.channel));
        let cc = |controller: u32, value: u32| -> Result<MidiMessage, SlotError> {
            Ok(MidiMessage::ControlChange {
                channel: channel()?,
                controller: data(controller)?,
                value: data(value)?,
            })
        };
        Ok(match self.kind {
            MidiOutKind::None | MidiOutKind::Unknown(_) => Vec::new(),
            MidiOutKind::ControlChange => vec![cc(self.param1, self.param2)?],
            MidiOutKind::ControlToggle => {
                let value = if toggle_low { self.param2 } else { self.param3 };
                vec![cc(self.param1, value)?]
            }
            MidiOutKind::ProgramChange => {
                let mut out = Vec::with_capacity(3);
                if self.param1 != u32::MAX {
                    out.push(cc(0, self.param1)?);
                }
                if self.param2 != u32::MAX {
                    out.push(cc(32, self.param2)?);
                }
                if self.param3 != u32::MAX {
                    out.push(MidiMessage::ProgramChange {
                        channel: channel()?,
                        program: data(self.param3)?,
                    });
                }
                out
            }
            MidiOutKind::Clock => vec![MidiMessage::Realtime(Realtime::TimingClock)],
        })
    }
}

/// A source of footswitch or expression MIDI output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MidiOutSource {
    /// Stomp footswitch A..H as 0..=7.
    Footswitch(u8),
    Expression1,
    Expression2,
}

impl MidiOutSource {
    pub const fn from_index(index: u8) -> Option<Self> {
        match index {
            0..=7 => Some(Self::Footswitch(index)),
            8 => Some(Self::Expression1),
            9 => Some(Self::Expression2),
            _ => None,
        }
    }

    pub const fn index(self) -> usize {
        match self {
            Self::Footswitch(stomp) => stomp as usize,
            Self::Expression1 => 8,
            Self::Expression2 => 9,
        }
    }
}

/// The preset's MIDI output table (preset object +0x40 and +0x220).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PresetMidiOut {
    pub on_load: [MidiOutSlot; SLOTS],
    pub sources: [[MidiOutSlot; SLOTS]; SOURCES],
}

/// Preset.proto lists as the stock writer emits them.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PresetMidiFields {
    /// Field 16 `midi_messages`.
    pub on_load: Vec<MidiMessageInfo>,
    /// Field 17 `midi_messages_general` (legacy, slot 0 of each source).
    pub general: Vec<MidiMessageInfo>,
    /// Field 33 `midi_messages_general_v2`, source-major, 12 per source.
    pub general_v2: Vec<MidiMessageInfo>,
}

impl PresetMidiOut {
    /// `ProtoBufPreset::doLoad` (`0x00681d44`). Extra entries are ignored.
    /// Legacy field 17 is read only when field 33 is empty; stock copies its
    /// channel and params into slot 0 of each source but not its type, so the
    /// slot stays `None`.
    pub fn from_fields(fields: &PresetMidiFields) -> Self {
        let mut out = Self::default();
        for (slot, info) in out.on_load.iter_mut().zip(&fields.on_load) {
            *slot = MidiOutSlot::from_info(*info);
        }
        if fields.general_v2.is_empty() {
            for (source, info) in out.sources.iter_mut().zip(&fields.general) {
                source[0] = MidiOutSlot {
                    kind: MidiOutKind::None,
                    ..MidiOutSlot::from_info(*info)
                };
            }
        } else {
            let slots = out.sources.iter_mut().flatten();
            for (slot, info) in slots.zip(&fields.general_v2) {
                *slot = MidiOutSlot::from_info(*info);
            }
        }
        out
    }

    /// The stock writer: all 12 on-load slots, slot 0 of each source to the
    /// legacy list, and all 120 source slots to field 33.
    pub fn to_fields(&self) -> PresetMidiFields {
        PresetMidiFields {
            on_load: self.on_load.iter().map(|slot| slot.to_info()).collect(),
            general: self
                .sources
                .iter()
                .map(|source| source[0].to_info())
                .collect(),
            general_v2: self
                .sources
                .iter()
                .flatten()
                .map(|slot| slot.to_info())
                .collect(),
        }
    }

    pub fn source(&self, source: MidiOutSource) -> &[MidiOutSlot; SLOTS] {
        &self.sources[source.index()]
    }

    /// True when any slot is set (`0x0032da4c`).
    pub fn has_messages(&self) -> bool {
        self.on_load
            .iter()
            .chain(self.sources.iter().flatten())
            .any(|slot| !slot.is_empty())
    }
}

/// Cortex Control `GeneralMIDIMessage`: a source and its messages.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GeneralMidiMessage {
    pub source: u32,
    pub messages: Vec<MidiMessageInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum RemoteMidiError {
    #[error("preset_load_messages has no GeneralMIDIMessage")]
    EmptyPresetLoad,
    #[error("{0} messages exceed the 12 slots of a list")]
    TooManyMessages(usize),
    #[error("source {0} is not 0..=9")]
    Source(u32),
}

fn slots(messages: &[MidiMessageInfo]) -> Result<[MidiOutSlot; SLOTS], RemoteMidiError> {
    if messages.len() > SLOTS {
        return Err(RemoteMidiError::TooManyMessages(messages.len()));
    }
    let mut slots = [MidiOutSlot::default(); SLOTS];
    for (slot, info) in slots.iter_mut().zip(messages) {
        *slot = MidiOutSlot::from_info(*info);
    }
    Ok(slots)
}

impl PresetMidiOut {
    /// Cortex Control `MIDISettingsMessage` (8) Set (`0x001912d4`).
    /// `preset_load` uses the messages of its first entry; each `general`
    /// entry replaces one source (unused slots become empty). Returns the
    /// last general source, which stock selects in the editor.
    ///
    /// Stock copies more than 12 on-load messages past its buffer, reads an
    /// empty list and trusts the source byte. This validates everything
    /// first and changes nothing on error.
    pub fn apply_remote(
        &mut self,
        preset_load: Option<&[GeneralMidiMessage]>,
        general: Option<&[GeneralMidiMessage]>,
    ) -> Result<Option<u8>, RemoteMidiError> {
        let on_load = preset_load
            .map(|list| {
                list.first()
                    .ok_or(RemoteMidiError::EmptyPresetLoad)
                    .and_then(|first| slots(&first.messages))
            })
            .transpose()?;
        let sources = general
            .unwrap_or_default()
            .iter()
            .map(|entry| {
                let source = u8::try_from(entry.source)
                    .ok()
                    .and_then(MidiOutSource::from_index)
                    .ok_or(RemoteMidiError::Source(entry.source))?;
                Ok((source, slots(&entry.messages)?))
            })
            .collect::<Result<Vec<_>, RemoteMidiError>>()?;
        if let Some(on_load) = on_load {
            self.on_load = on_load;
        }
        let mut selected = None;
        for (source, slots) in sources {
            self.sources[source.index()] = slots;
            selected = Some(source.index() as u8);
        }
        Ok(selected)
    }
}

/// CC Toggle state per stomp and slot, kept by the MIDI controller. Every
/// preset load (`0x0032e700`) and remote source edit (`0x0032dda0`)
/// sets all states to true, so the first press after a load sends param2.
/// (The constructor `0x00330a4c` zeroes all but the first byte, but a
/// preset load always runs before the first press.)
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToggleState([[bool; SLOTS]; 8]);

impl Default for ToggleState {
    fn default() -> Self {
        Self([[true; SLOTS]; 8])
    }
}

/// One slot send: its messages, or why the slot was refused.
pub type PlannedSend = Result<Vec<MidiMessage>, SlotError>;

impl ToggleState {
    /// Messages for a stomp footswitch press (`0x00332064`). CC Toggle
    /// slots send with the stored state, then flip it; other slots send as
    /// "not low". The caller applies the stock UI gating.
    pub fn footswitch(&mut self, table: &PresetMidiOut, stomp: u8) -> Vec<PlannedSend> {
        let Some(states) = self.0.get_mut(usize::from(stomp)) else {
            return Vec::new();
        };
        table.sources[usize::from(stomp)]
            .iter()
            .zip(states.iter_mut())
            .filter(|(slot, _)| !slot.is_empty())
            .map(|(slot, state)| {
                if slot.kind == MidiOutKind::ControlToggle {
                    let sent = slot.messages(*state);
                    *state = !*state;
                    sent
                } else {
                    slot.messages(true)
                }
            })
            .collect()
    }
}

impl PresetMidiOut {
    /// Messages queued on every preset load (`0x00331c28`, called first in
    /// the load path `0x00527d80`). CC Toggle slots send param3.
    pub fn on_load_messages(&self) -> Vec<PlannedSend> {
        self.on_load
            .iter()
            .filter(|slot| !slot.is_empty())
            .map(|slot| slot.messages(false))
            .collect()
    }

    /// Messages for an expression pedal position in 0.0..=1.0
    /// (`0x00331ae8`). Each set slot gets param2 replaced by
    /// `low + trunc(|param3 - param2| * t)`, where `low` is the smaller bound
    /// and `t` is the position, inverted when param3 <= param2. Then it is
    /// sent like a stored slot (so CC Toggle still sends param3, as stock).
    pub fn expression_messages(&self, pedal: MidiOutSource, position: f32) -> Vec<PlannedSend> {
        if matches!(pedal, MidiOutSource::Footswitch(_)) {
            return Vec::new();
        }
        self.source(pedal)
            .iter()
            .filter(|slot| !slot.is_empty())
            .map(|slot| {
                let (min, max) = (slot.param2 as i32, slot.param3 as i32);
                let span = max.wrapping_sub(min).wrapping_abs();
                let (low, t) = if max <= min {
                    (max, 1.0 - position)
                } else {
                    (min, position)
                };
                let scaled = (low as f32 + (span as f32 * t) as i32 as f32) as i32;
                MidiOutSlot {
                    param2: scaled as u32,
                    ..*slot
                }
                .messages(false)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(kind: MidiOutKind, param1: u32, param2: u32, param3: u32) -> MidiOutSlot {
        MidiOutSlot {
            kind,
            channel: 2,
            param1,
            param2,
            param3,
        }
    }

    fn cc(controller: u8, value: u8) -> MidiMessage {
        MidiMessage::ControlChange {
            channel: Channel::from_number(2).unwrap(),
            controller: U7::new(controller).unwrap(),
            value: U7::new(value).unwrap(),
        }
    }

    /// PC sends bank CC#0, bank CC#32, then the program; u32::MAX omits a part.
    #[test]
    fn program_change_bank_parts() {
        let full = slot(MidiOutKind::ProgramChange, 1, 2, 3)
            .messages(false)
            .unwrap();
        let program = MidiMessage::ProgramChange {
            channel: Channel::from_number(2).unwrap(),
            program: U7::new(3).unwrap(),
        };
        assert_eq!(full, [cc(0, 1), cc(32, 2), program.clone()]);
        let pc_only = slot(MidiOutKind::ProgramChange, u32::MAX, u32::MAX, 3);
        assert_eq!(pc_only.messages(false).unwrap(), [program]);
        let bad = MidiOutSlot {
            channel: 0,
            ..pc_only
        };
        assert_eq!(bad.messages(false), Err(SlotError::Channel(0)));
    }

    /// Footswitch CC Toggle alternates per stomp and slot, starting with param2.
    #[test]
    fn footswitch_toggle_alternates() {
        let mut table = PresetMidiOut::default();
        table.sources[0][0] = slot(MidiOutKind::ControlToggle, 20, 0, 127);
        table.sources[1][0] = slot(MidiOutKind::ControlToggle, 21, 0, 127);
        let mut state = ToggleState::default();
        let sends = |state: &mut ToggleState, stomp| state.footswitch(&table, stomp);
        assert_eq!(sends(&mut state, 0), [Ok(vec![cc(20, 0)])]);
        assert_eq!(sends(&mut state, 0), [Ok(vec![cc(20, 127)])]);
        assert_eq!(sends(&mut state, 1), [Ok(vec![cc(21, 0)])]);
        assert_eq!(sends(&mut state, 0), [Ok(vec![cc(20, 0)])]);
    }

    /// Expression scaling follows 0x00331ae8, including reversed ranges.
    #[test]
    fn expression_scaling() {
        let mut table = PresetMidiOut::default();
        table.sources[8][0] = slot(MidiOutKind::ControlChange, 11, 10, 110);
        table.sources[9][0] = slot(MidiOutKind::ControlChange, 11, 110, 10);
        let exp1 = table.expression_messages(MidiOutSource::Expression1, 0.5);
        assert_eq!(exp1, [Ok(vec![cc(11, 60)])]);
        let exp2 = table.expression_messages(MidiOutSource::Expression2, 0.25);
        assert_eq!(exp2, [Ok(vec![cc(11, 85)])]);
    }

    /// Load reads field 33 source-major; without it the legacy list keeps
    /// params but not the type. Save writes 12 + 10 + 120 entries.
    #[test]
    fn preset_fields_round_trip() {
        let info = MidiMessageInfo {
            message_type: 1,
            channel: 3,
            param1: 7,
            param2: 9,
            param3: 0,
        };
        let mut fields = PresetMidiFields {
            on_load: vec![info; 13],
            general: vec![info; 2],
            general_v2: Vec::new(),
        };
        let legacy = PresetMidiOut::from_fields(&fields);
        assert_eq!(legacy.sources[1][0].kind, MidiOutKind::None);
        assert_eq!(legacy.sources[1][0].param1, 7);
        fields.general_v2 = vec![MidiMessageInfo::default(); 13];
        fields.general_v2[12] = info;
        let table = PresetMidiOut::from_fields(&fields);
        assert_eq!(table.sources[1][0], MidiOutSlot::from_info(info));
        assert_eq!(table.sources[0][0], MidiOutSlot::default());
        let saved = table.to_fields();
        assert_eq!(
            (
                saved.on_load.len(),
                saved.general.len(),
                saved.general_v2.len()
            ),
            (12, 10, 120)
        );
        assert_eq!(PresetMidiOut::from_fields(&saved), table);
    }

    /// Remote MIDISettings edits validate everything first; a source entry
    /// replaces all 12 slots and becomes the selected source.
    #[test]
    fn remote_edit() {
        let info = MidiMessageInfo {
            message_type: 1,
            channel: 1,
            param1: 7,
            param2: 9,
            param3: 0,
        };
        let mut table = PresetMidiOut::default();
        table.sources[3][5] = MidiOutSlot::from_info(info);
        let general = [GeneralMidiMessage {
            source: 3,
            messages: vec![info],
        }];
        assert_eq!(table.apply_remote(None, Some(&general)), Ok(Some(3)));
        assert_eq!(table.sources[3][0], MidiOutSlot::from_info(info));
        assert!(table.sources[3][5].is_empty());
        let load = [GeneralMidiMessage {
            source: 0,
            messages: vec![info],
        }];
        let bad = [GeneralMidiMessage {
            source: 10,
            messages: Vec::new(),
        }];
        assert_eq!(
            table.apply_remote(Some(&load), Some(&bad)),
            Err(RemoteMidiError::Source(10))
        );
        assert!(table.on_load[0].is_empty());
    }
}
