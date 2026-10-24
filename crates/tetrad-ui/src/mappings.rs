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
    /// Continuous: the row's mode and CC window ([`FieldId::Low`], [`FieldId::High`]) decide
    /// what a value does.
    Cc(u8),
    /// A press on note-on with a velocity above 0, a release on note-off.
    Note(u8),
}

/// The part of a block an action drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlockTarget {
    Bypass,
    /// Catalog parameter index ([`MappingParameter::index`]).
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
    /// How the trigger's value drives the action; `None` without an action.
    pub mode: Option<ModeView>,
    /// The modes [`MappingCommand::SetMode`] accepts, in menu order. Empty: the mode is fixed.
    pub modes: Vec<ModeView>,
    /// The CC window of a CC trigger, then the mode's values. One or two show in the detail
    /// page's Values row; more open the Values page.
    pub fields: Vec<ValueField>,
    /// The values on one line, for the Values row and the list: "25 % ↔ 60 % · CC 64–127".
    /// Empty when there is nothing to show.
    pub summary: String,
    /// The Values page graph. CC (and extension CC) triggers only, except Relative.
    pub response: Option<ResponseView>,
}

/// How a mapping uses its trigger's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModeKind {
    /// Runs once on each press (a CC: when it enters its window).
    OnPress,
    /// The value follows the CC (a pedal: the stock expression link). Switches are on while the
    /// CC is inside its window.
    Follow,
    /// A CC from an endless encoder steps the value up or down.
    Relative,
    /// Each press sets one value.
    Set,
    /// Each press swaps two values (switches: on and off).
    Toggle,
    /// Held: one value; released: the value before the press (switches: flipped while held).
    Momentary,
    /// Each press moves the value by an amount.
    Step,
}

/// A mode as the host names it for this row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeView {
    pub kind: ModeKind,
    /// "Toggle".
    pub label: String,
    /// "Each press swaps A and B".
    pub detail: String,
}

/// One value of a row's mode or CC window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldId {
    /// Set and Momentary.
    Value,
    /// Toggle.
    A,
    B,
    /// Step: steps per press, signed.
    Amount,
    /// Step: stop or wrap at the ends.
    Wrap,
    /// Follow: the value at the window's Low and High.
    Min,
    Max,
    /// Follow: Jump or Pickup.
    Pickup,
    /// Relative: how the encoder codes its ticks.
    Encoding,
    /// The CC window, 0..=127.
    Low,
    High,
}

/// A value as the host formats it ("25 %", "Plate", "Offset 64"). Each stepper tap sends
/// [`MappingCommand::StepField`]; the host decides the step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueField {
    pub id: FieldId,
    /// "A", "Low", "Encoding".
    pub label: String,
    pub text: String,
}

/// What the Values page graph draws: CC value (0..=127) across, the target's range up.
/// Target values are in thousandths of the range (0..=1000).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseView {
    /// The CC window.
    pub low: u8,
    pub high: u8,
    /// Follow on a parameter: the target at `low` and at `high` (outside the window it stays
    /// at the nearer end). `None`: the window is where the CC presses or switches on.
    pub line: Option<(u16, u16)>,
    /// The CC's last value, when one arrived.
    pub level: Option<u8>,
    /// Follow on a parameter: the target's value now.
    pub current: Option<u16>,
    /// Pickup waits for the CC to reach `current`.
    pub waiting: bool,
}

/// A block of the loaded preset for the action menu's BLOCKS section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingBlock {
    pub position: GridPosition,
    /// Catalog name, such as "Parametric-3".
    pub name: String,
    /// "Row 1 slot 2".
    pub location: String,
    /// The parameters a mapping can drive, in catalog order (text and disabled parameters are
    /// left out).
    pub parameters: Vec<MappingParameter>,
}

/// One parameter of a [`MappingBlock`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappingParameter {
    /// Catalog index, as [`BlockTarget::Parameter`] holds it.
    pub index: u32,
    /// "Band 1 Gain".
    pub name: String,
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
    /// One of [`MappingRow::modes`]. The host fills the mode's values.
    SetMode {
        scope: MappingScope,
        id: MappingId,
        mode: ModeKind,
    },
    /// A stepper tap on a [`ValueField`]: `ticks` steps of the target's own size, or the next
    /// choice.
    StepField {
        scope: MappingScope,
        id: MappingId,
        field: FieldId,
        ticks: i32,
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
