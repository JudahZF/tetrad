//! Custom footswitch mode contract (owned, Tetrad). An extension declares the mode (its name
//! and 1..=4 pages); Tetrad runs it. On each page the ten switches A–D, Down (top row) and
//! E–H, Tap (bottom row) are slots, each with one [`SlotAction`]. Bank Up is the mode switch.
//!
//! A switch can also have a double press, and two [`adjacent`] switches a combo
//! ([`Gesture`]). Only a switch that has one waits before its press runs: up to
//! [`DOUBLE_PRESS`] for a second press, up to [`COMBO_PRESS`] for its partner.
//!
//! The layout of ALL PRESETS plays unless the loaded preset has its own layout (THIS PRESET),
//! which the preset stores. The application fills [`CustomModeView`] and applies
//! [`CustomCommand`]s; the UI shows what the host reports. The slot editor is [`crate::Screen::CustomMode`]; Gig View shows the
//! current page while the mode is on.

use std::time::Duration;

use crate::{BlockTarget, GridPosition, MidiOutMessage, SystemAction};

/// Slots per page. A slot is the switch's logical index: A–H are 0..=7, Down 8 and Tap 9.
pub const CUSTOM_SLOTS: usize = 10;
/// Most pages of a Custom mode.
pub const CUSTOM_PAGES: u8 = 4;
/// Switch names of the slots, in slot order.
pub const SLOT_NAMES: [&str; CUSTOM_SLOTS] =
    ["A", "B", "C", "D", "E", "F", "G", "H", "Down", "Tap"];

/// The second press of a double press comes within this time of the first.
pub const DOUBLE_PRESS: Duration = Duration::from_millis(300);
/// The two presses of a combo come within this time of each other.
pub const COMBO_PRESS: Duration = Duration::from_millis(80);

/// Which action of a switch: its press, its double press, or its combo with switch `0..=9`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Gesture {
    #[default]
    Press,
    Double,
    /// With an [`adjacent`] switch.
    Combo(u8),
}

/// Row and column of a switch on the unit: A–D, Down above E–H, Tap.
fn place(slot: u8) -> Option<(u8, u8)> {
    match slot {
        0..=7 => Some((slot / 4, slot % 4)),
        8 => Some((0, 4)),
        9 => Some((1, 4)),
        _ => None,
    }
}

/// Switches `a` and `b` are side by side in a row, or one directly above the other: the
/// pairs that make a combo.
pub fn adjacent(a: u8, b: u8) -> bool {
    match (place(a), place(b)) {
        (Some((row_a, column_a)), Some((row_b, column_b))) => {
            (row_a == row_b && column_a.abs_diff(column_b) == 1)
                || (column_a == column_b && row_a != row_b)
        }
        _ => false,
    }
}

/// "Switch D", "Double D", "C + D".
pub fn gesture_name(slot: u8, gesture: Gesture) -> String {
    let name = |slot: u8| SLOT_NAMES.get(usize::from(slot)).copied().unwrap_or("?");
    match gesture {
        Gesture::Press => format!("Switch {}", name(slot)),
        Gesture::Double => format!("Double {}", name(slot)),
        Gesture::Combo(partner) => {
            format!("{} + {}", name(slot.min(partner)), name(slot.max(partner)))
        }
    }
}

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
    /// The double press, as a slot shows it. It has no colour of its own (`swatch` is `None`)
    /// and no `double`.
    pub double: Option<Box<CustomSlotView>>,
}

/// The action of two adjacent switches pressed together.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomComboView {
    /// The two switches, lower first.
    pub switches: [u8; 2],
    /// As a slot shows it, without a colour of its own (`swatch` is `None`).
    pub binding: CustomSlotView,
}

/// One page of a layout.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CustomPageView {
    pub slots: [CustomSlotView; CUSTOM_SLOTS],
    pub combos: Vec<CustomComboView>,
}

impl CustomPageView {
    /// The combo of `a` and `b`, in either order.
    pub fn combo(&self, a: u8, b: u8) -> Option<&CustomComboView> {
        let pair = [a.min(b), a.max(b)];
        self.combos.iter().find(|combo| combo.switches == pair)
    }
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
    pub slots: Vec<CustomPageView>,
    /// The loaded preset's own layout (THIS PRESET); it plays instead of [`Self::slots`].
    pub preset_slots: Option<Vec<CustomPageView>>,
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
    pub fn layout(&self, scope: LayoutScope) -> Option<&[CustomPageView]> {
        match scope {
            LayoutScope::Global => Some(&self.slots),
            LayoutScope::Preset => self.preset_slots.as_deref(),
        }
    }

    /// The layout the switches play now.
    pub fn playing(&self) -> &[CustomPageView] {
        self.preset_slots.as_deref().unwrap_or(&self.slots)
    }

    pub fn slot(&self, scope: LayoutScope, page: u8, slot: usize) -> Option<&CustomSlotView> {
        self.layout(scope)?.get(usize::from(page))?.slots.get(slot)
    }

    /// The action `gesture` of a switch runs; `None` while it has none.
    pub fn binding(
        &self,
        scope: LayoutScope,
        page: u8,
        slot: u8,
        gesture: Gesture,
    ) -> Option<&CustomSlotView> {
        let page = self.layout(scope)?.get(usize::from(page))?;
        let switch = page.slots.get(usize::from(slot))?;
        match gesture {
            Gesture::Press => Some(switch),
            Gesture::Double => switch.double.as_deref(),
            Gesture::Combo(partner) => page.combo(slot, partner).map(|combo| &combo.binding),
        }
    }
}

/// Requests of the slot editor and of Gig View. `page` is 0-based, `slot` 0..=9.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustomCommand {
    /// An edit of the `gesture` action of one switch of the layout of `scope`. The colour,
    /// COPY and MOVE belong to the switch: they take [`Gesture::Press`].
    Slot {
        scope: LayoutScope,
        page: u8,
        slot: u8,
        gesture: Gesture,
        edit: SlotEdit,
    },
    /// Page `to` becomes a copy of page `from`, its combos included.
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
    /// The press empties the switch (its double press too); a double press or combo goes.
    Clear,
    /// An empty label uses the action's name again.
    SetLabel(String),
    SetColour(Option<u8>),
    /// The value of a parameter action, in percent.
    SetValue(u8),
    SetMomentary(bool),
    SetMessages(Vec<MidiOutMessage>),
    /// The slot at `page`, `slot` of the same layout becomes a copy of this one (its press,
    /// double press and colour; the combos stay).
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
