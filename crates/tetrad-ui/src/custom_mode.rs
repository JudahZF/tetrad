//! Custom footswitch mode contract (owned, Tetrad). An extension declares the mode (its name
//! and 1..=4 pages); Tetrad runs it. On each page the ten switches A–D, Down (top row) and
//! E–H, Tap (bottom row) are slots, each with one [`SlotAction`]. Bank Up is the mode switch.
//!
//! The layout of ALL PRESETS plays unless the loaded preset has its own layout (THIS PRESET),
//! which the preset stores. The application fills [`CustomModeView`] and applies
//! [`CustomCommand`]s; the UI shows what the host reports. The slot editor is [`crate::Screen::CustomMode`]; Gig View shows the
//! current page while the mode is on.

use crate::{BlockTarget, GridPosition, MidiOutMessage, SystemAction};

/// Slots per page. A slot is the switch's logical index: A–H are 0..=7, Down 8 and Tap 9.
pub const CUSTOM_SLOTS: usize = 10;
/// Most pages of a Custom mode.
pub const CUSTOM_PAGES: u8 = 4;
/// Switch names of the slots, in slot order.
pub const SLOT_NAMES: [&str; CUSTOM_SLOTS] =
    ["A", "B", "C", "D", "E", "F", "G", "H", "Down", "Tap"];

/// A page change of the Custom mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PageStep {
    /// The next page, wrapping from the last to the first.
    Next,
    /// The previous page, wrapping from the first to the last.
    Previous,
    /// Page 0..=3; a page the mode does not have does nothing.
    Go(u8),
}

/// What a slot does.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SlotAction {
    /// A Mappings system action, a page change ([`SystemAction::CustomPage`]) included.
    /// [`SystemAction::CustomSlot`] is refused: a slot cannot press another slot.
    System(SystemAction),
    /// The block at a grid position, in every preset. A parameter also needs the same model
    /// as when it was chosen.
    Block {
        position: GridPosition,
        target: BlockTarget,
    },
    /// Load a preset. As a command it takes the loaded preset; the host keeps its file and
    /// name, and finds the file in its setlist slot at each press.
    LoadPreset,
    /// As a command only: load the preset in this slot of the setlist the preset browser shows
    /// ([`crate::DirectoryView::slots`]). It is stored, and shown, as [`Self::LoadPreset`].
    PresetSlot(u64),
    /// Send the slot's own MIDI messages ([`CustomSlotView::messages`]).
    SendMidi,
    /// Run a command that a running extension registered.
    Command { key: String, command: String },
}

/// Which layout an edit changes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LayoutScope {
    /// ALL PRESETS: the device's layout.
    #[default]
    Global,
    /// THIS PRESET: the loaded preset's own layout, saved with the preset.
    Preset,
}

/// One slot as the editor and Gig View show it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomSlotView {
    /// `None` is an empty slot.
    pub action: Option<SlotAction>,
    /// The typed label, else the action's own name ("Scene A", "Delay"); empty without an
    /// action.
    pub label: String,
    /// The typed label only: `None` uses the action's name.
    pub custom_label: Option<String>,
    /// "Bypass · momentary", "Load 3B", "PC 12 · Ch 1".
    pub detail: String,
    /// The LED and tile colour.
    pub colour: [u8; 3],
    /// The chosen colour, an index into [`CustomModeView::colours`]; `None` picks one from
    /// the action.
    pub swatch: Option<u8>,
    /// The action is on: its scene is active, its block is on, its screen shows.
    pub engaged: bool,
    /// The target exists: `false` for a missing block or preset, or a command whose
    /// extension is not running.
    pub available: bool,
    /// The value a parameter action sets, in percent.
    pub value: Option<u8>,
    /// `Some` for a bypass action: true holds the block's state only while pressed.
    pub momentary: Option<bool>,
    /// The messages of a Send MIDI action.
    pub messages: Vec<MidiOutMessage>,
}

/// A command of a running extension, offered in the action menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandChoice {
    /// Installation key, `owner/repo:extension.id`.
    pub key: String,
    pub command: String,
    /// "Control Macro · Reset".
    pub label: String,
}

/// The Custom mode of the extension that provides it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomModeView {
    /// The mode's name, such as "Custom".
    pub name: String,
    /// The extension's name.
    pub extension: String,
    /// 1..=[`CUSTOM_PAGES`].
    pub pages: u8,
    /// The current page, 0-based.
    pub page: u8,
    /// The mode is the footswitch mode now.
    pub active: bool,
    /// The ALL PRESETS layout, one entry per page.
    pub slots: Vec<[CustomSlotView; CUSTOM_SLOTS]>,
    /// The loaded preset's own layout (THIS PRESET); it plays instead of [`Self::slots`].
    pub preset_slots: Option<Vec<[CustomSlotView; CUSTOM_SLOTS]>>,
    /// The loaded preset ("3B Clean"), for the "Load this preset" action.
    pub preset: String,
    pub commands: Vec<CommandChoice>,
    /// The eight LED colours a slot can choose.
    pub colours: [[u8; 3]; 8],
    /// The last refusal, such as "A slot cannot press another slot".
    pub notice: Option<String>,
}

impl CustomModeView {
    /// The layout of `scope`; `None` for THIS PRESET when the preset has none.
    pub fn layout(&self, scope: LayoutScope) -> Option<&[[CustomSlotView; CUSTOM_SLOTS]]> {
        match scope {
            LayoutScope::Global => Some(&self.slots),
            LayoutScope::Preset => self.preset_slots.as_deref(),
        }
    }

    /// The layout the switches play now.
    pub fn playing(&self) -> &[[CustomSlotView; CUSTOM_SLOTS]] {
        self.preset_slots.as_deref().unwrap_or(&self.slots)
    }

    pub fn slot(&self, scope: LayoutScope, page: u8, slot: usize) -> Option<&CustomSlotView> {
        self.layout(scope)?.get(usize::from(page))?.get(slot)
    }
}

/// Requests of the slot editor and of Gig View. `page` is 0-based, `slot` 0..=9.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomCommand {
    /// An edit of one slot of the layout of `scope`.
    Slot {
        scope: LayoutScope,
        page: u8,
        slot: u8,
        edit: SlotEdit,
    },
    /// Page `to` becomes a copy of page `from`.
    CopyPage {
        scope: LayoutScope,
        from: u8,
        to: u8,
    },
    /// THIS PRESET gets a copy of the ALL PRESETS layout (`true`), or loses its own (`false`).
    PresetLayout(bool),
    /// A Gig View tile: a press, then a release, of a slot of the current page.
    Tap {
        slot: u8,
    },
    Page(PageStep),
}

/// An edit of one slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotEdit {
    SetAction(SlotAction),
    Clear,
    /// An empty label uses the action's name again.
    SetLabel(String),
    SetColour(Option<u8>),
    /// The value of a parameter action, in percent.
    SetValue(u8),
    SetMomentary(bool),
    SetMessages(Vec<MidiOutMessage>),
    /// The slot at `page`, `slot` of the same layout becomes a copy of this one.
    CopyTo {
        page: u8,
        slot: u8,
    },
    /// This slot and the slot at `page`, `slot` of the same layout swap.
    MoveTo {
        page: u8,
        slot: u8,
    },
}
