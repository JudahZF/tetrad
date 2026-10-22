//! Per-screen layout builders and the shared chrome (top bars, toasts).

mod capture;
mod capture_browser;
mod directory;
mod directory_glyph;
mod directory_lists;
mod eq_editor;
mod expression;
pub(crate) mod extensions;
mod fx_editor;
mod gig;
mod global_eq;
pub(crate) mod grid;
mod io;
pub(crate) mod mappings;
mod media;
mod media_editor;
mod midi_out;
pub(crate) mod model_editor;
mod numeric;
pub(crate) mod overlay;
mod picker;
mod preset_metadata;
mod presets;
mod recovery;
mod settings;
mod settings_presets;
mod storage;
mod support;
mod synth_editor;
mod tools;
mod updater;
mod vd_presets;
mod wifi;

use tetrad_assets::{Icon, Palette};

use crate::layout::{BODY_BOLD, ButtonStyle, LARGE_BOLD, Layout, Look, MEDIUM_BOLD};
use crate::render::{Align, WIDTH};
use crate::state::{HEADER, TOP_BAR};
use crate::*;

#[cfg(test)]
pub(crate) use directory::PasteSlots;
pub(crate) use directory::{Category, CloudGate, Directory};
/// The Grid header, also drawn behind the picker drawer (shot 076).
use grid::header as main_bar;

pub(crate) use capture::{CaptureEdit, CaptureUi};
pub(crate) use numeric::{NumericEntry, NumericPurpose};
pub(crate) use support::{default_device_name, licence_file, product_name};
pub(crate) use tools::{PanelKnob, panel_knob};

/// Row pitch of choice lists (also used to pre-scroll to the current value).
pub(crate) const CHOICE_ROW: u32 = 48;
/// Controls in the second header row (scene strip, bank preview, Gig bank):
/// 44 px high between the top bar and the stock grid region at y 96.
pub(crate) const STRIP_Y: i32 = TOP_BAR + 1;
pub(crate) const STRIP_HEIGHT: u32 = 44;

pub(crate) fn layout(ui: &UiState) -> Layout {
    let screen = ui.screen();
    // Recovery stays stock, so a bad theme cannot make it unreadable.
    let palette = match screen {
        Screen::Recovery => Palette::STOCK,
        _ => ui.snapshot.appearance.palette,
    };
    let mut l = Layout::new(palette);
    // Gig View and the Directory have their own headers (manual §3, §5).
    // The Grid draws its own stock header (`grid::header`).
    if screen.is_main()
        && !matches!(screen, Screen::Gig | Screen::Presets)
        && ui.snapshot.bank_preview_active
    {
        preview_banner(ui, &mut l);
    }
    match screen {
        Screen::Grid => grid::layout(ui, &mut l),
        Screen::Gig => gig::layout(ui, &mut l),
        Screen::Presets => presets::layout(ui, &mut l),
        Screen::ModelPicker => picker::layout(ui, &mut l),
        Screen::MediaPicker => media::layout(ui, &mut l),
        Screen::Settings(page) => settings::layout(ui, &mut l, page),
        Screen::Io(tab) => io::layout(ui, &mut l, tab),
        Screen::Tuner => tools::tuner(ui, &mut l),
        Screen::Looper => tools::looper(ui, &mut l),
        Screen::Tempo => tools::tempo(ui, &mut l),
        Screen::Capture => capture::layout(ui, &mut l),
        Screen::Expression => expression::layout(ui, &mut l),
        Screen::MidiOut => midi_out::layout(ui, &mut l),
        Screen::Mappings => mappings::layout(ui, &mut l),
        Screen::Recovery => recovery::layout(ui, &mut l),
        Screen::GlobalEq(tab) => global_eq::layout(ui, &mut l, tab),
    }
    if let Some(dialog) = ui.snapshot.capture.nc2 {
        capture::nc2(ui, &mut l, dialog);
    }
    // A batch upload (`0x785dcc`): the progress dialog (Z 951) under the upload questions and
    // notices (MessageDialog, Z 952). The bar is handled / total (`0x77fef0`).
    let batch = ui.snapshot.upload_progress;
    if let Some(batch) = batch {
        let text = format!("Uploading {}, please wait.", batch.noun);
        let progress = batch.handled as f32 / batch.total.max(1) as f32;
        recovery::progress_dialog(&mut l, &text, progress, Some(Control::CancelUpload));
    }
    overlay::layout(ui, &mut l);
    if let Some(entry) = &ui.numeric {
        numeric::layout(&mut l, entry);
    }
    // A single upload has no stock progress dialog (`0x785dcc` is the batch); O: its status
    // "Uploading %1, please wait." shows as the toast pill, and a toast shows over it.
    let uploading = (ui.snapshot.uploading)
        .filter(|_| batch.is_none())
        .map(|noun| format!("Uploading {noun}, please wait."));
    let status = (ui.toast.as_ref().map(|toast| toast.text.as_str())).or(uploading.as_deref());
    if let Some(text) = status {
        let width = (BODY_BOLD.width(text) as u32 + 48).min(WIDTH - 40);
        l.push(
            Rect::new((WIDTH - width) as i32 / 2, 424, width, 36),
            text,
            Look::Toast,
        );
    }
    if ui.resetting {
        recovery::locked(&mut l);
    }
    if ui.snapshot.restoring {
        recovery::restoring(&mut l);
    }
    l
}

/// Bank preview replaces the second header row on every main view.
fn preview_banner(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let row = Rect::new(0, TOP_BAR + 1, WIDTH, (HEADER - TOP_BAR - 1) as u32);
    l.push(row, "", Look::Fill(p.surface));
    l.text(
        Rect::new(16, row.y, 300, row.height),
        format!("BANK {} PREVIEW", ui.snapshot.bank.index + 1),
        LARGE_BOLD,
        p.warning,
        Align::Left,
    );
    l.button(
        Rect::new(472, STRIP_Y, 150, STRIP_HEIGHT),
        Control::CancelBankPreview,
        true,
        "CANCEL",
        ButtonStyle::Secondary,
    );
    l.button(
        Rect::new(630, STRIP_Y, 162, STRIP_HEIGHT),
        Control::EndBankPreview,
        true,
        "END PREVIEW",
        ButtonStyle::Primary,
    );
}

/// A labelled −/+ stepper with the formatted value between.
pub(crate) fn stepper(
    l: &mut Layout,
    bounds: Rect,
    down: Control,
    up: Control,
    value: String,
    enabled: bool,
) {
    let p = l.palette;
    let (minus, rest) = bounds.take_left(44, 4);
    let (middle, plus) = rest.take_right(44, 4);
    let key = |icon| Look::Button {
        style: ButtonStyle::Secondary,
        icon: Some(icon),
        font: None,
    };
    l.control(minus, down, enabled, "-", key(Icon::Minus));
    l.text(
        middle,
        value,
        MEDIUM_BOLD,
        if enabled { p.text } else { p.faint },
        Align::Center,
    );
    l.control(plus, up, enabled, "+", key(Icon::Plus));
}

/// Stock scroll thumb, `width` px wide and 8 px in from the viewport edge; drawn only when
/// the content overflows. The picker drawer uses 6 px, the ⋮ menu 2 px (seen on the QC).
pub(crate) fn scrollbar(l: &mut Layout, viewport: Rect, content: u32, offset: i32, width: u32) {
    const INSET: i32 = 8;
    let p = l.palette;
    let travel = content.saturating_sub(viewport.height);
    if travel == 0 {
        return;
    }
    let track = viewport.height as i32 - 2 * INSET;
    let thumb = (track as u32 * viewport.height / content) as i32;
    let top = offset.clamp(0, travel as i32) as u32 * (track - thumb) as u32 / travel;
    l.panel(
        Rect::new(
            viewport.right() - INSET - width as i32,
            viewport.y + INSET + top as i32,
            width,
            thumb as u32,
        ),
        p.scrollbar,
        width as f32 / 2.0,
        None,
    );
}

/// Previous/next scene around the active scene letter (Looper X editor bar, manual §4).
/// `badge_px` is the square letter badge, centred in the bar: 28 here, 24 in the block
/// editor (`b4-stock1`).
pub(crate) fn scene_stepper(ui: &UiState, l: &mut Layout, bounds: Rect, badge_px: u32) {
    let p = l.palette;
    let scene = ui.snapshot.selected_scene;
    let step = |delta: u8| {
        let index = (scene.get() + delta) % SCENE_COUNT as u8;
        Control::SelectScene(SceneIndex::new(index).expect("scene index in range"))
    };
    let (previous, rest) = bounds.take_left(40, 0);
    let (middle, next) = rest.take_right(40, 0);
    let key = |icon| Look::Button {
        style: ButtonStyle::Ghost,
        icon: Some(icon),
        font: None,
    };
    l.control(
        previous,
        step(SCENE_COUNT as u8 - 1),
        true,
        "PREVIOUS SCENE",
        key(Icon::StepBack),
    );
    let side = badge_px as i32;
    let badge = Rect::new(
        middle.x + (middle.width as i32 - side) / 2,
        middle.y + (middle.height as i32 - side) / 2,
        badge_px,
        badge_px,
    );
    l.panel(badge, p.warning, 5.0, None);
    l.text(
        badge,
        scene.letter().to_string(),
        MEDIUM_BOLD,
        p.on_accent,
        Align::Center,
    );
    l.control(next, step(1), true, "NEXT SCENE", key(Icon::StepForward));
}
