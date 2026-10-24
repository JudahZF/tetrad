//! The Copy and Move page (owned; preset ⋮ → Copy or Move). Stock moves a preset only inside
//! its setlist and copies one to another setlist only with Multi Select. This page lists the
//! writable setlists and every slot of the chosen one, so one preset goes to any slot of any
//! setlist. An occupied slot asks before it is replaced.

use tetrad_assets::Icon;

use super::directory::{SETLIST_SLOTS, bank_size, slot_label};
use super::directory_lists::{CONTENT, Mark, SIDE_ROW, SIDEBAR, SideRow, sidebar_row};
use crate::layout::{
    BODY, BODY_BOLD, ButtonStyle, CAPTION, LARGE_BOLD, Layout, ListId, Look, MEDIUM,
};
use crate::render::Align;
use crate::*;

const ROW: u32 = 51;

/// The open Copy and Move page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PresetTarget {
    /// The slot of the browsed setlist that is copied or moved.
    pub slot: u64,
    /// Move: the source slot is emptied.
    pub remove: bool,
    /// The setlist whose slots show (`SetlistItem::id`).
    pub setlist: u64,
    /// The occupied slot of `setlist` whose question is open.
    pub conflict: Option<u64>,
}

/// The read-only factory setlist, by its title as the setlist ⋮ menu finds it (the
/// application lists it first only when the factory library can be read).
const FACTORY: &str = "Factory Presets";

/// Setlists a preset can go to, with their sidebar index: all but Factory Presets.
pub(crate) fn writable(snapshot: &UiSnapshot) -> impl Iterator<Item = (usize, &SetlistItem)> {
    (snapshot.setlists.iter().enumerate()).filter(|(_, setlist)| setlist.title != FACTORY)
}

/// The title of slot `slot` of setlist `setlist`; `None` when the slot is empty.
pub(crate) fn slot_title(snapshot: &UiSnapshot, setlist: u64, slot: u64) -> Option<&str> {
    (snapshot.directory.presets.iter())
        .find(|preset| preset.setlist == setlist && preset.slot == slot)
        .map(|preset| preset.title.as_str())
}

impl PresetTarget {
    /// SWAP is only for a move inside the browsed setlist (`DirectoryRequest::MovePreset`).
    fn swaps(&self, snapshot: &UiSnapshot) -> bool {
        self.remove && snapshot.active_setlist == Some(self.setlist)
    }
}

impl UiState {
    /// Preset ⋮ Copy or Move: the page opens on the browsed setlist, or on the first
    /// writable one when Factory Presets is browsed.
    pub(crate) fn open_target(&mut self, slot: u64, remove: bool) {
        let s = &self.snapshot;
        let setlist = (writable(s).find(|(_, setlist)| s.active_setlist == Some(setlist.id)))
            .or_else(|| writable(s).next())
            .map(|(_, setlist)| setlist.id);
        let Some(setlist) = setlist else {
            return;
        };
        self.reset_scroll(ListId::TargetSetlists);
        self.reset_scroll(ListId::TargetSlots);
        self.directory.target = Some(PresetTarget {
            slot,
            remove,
            setlist,
            conflict: None,
        });
    }

    pub(crate) fn target_setlist(&mut self, id: u64) {
        let shown = writable(&self.snapshot).any(|(_, setlist)| setlist.id == id);
        if let Some(target) = &mut self.directory.target
            && shown
        {
            target.setlist = id;
            target.conflict = None;
            self.reset_scroll(ListId::TargetSlots);
        }
    }

    /// A slot row: an empty slot takes the preset at once, an occupied one asks first.
    pub(crate) fn target_slot(&mut self, slot: u64) -> Vec<UiCommand> {
        let Some(target) = &mut self.directory.target else {
            return Vec::new();
        };
        let own = self.snapshot.active_setlist == Some(target.setlist) && slot == target.slot;
        if own || slot >= SETLIST_SLOTS {
            return Vec::new();
        }
        if slot_title(&self.snapshot, target.setlist, slot).is_some() {
            target.conflict = Some(slot);
            return Vec::new();
        }
        self.send_target(slot, false)
    }

    /// SWAP (`overwrite` false) or OVERWRITE of the occupied-slot question.
    pub(crate) fn confirm_target(&mut self, overwrite: bool) -> Vec<UiCommand> {
        let Some(target) = &mut self.directory.target else {
            return Vec::new();
        };
        if !overwrite && !target.swaps(&self.snapshot) {
            return Vec::new();
        }
        match target.conflict.take() {
            Some(slot) => self.send_target(slot, overwrite),
            None => Vec::new(),
        }
    }

    /// The page closes. A move inside the browsed setlist is the Directory move (SWAP or
    /// OVERWRITE onto an occupied slot); every other copy or move is a one-preset PASTE,
    /// which overwrites an occupied slot.
    fn send_target(&mut self, to: u64, overwrite: bool) -> Vec<UiCommand> {
        let Some(target) = self.directory.target.take() else {
            return Vec::new();
        };
        if target.swaps(&self.snapshot) {
            return self.move_preset(target.slot, to, overwrite);
        }
        let Some(from) = self.snapshot.active_setlist_name() else {
            return Vec::new();
        };
        vec![UiCommand::CopyPresets {
            ids: vec![target.slot],
            from: from.to_owned(),
            setlist: target.setlist,
            remove: target.remove,
            to: PasteTarget::Slots(vec![to]),
        }]
    }
}

/// The page: CANCEL and the title, the writable setlists, then the slots of the chosen one.
pub(super) fn page(ui: &UiState, l: &mut Layout, target: &PresetTarget) {
    let p = l.palette;
    let s = &ui.snapshot;
    let size = bank_size(s);
    l.control(
        Rect::new(8, 8, 66, 44),
        Control::Back,
        true,
        "CANCEL",
        Look::IconButton {
            icon: Icon::Close,
            fill: p.surface,
        },
    );
    let title = (s.presets.iter())
        .find(|preset| preset.id == target.slot)
        .map_or("", |preset| preset.title.as_str());
    let verb = if target.remove { "Move" } else { "Copy" };
    l.text(
        Rect::new(86, 6, 700, 24),
        format!("{verb} {} {title} to:", slot_label(target.slot, size)),
        BODY_BOLD,
        p.text,
        Align::Left,
    );
    l.text(
        Rect::new(86, 30, 700, 22),
        "Choose a setlist, then a slot",
        CAPTION,
        p.muted,
        Align::Left,
    );
    setlists(ui, l, target);
    slots(ui, l, target);
    if let Some(slot) = target.conflict {
        conflict(ui, l, target, slot);
    }
}

fn setlists(ui: &UiState, l: &mut Layout, target: &PresetTarget) {
    let p = l.palette;
    l.panel(SIDEBAR, p.panel, 8.0, None);
    let rows: Vec<SideRow> = writable(&ui.snapshot)
        .map(|(index, setlist)| SideRow {
            selected: setlist.id == target.setlist,
            ..SideRow::new(
                Control::Library(LibraryControl::TargetSetlist(setlist.id)),
                setlist.title.clone(),
                Mark::Setlist(index),
            )
        })
        .collect();
    l.scroll(
        ListId::TargetSetlists,
        SIDEBAR,
        rows.len() as u32 * SIDE_ROW,
        ui.offset(ListId::TargetSetlists),
        |l, top| {
            for (index, row) in rows.into_iter().enumerate() {
                let y = top + (index as u32 * SIDE_ROW) as i32;
                sidebar_row(l, Rect::new(SIDEBAR.x, y, SIDEBAR.width, SIDE_ROW), row);
            }
        },
    );
}

/// Every slot of the chosen setlist: "1A Name", or a faint "Empty". The preset's own slot is
/// green and cannot be chosen.
fn slots(ui: &UiState, l: &mut Layout, target: &PresetTarget) {
    let p = l.palette;
    let s = &ui.snapshot;
    let size = bank_size(s);
    l.panel(CONTENT, p.panel, 8.0, None);
    let mut titles = vec![None; SETLIST_SLOTS as usize];
    for preset in (s.directory.presets.iter()).filter(|preset| preset.setlist == target.setlist) {
        if let Some(title) = titles.get_mut(preset.slot as usize) {
            *title = Some(preset.title.as_str());
        }
    }
    let browsed = s.active_setlist == Some(target.setlist);
    l.scroll(
        ListId::TargetSlots,
        CONTENT,
        SETLIST_SLOTS as u32 * ROW,
        ui.offset(ListId::TargetSlots),
        |l, top| {
            for (slot, title) in (0u64..).zip(titles) {
                let b = Rect::new(
                    CONTENT.x,
                    top + (slot as u32 * ROW) as i32,
                    CONTENT.width,
                    ROW,
                );
                let own = browsed && slot == target.slot;
                let label = slot_label(slot, size);
                let name = title.unwrap_or("Empty");
                let control = Control::Library(LibraryControl::TargetSlot(slot));
                l.control(b, control, !own, format!("{label} {name}"), Look::Area);
                let color = match title {
                    _ if own => p.accent,
                    Some(_) => p.text,
                    None => p.faint,
                };
                l.text(
                    Rect::new(b.x + 16, b.y, 48, b.height),
                    label,
                    BODY,
                    p.muted,
                    Align::Left,
                );
                l.text(
                    Rect::new(b.x + 72, b.y, b.width - 88, b.height),
                    name,
                    BODY,
                    color,
                    Align::Left,
                );
            }
        },
    );
}

/// "This slot is not empty": CANCEL, SWAP (a move inside the browsed setlist) and OVERWRITE.
fn conflict(ui: &UiState, l: &mut Layout, target: &PresetTarget, slot: u64) {
    let p = l.palette;
    let s = &ui.snapshot;
    let name = slot_title(s, target.setlist, slot).unwrap_or_default();
    let swaps = target.swaps(s);
    l.overlay();
    let panel = Rect::new(120, 110, 560, 260);
    l.panel(panel, p.panel, 8.0, None);
    l.text(
        Rect::new(144, 126, 512, 28),
        "This slot is not empty",
        LARGE_BOLD,
        p.text,
        Align::Left,
    );
    let slot = slot_label(slot, bank_size(s));
    let body = if swaps {
        format!(
            "{slot} holds {name}.\nSwap - the two presets change places.\nOverwrite - {name} \
             is deleted."
        )
    } else {
        format!("{slot} holds {name}.\nOverwrite - {name} is deleted.")
    };
    l.paragraph(Rect::new(144, 166, 512, 110), body, MEDIUM, p.muted);
    let answer = |overwrite| Control::Library(LibraryControl::ConfirmMove { overwrite });
    l.button(
        Rect::new(144, 294, 160, 48),
        Control::Back,
        true,
        "CANCEL",
        ButtonStyle::Secondary,
    );
    if swaps {
        l.button(
            Rect::new(320, 294, 160, 48),
            answer(false),
            true,
            "SWAP",
            ButtonStyle::Primary,
        );
    }
    l.button(
        Rect::new(496, 294, 160, 48),
        answer(true),
        true,
        "OVERWRITE",
        ButtonStyle::Danger,
    );
}
