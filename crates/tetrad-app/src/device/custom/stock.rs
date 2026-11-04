//! The loaded preset's stock Stomp data as Custom mode actions (owned, Tetrad). Custom plays in
//! the place of stock Stomp mode, so switches A–H on page 1 of THIS PRESET show the preset's
//! Stomp-mode data: one bypass action per assigned block (Toggle, or Momentary when the
//! switch's stock momentary flag is on), a Send MIDI action with the footswitch's Preset MIDI
//! Out list, and the switch's stomp label.
//!
//! Stock CorOS reads that data, so it stays stock: an edit of these switches writes it back
//! ([`store`]), and a press plays it through the stock stomp path ([`press`]: the toggle of the
//! assigned blocks, secondary toggles, the driver hooks and the MIDI Out list). Only what stock
//! cannot hold (another action, a bypass in Set mode, a second Send MIDI) goes to the preset's
//! own layout field.
//!
//! The preset's old footswitch mappings (Tetrad 0.1.0-alpha.10 and earlier) show as ordinary
//! actions after the stock ones, and move to the preset's layout with its next edit.

use tetrad_core::performance::stomp::StompIndex;
use tetrad_preset::StompRole;
use tetrad_preset::model::Preset;
use tetrad_ui::{Footswitch, MidiOutMessage, MidiOutTarget, PresetMidiView};

use super::{Act, Binding, Page, StoredAction, empty_layout, encode, stored_message, ui_message};
use crate::device::mappings::mode::Mode;
use crate::device::mappings::{self, FootswitchAction, StoredTarget};
use crate::device::preset::PresetEdit;
use crate::device::{Action, Effect, State};

/// The stock switches: A–H on page 1.
pub(super) const STOMPS: u8 = 8;

/// Positions of the blocks that switch `stomp` toggles (primary assignments), in stored order.
fn assigned(preset: &Preset, stomp: u8) -> Vec<(u8, u8)> {
    (preset.stomp_mode_assignments.iter())
        .filter(|a| a.stomp_index == u32::from(stomp) && a.role == Some(StompRole::Primary))
        .filter_map(|a| Some((u8::try_from(a.row).ok()?, u8::try_from(a.column).ok()?)))
        .filter(|&(row, column)| mappings::position(row, column).is_some())
        .collect()
}

fn momentary(preset: &Preset, stomp: u8) -> bool {
    (preset.stomp_is_momentary.get(u32::from(stomp)))
        .copied()
        .unwrap_or(false)
}

fn midi(midi: &PresetMidiView, stomp: u8) -> &[MidiOutMessage] {
    midi.messages(MidiOutTarget::Footswitch(
        Footswitch::TILES[usize::from(stomp)],
    ))
}

/// A bypass action in a mode stock can hold: Toggle or Momentary.
fn stock_bypass(act: &Act) -> bool {
    matches!(
        act.action,
        StoredAction::Block {
            target: StoredTarget::Bypass,
            ..
        }
    ) && matches!(
        act.mode,
        None | Some(Mode::Toggle { .. } | Mode::Momentary { .. })
    )
}

/// The actions of switch `stomp`'s stock data.
fn stock_acts(preset: &Preset, list: &PresetMidiView, stomp: u8) -> Vec<Act> {
    let mode = match momentary(preset, stomp) {
        true => Mode::Momentary { value: 0.0 },
        false => Mode::Toggle { a: 1.0, b: 0.0 },
    };
    let bypasses = assigned(preset, stomp)
        .into_iter()
        .map(|(row, column)| Act {
            mode: Some(mode),
            stock: true,
            ..Act::new(StoredAction::Block {
                row,
                column,
                model: None,
                target: StoredTarget::Bypass,
            })
        });
    let messages = midi(list, stomp);
    let send = (!messages.is_empty()).then(|| Act {
        messages: messages.iter().map(stored_message).collect(),
        stock: true,
        ..Act::new(StoredAction::SendMidi)
    });
    bypasses.chain(send).collect()
}

/// The loaded preset's own layout (`own`, its layout field) with its stock Stomp data and its
/// old footswitch mappings on page 1; `None` without a loaded preset.
pub(super) fn merged(state: &State, own: Option<Vec<Page>>) -> Option<Vec<Page>> {
    let preset = &state.current.as_ref()?.preset;
    let list = crate::device::ui::project::preset_midi(preset);
    let mut layout = own.unwrap_or_else(empty_layout);
    let old = mappings::footswitch_rows(state);
    for stomp in 0..STOMPS {
        let binding = &mut layout[0].slots[usize::from(stomp)].press;
        // The layout field never holds stock data; a field written elsewhere might.
        binding.actions.retain(|act| !stock_bypass(act));
        let mut actions = stock_acts(preset, &list, stomp);
        actions.append(&mut binding.actions);
        let moved = (old.iter())
            .filter(|row| row.stomp == stomp)
            .map(|row| Act {
                mode: row.mode,
                ..Act::new(match row.action {
                    FootswitchAction::System(action) => StoredAction::System { action },
                    FootswitchAction::Block {
                        row,
                        column,
                        model,
                        target,
                    } => StoredAction::Block {
                        row,
                        column,
                        model: Some(model),
                        target,
                    },
                })
            });
        actions.extend(moved);
        actions.truncate(tetrad_ui::MAX_ACTIONS);
        binding.actions = actions;
        let label = preset.single_stomp_labels.get(u32::from(stomp));
        if binding.label.is_none() {
            binding.label = label.filter(|label| !label.is_empty()).cloned();
        }
    }
    Some(layout)
}

/// The stock stomp press or release of switch `stomp` (A–H on page 1 of the preset's
/// layout), when the preset has stock data for it.
pub(super) fn press(state: &State, binding: &Binding, stomp: u8, pressed: bool) -> Vec<Effect> {
    let Some(index) = StompIndex::new(u32::from(stomp)) else {
        return Vec::new();
    };
    if !binding.actions.iter().any(|act| act.stock) {
        return Vec::new();
    }
    crate::device::input::stock_stomp(state, index, pressed)
}

/// The edits that store the preset's own `layout`: switches A–H on page 1 keep their stock
/// part (bypasses in Toggle or Momentary, the first Send MIDI, the label) in the stock data,
/// the rest goes to the layout field. The old footswitch mappings leave the Mappings field.
pub(super) fn store(state: &State, layout: &[Page]) -> Vec<Effect> {
    let Some(current) = state.current.as_ref() else {
        return Vec::new();
    };
    let preset = &current.preset;
    let list = crate::device::ui::project::preset_midi(preset);
    let mut own = layout.to_vec();
    let mut effects = Vec::new();
    let edit = |edit| Effect::Action(Action::PresetEdit(edit));
    for stomp in 0..STOMPS {
        let binding = &mut own[0].slots[usize::from(stomp)].press;
        let mut send = None;
        let mut bypasses = Vec::new();
        let mut held = false;
        binding.actions.retain(|act| match &act.action {
            StoredAction::Block { row, column, .. } if stock_bypass(act) => {
                held |= matches!(act.mode, Some(Mode::Momentary { .. }));
                bypasses.push((*row, *column));
                false
            }
            StoredAction::SendMidi if send.is_none() => {
                send = Some(act.messages.iter().map(ui_message).collect::<Vec<_>>());
                false
            }
            _ => true,
        });
        let before = assigned(preset, stomp);
        let slot = |(row, column)| mappings::position(row, column).and_then(mappings::engine_slot);
        for &gone in before.iter().filter(|at| !bypasses.contains(at)) {
            if let Some(slot) = slot(gone) {
                effects.push(edit(PresetEdit::Stomp {
                    slot,
                    assignment: None,
                }));
            }
        }
        for &new in bypasses.iter().filter(|at| !before.contains(at)) {
            if let Some(slot) = slot(new) {
                effects.push(edit(PresetEdit::Stomp {
                    slot,
                    assignment: Some((u32::from(stomp), StompRole::Primary)),
                }));
            }
        }
        if !bypasses.is_empty() && held != momentary(preset, stomp) {
            effects.push(edit(PresetEdit::Momentary {
                stomp: u32::from(stomp),
                momentary: held,
            }));
        }
        let messages = send.unwrap_or_default();
        if messages.as_slice() != midi(&list, stomp) {
            effects.push(Effect::Action(Action::SetPresetMidi {
                target: MidiOutTarget::Footswitch(Footswitch::TILES[usize::from(stomp)]),
                messages,
            }));
        }
        let label = binding.label.take().unwrap_or_default();
        let stored = preset.single_stomp_labels.get(u32::from(stomp));
        if stored.map_or("", String::as_str) != label {
            effects.push(Effect::Action(Action::StompLabel {
                stomp: u32::from(stomp),
                label,
            }));
        }
    }
    let empty = own.iter().all(|page| *page == Page::default());
    effects.push(edit(PresetEdit::CustomLayout(
        (!empty).then(|| encode(&own)),
    )));
    effects.extend(mappings::without_footswitch_rows(state).map(edit));
    effects
}
