//! PREFERRED INSTRUMENT page (`PresetMetadataDialog` `0x7e79b8`) of the preset save dialog
//! (`PresetSaveDialogBase` `0x7e4768`) for Save As and Edit Details (`0x58fe80`: the page's
//! instrument goes with `presetSaveSignal` or `modifyPresetMetadataSignal`). The header
//! follows the capture save page (shot 030); the picker values are from `0x7e2460`.

use tetrad_assets::{Icon, IconSize, TextSize, Weight};

use crate::layout::{BODY, Layout, Look};
use crate::render::{Align, Font, HEIGHT, WIDTH};
use crate::state::{Keyboard, TextTarget};
use crate::widgets::PAGE;
use crate::*;

/// `NewDialogWithHeader` close button (`0x3dce80`, `icons/24/close.png`).
const CLOSE: Rect = Rect::new(8, 0, 98, 60);
/// The header buttons: each x = the previous right + 8, y 8, height 44 (`0x536ef0`,
/// `0x537350`); the divider is 2 × 21 `#2e2e2e` (re-r44 §4, C).
const DIVIDER: Rect = Rect::new(114, 19, 2, 21);
const FOLDER: Rect = Rect::new(124, 8, 179, 44);
const NAME_X: i32 = 311;
const GAP: i32 = 8;
/// Forward 98 × 60 (`PresetSaveDialog` `0x58e044`, `icons/24/forward.png`).
const FORWARD: Rect = Rect::new(694, 0, 98, 60);
/// "KeyboardTextfield" at (40, 97), text width 720, on "TextfieldBackground" `#121212`
/// (`0x2dcff8`, C). The height is I: it ends at the undo button top (y 171).
pub(crate) const FIELD: Rect = Rect::new(40, 97, 720, 74);
/// IBM Plex Sans weight 50 in the stock steps 60, 40, 32, 24 px (`+0x1ac..+0x1c4`): the
/// first size whose text fits 720 (I).
const FIELD_FONTS: [Font; 4] = [
    Font::new(TextSize::Px60, Weight::Medium),
    Font::new(TextSize::Px40, Weight::Medium),
    Font::new(TextSize::Px32, Weight::Medium),
    Font::new(TextSize::Px24, Weight::Medium),
];
/// Picker origin on the page (`0x7e7aa0`).
const PICKER: (i32, i32) = (156, 115);

impl UiState {
    pub(crate) fn preset_metadata(&mut self, control: PresetMetadataControl) -> Vec<UiCommand> {
        let Some(keyboard) = self
            .keyboard
            .as_mut()
            .filter(|keyboard| keyboard.target.has_metadata())
        else {
            return Vec::new();
        };
        match control {
            PresetMetadataControl::Open => keyboard.metadata_page = true,
            PresetMetadataControl::Back => keyboard.metadata_page = false,
            PresetMetadataControl::Instrument(instrument) => keyboard.instrument = instrument,
            PresetMetadataControl::Save => return self.apply(Control::Key(Key::Done)),
        }
        Vec::new()
    }
}

/// The stock preset save header (re-r44 §4, C) over the name keys and the PREFERRED
/// INSTRUMENT page. The active page's button (name or metadata) is `#17291a` under a
/// `#45f862` outline (`0x7e2af8(.., 1)`); the other is `#1e1e1e` and opens its page.
pub(super) fn header(ui: &UiState, l: &mut Layout, keyboard: &Keyboard) {
    let p = l.palette;
    let target = &keyboard.target;
    let (min, max) = target.limits();
    let (close, label) = match keyboard.metadata_page {
        false => (Control::Key(Key::Cancel), "CLOSE"),
        true => (Control::PresetMetadata(PresetMetadataControl::Back), "BACK"),
    };
    let flat = match keyboard.metadata_page {
        false => p.background,
        true => p.tone(PAGE),
    };
    let look = |icon, fill| Look::IconButton { icon, fill };
    l.control(CLOSE, close, true, label, look(Icon::Close, flat));
    l.panel(DIVIDER, p.panel, 0.0, None);
    // I: the folder shows the destination setlist; Tetrad picks it after the save. A Virtual
    // Device preset has no folder (O).
    if !matches!(target, TextTarget::DevicePresetSaveAs(_)) {
        let snapshot = &ui.snapshot;
        let setlist = (snapshot.active_setlist)
            .and_then(|id| snapshot.setlists.iter().find(|setlist| setlist.id == id))
            .map_or("Setlist", |setlist| setlist.title.as_str());
        l.panel(FOLDER, flat, 8.0, Some(p.panel));
        l.push(
            Rect::new(FOLDER.x + 10, FOLDER.y + 10, 24, 24),
            "",
            Look::Glyph {
                icon: Icon::FolderFilled,
                size: IconSize::Medium,
                color: p.text,
            },
        );
        l.text(
            Rect::new(FOLDER.x + 42, FOLDER.y, FOLDER.width - 50, FOLDER.height),
            setlist,
            BODY,
            p.text,
            Align::Left,
        );
    }
    // The name button (`0x7e2f50`, "Name"): `0x7e2bfc` relays its width to the text (I).
    let shown = if keyboard.text.is_empty() {
        "Name"
    } else {
        keyboard.text.as_str()
    };
    let width = (BODY.width(shown).ceil() as u32 + 26).clamp(76, 280);
    let name = Rect::new(NAME_X, 8, width, 44);
    let active = |page: bool| match page {
        true => (p.accent_fill, Some(p.accent)),
        false => (p.surface, None),
    };
    let (fill, border) = active(!keyboard.metadata_page);
    l.panel(name, fill, 8.0, border);
    // The name page shows the keys (on that page already: no change).
    let back = Control::PresetMetadata(PresetMetadataControl::Back);
    l.control(name, back, true, "NAME", Look::Area);
    l.text(name, shown, BODY, p.text, Align::Center);
    if target.has_metadata() {
        let meta = Rect::new(name.right() + GAP, 8, 66, 44);
        let (fill, border) = active(keyboard.metadata_page);
        l.panel(meta, fill, 8.0, border);
        glyph(l, meta, border.unwrap_or(p.muted));
        let open = Control::PresetMetadata(PresetMetadataControl::Open);
        l.control(meta, open, !keyboard.metadata_page, "METADATA", Look::Area);
    }
    if keyboard.metadata_page {
        let save = Control::PresetMetadata(PresetMetadataControl::Save);
        l.control(FORWARD, save, true, "SAVE", look(Icon::Save, p.action));
    } else {
        // `0x590c34`: a name that is not min..max characters greys and disables forward.
        let ok = (min..=max).contains(&keyboard.text.chars().count());
        let fill = if ok { p.action } else { p.surface };
        let done = Control::Key(Key::Done);
        l.control(FORWARD, done, ok, "FORWARD", look(Icon::Forward, fill));
    }
}

/// The "KeyboardTextfield" of the preset save keyboard: no title and no counter (the
/// "Characters left:" paint `0x2d93f0` is only in the KeyboardRename* panels, C).
pub(super) fn field(l: &mut Layout, keyboard: &Keyboard) {
    let p = l.palette;
    l.panel(FIELD, p.background, 0.0, None);
    let text = keyboard.text.as_str();
    let font = (FIELD_FONTS.into_iter())
        .find(|font| font.width(text) <= FIELD.width as f32)
        .unwrap_or(FIELD_FONTS[FIELD_FONTS.len() - 1]);
    if text.is_empty() {
        // "PlaceholderItem": `#404040` text on no fill.
        l.text(FIELD, keyboard.target.title(), font, p.section, Align::Left);
    } else {
        l.text(FIELD, text, font, p.text, Align::Left);
    }
    // I: the first text item colour `#45f862` is the caret.
    let size = font.size.px() as i32;
    let caret = (font.width(text).ceil() as i32).min(FIELD.width as i32 - 2);
    l.push(
        Rect::new(
            FIELD.x + caret + 2,
            FIELD.y + (FIELD.height as i32 - size) / 2,
            2,
            size as u32,
        ),
        "",
        Look::Fill(p.accent),
    );
}

pub(super) fn page(ui: &UiState, l: &mut Layout, keyboard: &Keyboard) {
    let p = l.palette;
    l.push(Rect::new(0, 0, WIDTH, HEIGHT), "", Look::Fill(p.tone(PAGE)));
    header(ui, l, keyboard);
    let (x, y) = PICKER;
    // The label is white at opacity 0.5.
    l.text(
        Rect::new(x, y, 200, 21),
        "PREFERRED INSTRUMENT",
        BODY,
        p.text.mix(p.tone(PAGE), 128),
        Align::Left,
    );
    for (index, instrument) in PreferredInstrument::CHOICES.into_iter().enumerate() {
        let index = index as i32;
        let button = Rect::new(x + index % 3 * 165, y + 37 + index / 3 * 52, 157, 44);
        let selected = instrument == keyboard.instrument;
        let (fill, ink) = if selected {
            (p.text, p.canvas)
        } else {
            (p.surface, p.muted)
        };
        l.panel(button, fill, 8.0, None);
        l.control(
            button,
            Control::PresetMetadata(PresetMetadataControl::Instrument(instrument)),
            true,
            instrument.label(),
            Look::Area,
        );
        l.text(button, instrument.label(), BODY, ink, Align::Center);
    }
}

/// Owned document glyph in place of the stock `metadata.png`.
fn glyph(l: &mut Layout, button: Rect, color: tetrad_assets::Rgb) {
    let (cx, cy) = button.center();
    l.push(
        Rect::new(cx - 12, cy - 12, 24, 24),
        "",
        Look::Glyph {
            icon: Icon::Document,
            size: IconSize::Medium,
            color,
        },
    );
}
