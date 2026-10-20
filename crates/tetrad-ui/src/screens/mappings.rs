//! Owned Mappings page (Tetrad; it replaces stock Preset MIDI Out in the grid ⋮ menu): the
//! trigger → action rows of the loaded preset (THIS PRESET) and of the device (GLOBAL), the
//! detail page of one row and its action menu. The host decides storage and validity
//! (`crate::mappings`): each control sends a [`MappingCommand`] and the page shows the next
//! snapshot. The page follows [`MappingsView::focus`], as an edit can give a row a new id.
//! Full-screen chrome as Preset MIDI Out: a muted title, 66 × 44 header icons, the ✓ and
//! 64 px rows on `#2e2e2e` panels. The action menu has the ⋮ menu's look.

use tetrad_assets::{ACCENT, ACCENT_FILL, CANVAS, DIVIDER, FAINT, Icon, IconSize, MUTED, PANEL};
use tetrad_assets::{Rgb, SECTION, SURFACE, TEXT, WARNING};

use super::NumericPurpose;
use crate::layout::{
    BODY, BODY_BOLD, ButtonStyle, CAPTION, GREY_VEIL, Layout, ListId, Look, MEDIUM,
};
use crate::render::{Align, fit};
use crate::state::{ActionMenu, ChoicePurpose, MappingsPage, Overlay};
use crate::*;

const ROW: u32 = 64;
const LIST: Rect = Rect::new(8, 88, 784, 384);
const TRIGGER_PANEL: Rect = Rect::new(8, 88, 784, 2 * ROW);
const ACTION_Y: i32 = 256;
/// Trigger dropdowns: 264 × 40 at the right end of their row.
const FIELD_WIDTH: u32 = 264;
/// The action menu: the ⋮ menu's panel, rows and section headers.
const MENU: Rect = Rect::new(472, 16, 270, 420);
const MENU_ROW: u32 = 60;
const MENU_HEADER: u32 = 44;
/// One step of the value stepper, in percent.
const VALUE_STEP: i32 = 5;
/// A new MIDI trigger starts on CC 20, the first CC that the MIDI specification leaves
/// undefined. The host refuses it when the stock MIDI map uses it.
const FIRST_FREE_CC: u8 = 20;

const STOMPS: [&str; 8] = [
    "Stomp A", "Stomp B", "Stomp C", "Stomp D", "Stomp E", "Stomp F", "Stomp G", "Stomp H",
];
const PEDALS: [&str; 2] = ["EXP 1", "EXP 2"];
const MIDI_KINDS: [&str; 2] = ["CC", "Note"];
const SCENES: [&str; 8] = [
    "Scene A", "Scene B", "Scene C", "Scene D", "Scene E", "Scene F", "Scene G", "Scene H",
];
const LOOPER: [(&str, LooperButton); 8] = [
    ("Rec / Overdub", LooperButton::RecordOverdub),
    ("Play / Stop", LooperButton::PlayStop),
    ("Undo", LooperButton::Undo),
    ("Reverse", LooperButton::Reverse),
    ("Half speed", LooperButton::HalfSpeed),
    ("One shot", LooperButton::OneShot),
    ("Punch in", LooperButton::PunchIn),
    ("Duplicate", LooperButton::Duplicate),
];

/// The row whose detail page is open.
fn open_row(ui: &UiState) -> Option<&MappingRow> {
    let page = &ui.mappings;
    ui.snapshot.mappings.row(page.tab, page.detail.as_ref()?)
}

fn learning(view: &MappingsView, scope: MappingScope, id: &MappingId) -> bool {
    (view.learning.as_ref()).is_some_and(|(learning, row)| *learning == scope && row == id)
}

pub(super) fn layout(ui: &UiState, l: &mut Layout) {
    match open_row(ui) {
        Some(row) => detail(ui, l, row),
        None => list(ui, l),
    }
}

fn trigger_icon(trigger: &Trigger) -> Icon {
    match trigger {
        Trigger::PresetLoaded => Icon::Download,
        Trigger::Stomp(_) => Icon::Footswitch,
        Trigger::Expression(_) => Icon::Pedal,
        Trigger::Midi(_) => Icon::Midi,
        Trigger::Extension { .. } => Icon::Plugin,
    }
}

/// The icon of a system action: the one of its action menu entry.
fn system_icon(action: SystemAction) -> Icon {
    use SystemAction as S;
    match action {
        S::NextPreset | S::PreviousPreset => Icon::SaveArrow,
        S::Scene(_) | S::NextScene | S::PreviousScene => Icon::Copy,
        S::Tuner => Icon::Tuner,
        S::TapTempo => Icon::Metronome,
        S::Mode(_) | S::NextMode => Icon::Footswitch,
        S::GigView => Icon::Gig,
        S::Looper(_) => Icon::Looper,
    }
}

fn glyph(l: &mut Layout, bounds: Rect, icon: Icon, size: IconSize, color: Rgb) {
    l.push(bounds, "", Look::Glyph { icon, size, color });
}

/// A grid block tile at a small size: the category ring, the tile fill and the device glyph.
fn block_tile(l: &mut Layout, s: &UiSnapshot, position: GridPosition, bounds: Rect) {
    let (info, capture) = (s.blocks[position.index()], s.captures[position.index()]);
    l.panel(bounds, info.hue(capture).unwrap_or(MUTED), 8.0, None);
    l.panel(bounds.inset(2, 2), info.fill(capture), 6.0, None);
    if let Some(icon) = info.icon(capture) {
        let size = match bounds.width {
            40.. => IconSize::Medium,
            _ => IconSize::Compact,
        };
        glyph(l, bounds, icon, size, TEXT);
    }
}

/// A block action shows its block tile, another action its icon, in the square `bounds`.
fn action_mark(ui: &UiState, l: &mut Layout, action: Option<&MappingAction>, bounds: Rect) {
    let icon = match action {
        None => return,
        Some(MappingAction::Block { position, .. }) => {
            return block_tile(l, &ui.snapshot, *position, bounds);
        }
        Some(MappingAction::System(action)) => system_icon(*action),
        Some(MappingAction::SendMidi) => Icon::Midi,
    };
    glyph(l, bounds, icon, IconSize::Medium, TEXT);
}

/// Label, detail line and colour of a row's action.
fn action_text(row: &MappingRow) -> (&str, &str, Rgb) {
    match row.action {
        None => ("Choose an action", "System, block or MIDI out", ACCENT),
        Some(_) => (&row.action_label, &row.action_detail, TEXT),
    }
}

/// An 18 px label over a 14 px muted line, from `x` to `right`, centred in `row`.
fn lines(
    l: &mut Layout,
    row: Rect,
    (x, right): (i32, i32),
    (label, detail): (&str, &str),
    color: Rgb,
) {
    let width = (right - x).max(0) as u32;
    let top = row.y + (row.height as i32 - 46) / 2;
    l.text(
        Rect::new(x, top, width, 26),
        fit(label, MEDIUM, width as f32),
        MEDIUM,
        color,
        Align::Left,
    );
    l.text(
        Rect::new(x, top + 24, width, 20),
        fit(detail, CAPTION, width as f32),
        CAPTION,
        MUTED,
        Align::Left,
    );
}

/// The faint → between a trigger and its action.
fn arrow(l: &mut Layout, x: i32, cy: i32) {
    let (x, y) = (x as f32, cy as f32);
    l.line((x, y), (x + 18.0, y), 2.0, FAINT);
    l.line((x + 12.0, y - 6.0), (x + 18.0, y), 2.0, FAINT);
    l.line((x + 12.0, y + 6.0), (x + 18.0, y), 2.0, FAINT);
}

fn chevron(l: &mut Layout, row: Rect) {
    let bounds = Rect::new(row.right() - 40, row.y, 24, row.height);
    glyph(l, bounds, Icon::Forward, IconSize::Small, FAINT);
}

/// A 1 px divider under a panel row, 16 px in from the panel edges.
fn divider(l: &mut Layout, row: Rect) {
    let line = Rect::new(row.x + 16, row.bottom() - 1, row.width - 32, 1);
    l.push(line, "", Look::Fill(DIVIDER));
}

fn label(l: &mut Layout, row: Rect, text: &str) {
    let bounds = Rect::new(row.x + 16, row.y, 260, row.height);
    l.text(bounds, text, MEDIUM, TEXT, Align::Left);
}

/// A 14 px muted section caption above a panel.
fn caption(l: &mut Layout, y: i32, text: &str) {
    l.text(Rect::new(16, y, 300, 32), text, CAPTION, MUTED, Align::Left);
}

/// The + and trash header buttons left of the ✓, with the stock divider between.
fn header_button(l: &mut Layout, control: MappingControl, label: &str, icon: Icon, fill: Rgb) {
    let bounds = Rect::new(620, 8, 66, 44);
    l.control(
        bounds,
        Control::Mapping(control),
        true,
        label,
        Look::IconButton { icon, fill },
    );
    l.push(Rect::new(685, 18, 1, 24), "", Look::Fill(SECTION));
}

/// The list: title, THIS PRESET / GLOBAL, + and ✓, the caption and the rows.
fn list(ui: &UiState, l: &mut Layout) {
    let view = &ui.snapshot.mappings;
    let tab = ui.mappings.tab;
    crate::widgets::page_header(l, "Mappings", Control::Back, Icon::Check);
    let tabs = [
        (MappingScope::Preset, "THIS PRESET"),
        (MappingScope::Global, "GLOBAL"),
    ];
    for (index, (scope, text)) in tabs.into_iter().enumerate() {
        let bounds = Rect::new(122 + index as i32 * 134, 8, 128, 44);
        let selected = scope == tab;
        let fill = if selected { PANEL } else { SURFACE };
        let look = Look::Panel {
            fill,
            radius: 8.0,
            border: None,
        };
        l.control(
            bounds,
            Control::Mapping(MappingControl::Tab(scope)),
            true,
            text,
            look,
        );
        let ink = if selected { ACCENT } else { MUTED };
        l.text(bounds, text, BODY_BOLD, ink, Align::Center);
    }
    header_button(l, MappingControl::Add, "ADD", Icon::Plus, SECTION);
    let rows = view.rows(tab);
    let owner = match tab {
        MappingScope::Preset if view.preset_name.is_empty() => "This preset",
        MappingScope::Preset => view.preset_name.as_str(),
        MappingScope::Global => "All presets",
    };
    let count = match rows.len() {
        1 => "1 mapping".to_owned(),
        count => format!("{count} mappings"),
    };
    let text = format!("{owner} · {count}");
    let width = CAPTION.width(&text).ceil() as u32 + 8;
    l.text(
        Rect::new(16, 56, width, 32),
        text,
        CAPTION,
        MUTED,
        Align::Left,
    );
    // A refusal shows beside the caption until the next accepted command.
    if let Some(notice) = &view.notice {
        let x = 16 + width as i32 + 16;
        let space = (LIST.right() - 8 - x).max(0) as u32;
        let notice = fit(notice, CAPTION, space as f32);
        l.text(
            Rect::new(x, 56, space, 32),
            notice,
            CAPTION,
            WARNING,
            Align::Right,
        );
    }
    l.panel(LIST, PANEL, 8.0, None);
    if rows.is_empty() {
        l.text(LIST, "Tap + to add a mapping.", BODY, MUTED, Align::Center);
        return;
    }
    let content = rows.len() as u32 * ROW;
    let offset = ui.offset(ListId::Mappings);
    l.scroll(ListId::Mappings, LIST, content, offset, |l, top| {
        for (index, row) in rows.iter().enumerate() {
            let bounds = Rect::new(LIST.x, top + (index as u32 * ROW) as i32, LIST.width, ROW);
            list_row(ui, l, bounds, index as u16, row);
        }
    });
    super::scrollbar(l, LIST, content, offset, 2);
}

/// Trigger icon, label and detail, the arrow, the action mark, label and detail, a chevron.
fn list_row(ui: &UiState, l: &mut Layout, bounds: Rect, index: u16, row: &MappingRow) {
    let open = Control::Mapping(MappingControl::Open(index));
    l.control(bounds, open, true, row.trigger_label.clone(), Look::Area);
    if index > 0 {
        let line = Rect::new(bounds.x + 16, bounds.y, bounds.width - 32, 1);
        l.push(line, "", Look::Fill(DIVIDER));
    }
    let icon = Rect::new(bounds.x + 12, bounds.y, 40, ROW);
    glyph(l, icon, trigger_icon(&row.trigger), IconSize::Medium, TEXT);
    let trigger = (row.trigger_label.as_str(), row.trigger_detail.as_str());
    lines(l, bounds, (bounds.x + 52, bounds.x + 276), trigger, TEXT);
    arrow(l, bounds.x + 286, bounds.y + ROW as i32 / 2);
    let mark = Rect::new(bounds.x + 322, bounds.y + 14, 36, 36);
    action_mark(ui, l, row.action.as_ref(), mark);
    let (text, detail, color) = action_text(row);
    lines(
        l,
        bounds,
        (bounds.x + 372, bounds.right() - 48),
        (text, detail),
        color,
    );
    chevron(l, bounds);
}

/// One row's page: the Trigger panel, the Action panel and the storage footer.
fn detail(ui: &UiState, l: &mut Layout, row: &MappingRow) {
    let view = &ui.snapshot.mappings;
    let scope = ui.mappings.tab;
    let title = format!("{} mapping", row.trigger_label);
    crate::widgets::page_header(l, &title, Control::Back, Icon::Check);
    header_button(l, MappingControl::Remove, "REMOVE", Icon::Trash, SURFACE);
    caption(l, 56, "Trigger");
    l.panel(TRIGGER_PANEL, PANEL, 8.0, None);
    let kind = Rect::new(TRIGGER_PANEL.x, TRIGGER_PANEL.y, TRIGGER_PANEL.width, ROW);
    label(l, kind, "Type");
    dropdown(l, kind, MappingChoice::Kind, kind_label(view, &row.trigger));
    divider(l, kind);
    let second = Rect::new(kind.x, kind.bottom(), kind.width, ROW);
    trigger_row(l, row, second, learning(view, scope, &row.id));
    caption(l, 224, "Action");
    let target = midi_target(&row.trigger).filter(|_| row.action == Some(MappingAction::SendMidi));
    let rows = if target.is_some() { 3 } else { 2 };
    let panel = Rect::new(8, ACTION_Y, 784, rows * ROW);
    l.panel(panel, PANEL, 8.0, None);
    let action = Rect::new(panel.x, panel.y, panel.width, ROW);
    let open = Control::Mapping(MappingControl::Actions);
    l.control(action, open, true, "ACTION", Look::Area);
    let mark = Rect::new(action.x + 16, action.y + 12, 40, 40);
    action_mark(ui, l, row.action.as_ref(), mark);
    let (text, detail, color) = action_text(row);
    lines(
        l,
        action,
        (action.x + 70, action.right() - 48),
        (text, detail),
        color,
    );
    chevron(l, action);
    divider(l, action);
    let value = Rect::new(panel.x, action.bottom(), panel.width, ROW);
    label(l, value, "Value");
    let mut right = value.right() - 16;
    if let Some(percent) = row.value {
        let stepper = Rect::new(right - 212, value.y + 10, 212, 44);
        super::stepper(
            l,
            stepper,
            Control::Mapping(MappingControl::Value(-1)),
            Control::Mapping(MappingControl::Value(1)),
            format!("{percent} %"),
            true,
        );
        right = stepper.x - 16;
    }
    let x = value.x + 120;
    let width = (right - x).max(0) as u32;
    let used = fit(&row.value_use, MEDIUM, width as f32);
    l.text(
        Rect::new(x, value.y, width, ROW),
        used,
        MEDIUM,
        MUTED,
        Align::Right,
    );
    if target.is_some() {
        divider(l, value);
        let messages = Rect::new(panel.x, value.bottom(), panel.width, ROW);
        let edit = Control::Mapping(MappingControl::EditMessages);
        l.control(messages, edit, true, "EDIT MESSAGES", Look::Area);
        label(l, messages, "Edit messages");
        chevron(l, messages);
    }
    // A refusal replaces the storage note until the next accepted command.
    let footer = Rect::new(16, panel.bottom() + 6, 768, 24);
    match &view.notice {
        Some(notice) => {
            let notice = fit(notice, CAPTION, footer.width as f32);
            l.text(footer, notice, CAPTION, WARNING, Align::Left);
        }
        None => {
            let note = match scope {
                MappingScope::Preset => {
                    "Saved in this preset. Block actions are always per preset."
                }
                MappingScope::Global => {
                    "Saved for all presets. A preset mapping with the same trigger wins."
                }
            };
            l.text(footer, note, CAPTION, FAINT, Align::Left);
        }
    }
}

/// A Trigger dropdown at the right end of `row`.
fn dropdown(l: &mut Layout, row: Rect, choice: MappingChoice, value: &str) {
    let bounds = Rect::new(
        row.right() - 16 - FIELD_WIDTH as i32,
        row.y + 12,
        FIELD_WIDTH,
        40,
    );
    let look = Look::Dropdown {
        value: value.to_owned(),
    };
    l.control(
        bounds,
        Control::Mapping(MappingControl::Choice(choice)),
        true,
        value,
        look,
    );
}

/// The Trigger panel's second row: the footswitch, the pedal, the load note, or the MIDI
/// message or extension source with LEARN.
fn trigger_row(l: &mut Layout, row: &MappingRow, bounds: Rect, learning: bool) {
    let field = Rect::new(bounds.x + 292, bounds.y + 12, 284, 40);
    match &row.trigger {
        Trigger::PresetLoaded => {
            label(l, bounds, "When");
            let note = Rect::new(bounds.x + 292, bounds.y, bounds.width - 308, ROW);
            l.text(
                note,
                "Each time this preset loads",
                MEDIUM,
                MUTED,
                Align::Right,
            );
            return;
        }
        Trigger::Stomp(stomp) => {
            label(l, bounds, "Footswitch");
            let value = STOMPS.get(usize::from(*stomp)).copied().unwrap_or("-");
            dropdown(l, bounds, MappingChoice::Stomp, value);
            return;
        }
        Trigger::Expression(pedal) => {
            label(l, bounds, "Pedal");
            let value = (usize::from(*pedal).checked_sub(1))
                .and_then(|index| PEDALS.get(index))
                .copied()
                .unwrap_or("-");
            dropdown(l, bounds, MappingChoice::Pedal, value);
            return;
        }
        _ if learning => {
            l.panel(field, CANVAS, 4.0, None);
            l.text(field.inset(8, 0), "Listening…", MEDIUM, ACCENT, Align::Left);
        }
        Trigger::Midi(input) => {
            let (kind, number) = match input {
                MidiInput::Cc(number) => (MIDI_KINDS[0], number),
                MidiInput::Note(number) => (MIDI_KINDS[1], number),
            };
            let (kind_field, number_field) = field.take_left(132, 8);
            let choice = Control::Mapping(MappingControl::Choice(MappingChoice::Midi));
            let look = Look::Dropdown { value: kind.into() };
            l.control(kind_field, choice, true, kind, look);
            l.button_in(
                number_field,
                Control::Mapping(MappingControl::Number),
                true,
                number.to_string(),
                ButtonStyle::Inset,
                MEDIUM,
            );
        }
        Trigger::Extension { source, .. } => {
            let (text, color) = match source.is_empty() {
                true => ("Not learned yet", MUTED),
                false => (row.trigger_label.as_str(), TEXT),
            };
            let text = fit(text, MEDIUM, (field.width - 16) as f32);
            l.text(field.inset(8, 0), text, MEDIUM, color, Align::Left);
        }
    }
    let message = match row.trigger {
        Trigger::Midi(_) => "Message",
        _ => "Source",
    };
    label(l, bounds, message);
    let learn = Rect::new(bounds.right() - 16 - 184, bounds.y + 12, 184, 40);
    let control = Control::Mapping(MappingControl::Learn);
    if learning {
        let look = Look::Panel {
            fill: ACCENT_FILL,
            radius: 8.0,
            border: Some(ACCENT),
        };
        l.control(learn, control, true, "CANCEL", look);
        l.text(learn, "CANCEL", MEDIUM, ACCENT, Align::Center);
    } else {
        l.button_in(learn, control, true, "LEARN", ButtonStyle::Inset, MEDIUM);
    }
}

/// One entry of the Type dropdown.
#[derive(Debug, Clone, Copy)]
enum Kind<'a> {
    PresetLoaded,
    Stomp,
    Expression,
    Midi,
    Extension(&'a ExtensionTriggerChoice),
}

impl<'a> Kind<'a> {
    fn label(self) -> &'a str {
        match self {
            Self::PresetLoaded => "Preset loaded",
            Self::Stomp => "Footswitch",
            Self::Expression => "Expression pedal",
            Self::Midi => "MIDI (QC channel)",
            Self::Extension(choice) => &choice.label,
        }
    }

    fn matches(self, trigger: &Trigger) -> bool {
        match (self, trigger) {
            (Self::PresetLoaded, Trigger::PresetLoaded)
            | (Self::Stomp, Trigger::Stomp(_))
            | (Self::Expression, Trigger::Expression(_))
            | (Self::Midi, Trigger::Midi(_)) => true,
            (Self::Extension(choice), Trigger::Extension { key, trigger, .. }) => {
                choice.key == *key && choice.trigger == *trigger
            }
            _ => false,
        }
    }

    /// A trigger of this kind: `current` when it is of this kind already, else the first
    /// switch or pedal, CC 20, or an extension trigger that waits for LEARN.
    fn trigger(self, current: &Trigger) -> Trigger {
        if self.matches(current) {
            return current.clone();
        }
        match self {
            Self::PresetLoaded => Trigger::PresetLoaded,
            Self::Stomp => Trigger::Stomp(0),
            Self::Expression => Trigger::Expression(1),
            Self::Midi => Trigger::Midi(MidiInput::Cc(FIRST_FREE_CC)),
            Self::Extension(choice) => Trigger::Extension {
                key: choice.key.clone(),
                trigger: choice.trigger.clone(),
                source: String::new(),
            },
        }
    }
}

/// The Type dropdown of a scope: Preset loaded, footswitches and pedals only in a preset;
/// then MIDI on the QC channel and the extensions' triggers.
fn kinds(view: &MappingsView, scope: MappingScope) -> Vec<Kind<'_>> {
    let own: &[Kind] = match scope {
        MappingScope::Preset => &[
            Kind::PresetLoaded,
            Kind::Stomp,
            Kind::Expression,
            Kind::Midi,
        ],
        MappingScope::Global => &[Kind::Midi],
    };
    let extensions = view.extension_triggers.iter().map(Kind::Extension);
    own.iter().copied().chain(extensions).collect()
}

/// The Type dropdown value; an extension that no longer declares the trigger reads
/// "Extension".
fn kind_label<'a>(view: &'a MappingsView, trigger: &Trigger) -> &'a str {
    let declared = view.extension_triggers.iter().map(Kind::Extension);
    let own = [
        Kind::PresetLoaded,
        Kind::Stomp,
        Kind::Expression,
        Kind::Midi,
    ];
    (own.into_iter().chain(declared))
        .find(|kind| kind.matches(trigger))
        .map_or("Extension", Kind::label)
}

/// The Preset MIDI Out list that a Send MIDI row sends.
fn midi_target(trigger: &Trigger) -> Option<MidiOutTarget> {
    match trigger {
        Trigger::PresetLoaded => Some(MidiOutTarget::OnLoad),
        Trigger::Stomp(stomp) => (Footswitch::TILES.get(usize::from(*stomp)))
            .map(|tile| MidiOutTarget::Footswitch(*tile)),
        Trigger::Expression(1) => Some(MidiOutTarget::Expression1),
        Trigger::Expression(2) => Some(MidiOutTarget::Expression2),
        _ => None,
    }
}

/// Options and selection of a Trigger dropdown of the open row.
pub(super) fn choice(ui: &UiState, choice: MappingChoice) -> Option<(Vec<&str>, Option<usize>)> {
    let row = open_row(ui)?;
    Some(match (choice, &row.trigger) {
        (MappingChoice::Kind, trigger) => {
            let kinds = kinds(&ui.snapshot.mappings, ui.mappings.tab);
            let current = kinds.iter().position(|kind| kind.matches(trigger));
            (kinds.into_iter().map(Kind::label).collect(), current)
        }
        (MappingChoice::Stomp, Trigger::Stomp(stomp)) => {
            (STOMPS.to_vec(), Some(usize::from(*stomp)))
        }
        (MappingChoice::Pedal, Trigger::Expression(pedal)) => {
            (PEDALS.to_vec(), usize::from(*pedal).checked_sub(1))
        }
        (MappingChoice::Midi, Trigger::Midi(input)) => {
            let note = matches!(input, MidiInput::Note(_));
            (MIDI_KINDS.to_vec(), Some(usize::from(note)))
        }
        _ => return None,
    })
}

/// One row of the action menu.
enum Entry<'a> {
    Header(&'static str),
    /// Back to the root: the submenu title and, for a block, its location.
    Back(&'a str, Option<&'a str>),
    /// A submenu: a system group's icon or a block tile.
    Open {
        icon: Option<Icon>,
        block: Option<GridPosition>,
        label: &'a str,
        menu: ActionMenu,
    },
    Set {
        icon: Option<Icon>,
        label: &'a str,
        action: MappingAction,
    },
}

impl Entry<'_> {
    fn height(&self) -> u32 {
        match self {
            Self::Header(_) => MENU_HEADER,
            _ => MENU_ROW,
        }
    }
}

/// A submenu of system actions under its back row.
fn system<'a>(
    title: &'a str,
    items: impl IntoIterator<Item = (&'a str, SystemAction)>,
) -> Vec<Entry<'a>> {
    let items = items.into_iter().map(|(label, action)| Entry::Set {
        icon: None,
        label,
        action: MappingAction::System(action),
    });
    std::iter::once(Entry::Back(title, None))
        .chain(items)
        .collect()
}

/// The rows of an action menu level. Block and Send MIDI actions exist only in a preset.
fn entries(ui: &UiState, menu: ActionMenu) -> Vec<Entry<'_>> {
    use SystemAction as S;
    let view = &ui.snapshot.mappings;
    match menu {
        ActionMenu::Root => {
            let open = |icon, label, menu| Entry::Open {
                icon: Some(icon),
                block: None,
                label,
                menu,
            };
            let set = |icon, label, action| Entry::Set {
                icon: Some(icon),
                label,
                action,
            };
            let mut entries = vec![
                Entry::Header("SYSTEM"),
                open(Icon::SaveArrow, "Preset", ActionMenu::Preset),
                open(Icon::Copy, "Scene", ActionMenu::Scene),
                set(Icon::Tuner, "Tuner", MappingAction::System(S::Tuner)),
                open(Icon::Metronome, "Tempo", ActionMenu::Tempo),
                open(Icon::Footswitch, "Mode", ActionMenu::Mode),
                set(Icon::Gig, "Gig View", MappingAction::System(S::GigView)),
                open(Icon::Looper, "Looper", ActionMenu::Looper),
            ];
            if ui.mappings.tab == MappingScope::Preset {
                entries.push(Entry::Header("BLOCKS"));
                entries.extend(view.blocks.iter().map(|block| Entry::Open {
                    icon: None,
                    block: Some(block.position),
                    label: &block.name,
                    menu: ActionMenu::Block(block.position),
                }));
                entries.push(Entry::Header("MIDI OUT"));
                entries.push(set(Icon::Midi, "Send MIDI", MappingAction::SendMidi));
            }
            entries
        }
        ActionMenu::Preset => system(
            "Preset",
            [
                ("Next preset", S::NextPreset),
                ("Previous preset", S::PreviousPreset),
            ],
        ),
        ActionMenu::Scene => system(
            "Scene",
            (SCENES
                .into_iter()
                .zip(0..)
                .map(|(label, scene)| (label, S::Scene(scene))))
            .chain([
                ("Next scene", S::NextScene),
                ("Previous scene", S::PreviousScene),
            ]),
        ),
        ActionMenu::Tempo => system("Tempo", [("Tap tempo", S::TapTempo)]),
        ActionMenu::Mode => system(
            "Mode",
            [
                ("Preset mode", S::Mode(ModeChoice::Preset)),
                ("Scene mode", S::Mode(ModeChoice::Scene)),
                ("Stomp mode", S::Mode(ModeChoice::Stomp)),
                ("Next mode", S::NextMode),
            ],
        ),
        ActionMenu::Looper => system(
            "Looper",
            LOOPER.map(|(label, button)| (label, S::Looper(button))),
        ),
        ActionMenu::Block(position) => {
            let Some(block) = view.blocks.iter().find(|block| block.position == position) else {
                return Vec::new();
            };
            let set = |label, target| Entry::Set {
                icon: None,
                label,
                action: MappingAction::Block { position, target },
            };
            let parameters = (block.parameters.iter())
                .map(|parameter| set(&parameter.name, BlockTarget::Parameter(parameter.index)));
            [
                Entry::Back(&block.name, Some(&block.location)),
                set("Bypass", BlockTarget::Bypass),
            ]
            .into_iter()
            .chain(parameters)
            .collect()
        }
    }
}

/// The action menu over the detail page: the ⋮ menu's grey veil, 270 × 420 panel, 60 px rows,
/// 44 px section headers and 2 px scroll thumb. A tap outside closes it.
pub(super) fn action_menu(ui: &UiState, l: &mut Layout, menu: ActionMenu) {
    let entries = entries(ui, menu);
    l.veil(GREY_VEIL);
    l.popover(MENU);
    l.panel(MENU, crate::widgets::PAGE, 8.0, None);
    let content: u32 = entries.iter().map(Entry::height).sum();
    let offset = ui.offset(ListId::MappingActions);
    l.scroll(ListId::MappingActions, MENU, content, offset, |l, mut y| {
        for (index, entry) in entries.iter().enumerate() {
            let row = Rect::new(MENU.x, y, MENU.width, entry.height());
            menu_entry(ui, l, row, index as u16, entry);
            y += row.height as i32;
        }
    });
    super::scrollbar(l, MENU, content, offset, 2);
}

fn menu_entry(ui: &UiState, l: &mut Layout, row: Rect, index: u16, entry: &Entry) {
    let pick = Control::Mapping(MappingControl::Pick(index));
    let (icon, block, text) = match entry {
        Entry::Header(title) => {
            let bounds = Rect::new(row.x + 16, row.y + 8, row.width - 32, 36);
            return l.text(bounds, *title, CAPTION, TEXT, Align::Left);
        }
        Entry::Back(title, location) => {
            l.control(row, pick, true, *title, Look::Area);
            let mark = Rect::new(row.x + 8, row.y, 32, row.height);
            glyph(l, mark, Icon::Back, IconSize::Medium, ACCENT);
            let span = (row.x + 46, row.right() - 16);
            match location {
                Some(location) => lines(l, row, span, (title, location), ACCENT),
                None => {
                    let bounds = Rect::new(span.0, row.y, (span.1 - span.0) as u32, row.height);
                    l.text(
                        bounds,
                        fit(title, MEDIUM, bounds.width as f32),
                        MEDIUM,
                        ACCENT,
                        Align::Left,
                    );
                }
            }
            let line = Rect::new(row.x, row.bottom() - 1, row.width, 1);
            l.push(line, "", Look::Fill(PANEL));
            return;
        }
        Entry::Open {
            icon, block, label, ..
        } => {
            chevron(l, Rect::new(row.x, row.y, row.width - 8, row.height));
            (*icon, *block, *label)
        }
        Entry::Set { icon, label, .. } => (*icon, None, *label),
    };
    l.control(row, pick, true, text, Look::Area);
    let mark = Rect::new(row.x + 14, row.y + 14, 32, 32);
    if let Some(position) = block {
        block_tile(l, &ui.snapshot, position, mark);
    } else if let Some(icon) = icon {
        glyph(l, mark, icon, IconSize::Medium, TEXT);
    }
    let x = if icon.is_some() || block.is_some() {
        row.x + 56
    } else {
        row.x + 16
    };
    let width = (row.right() - 44 - x) as u32;
    let bounds = Rect::new(x, row.y, width, row.height);
    l.text(
        bounds,
        fit(text, MEDIUM, width as f32),
        MEDIUM,
        TEXT,
        Align::Left,
    );
}

/// Whether the rows and blocks of both views address the same items: a touch in progress on a
/// Mappings control is cancelled when they move.
pub(crate) fn same_targets(a: &MappingsView, b: &MappingsView) -> bool {
    type Targets<'a> = (
        Vec<&'a MappingId>,
        Vec<&'a MappingId>,
        Vec<(GridPosition, usize)>,
    );
    fn targets(view: &MappingsView) -> Targets<'_> {
        (
            view.preset.iter().map(|row| &row.id).collect(),
            view.global.iter().map(|row| &row.id).collect(),
            (view.blocks.iter())
                .map(|block| (block.position, block.parameters.len()))
                .collect(),
        )
    }
    a == b || targets(a) == targets(b)
}

impl UiState {
    pub(crate) fn mapping_control(&mut self, control: MappingControl) -> Vec<UiCommand> {
        use MappingControl as C;
        let send = |command| vec![UiCommand::Mapping(command)];
        let scope = self.mappings.tab;
        match control {
            C::Tab(tab) => {
                self.mappings = MappingsPage { tab, detail: None };
                self.reset_scroll(ListId::Mappings);
                return Vec::new();
            }
            // The detail page of the new row opens when it becomes the focus.
            C::Add => return send(MappingCommand::Add(scope)),
            C::Open(index) => {
                let rows = self.snapshot.mappings.rows(scope);
                self.mappings.detail = rows.get(usize::from(index)).map(|row| row.id.clone());
                return Vec::new();
            }
            _ => {}
        }
        let Some(row) = open_row(self).cloned() else {
            return Vec::new();
        };
        let id = row.id.clone();
        match control {
            C::Remove => {
                self.mappings.detail = None;
                send(MappingCommand::Remove { scope, id })
            }
            C::Choice(dropdown) => {
                let current = choice(self, dropdown).and_then(|(_, current)| current);
                self.open_choice(ChoicePurpose::Mapping(dropdown), current.unwrap_or(0));
                Vec::new()
            }
            C::Number => {
                if let Trigger::Midi(input) = row.trigger {
                    let (note, number) = match input {
                        MidiInput::Cc(number) => (false, number),
                        MidiInput::Note(number) => (true, number),
                    };
                    let range = NumericRange {
                        value: f32::from(number),
                        min: 0.0,
                        max: 127.0,
                        integer: true,
                    };
                    let purpose = NumericPurpose::MappingMidi { scope, id, note };
                    self.numeric = Some(super::NumericEntry::new(purpose, range));
                }
                Vec::new()
            }
            C::Learn => {
                let on = !learning(&self.snapshot.mappings, scope, &id);
                send(MappingCommand::Learn { scope, id, on })
            }
            C::Actions => {
                self.reset_scroll(ListId::MappingActions);
                self.overlay = Some(Overlay::MappingActions(ActionMenu::Root));
                Vec::new()
            }
            C::Pick(index) => {
                let Some(Overlay::MappingActions(menu)) = self.overlay else {
                    return Vec::new();
                };
                let next = match entries(self, menu).into_iter().nth(usize::from(index)) {
                    Some(Entry::Open { menu, .. }) => menu,
                    Some(Entry::Back(..)) => ActionMenu::Root,
                    Some(Entry::Set { action, .. }) => {
                        self.overlay = None;
                        if row.action.as_ref() == Some(&action) {
                            return Vec::new();
                        }
                        return send(MappingCommand::SetAction { scope, id, action });
                    }
                    Some(Entry::Header(_)) | None => return Vec::new(),
                };
                self.reset_scroll(ListId::MappingActions);
                self.overlay = Some(Overlay::MappingActions(next));
                Vec::new()
            }
            C::Value(steps) => {
                let Some(value) = row.value else {
                    return Vec::new();
                };
                let next = (i32::from(value) + i32::from(steps) * VALUE_STEP).clamp(0, 100) as u8;
                if next == value {
                    return Vec::new();
                }
                send(MappingCommand::SetValue {
                    scope,
                    id,
                    value: next,
                })
            }
            C::EditMessages => match midi_target(&row.trigger) {
                Some(target) => {
                    self.midi_edit = Some((target, 0));
                    self.navigate(Screen::MidiOut)
                }
                None => Vec::new(),
            },
            C::Tab(_) | C::Add | C::Open(_) => Vec::new(),
        }
    }

    /// A pick from a Trigger dropdown of the open row: its full new trigger.
    pub(crate) fn choose_mapping(&self, choice: MappingChoice, index: usize) -> Vec<UiCommand> {
        let Some(row) = open_row(self) else {
            return Vec::new();
        };
        let trigger = match (choice, &row.trigger) {
            (MappingChoice::Kind, current) => {
                match kinds(&self.snapshot.mappings, self.mappings.tab).get(index) {
                    Some(kind) => kind.trigger(current),
                    None => return Vec::new(),
                }
            }
            (MappingChoice::Stomp, Trigger::Stomp(_)) if index < STOMPS.len() => {
                Trigger::Stomp(index as u8)
            }
            (MappingChoice::Pedal, Trigger::Expression(_)) if index < PEDALS.len() => {
                Trigger::Expression(index as u8 + 1)
            }
            (
                MappingChoice::Midi,
                Trigger::Midi(MidiInput::Cc(number) | MidiInput::Note(number)),
            ) => match index {
                0 => Trigger::Midi(MidiInput::Cc(*number)),
                1 => Trigger::Midi(MidiInput::Note(*number)),
                _ => return Vec::new(),
            },
            _ => return Vec::new(),
        };
        if trigger == row.trigger {
            return Vec::new();
        }
        vec![UiCommand::Mapping(MappingCommand::SetTrigger {
            scope: self.mappings.tab,
            id: row.id.clone(),
            trigger,
        })]
    }

    /// A new snapshot: the page follows [`MappingsView::focus`] when it changes, so the detail
    /// page stays on a row that an edit gave a new id (or opens a row that + added). A detail
    /// page whose row is gone returns to the list; its dropdown, action menu and keyboard close.
    pub(crate) fn follow_mappings(&mut self, old: &MappingsView) {
        let view = &self.snapshot.mappings;
        if view.focus != old.focus
            && self.stack.contains(&Screen::Mappings)
            && let Some((tab, id)) = &view.focus
        {
            self.mappings = MappingsPage {
                tab: *tab,
                detail: Some(id.clone()),
            };
        }
        if open_row(self).is_none() {
            self.mappings.detail = None;
        }
        let view = &self.snapshot.mappings;
        let page = &self.mappings;
        let stale = match &self.overlay {
            Some(Overlay::MappingActions(ActionMenu::Block(position))) => {
                page.detail.is_none()
                    || !view.blocks.iter().any(|block| block.position == *position)
            }
            Some(Overlay::MappingActions(_)) => page.detail.is_none(),
            // A dropdown whose options no longer fit the open row (its trigger kind changed,
            // or the extension triggers behind the Type list did) would pick the wrong thing.
            Some(Overlay::Choice(ChoicePurpose::Mapping(dropdown))) => {
                page.detail.is_none()
                    || choice(self, *dropdown).is_none()
                    || (*dropdown == MappingChoice::Kind
                        && view.extension_triggers != old.extension_triggers)
            }
            _ => false,
        };
        if stale {
            self.overlay = None;
        }
        if let Some(entry) = &self.numeric
            && let NumericPurpose::MappingMidi { scope, id, .. } = &entry.purpose
            && (*scope, Some(id)) != (page.tab, page.detail.as_ref())
        {
            self.numeric = None;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::tests::activate;

    const MAPPER: &str = "tetrad-dev/extensions:midi-mapper";

    fn cell(row: u8, column: u8) -> GridPosition {
        GridPosition::new(row, column).expect("grid position")
    }

    fn row(id: MappingId, trigger: Trigger, action: Option<MappingAction>) -> MappingRow {
        MappingRow {
            id,
            trigger,
            action,
            trigger_label: "Trigger".into(),
            trigger_detail: "Detail".into(),
            action_label: "Action".into(),
            action_detail: "Detail".into(),
            value_use: "Toggles on each press".into(),
            value: None,
        }
    }

    /// Five preset rows (Send MIDI on load, a stock Stomp bypass, a CC → parameter with a
    /// value, Send MIDI on Stomp B, an extension trigger), two global rows and two blocks
    /// whose parameter lists skip catalog indices.
    pub(crate) fn sample() -> MappingsView {
        let block = |position, name: &str, parameters: &[(u32, &str)]| MappingBlock {
            position,
            name: name.into(),
            location: "Row 1 slot 2".into(),
            parameters: (parameters.iter())
                .map(|(index, name)| MappingParameter {
                    index: *index,
                    name: (*name).into(),
                })
                .collect(),
        };
        let bypass = MappingAction::Block {
            position: cell(0, 1),
            target: BlockTarget::Bypass,
        };
        let gain = MappingAction::Block {
            position: cell(0, 2),
            target: BlockTarget::Parameter(3),
        };
        let tuner = MappingAction::System(SystemAction::Tuner);
        MappingsView {
            preset_name: "Studio Crunch".into(),
            preset: vec![
                row(
                    MappingId::MidiOut(MidiOutTarget::OnLoad),
                    Trigger::PresetLoaded,
                    Some(MappingAction::SendMidi),
                ),
                row(
                    MappingId::Stomp {
                        stomp: 0,
                        position: cell(0, 1),
                    },
                    Trigger::Stomp(0),
                    Some(bypass),
                ),
                MappingRow {
                    value: Some(50),
                    ..row(
                        MappingId::Tetrad(0),
                        Trigger::Midi(MidiInput::Cc(21)),
                        Some(gain),
                    )
                },
                row(
                    MappingId::MidiOut(MidiOutTarget::Footswitch(Footswitch::B)),
                    Trigger::Stomp(1),
                    Some(MappingAction::SendMidi),
                ),
                row(
                    MappingId::Tetrad(1),
                    Trigger::Extension {
                        key: MAPPER.into(),
                        trigger: "midi".into(),
                        source: "cc:64".into(),
                    },
                    Some(tuner.clone()),
                ),
            ],
            global: vec![
                row(
                    MappingId::Tetrad(0),
                    Trigger::Midi(MidiInput::Cc(20)),
                    Some(tuner),
                ),
                row(
                    MappingId::Tetrad(1),
                    Trigger::Midi(MidiInput::Note(36)),
                    Some(MappingAction::System(SystemAction::TapTempo)),
                ),
            ],
            blocks: vec![
                block(cell(0, 1), "Green Drive", &[(0, "Drive"), (2, "Tone")]),
                block(
                    cell(0, 2),
                    "Crunch Amp",
                    &[(0, "Gain"), (3, "Bass"), (9, "Master")],
                ),
            ],
            extension_triggers: vec![ExtensionTriggerChoice {
                key: MAPPER.into(),
                trigger: "midi".into(),
                label: "MIDI Mapper · Ch 3".into(),
            }],
            focus: None,
            learning: None,
            notice: None,
        }
    }

    fn page(mappings: MappingsView) -> UiState {
        let mut ui = UiState::new();
        ui.set_snapshot(UiSnapshot {
            mappings,
            ..UiSnapshot::default()
        })
        .expect("valid snapshot");
        ui.navigate(Screen::Mappings);
        ui
    }

    fn map(control: MappingControl) -> Control {
        Control::Mapping(control)
    }

    fn sent(command: MappingCommand) -> Vec<UiCommand> {
        vec![UiCommand::Mapping(command)]
    }

    fn with_mappings(ui: &mut UiState, mappings: MappingsView) {
        let snapshot = UiSnapshot {
            mappings,
            ..ui.snapshot().clone()
        };
        ui.set_snapshot(snapshot).expect("valid snapshot");
    }

    fn shown(ui: &UiState, control: Control) -> bool {
        ui.hit_regions()
            .iter()
            .any(|region| region.control == control)
    }

    /// Taps the action menu row labelled `label`, scrolling the menu as a user does.
    fn pick(ui: &mut UiState, label: &str) -> Vec<UiCommand> {
        for _ in 0..4 {
            let regions = ui.hit_regions();
            let found = regions.iter().find(|region| {
                matches!(region.control, Control::Mapping(MappingControl::Pick(_)))
                    && region.label == label
            });
            if let Some(region) = found {
                return activate(ui, region.control);
            }
            ui.handle_input(InputEvent::PointerDown { x: 600, y: 400 });
            ui.handle_input(InputEvent::PointerMove { x: 600, y: 100 });
            ui.handle_input(InputEvent::PointerUp { x: 600, y: 100 });
        }
        panic!("no action menu row {label}");
    }

    /// The tabs show their own rows; + asks for a row in the shown tab, and its detail page
    /// opens when the host names it the focus.
    #[test]
    fn tabs_switch_lists_and_add_opens_the_new_row() {
        let mut ui = page(sample());
        assert!(shown(&ui, map(MappingControl::Open(4))));
        activate(&mut ui, map(MappingControl::Tab(MappingScope::Global)));
        assert!(shown(&ui, map(MappingControl::Open(1))));
        assert!(!shown(&ui, map(MappingControl::Open(2))));
        assert_eq!(
            activate(&mut ui, map(MappingControl::Add)),
            sent(MappingCommand::Add(MappingScope::Global))
        );
        let mut added = sample();
        (added.global).push(row(
            MappingId::Tetrad(2),
            Trigger::Midi(MidiInput::Cc(22)),
            None,
        ));
        added.focus = Some((MappingScope::Global, MappingId::Tetrad(2)));
        with_mappings(&mut ui, added);
        assert_eq!(ui.mappings.detail, Some(MappingId::Tetrad(2)));
        assert!(shown(&ui, map(MappingControl::Learn)));
    }

    /// A row opens its detail page; the trash removes it and returns to the list, and Back
    /// leaves the list for the Grid.
    #[test]
    fn rows_open_their_detail_page_and_remove_returns_to_the_list() {
        let mut ui = page(sample());
        activate(&mut ui, map(MappingControl::Open(2)));
        assert_eq!(ui.mappings.detail, Some(MappingId::Tetrad(0)));
        assert!(shown(&ui, map(MappingControl::Number)));
        // The value stepper moves in 5 % steps.
        assert_eq!(
            activate(&mut ui, map(MappingControl::Value(1))),
            sent(MappingCommand::SetValue {
                scope: MappingScope::Preset,
                id: MappingId::Tetrad(0),
                value: 55,
            })
        );
        assert_eq!(
            activate(&mut ui, map(MappingControl::Remove)),
            sent(MappingCommand::Remove {
                scope: MappingScope::Preset,
                id: MappingId::Tetrad(0),
            })
        );
        assert_eq!(ui.mappings.detail, None);
        ui.handle_input(InputEvent::Back);
        assert_eq!(ui.screen(), Screen::Grid);
    }

    /// BLOCKS › a block › a parameter sets a Block action with the catalog index, not the
    /// list position, and closes the menu.
    #[test]
    fn action_menu_drills_down_to_a_block_parameter() {
        let mut ui = page(sample());
        activate(&mut ui, map(MappingControl::Open(1)));
        activate(&mut ui, map(MappingControl::Actions));
        assert!(pick(&mut ui, "Crunch Amp").is_empty());
        assert_eq!(
            ui.overlay,
            Some(Overlay::MappingActions(ActionMenu::Block(cell(0, 2))))
        );
        assert_eq!(
            pick(&mut ui, "Master"),
            sent(MappingCommand::SetAction {
                scope: MappingScope::Preset,
                id: MappingId::Stomp {
                    stomp: 0,
                    position: cell(0, 1),
                },
                action: MappingAction::Block {
                    position: cell(0, 2),
                    target: BlockTarget::Parameter(9),
                },
            })
        );
        assert!(!ui.overlay_open());
        // GLOBAL has no block or Send MIDI actions.
        ui.handle_input(InputEvent::Back);
        activate(&mut ui, map(MappingControl::Tab(MappingScope::Global)));
        activate(&mut ui, map(MappingControl::Open(0)));
        activate(&mut ui, map(MappingControl::Actions));
        let Some(Overlay::MappingActions(menu)) = ui.overlay else {
            panic!("action menu closed");
        };
        let labels: Vec<&str> = (entries(&ui, menu).iter())
            .filter_map(|entry| match entry {
                Entry::Header(title) => Some(*title),
                _ => None,
            })
            .collect();
        assert_eq!(labels, ["SYSTEM"]);
    }

    /// The Type dropdown and the MIDI fields each send the row's full new trigger.
    #[test]
    fn trigger_controls_send_full_triggers() {
        let mut ui = page(sample());
        activate(&mut ui, map(MappingControl::Open(1)));
        let set = |trigger| {
            sent(MappingCommand::SetTrigger {
                scope: MappingScope::Preset,
                id: MappingId::Stomp {
                    stomp: 0,
                    position: cell(0, 1),
                },
                trigger,
            })
        };
        activate(&mut ui, map(MappingControl::Choice(MappingChoice::Stomp)));
        assert_eq!(
            activate(&mut ui, Control::Choose(2)),
            set(Trigger::Stomp(2))
        );
        activate(&mut ui, map(MappingControl::Choice(MappingChoice::Kind)));
        assert_eq!(
            activate(&mut ui, Control::Choose(3)),
            set(Trigger::Midi(MidiInput::Cc(FIRST_FREE_CC)))
        );
        activate(&mut ui, map(MappingControl::Choice(MappingChoice::Kind)));
        assert_eq!(
            activate(&mut ui, Control::Choose(4)),
            set(Trigger::Extension {
                key: MAPPER.into(),
                trigger: "midi".into(),
                source: String::new(),
            })
        );
        // The number of a MIDI trigger is typed on the numeric keyboard.
        ui.handle_input(InputEvent::Back);
        activate(&mut ui, map(MappingControl::Open(2)));
        activate(&mut ui, map(MappingControl::Number));
        for key in [NumericKey::Digit(6), NumericKey::Digit(4)] {
            activate(&mut ui, Control::Numeric(key));
        }
        assert_eq!(
            activate(&mut ui, Control::Numeric(NumericKey::Done)),
            sent(MappingCommand::SetTrigger {
                scope: MappingScope::Preset,
                id: MappingId::Tetrad(0),
                trigger: Trigger::Midi(MidiInput::Cc(64)),
            })
        );
    }

    /// LEARN asks the host to listen; while the row listens the button is CANCEL.
    #[test]
    fn learn_starts_and_cancels_listening() {
        let mut ui = page(sample());
        activate(&mut ui, map(MappingControl::Open(2)));
        let learn = |on| {
            sent(MappingCommand::Learn {
                scope: MappingScope::Preset,
                id: MappingId::Tetrad(0),
                on,
            })
        };
        assert_eq!(activate(&mut ui, map(MappingControl::Learn)), learn(true));
        let mut listening = sample();
        listening.learning = Some((MappingScope::Preset, MappingId::Tetrad(0)));
        with_mappings(&mut ui, listening);
        assert!(
            !shown(&ui, map(MappingControl::Number)),
            "the message field waits"
        );
        let regions = ui.hit_regions();
        let button = regions
            .iter()
            .find(|region| region.control == map(MappingControl::Learn));
        assert_eq!(button.map(|region| region.label.as_str()), Some("CANCEL"));
        assert_eq!(activate(&mut ui, map(MappingControl::Learn)), learn(false));
    }

    /// Send MIDI rows edit their messages in the stock editor; its ✓ returns to the mapping.
    #[test]
    fn send_midi_messages_open_the_editor_and_return_to_the_mapping() {
        let mut ui = page(sample());
        activate(&mut ui, map(MappingControl::Open(3)));
        activate(&mut ui, map(MappingControl::EditMessages));
        assert_eq!(ui.screen(), Screen::MidiOut);
        let target = MidiOutTarget::Footswitch(Footswitch::B);
        assert_eq!(ui.midi_edit, Some((target, 0)));
        activate(&mut ui, Control::Back);
        assert_eq!(ui.screen(), Screen::Mappings);
        assert_eq!(ui.midi_edit, None);
        let id = MappingId::MidiOut(target);
        assert_eq!(ui.mappings.detail, Some(id));
        // Rows without Send MIDI have no message editor.
        ui.handle_input(InputEvent::Back);
        activate(&mut ui, map(MappingControl::Open(1)));
        assert!(!shown(&ui, map(MappingControl::EditMessages)));
    }

    /// An edit can give a row a new id: the detail page follows the focus, and returns to the
    /// list when its row is gone.
    #[test]
    fn detail_follows_the_focus_and_closes_without_its_row() {
        let mut ui = page(sample());
        activate(&mut ui, map(MappingControl::Open(1)));
        activate(&mut ui, map(MappingControl::Actions));
        // The stock Stomp assignment becomes a Tetrad mapping with a new id.
        let mut moved = sample();
        moved.preset[1].id = MappingId::Tetrad(2);
        moved.focus = Some((MappingScope::Preset, MappingId::Tetrad(2)));
        with_mappings(&mut ui, moved.clone());
        assert_eq!(ui.mappings.detail, Some(MappingId::Tetrad(2)));
        assert!(
            ui.overlay_open(),
            "the action menu stays on the followed row"
        );
        // A refusal shows on the page.
        moved.notice = Some("CC 43 is used by the QC's MIDI map".into());
        with_mappings(&mut ui, moved.clone());
        let notice = ui
            .layout()
            .items
            .iter()
            .any(|item| item.label.starts_with("CC 43"));
        assert!(notice);
        moved.preset.remove(1);
        with_mappings(&mut ui, moved);
        assert_eq!(
            (ui.screen(), ui.mappings.detail.clone()),
            (Screen::Mappings, None)
        );
        assert!(!ui.overlay_open());
    }
}
