//! Preset MIDI Out (Grid main menu → Preset MIDI Out; manual §8, shot 016,
//! `MidiMatrixDialog` `0x3bf8a4`): a device drawing with the ON PRESET LOAD MESSAGE screen,
//! footswitches A–H and EXP 1/2. A target opens its message editor (`MidiOnLoadDialog`,
//! layout I): up to 12 messages, TYPE, CHANNEL and the type's three values. Every edit
//! sends the target's whole list; the application stores it in the preset.

use tetrad_assets::{Icon, Rgb};

use crate::layout::{BODY, ButtonStyle, Layout, Look};
use crate::render::Align;
use crate::settings::DeviceAction;
use crate::*;

const BODY_RECT: Rect = Rect::new(26, 110, 456, 286);
const ON_LOAD: Rect = Rect::new(144, 122, 220, 124);
const SWITCH_X: [i32; 4] = [63, 158, 253, 348];
const SWITCH_Y: [i32; 2] = [291, 361];
const PEDALS: [(MidiOutTarget, &str, i32); 2] = [
    (MidiOutTarget::Expression1, "EXP 1", 524),
    (MidiOutTarget::Expression2, "EXP 2", 670),
];
/// Stock pedal body fill (shot 016, unmapped stock colour).
const PEDAL: Rgb = Rgb(0x0a, 0x0a, 0x0a);
const CLEAR: Rect = Rect::new(610, 10, 66, 42);
const CARD: Rect = Rect::new(8, 366, 784, 106);
const CELL_WIDTH: i32 = 157;
const LIST: Rect = Rect::new(8, 92, 784, 266);
const LIST_ROW: i32 = 44;

pub(super) fn layout(ui: &UiState, l: &mut Layout) {
    match ui.midi_edit {
        Some((target, selected)) => editor(ui, l, target, selected),
        None => matrix(ui, l),
    }
}

fn matrix(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let midi = &ui.snapshot.preset_midi;
    crate::widgets::page_header(l, "Preset MIDI Out settings", Control::Back, Icon::Check);
    l.control(
        CLEAR,
        Control::MidiOut(MidiOutControl::ClearAll),
        !midi.is_empty(),
        "CLEAR ALL",
        Look::IconButton {
            icon: Icon::Trash,
            fill: p.accent_fill,
        },
    );
    l.push(Rect::new(685, 18, 1, 24), "", Look::Fill(p.section));
    let assigned = |target| !midi.messages(target).is_empty();
    // Device body with its darker footswitch band.
    l.panel(BODY_RECT, p.panel, 16.0, None);
    l.push(
        Rect::new(BODY_RECT.x, 260, BODY_RECT.width, 100),
        "",
        Look::Fill(p.row_selected),
    );
    l.panel(
        Rect::new(BODY_RECT.x, 340, BODY_RECT.width, 56),
        p.row_selected,
        16.0,
        None,
    );
    for (cx, cy, radius) in [
        (83, 180, 21),
        (443, 180, 12),
        (443, 291, 11),
        (443, 361, 11),
    ] {
        dot(l, (cx, cy), radius, p.surface);
    }
    l.panel(
        ON_LOAD,
        if assigned(MidiOutTarget::OnLoad) {
            p.accent
        } else {
            p.background
        },
        6.0,
        None,
    );
    l.control(
        ON_LOAD.inset(2, 2),
        Control::MidiOut(MidiOutControl::Target(MidiOutTarget::OnLoad)),
        true,
        "ON PRESET LOAD MESSAGE",
        Look::Panel {
            fill: p.canvas,
            radius: 5.0,
            border: None,
        },
    );
    l.push(
        Rect::new(ON_LOAD.x, ON_LOAD.y + 38, ON_LOAD.width, 50),
        "ON PRESET LOAD\nMESSAGE",
        Look::Paragraph {
            font: BODY,
            color: p.text,
            align: Align::Center,
        },
    );
    for (index, switch) in Footswitch::TILES.into_iter().enumerate() {
        let (cx, cy) = (SWITCH_X[index % 4], SWITCH_Y[index / 4]);
        let target = MidiOutTarget::Footswitch(switch);
        let letter = char::from(b'A' + index as u8).to_string();
        l.control(
            Rect::new(cx - 22, cy - 22, 76, 44),
            Control::MidiOut(MidiOutControl::Target(target)),
            true,
            letter.clone(),
            Look::Area,
        );
        dot(
            l,
            (cx, cy),
            11,
            if assigned(target) { p.accent } else { p.canvas },
        );
        l.text(
            Rect::new(cx + 20, cy - 12, 24, 24),
            letter,
            BODY,
            p.text,
            Align::Left,
        );
    }
    for (target, label, x) in PEDALS {
        let pedal = Rect::new(x, 140, 104, 258);
        l.text(
            Rect::new(x, 108, 104, 24),
            label,
            BODY,
            p.text,
            Align::Center,
        );
        l.control(
            pedal,
            Control::MidiOut(MidiOutControl::Target(target)),
            true,
            label,
            Look::Area,
        );
        // Stock 016 pedal: a trapezoid with a 2 px rim, a narrower grey face and dark
        // grip bars (7 at the toe, 5 at the heel), 64 px wide.
        let cx = x as f32 + 52.0;
        let rim = if assigned(target) {
            p.accent
        } else {
            p.surface
        };
        trapezoid(l, cx, (140, 398), (52.0, 42.0), 14.0, rim);
        trapezoid(l, cx, (142, 396), (50.0, 40.0), 12.0, p.tone(PEDAL));
        trapezoid(l, cx, (163, 389), (43.0, 34.0), 8.0, p.panel);
        for line in (0..7)
            .map(|n| 175 + n * 10)
            .chain((0..5).map(|n| 335 + n * 10))
        {
            l.push(Rect::new(x + 20, line, 64, 4), "", Look::Fill(p.surface));
        }
    }
}

/// Filled trapezoid centred on `cx` from `top` to `bottom` (exclusive), its half width
/// going from `half.0` to `half.1`, corners rounded by `radius`; drawn as 1 px rows.
fn trapezoid(
    l: &mut Layout,
    cx: f32,
    (top, bottom): (i32, i32),
    (top_half, bottom_half): (f32, f32),
    radius: f32,
    color: Rgb,
) {
    let height = (bottom - top) as f32;
    for row in top..bottom {
        let y = row as f32 + 0.5;
        let along = (y - top as f32) / height;
        let edge = (y - top as f32).min(bottom as f32 - y);
        let corner = if edge < radius {
            radius - (radius * radius - (radius - edge).powi(2)).sqrt()
        } else {
            0.0
        };
        let half = top_half + (bottom_half - top_half) * along - corner;
        l.line((cx - half, y), (cx + half, y), 1.0, color);
    }
}

fn dot(l: &mut Layout, (cx, cy): (i32, i32), radius: i32, color: Rgb) {
    let size = (radius * 2) as u32;
    l.push(
        Rect::new(cx - radius, cy - radius, size, size),
        "",
        Look::Dot { color },
    );
}

/// Editor titles (`0xa35f48`..`0xa35f84`); "Footswitch %1%2" with the letter.
fn title(target: MidiOutTarget) -> String {
    match target {
        MidiOutTarget::OnLoad => "Preset load messages".into(),
        MidiOutTarget::Footswitch(switch) => format!(
            "Footswitch {}",
            switch
                .tile()
                .map_or('?', |tile| char::from(b'A' + tile as u8))
        ),
        MidiOutTarget::Expression1 => "Expression pedal 1".into(),
        MidiOutTarget::Expression2 => "Expression pedal 2".into(),
        MidiOutTarget::Slot { page, slot, .. } => format!(
            "Switch {} · page {}",
            SLOT_NAMES.get(usize::from(slot)).copied().unwrap_or("?"),
            u16::from(page) + 1
        ),
    }
}

/// The messages behind an editor target: a preset list, or a Custom mode slot's own.
fn messages(ui: &UiState, target: MidiOutTarget) -> &[MidiOutMessage] {
    match target {
        MidiOutTarget::Slot { scope, page, slot } => (ui.snapshot.custom.as_ref())
            .and_then(|custom| custom.slot(scope, page, usize::from(slot)))
            .map_or(&[][..], |slot| &slot.messages),
        target => ui.snapshot.preset_midi.messages(target),
    }
}

/// TYPE options per target (manual §8): the on-load list has no CC Toggle.
fn kinds(target: MidiOutTarget) -> &'static [MidiMessageKind] {
    match target {
        MidiOutTarget::OnLoad => &[MidiMessageKind::Cc, MidiMessageKind::Pc],
        _ => &[
            MidiMessageKind::Cc,
            MidiMessageKind::CcToggle,
            MidiMessageKind::Pc,
        ],
    }
}

fn kind_label(kind: MidiMessageKind) -> &'static str {
    match kind {
        MidiMessageKind::Cc => "CC",
        MidiMessageKind::CcToggle => "CC Toggle",
        MidiMessageKind::Pc => "PC",
    }
}

/// Value labels by type (`0xa36070`..`0xa360d8`, `0xa31c28`); CC has two values.
fn value_labels(kind: MidiMessageKind) -> &'static [&'static str] {
    match kind {
        MidiMessageKind::Cc => &["CC#", "VALUE"],
        MidiMessageKind::CcToggle => &["CC#", "MIN VALUE", "MAX VALUE"],
        MidiMessageKind::Pc => &["BANK CC#0", "BANK CC#32", "PROGRAM#"],
    }
}

fn value_text(value: Option<u8>) -> String {
    value.map_or_else(|| "OFF".into(), |value| value.to_string())
}

fn summary(message: &MidiOutMessage) -> String {
    let mut text = format!("{}   CH {}", kind_label(message.kind), message.channel);
    // Parts left out (PC banks at OFF) are not listed.
    for (label, value) in value_labels(message.kind).iter().zip(message.values) {
        if let Some(value) = value {
            text.push_str(&format!("   {label} {value}"));
        }
    }
    text
}

fn editor(ui: &UiState, l: &mut Layout, target: MidiOutTarget, selected: usize) {
    let p = l.palette;
    let messages = messages(ui, target);
    crate::widgets::page_header(l, &title(target), Control::Back, Icon::Check);
    l.control(
        Rect::new(548, 8, 66, 44),
        Control::MidiOut(MidiOutControl::Add),
        messages.len() < MIDI_OUT_MESSAGES,
        "ADD",
        Look::Button {
            style: ButtonStyle::Secondary,
            icon: Some(Icon::Plus),
            font: None,
        },
    );
    l.control(
        Rect::new(620, 8, 66, 44),
        Control::MidiOut(MidiOutControl::Remove),
        selected < messages.len(),
        "DELETE",
        Look::Button {
            style: ButtonStyle::Danger,
            icon: Some(Icon::Trash),
            font: None,
        },
    );
    l.text(
        Rect::new(16, 62, 300, 24),
        "Midi messages",
        BODY,
        p.muted,
        Align::Left,
    );
    l.panel(LIST, p.panel, 8.0, None);
    // Two columns of six: all twelve slots stay visible.
    for index in 0..MIDI_OUT_MESSAGES {
        let column = (index / 6) as i32;
        let row = (index % 6) as i32;
        let bounds = Rect::new(
            LIST.x + column * (LIST.width as i32 / 2),
            LIST.y + 1 + row * LIST_ROW,
            LIST.width / 2,
            LIST_ROW as u32,
        );
        let Some(message) = messages.get(index) else {
            l.text(
                bounds.inset(16, 0),
                format!("{}   -", index + 1),
                BODY,
                p.faint,
                Align::Left,
            );
            continue;
        };
        crate::widgets::list_row(
            l,
            bounds,
            Control::MidiOut(MidiOutControl::Select(index as u8)),
            true,
            format!("{}   {}", index + 1, summary(message)),
            None,
            index == selected,
            None,
        );
    }
    l.push(
        Rect::new(
            LIST.x + LIST.width as i32 / 2,
            LIST.y + 8,
            1,
            LIST.height - 16,
        ),
        "",
        Look::Fill(p.divider),
    );
    l.panel(CARD, p.panel, 8.0, None);
    let Some(message) = messages.get(selected) else {
        l.text(
            CARD,
            "Tap ADD to create a message.",
            BODY,
            p.muted,
            Align::Center,
        );
        return;
    };
    let mut fields = vec![
        ("TYPE", MidiField::Kind, kind_label(message.kind).to_owned()),
        ("CHANNEL", MidiField::Channel, message.channel.to_string()),
    ];
    for (index, label) in value_labels(message.kind).iter().enumerate() {
        fields.push((
            label,
            MidiField::Value(index as u8 + 1),
            value_text(message.values[index]),
        ));
    }
    for (column, (label, field, value)) in fields.into_iter().enumerate() {
        let cell = Rect::new(
            CARD.x + column as i32 * CELL_WIDTH,
            CARD.y,
            CELL_WIDTH as u32,
            CARD.height,
        );
        if column > 0 {
            l.push(
                Rect::new(cell.x - 1, cell.y + 8, 2, cell.height - 16),
                "",
                Look::Fill(p.divider),
            );
        }
        l.text(
            Rect::new(cell.x + 10, cell.y + 6, cell.width - 20, 28),
            label,
            BODY,
            p.text,
            Align::Left,
        );
        super::stepper(
            l,
            Rect::new(cell.x + 8, cell.y + 50, cell.width - 16, 44),
            Control::MidiOut(MidiOutControl::Step(field, -1)),
            Control::MidiOut(MidiOutControl::Step(field, 1)),
            value,
            true,
        );
    }
}

/// `message` with one field stepped by `delta`. TYPE cycles the target's list and fills
/// the new type's values; channels clamp to 1..=16, values to 0..=127, and the PC bank
/// values step below 0 to OFF.
fn step(
    target: MidiOutTarget,
    mut message: MidiOutMessage,
    field: MidiField,
    delta: i32,
) -> MidiOutMessage {
    match field {
        MidiField::Kind => {
            let kinds = kinds(target);
            let index = kinds
                .iter()
                .position(|kind| *kind == message.kind)
                .unwrap_or(0) as i32;
            message.kind = kinds[(index + delta).rem_euclid(kinds.len() as i32) as usize];
            let used = value_labels(message.kind).len();
            for (index, value) in message.values.iter_mut().enumerate() {
                *value = if index < used {
                    Some(value.unwrap_or(0))
                } else {
                    None
                };
            }
        }
        MidiField::Channel => {
            message.channel = (i32::from(message.channel) + delta).clamp(1, 16) as u8;
        }
        MidiField::Value(part) => {
            let index = usize::from(part.clamp(1, 3) - 1);
            if index >= value_labels(message.kind).len() {
                return message;
            }
            let bank = message.kind == MidiMessageKind::Pc && index < 2;
            let next = message.values[index].map_or(-1, i32::from) + delta;
            message.values[index] = match next {
                ..0 if bank => None,
                next => Some(next.clamp(0, 127) as u8),
            };
        }
    }
    message
}

impl UiState {
    pub(crate) fn midi_out(&mut self, control: MidiOutControl) -> Vec<UiCommand> {
        if let MidiOutControl::Target(target) = control {
            self.midi_edit = Some((target, 0));
            return Vec::new();
        }
        if control == MidiOutControl::ClearAll {
            self.overlay = Some(crate::state::Overlay::Confirm(
                crate::state::Confirm::Device(DeviceAction::ClearPresetMidi),
            ));
            return Vec::new();
        }
        let Some((target, selected)) = self.midi_edit else {
            return Vec::new();
        };
        let mut messages = messages(self, target).to_vec();
        match control {
            MidiOutControl::Select(index) => {
                self.midi_edit = Some((target, usize::from(index)));
                return Vec::new();
            }
            MidiOutControl::Add if messages.len() < MIDI_OUT_MESSAGES => {
                messages.push(MidiOutMessage {
                    kind: MidiMessageKind::Cc,
                    channel: 1,
                    values: [Some(0), Some(0), None],
                });
                self.midi_edit = Some((target, messages.len() - 1));
            }
            MidiOutControl::Remove if selected < messages.len() => {
                messages.remove(selected);
                self.midi_edit = Some((target, selected.min(messages.len().saturating_sub(1))));
            }
            MidiOutControl::Step(field, delta) => {
                let Some(message) = messages.get_mut(selected) else {
                    return Vec::new();
                };
                *message = step(target, *message, field, i32::from(delta));
            }
            _ => return Vec::new(),
        }
        match target {
            MidiOutTarget::Slot { scope, page, slot } => {
                vec![UiCommand::Custom(crate::CustomCommand::Slot {
                    scope,
                    page,
                    slot,
                    edit: crate::SlotEdit::SetMessages(messages),
                })]
            }
            target => vec![UiCommand::SetPresetMidi { target, messages }],
        }
    }

    /// Encoders 0..=4 turn TYPE, CHANNEL and the three values of the selected message.
    pub(crate) fn midi_out_knob(&mut self, knob: KnobIndex, delta: i32) -> Vec<UiCommand> {
        let field = match knob.get() {
            0 => MidiField::Kind,
            1 => MidiField::Channel,
            part @ 2..=4 => MidiField::Value(part - 1),
            _ => return Vec::new(),
        };
        let delta = delta.clamp(i32::from(i8::MIN), i32::from(i8::MAX)) as i8;
        self.midi_out(MidiOutControl::Step(field, delta))
    }

    /// CLEAR ALL after its confirm: one empty list per assigned target.
    pub(crate) fn clear_preset_midi(&self) -> Vec<UiCommand> {
        let midi = &self.snapshot.preset_midi;
        std::iter::once(MidiOutTarget::OnLoad)
            .chain(Footswitch::TILES.map(MidiOutTarget::Footswitch))
            .chain([MidiOutTarget::Expression1, MidiOutTarget::Expression2])
            .filter(|target| !midi.messages(*target).is_empty())
            .map(|target| UiCommand::SetPresetMidi {
                target,
                messages: Vec::new(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc(channel: u8, cc: u8, value: u8) -> MidiOutMessage {
        MidiOutMessage {
            kind: MidiMessageKind::Cc,
            channel,
            values: [Some(cc), Some(value), None],
        }
    }

    /// Manual §8: per-type value sets, 1..16 channels, 0..127 values, PC banks may be OFF;
    /// the on-load list has no CC Toggle.
    #[test]
    fn editor_steps_follow_the_stock_value_rules() {
        let on_load = MidiOutTarget::OnLoad;
        let foot = MidiOutTarget::Footswitch(Footswitch::B);
        let message = cc(16, 127, 0);
        assert_eq!(step(foot, message, MidiField::Channel, 1).channel, 16);
        assert_eq!(
            step(foot, message, MidiField::Value(1), 1).values[0],
            Some(127)
        );
        assert_eq!(step(foot, message, MidiField::Value(3), 1), message);
        assert_eq!(
            step(foot, message, MidiField::Kind, 1).kind,
            MidiMessageKind::CcToggle
        );
        let pc = step(on_load, message, MidiField::Kind, 1);
        assert_eq!(pc.kind, MidiMessageKind::Pc);
        assert_eq!(pc.values, [Some(127), Some(0), Some(0)]);
        let off = step(on_load, pc, MidiField::Value(2), -1);
        assert_eq!(off.values[1], None);
        assert_eq!(
            step(on_load, off, MidiField::Value(2), 1).values[1],
            Some(0)
        );
    }

    /// Each edit sends the target's whole list; the 12-message bound holds.
    #[test]
    fn edits_send_the_whole_list_and_stop_at_twelve() {
        let mut snapshot = UiSnapshot::default();
        snapshot.preset_midi.footswitches[0] = vec![cc(1, 64, 127); MIDI_OUT_MESSAGES - 1];
        let mut ui = UiState::new();
        ui.set_snapshot(snapshot.clone()).expect("valid snapshot");
        ui.navigate(Screen::MidiOut);
        let target = MidiOutTarget::Footswitch(Footswitch::A);
        let open = Control::MidiOut(MidiOutControl::Target(target));
        assert!(ui.handle_input(InputEvent::Activate(open)).is_empty());
        let add = Control::MidiOut(MidiOutControl::Add);
        let commands = ui.handle_input(InputEvent::Activate(add));
        let [
            UiCommand::SetPresetMidi {
                target: sent,
                messages,
            },
        ] = commands.as_slice()
        else {
            panic!("expected one list: {commands:?}");
        };
        assert_eq!((*sent, messages.len()), (target, MIDI_OUT_MESSAGES));
        snapshot.preset_midi.footswitches[0] = messages.clone();
        ui.set_snapshot(snapshot).expect("valid snapshot");
        assert!(ui.handle_input(InputEvent::Activate(add)).is_empty());
        // Back closes the editor first, then the screen.
        ui.handle_input(InputEvent::Back);
        assert_eq!((ui.screen(), ui.midi_edit), (Screen::MidiOut, None));
        ui.handle_input(InputEvent::Back);
        assert_eq!(ui.screen(), Screen::Grid);
    }
}
