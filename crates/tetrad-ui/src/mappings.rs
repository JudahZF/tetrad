//! Mappings page contract (owned, Tetrad). The grid ⋮ menu item "Mappings" (it replaces stock
//! Preset MIDI Out) lists every trigger → action of the loaded preset and of the device. The
//! application fills [`MappingsView`] and applies [`MappingCommand`]s; the UI never decides
//! where a mapping is stored or whether a combination is allowed, it shows what the host
//! reports and asks again.
//!
//! Stock data shows as rows too: Stomp-mode footswitch assignments (Stomp → block bypass),
//! expression links (EXP → block parameter or bypass) and Preset MIDI Out lists (Preset loaded,
//! Stomp or EXP → Send MIDI). Editing such a row edits the stock data. The Send MIDI messages
//! themselves are edited in the stock message editor ([`crate::Screen::MidiOut`] with
//! [`crate::UiCommand::SetPresetMidi`]).

use crate::{GridPosition, MidiOutTarget};

/// Which list a row belongs to. Block actions are always [`MappingScope::Preset`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MappingScope {
    /// Stored in the loaded preset.
    #[default]
    Preset,
    /// Stored on the device, for every preset. A preset mapping with the same trigger wins.
    Global,
}

/// Stable id of a row. It can change when an edit moves the row between stock and Tetrad
/// storage; [`MappingsView::focus`] then names the new id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MappingId {
    /// A Tetrad mapping: its index in the scope's own list.
    Tetrad(u32),
    /// A stock Stomp-mode assignment: footswitch 0..=7 (A..=H) → the block's bypass.
    Stomp { stomp: u8, position: GridPosition },
    /// A stock expression link: pedal 1 or 2 → a block parameter or its bypass.
    Expression {
        pedal: u8,
        position: GridPosition,
        target: BlockTarget,
    },
    /// A stock Preset MIDI Out list.
    MidiOut(MidiOutTarget),
}

/// What starts a mapping.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Trigger {
    /// The preset finished loading. Preset scope only.
    PresetLoaded,
    /// A footswitch press in Stomp mode, 0..=7 (A..=H). Preset scope only.
    Stomp(u8),
    /// Expression pedal 1 or 2. Preset scope only; block and Send MIDI actions only.
    Expression(u8),
    /// A MIDI message on the QC's own channel that the stock MIDI map does not use.
    Midi(MidiInput),
    /// A trigger an extension declared ([`ExtensionTriggerChoice`]), with the source it fired,
    /// such as `cc:20`. An empty `source` waits for LEARN.
    Extension {
        key: String,
        trigger: String,
        source: String,
    },
}

/// A MIDI-in source on the QC channel. PCs load presets (stock) and cannot be mapped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MidiInput {
    /// Continuous: CC → parameter is value / 127, CC → on/off is a switch at 64.
    Cc(u8),
    /// One shot on each note-on with a velocity above 0.
    Note(u8),
}

/// The part of a block an action drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockTarget {
    Bypass,
    /// Parameter index in the block's catalog order ([`MappingBlock::parameters`]).
    Parameter(u32),
}

/// Looper X buttons, as MIDI CC#49..=56 drive them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LooperButton {
    RecordOverdub,
    PlayStop,
    Undo,
    Reverse,
    HalfSpeed,
    OneShot,
    PunchIn,
    Duplicate,
}

/// Footswitch modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModeChoice {
    Preset,
    Scene,
    Stomp,
}

/// Device actions of the action menu's SYSTEM section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemAction {
    NextPreset,
    PreviousPreset,
    /// Scene 0..=7 (A..=H).
    Scene(u8),
    NextScene,
    PreviousScene,
    /// Open or close the tuner (a switch for CC triggers, a toggle otherwise).
    Tuner,
    TapTempo,
    Mode(ModeChoice),
    NextMode,
    /// Open or close Gig View.
    GigView,
    Looper(LooperButton),
}

/// What a mapping does.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MappingAction {
    System(SystemAction),
    /// A block of the loaded preset ([`MappingsView::blocks`]). Preset scope only.
    Block {
        position: GridPosition,
        target: BlockTarget,
    },
    /// Send the trigger's Preset MIDI Out messages. Preset loaded, Stomp and EXP triggers only.
    SendMidi,
}

/// One row of a list. The application resolves every label (catalog, extension and MIDI
/// names); the UI draws them as given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingRow {
    pub id: MappingId,
    pub trigger: Trigger,
    /// `None` until an action is chosen; such a row does nothing.
    pub action: Option<MappingAction>,
    /// "Stomp A", "CC 20", "Preset loaded", "Ch 3 CC 21".
    pub trigger_label: String,
    /// "Footswitch press", "MIDI · QC channel", "MIDI Mapper · Ch 3".
    pub trigger_detail: String,
    /// "Parametric-3", "Tuner", "Send MIDI"; empty without an action.
    pub action_label: String,
    /// "Bypass", "Open and close", "PC 12 · Ch 1"; empty without an action.
    pub action_detail: String,
    /// How the trigger's value is used: "Switch · 64 or more engages", "Toggles on each press".
    pub value_use: String,
    /// The value a one-shot trigger sets a parameter to, in percent. `Some` only where
    /// [`MappingCommand::SetValue`] applies.
    pub value: Option<u8>,
}

/// A block of the loaded preset for the action menu's BLOCKS section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingBlock {
    pub position: GridPosition,
    /// Catalog name, such as "Parametric-3".
    pub name: String,
    /// "Row 1 slot 2".
    pub location: String,
    /// Parameter names in catalog order; [`BlockTarget::Parameter`] indexes this list.
    pub parameters: Vec<String>,
}

/// A trigger an extension declared, offered in the Trigger dropdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionTriggerChoice {
    /// Installation key, `owner/repo:extension.id`.
    pub key: String,
    /// The extension's own trigger id, such as `midi`.
    pub trigger: String,
    /// "MIDI Mapper · Ch 3".
    pub label: String,
}

/// Everything the Mappings page shows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MappingsView {
    /// The loaded preset's name ("3D Worship V5").
    pub preset_name: String,
    pub preset: Vec<MappingRow>,
    pub global: Vec<MappingRow>,
    /// The loaded preset's blocks, in grid order.
    pub blocks: Vec<MappingBlock>,
    pub extension_triggers: Vec<ExtensionTriggerChoice>,
    /// The row the last command added or changed; the mapping page follows it.
    pub focus: Option<(MappingScope, MappingId)>,
    /// The row waiting for its LEARN message.
    pub learning: Option<(MappingScope, MappingId)>,
    /// The last refusal, such as "CC 43 is used by the QC's MIDI map". Cleared by the next
    /// accepted command.
    pub notice: Option<String>,
}

impl MappingsView {
    pub fn rows(&self, scope: MappingScope) -> &[MappingRow] {
        match scope {
            MappingScope::Preset => &self.preset,
            MappingScope::Global => &self.global,
        }
    }

    pub fn row(&self, scope: MappingScope, id: &MappingId) -> Option<&MappingRow> {
        self.rows(scope).iter().find(|row| &row.id == id)
    }
}

/// Requests of the Mappings page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingCommand {
    /// A new row with a free default trigger and no action. It becomes the focus.
    Add(MappingScope),
    SetTrigger {
        scope: MappingScope,
        id: MappingId,
        trigger: Trigger,
    },
    SetAction {
        scope: MappingScope,
        id: MappingId,
        action: MappingAction,
    },
    /// The one-shot value of a parameter action, in percent.
    SetValue {
        scope: MappingScope,
        id: MappingId,
        value: u8,
    },
    /// Start (`true`) or cancel LEARN: the next free QC-channel MIDI message, or the next
    /// firing of the row's extension trigger, becomes the row's trigger.
    Learn {
        scope: MappingScope,
        id: MappingId,
        on: bool,
    },
    Remove {
        scope: MappingScope,
        id: MappingId,
    },
}
