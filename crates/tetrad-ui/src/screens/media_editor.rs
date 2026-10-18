//! Media block editors. `IRLoaderModelEditor` (`0x800008`, models 29001–29008): one
//! `IRLoaderParamPanel` (`0x7fe098`) per IR channel, each an `IRLoaderButtonArea`
//! (`0x7fca70`, 776 × 98: power and phase 98 × 48 on the left, the 532 × 98 label area with
//! "IMPULSE RESPONSE" and the IR name or "Choose IR file", up / down 98 × 48 on the right)
//! over its knob row; the `IRLoaderReverbParamPanel` (`0x80324c`, 784 × 108 or 217) holds
//! the room knobs. `NCModelEditor` (`0x3c7cc4`, 800 × 224 in the block panel under the Grid;
//! models 14000, 14001): GAIN, BASS, MID, TREBLE, VOLUME and, for a missing capture file, the
//! warning, the missing text (`0x3c5998`) and the DOWNLOAD button; after a failed download,
//! the failure text (`0x3c5aa0`). It has no capture field (shot sn-7): the editor title is the
//! capture name, and a title tap opens the capture browser (`capture_browser.rs`).
//! Panel places and page split are inferred; the parts and texts are stock.

use tetrad_assets::{
    BACKGROUND, CANVAS, FAINT, Icon, IconSize, MUTED, PANEL, Rgb, SURFACE, TEXT, WARNING,
};

use super::model_editor::{BODY_TOP, ModelEditor, parameter};
use crate::layout::{BODY, BODY_BOLD, LARGE, Layout, Look};
use crate::render::{Align, Font, WIDTH};
use crate::state::MediaPick;
use crate::{
    Control, EditorControl, GridPosition, KnobIndex, MediaKind, ParameterKind, ParameterView, Rect,
    UiCommand, UiState,
};

/// Per channel (base 8·channel): MUTE, INVERT, IR PATH, LEVEL, HI PASS, LOW PASS, PAN (or
/// BALANCE), DELAY.
const MUTE: u16 = 0;
const INVERT: u16 = 1;
const IR_PATH: u16 = 2;
const CHANNEL_KNOBS: [u16; 5] = [3, 4, 5, 6, 7];
/// ROOM MIX, PRE DELAY, REV HI PASS, REV LOW PASS, SIZE, then GLOBAL OUTPUT.
const ROOM_KNOBS: [u16; 5] = [16, 17, 18, 19, 20];
const GLOBAL_OUTPUT: u16 = 21;
/// Capture `file_name` and the knobs (ModelRepo 14000, 14001).
const FILE_NAME: u16 = 5;
const CAPTURE_KNOBS: [u16; 5] = [0, 1, 2, 3, 4];
/// `0x3c5998`, set at each editor update (`0x3c5d38`); it replaces the constructor text
/// "It seems that the original Neural Capture file doesn't exist anymore" (`0x3c6a00`).
const MISSING_CAPTURE: &str =
    "The Neural Capture %1 is missing. Please re-download it from Cortex Cloud.";
/// `0x3c5aa0(editor, 1)` (string `0xa3635c`) after a failed cloud search or download.
const DOWNLOAD_FAILED: &str = "The Neural Capture %1 is missing. Re-downloading is not possible\u{2014}Neural Capture is either private or does not exist on the Cortex Cloud.";

const CHANNEL_HEIGHT: i32 = 207;
const NAME_FONT: Font = LARGE;

fn base(channel: u8) -> u16 {
    8 * u16::from(channel)
}

fn on(ui: &UiState, index: u16) -> bool {
    matches!(
        parameter(ui, index).map(|p| p.kind),
        Some(ParameterKind::Toggle { on: true })
    )
}

fn knob_cell(l: &mut Layout, cell: Rect, p: &ParameterView, hue: Rgb) {
    l.control(
        cell,
        Control::Editor(EditorControl::Knob(p.index)),
        true,
        p.label.clone(),
        Look::Knob {
            value: p.value.clone(),
            kind: p.kind,
            color: hue,
            focused: false,
        },
    );
    if p.scene {
        crate::widgets::scene_mark(l, cell.x + 10, cell.y + 36);
    }
}

/// A row of knob cells (157 px pitch, `#121212` dividers) on a `#2e2e2e` card.
fn knob_row(ui: &UiState, l: &mut Layout, card: Rect, knobs: &[u16], hue: Rgb) {
    for (column, &index) in knobs.iter().enumerate() {
        let x = card.x + column as i32 * 157;
        if column > 0 {
            l.push(
                Rect::new(x - 1, card.y + 15, 1, 78),
                "",
                Look::Fill(BACKGROUND),
            );
        }
        if let Some(p) = parameter(ui, index) {
            knob_cell(l, Rect::new(x, card.y, 157, card.height), p, hue);
        }
    }
}

// --- IR Loader -------------------------------------------------------------------------

pub(super) fn ir_loader(ui: &UiState, l: &mut Layout, dual: bool, lite: bool, hue: Rgb) {
    l.push(
        Rect::new(0, BODY_TOP, WIDTH, 420),
        "",
        Look::Fill(BACKGROUND),
    );
    if ui.param_page == 0 {
        for channel in 0..if dual { 2 } else { 1 } {
            ir_channel(
                ui,
                l,
                channel,
                BODY_TOP + i32::from(channel) * (CHANNEL_HEIGHT + 4),
                hue,
            );
        }
        return;
    }
    let mut top = BODY_TOP;
    if !lite {
        let card = Rect::new(8, top, 784, 108);
        l.panel(card, PANEL, 8.0, None);
        knob_row(ui, l, card, &ROOM_KNOBS, hue);
        top += 112;
    }
    if dual {
        let card = Rect::new(8, top, 157, 108);
        l.panel(card, PANEL, 8.0, None);
        knob_row(ui, l, card, &[GLOBAL_OUTPUT], hue);
    }
}

fn ir_channel(ui: &UiState, l: &mut Layout, channel: u8, top: i32, hue: Rgb) {
    let base = base(channel);
    l.panel(
        Rect::new(8, top, 784, CHANNEL_HEIGHT as u32),
        PANEL,
        8.0,
        None,
    );
    let area = (12, top + 4);
    let button = |x: i32, y: i32| Rect::new(area.0 + x, area.1 + y, 98, 48);
    // Power: MUTE on dims the glyph (`icons/24/bypass_white.png`).
    let power = button(0, 0);
    l.panel(power, SURFACE, 8.0, None);
    let muted = on(ui, base + MUTE);
    l.control(
        power,
        Control::Editor(EditorControl::IrPower(channel)),
        parameter(ui, base + MUTE).is_some(),
        if muted { "IR MUTED" } else { "IR ON" },
        Look::Glyph {
            icon: Icon::Power,
            size: IconSize::Medium,
            color: if muted { FAINT } else { TEXT },
        },
    );
    // Phase (`icons/24/phase_on.png`): owned Ø, white while INVERT is on.
    let phase = button(0, 50);
    l.panel(phase, SURFACE, 8.0, None);
    l.control(
        phase,
        Control::Editor(EditorControl::IrPhase(channel)),
        parameter(ui, base + INVERT).is_some(),
        "PHASE",
        Look::Area,
    );
    l.text(
        phase,
        "Ø",
        Font::new(
            tetrad_assets::TextSize::Px24,
            tetrad_assets::Weight::Regular,
        ),
        if on(ui, base + INVERT) { TEXT } else { MUTED },
        Align::Center,
    );
    // Label area: the IR browser (`selectFirstIRPressed` / `selectSecondIRPressed`).
    let label = Rect::new(area.0 + 100, area.1, 532, 98);
    l.panel(label, SURFACE, 8.0, None);
    let path = parameter(ui, base + IR_PATH);
    let name = path.map_or("", |p| p.value.as_str());
    let empty = name.is_empty() || name == "-";
    l.control(
        label,
        Control::Editor(EditorControl::IrName(channel)),
        path.is_some(),
        if empty { "Choose IR file" } else { name },
        Look::Area,
    );
    let text = Rect::new(label.x + 16, label.y, label.width - 32, 21);
    l.text(
        Rect::new(text.x, label.y + 9, text.width, 21),
        "IMPULSE RESPONSE",
        BODY,
        MUTED,
        Align::Left,
    );
    l.text(
        Rect::new(text.x, label.y + 38, text.width, 46),
        if empty { "Choose IR file" } else { name },
        if empty { BODY } else { NAME_FONT },
        if empty { MUTED } else { TEXT },
        Align::Left,
    );
    for (y, forward, icon) in [(0, false, Icon::Up), (50, true, Icon::Down)] {
        let rect = button(678, y);
        l.panel(rect, SURFACE, 8.0, None);
        l.control(
            rect,
            Control::Editor(EditorControl::IrNext { channel, forward }),
            path.is_some(),
            if forward { "NEXT IR" } else { "PREVIOUS IR" },
            Look::Glyph {
                icon,
                size: IconSize::Small,
                color: TEXT,
            },
        );
    }
    let knobs: Vec<u16> = CHANNEL_KNOBS
        .iter()
        .map(|offset| base + offset)
        .filter(|&index| parameter(ui, index).is_some())
        .collect();
    knob_row(ui, l, Rect::new(8, top + 100, 784, 107), &knobs, hue);
}

// --- Neural Capture --------------------------------------------------------------------

/// The stored capture when neither the media list nor the loaded captures have it (I: the
/// stock check behind the `+0x14c` notice is not traced; an empty list is not missing). A
/// capture that loaded has its device type ([`crate::UiSnapshot::captures`]); factory captures
/// are not in the media list.
fn missing_capture(ui: &UiState) -> bool {
    let Some(stored) = parameter(ui, FILE_NAME) else {
        return false;
    };
    let s = &ui.snapshot;
    let loaded = (ui.editing_block()).is_some_and(|at| s.captures[at.index()].is_some());
    let known = (s.media.iter())
        .any(|entry| entry.folder.kind() == MediaKind::Capture && entry.name == stored.value);
    !s.media.is_empty() && !known && !loaded && !stored.value.is_empty() && stored.value != "-"
}

/// The body under the editor header at `top` (shot sn-7: the knob card at y 256).
pub(super) fn capture(ui: &UiState, l: &mut Layout, top: i32, hue: Rgb) {
    let name = parameter(ui, FILE_NAME).map_or("-", |p| p.value.as_str());
    // The 108 px knob card (shot sn-7: y 256..364), with the missing-file notice row below.
    let missing = missing_capture(ui);
    let card = Rect::new(8, top, 784, if missing { 216 } else { 108 });
    l.panel(card, PANEL, 8.0, None);
    knob_row(ui, l, Rect::new(8, card.y, 784, 108), &CAPTURE_KNOBS, hue);
    if missing {
        let row = Rect::new(8, card.y + 108, 784, 108);
        let mark = Rect::new(row.x + 19, row.y + 42, 24, 24);
        l.push(mark, "", Look::Dot { color: WARNING });
        l.text(mark, "!", BODY_BOLD, CANVAS, Align::Center);
        // I: the failure hides DOWNLOAD (the `0x3c5aa0` visibility flags are not decoded)
        // and its text takes the button's width.
        let failed = ui.capture_download_failed.as_deref() == Some(name);
        let (text, width) = match failed {
            true => (DOWNLOAD_FAILED, 712),
            false => (MISSING_CAPTURE, 622),
        };
        l.paragraph(
            Rect::new(row.x + 64, row.y + 18, width, 72),
            text.replace("%1", name),
            BODY,
            TEXT,
        );
        if !failed {
            l.control(
                Rect::new(row.x + 702, row.y + 30, 74, 48),
                Control::Editor(EditorControl::CaptureDownload),
                true,
                "DOWNLOAD",
                Look::IconButton {
                    icon: Icon::Download,
                    fill: SURFACE,
                },
            );
        }
    }
}

// --- Actions ---------------------------------------------------------------------------

impl UiState {
    /// Encoders. IR Loader (`0x8023e4`): page 1 A–D and DOWN turn the knobs of IR 1, E–H and
    /// TEMPO those of IR 2 (in panel order, hidden knobs skipped); page 2 (I) A–D and DOWN
    /// the room knobs, E GLOBAL OUTPUT. Neural Capture (I): A–D and DOWN GAIN, BASS, MID,
    /// TREBLE, VOLUME.
    pub(super) fn media_editor_knob(&self, editor: ModelEditor, knob: KnobIndex) -> Option<u16> {
        let k = usize::from(knob.get());
        // HAL rows: A–D then DOWN, E–H then TEMPO.
        let (row, column) = match k {
            0..=3 => (0, k),
            8 => (0, 4),
            4..=7 => (1, k - 4),
            _ => (1, 4),
        };
        let visible = |indices: &mut dyn Iterator<Item = u16>| -> Vec<u16> {
            indices
                .filter(|&index| parameter(self, index).is_some())
                .collect()
        };
        let list = match editor {
            ModelEditor::IrLoader { .. } if self.param_page == 1 => match row {
                0 => visible(&mut ROOM_KNOBS.into_iter()),
                _ => vec![GLOBAL_OUTPUT],
            },
            ModelEditor::IrLoader { .. } => {
                visible(&mut CHANNEL_KNOBS.iter().map(|offset| base(row) + offset))
            }
            ModelEditor::Capture if row == 0 => CAPTURE_KNOBS.to_vec(),
            _ => Vec::new(),
        };
        list.get(column).copied()
    }

    /// A tap on an IR Loader or Neural Capture control.
    pub(super) fn media_control_tap(
        &mut self,
        position: GridPosition,
        control: EditorControl,
    ) -> Vec<UiCommand> {
        let nudge = |parameter: u16| {
            vec![UiCommand::NudgeParameter {
                position,
                parameter,
                steps: 1,
            }]
        };
        let pick = |state: &mut Self, parameter: u16, kind: MediaKind| {
            if let Some(current) = self::parameter(state, parameter).map(|p| p.value.clone()) {
                state.open_media(
                    MediaPick {
                        position,
                        parameter,
                        kind,
                        folder: None,
                    },
                    &current,
                );
            }
            Vec::new()
        };
        match control {
            EditorControl::IrPower(channel) => nudge(base(channel) + MUTE),
            EditorControl::IrPhase(channel) => nudge(base(channel) + INVERT),
            EditorControl::IrName(channel) => pick(self, base(channel) + IR_PATH, MediaKind::Ir),
            EditorControl::IrNext { channel, forward } => {
                self.next_media(position, base(channel) + IR_PATH, MediaKind::Ir, forward)
            }
            // The button shows only for a missing capture, whose name is never empty.
            // A new attempt drops an earlier failure (review r27 R2).
            EditorControl::CaptureDownload => {
                self.capture_download_failed = None;
                parameter(self, FILE_NAME)
                    .filter(|stored| !stored.value.is_empty())
                    .map(|stored| UiCommand::DownloadCapture {
                        hash: stored.value.clone(),
                    })
                    .into_iter()
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// The previous or next item of the same folder in the media list, no wrap (I: the
    /// stock list order behind `onPrevPressed` / `onNextPressed` is not traced).
    fn next_media(
        &self,
        position: GridPosition,
        parameter: u16,
        kind: MediaKind,
        forward: bool,
    ) -> Vec<UiCommand> {
        let Some(current) = self::parameter(self, parameter) else {
            return Vec::new();
        };
        let media = &self.snapshot.media;
        let same_kind = |entry: &&crate::MediaEntry| entry.folder.kind() == kind;
        let at = media
            .iter()
            .position(|entry| entry.folder.kind() == kind && entry.name == current.value);
        let next = match at {
            Some(at) => {
                let folder = media[at].folder;
                let same = |entry: &&crate::MediaEntry| entry.folder == folder;
                if forward {
                    media[at + 1..].iter().find(same)
                } else {
                    media[..at].iter().rev().find(same)
                }
            }
            // Nothing loaded: the first IR.
            None => media.iter().find(same_kind),
        };
        next.map(|entry| UiCommand::SelectText {
            position,
            parameter,
            text: entry.value.clone(),
        })
        .into_iter()
        .collect()
    }
}
