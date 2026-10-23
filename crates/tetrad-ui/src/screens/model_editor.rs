//! Full-screen model editors (`ModelEditor` child pick `0x4f1f5c`): the Pattern Tremolo step
//! editor (`PatternTremoloModelEditor` `0x3ae61c`, model 7040) and the cab editor
//! (`CabsimModelEditor` `0x643e44`, categories 12, 21, 32, 33) here; the EQ editors in
//! `eq_editor.rs`; IR Loader and Neural Capture in `media_editor.rs`. Each body is under the
//! editor header row. Stock geometry and colours; the step bars, speaker and mic are
//! owned drawings (the stock `pattern_tremolo_steps/*.png` and cab icons are not used).

use tetrad_assets::{Icon, IconSize, Rgb, TextSize, Weight, block};

use crate::layout::{BODY, BODY_BOLD, CAPTION, LARGE, Layout, ListId, Look};
use crate::render::{Align, Font, WIDTH};
use crate::state::{MediaPick, Overlay};
use crate::{
    BlockInfo, CabMic, Control, EditorControl, GridPosition, KnobIndex, MediaKind, ModelCategory,
    ParameterKind, ParameterTarget, ParameterView, Rect, UiCommand, UiState,
};

/// Top of the editor body: the header row above it is the generic editor header.
pub(crate) const BODY_TOP: i32 = 60;

const PATTERN_TREMOLO: u32 = 7040;
/// STEPS (1..=16), then STEP 1..=16 at 11..=26 and STEP n CONNECTED at 27..=41.
const STEPS: u16 = 10;
const FIRST_STEP: u16 = 11;
const FIRST_LINK: u16 = 27;
const STEP_COUNT: u8 = 16;
/// The Step Groups tooltip body (`0x3adf84`, shown by the shared info popup `0x54617c`).
const STEP_GROUPS: &str = "Tap the top of a step, then drag horizontally to group them. Grouped steps share the same volume value.\nTo ungroup, drag the edges inward until only one step remains.";
/// STEP values have 21 steps; the bar grows 10 px per step up to its 198 px track
/// (`0x3a68dc`: 200 / (steps − 1) · index, clamped to height − 40).
const STEP_MAX: f32 = 20.0;
const STEP_PX: i32 = 10;
/// Cab IR table row height (`0x53de24(51)`).
const IR_ROW: u32 = 51;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModelEditor {
    PatternTremolo,
    /// `plugin`: the cab has a plugin id (`0x2ed194`), which picks its page 2 encoder table.
    Cab {
        model: u32,
        plugin: bool,
    },
    /// `EQModelEditor` (`+0x120`): `bands` 8, 3 or 5 (4000, 4001, 4004); OUTPUT index.
    Eq {
        bands: u8,
        output: u16,
    },
    /// `GraphicEQModelEditor` (`+0x144`, 4005–4007); Nolly (4007) adds the Q knob.
    GraphicEq {
        nolly: bool,
    },
    /// `IRLoaderModelEditor` (`+0x130`, 29001–29008).
    IrLoader {
        dual: bool,
        lite: bool,
    },
    /// `NCModelEditor` (`+0x128`, 14000–14001).
    Capture,
    /// `ArpeggioDelayModelEditor` (`+0x13c`, 6031).
    Arpeggio,
    /// `MinivoicerModelEditor` (`+0x134`, 18007).
    Minivoicer,
    /// `MultivoicerModelEditor` (`+0x138`, 18012).
    Multivoicer,
    /// `MonosynthModelEditor` (`+0x148`, 30001 Overlord Synth).
    Monosynth,
    /// `ParallaxModelEditor` (`+0x140`, 3008).
    Parallax,
}

impl ModelEditor {
    pub(crate) fn of(block: BlockInfo) -> Option<Self> {
        use ModelCategory as C;
        match (block.model_id, block.category) {
            (Some(PATTERN_TREMOLO), _) => Some(Self::PatternTremolo),
            (
                model,
                Some(C::GuitarCabMono | C::BassCabMono | C::GuitarCabStereo | C::BassCabStereo),
            ) => Some(Self::Cab {
                model: model.unwrap_or(0),
                plugin: block.plugin.is_some(),
            }),
            (Some(4000), _) => Some(Self::Eq {
                bands: 8,
                output: 40,
            }),
            (Some(4001), _) => Some(Self::Eq {
                bands: 3,
                output: 15,
            }),
            (Some(4004), _) => Some(Self::Eq {
                bands: 5,
                output: 25,
            }),
            (Some(id @ 4005..=4007), _) => Some(Self::GraphicEq { nolly: id == 4007 }),
            (Some(id @ 29001..=29008), _) => Some(Self::IrLoader {
                dual: matches!(id, 29003 | 29004 | 29007 | 29008),
                lite: id >= 29005,
            }),
            (Some(14000 | 14001), _) => Some(Self::Capture),
            (Some(6031), _) => Some(Self::Arpeggio),
            (Some(18007), _) => Some(Self::Minivoicer),
            (Some(18012), _) => Some(Self::Multivoicer),
            (Some(30001), _) => Some(Self::Monosynth),
            (Some(3008), _) => Some(Self::Parallax),
            _ => None,
        }
    }

    /// Full-screen editors cover the Grid. The Neural Capture editor (`NCModelEditor`,
    /// 800 × 224) sits in the block panel under the Grid (shot sn-7).
    pub(crate) fn full_screen(self) -> bool {
        self != Self::Capture
    }

    /// Knob pages (the header TabBar): Pattern Tremolo, cab and Graphic EQ have two
    /// (`0x3af8f8`; cab and Graphic EQ page 2 are I); EQ uses its band tabs; IR Loader has
    /// the IR page and, unless a Single Lite, a second page (reverb, GLOBAL OUTPUT; I).
    pub(crate) fn pages(self) -> usize {
        match self {
            Self::PatternTremolo
            | Self::Cab { .. }
            | Self::GraphicEq { .. }
            | Self::Minivoicer
            | Self::Multivoicer => 2,
            Self::Parallax => 3,
            Self::IrLoader { dual, lite } if dual || !lite => 2,
            Self::Eq { .. }
            | Self::IrLoader { .. }
            | Self::Capture
            | Self::Arpeggio
            | Self::Monosynth => 1,
        }
    }
}

pub(super) fn parameter(ui: &UiState, index: u16) -> Option<&ParameterView> {
    ui.snapshot.parameters.iter().find(|p| p.index == index)
}

fn position_of(ui: &UiState, index: u16) -> f32 {
    match parameter(ui, index).map(|p| p.kind) {
        Some(
            ParameterKind::Continuous { position }
            | ParameterKind::Centred { position }
            | ParameterKind::Int { position },
        ) => position,
        Some(ParameterKind::Toggle { on }) => f32::from(u8::from(on)),
        _ => 0.0,
    }
}

fn toggle(ui: &UiState, index: u16) -> bool {
    matches!(
        parameter(ui, index).map(|p| p.kind),
        Some(ParameterKind::Toggle { on: true })
    )
}

/// STEP `step` in value steps 0..=20.
fn step_value(ui: &UiState, step: u8) -> u8 {
    (position_of(ui, FIRST_STEP + u16::from(step)) * STEP_MAX).round() as u8
}

/// STEPS as a count 1..=16.
fn step_count(ui: &UiState) -> u8 {
    1 + (position_of(ui, STEPS) * f32::from(STEP_COUNT - 1)).round() as u8
}

pub(crate) fn body(ui: &UiState, l: &mut Layout, editor: ModelEditor, hue: Rgb) {
    match editor {
        ModelEditor::PatternTremolo => pattern(ui, l, hue),
        ModelEditor::Cab { .. } => cab(ui, l),
        ModelEditor::Eq { bands, output } => super::eq_editor::eq(ui, l, bands, output),
        ModelEditor::GraphicEq { nolly } => super::eq_editor::graphic(ui, l, nolly),
        ModelEditor::IrLoader { dual, lite } => {
            super::media_editor::ir_loader(ui, l, dual, lite, hue)
        }
        ModelEditor::Capture => {
            let top = super::grid::PANEL_TOP + BODY_TOP;
            super::media_editor::capture(ui, l, top, hue)
        }
        ModelEditor::Arpeggio => super::fx_editor::arpeggio(ui, l, hue),
        ModelEditor::Minivoicer
        | ModelEditor::Multivoicer
        | ModelEditor::Monosynth
        | ModelEditor::Parallax => super::synth_editor::body(ui, l, editor, hue),
    }
}

// --- Pattern Tremolo -------------------------------------------------------------------

/// The scroller strip (`0x15494ac`: 583 × 52 at 207,16) and its 291 px page box.
const STRIP: Rect = Rect::new(207, BODY_TOP + 16, 583, 52);
/// Fader viewport (`0x1549458`: 784 × 238 at 8,68); faders are 91 px on a 99 px pitch.
const FADERS: Rect = Rect::new(8, BODY_TOP + 68, 784, 238);
const FADER_PITCH: i32 = 99;
const FADER_WIDTH: u32 = 91;
/// The scroller has 9 positions (`0x3a74e4(9)`): the first shown fader 0..=8.
pub(crate) const LAST_OFFSET: u8 = 8;
/// Knob columns (`0x154c074`).
const KNOB_X: [i32; 5] = [8, 165, 322, 479, 636];
const COUNT_FONT: Font = Font::new(TextSize::Px32, Weight::Regular);
const SIGN_FONT: Font = Font::new(TextSize::Px40, Weight::Regular);
/// Scroller page box: `#111c26` with a 3 px `#3500f1` (modulation) border, radius 6.
const BOX_FILL: Rgb = Rgb(0x11, 0x1c, 0x26);
const BOX_BORDER: Rgb = block::MODULATION;

/// The scroller offset for a touch at `x`: the page box centres on the finger (I).
pub(crate) fn scroller_offset(x: i32) -> u8 {
    let step = STRIP.width as f32 / f32::from(STEP_COUNT);
    let first = (x - STRIP.x) as f32 / step - f32::from(LAST_OFFSET) / 2.0;
    first.round().clamp(0.0, f32::from(LAST_OFFSET)) as u8
}

/// Knobs of a page (`0x3af8f8`): page 1 SYNC, RATE (SYNC off) or SYNC NOTE (SYNC on),
/// SWING, DEPTH, SMOOTH; page 2 PING PONG, RETRIGGER, OUTPUT.
fn pattern_knobs(ui: &UiState) -> Vec<u16> {
    if ui.param_page == 0 {
        let rate = if toggle(ui, 0) { 2 } else { 1 };
        vec![0, rate, 3, 4, 5]
    } else {
        vec![6, 7, 8]
    }
}

fn pattern(ui: &UiState, l: &mut Layout, hue: Rgb) {
    let palette = l.palette;
    let top = BODY_TOP;
    l.push(Rect::new(0, top, WIDTH, 420), "", Look::Fill(palette.panel));
    let count = step_count(ui);
    // A hold on the STEPS area assigns STEPS to Scenes (`onLongPressStepsSceneModeButton`).
    l.control(
        Rect::new(9, top + 8, 70, 65),
        Control::Editor(EditorControl::Knob(STEPS)),
        true,
        "STEPS",
        Look::Area,
    );
    l.text(
        Rect::new(16, top + 13, 52, 22),
        "STEPS",
        BODY,
        palette.muted,
        Align::Left,
    );
    l.text(
        Rect::new(16, top + 35, 40, 38),
        count.to_string(),
        COUNT_FONT,
        palette.text,
        Align::Left,
    );
    if parameter(ui, STEPS).is_some_and(|p| p.scene) {
        l.panel(Rect::new(55, top + 44, 7, 7), palette.text, 1.0, None);
    }
    for (x, control, sign, enabled) in [
        (79, EditorControl::StepsMinus, "-", count > 1),
        (143, EditorControl::StepsPlus, "+", count < STEP_COUNT),
    ] {
        let button = Rect::new(x, top + 16, 56, 52);
        l.panel(button, palette.background, 8.0, None);
        l.control(button, Control::Editor(control), enabled, sign, Look::Area);
        l.text(
            button,
            sign,
            SIGN_FONT,
            if enabled { palette.text } else { palette.faint },
            Align::Center,
        );
    }
    scroller(ui, l);
    l.clipped(FADERS, |l| {
        for step in 0..STEP_COUNT {
            fader(ui, l, step, count, hue);
        }
    });
    // The 16 px info icon at (765, 84); I: the touch area around it is 36 px.
    l.control(
        Rect::new(755, top + 74, 36, 36),
        Control::Editor(EditorControl::StepGroupsInfo),
        true,
        "STEP GROUPS",
        Look::Glyph {
            icon: Icon::Info,
            size: IconSize::Small,
            color: palette.muted,
        },
    );
    for (x, index) in KNOB_X.into_iter().zip(pattern_knobs(ui)) {
        if let Some(p) = parameter(ui, index) {
            // Stock cells are 157 × 96 at y 319; the owned knob face needs 108 px, so the
            // cell starts at the fader bottom (y 307).
            knob_cell(l, Rect::new(x, top + 307, 157, 108), p, hue);
        }
    }
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

fn scroller(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    l.panel(STRIP, p.canvas, 6.0, None);
    let step = STRIP.width as f32 / f32::from(STEP_COUNT);
    let page = Rect::new(
        STRIP.x + (f32::from(ui.step_offset) * step).round() as i32,
        STRIP.y,
        STRIP.width / 2,
        STRIP.height,
    );
    l.panel(page, BOX_BORDER, 6.0, None);
    l.panel(page.inset(3, 3), p.tone(BOX_FILL), 3.0, None);
    let steps: [f32; 16] =
        std::array::from_fn(|step| f32::from(step_value(ui, step as u8)) / STEP_MAX);
    let points = envelope(
        &steps,
        position_of(ui, 3),
        position_of(ui, 4),
        position_of(ui, 5),
    );
    l.clipped(STRIP, |l| {
        for pair in points.windows(2) {
            let at = |(x, y): (f32, f32)| (STRIP.x as f32 + x, STRIP.y as f32 + y);
            l.line(at(pair[0]), at(pair[1]), 2.0, p.text);
        }
    });
    l.control(
        STRIP,
        Control::Editor(EditorControl::Scroller),
        true,
        "STEP SCROLLER",
        Look::Area,
    );
}

/// The amplitude graph of the scroller (`0x3b01e0`) in strip coordinates: one level per
/// step from DEPTH and the step value; odd steps start late by SWING / 2 of a step; SMOOTH
/// sets the slope of each level change. All inputs are normalised.
fn envelope(steps: &[f32; 16], swing: f32, depth: f32, smooth: f32) -> Vec<(f32, f32)> {
    let width = STRIP.width as f32 / 16.0;
    let span = STRIP.height as f32 - 3.0;
    let base = STRIP.height as f32 - 1.0;
    let slope = (1.0 / width).max(span * (smooth * 0.01).min(0.01));
    let level = |value: f32| (span * (1.0 - depth) + span * value).min(span);
    let mut carry = 0.0;
    let mut points = Vec::with_capacity(64);
    for (index, &value) in steps.iter().enumerate() {
        let i = index as f32;
        let odd = index % 2 == 1;
        let x0 = if odd { i + swing * 0.5 } else { i } * width;
        let x1 = if odd { i + 1.0 } else { i + swing * 0.5 + 1.0 } * width;
        let before = if carry == 0.0 {
            level(steps[(index + 15) % 16])
        } else {
            carry
        };
        let target = level(value);
        let dir = if before <= target { slope } else { -slope };
        let mut x = x0 + dir * (target - before);
        let mut reached = target;
        carry = 0.0;
        if x1 < x {
            reached = before + (x1 - x0) / dir;
            carry = reached;
            x = x1;
        }
        points.push((x0, base - before));
        points.push((x, base - reached));
        if x < x1 {
            points.push((x1, base - reached));
        }
    }
    points
}

fn fader(ui: &UiState, l: &mut Layout, step: u8, count: u8, hue: Rgb) {
    let p = l.palette;
    let x = FADERS.x + (i32::from(step) - i32::from(ui.step_offset)) * FADER_PITCH;
    if x >= FADERS.right() || x + FADER_PITCH <= FADERS.x {
        return;
    }
    let links = &ui.snapshot.step_connected;
    let joined_next = usize::from(step) < links.len() && links[usize::from(step)];
    let joined_prev = step > 0 && links[usize::from(step) - 1];
    // A step joined to the next one is 8 px wider and meets it (`step20_plus8.png`).
    let width = FADER_WIDTH + if joined_next { 8 } else { 0 };
    // Steps past STEPS show at opacity 0.3 (`0x3a8560`).
    let dim = |color: Rgb| {
        if step < count {
            color
        } else {
            p.panel.mix(color, 77)
        }
    };
    let top = FADERS.y;
    let label = Rect::new(x, top + 16, width, 22);
    // Grouped step numbers turn `#121212` (`0x3a81a8`); the band under them is owned.
    let grouped = joined_next || joined_prev;
    if grouped {
        l.push(label, "", Look::Fill(dim(p.muted)));
    }
    l.text(
        Rect::new(x, top + 16, FADER_WIDTH, 22),
        (step + 1).to_string(),
        BODY,
        dim(if grouped { p.background } else { p.muted }),
        Align::Center,
    );
    let track = Rect::new(x, top + 40, width, 198);
    l.panel(track, dim(p.surface), 6.0, None);
    let filled = (i32::from(step_value(ui, step)) * STEP_PX).clamp(0, track.height as i32) as u32;
    if filled > 0 {
        l.panel(
            Rect::new(x, track.bottom() - filled as i32, width, filled),
            dim(hue),
            6.0_f32.min(filled as f32 / 2.0),
            None,
        );
    }
    let index = FIRST_STEP + u16::from(step);
    if parameter(ui, index).is_some_and(|p| p.scene) {
        crate::widgets::scene_mark(l, x + 8, track.y + 8);
    }
    l.control(
        Rect::new(x, top, FADER_WIDTH, FADERS.height),
        Control::Editor(EditorControl::Step(step)),
        true,
        format!("STEP {}", step + 1),
        Look::Area,
    );
}

// --- Cab ------------------------------------------------------------------------------

/// `CabsimPanel` colours by mic (`0x821408`, `0x820c4c`).
const MIC_HUES: [Rgb; 2] = [block::CAB, block::PITCH];
/// Panels 390 × 413 at x 8 and 401.
const PANEL_X: [i32; 2] = [8, 401];
/// Mic graph item at 54,74 of its panel, 282 × 272; the mic stays 26 px off the edges.
const GRAPH: (i32, i32, f32, f32) = (54, 74, 282.0, 272.0);
const GRAPH_MARGIN: f32 = 26.0;
/// Mic knob offsets from [`CabMic::base`], left to right: POSITION, DISTANCE, LEVEL, PAN
/// (BALANCE on stereo cabs), the `displayPos` order.
const MIC_KNOBS: [u16; 4] = [5, 4, 2, 3];
/// Speaker cone fill while the mic is on.
const CONE: Rgb = Rgb(0x16, 0x16, 0x16);

fn mic_index(mic: CabMic) -> usize {
    match mic {
        CabMic::A => 0,
        CabMic::B => 1,
    }
}

/// POSITION and DISTANCE (normalised) for a touch at screen `(x, y)` (`0x825c68`): mic A
/// moves in the lower left quarter of the speaker, mic B in the lower right one. DISTANCE
/// grows downwards from the centre line, POSITION outwards from the centre (I: which slot
/// argument `micPositionChanged` writes to which parameter is not traced).
pub(crate) fn mic_place(mic: CabMic, x: i32, y: i32) -> (f32, f32) {
    let (gx, gy, w, h) = GRAPH;
    let local_x = (x - PANEL_X[mic_index(mic)] - gx) as f32;
    let local_y = (y - BODY_TOP - gy) as f32;
    let (cx, cy) = (w / 2.0, h / 2.0);
    let distance =
        ((local_y.clamp(cy, h - GRAPH_MARGIN) - cy) / (h - cy - GRAPH_MARGIN)).clamp(0.0, 1.0);
    let position = match mic {
        CabMic::A => 1.0 - (local_x.clamp(GRAPH_MARGIN, cx) - GRAPH_MARGIN) / (cx - GRAPH_MARGIN),
        CabMic::B => (local_x.clamp(cx, w - GRAPH_MARGIN) - cx) / (w - cx - GRAPH_MARGIN),
    };
    (position.clamp(0.0, 1.0), distance)
}

fn cab(ui: &UiState, l: &mut Layout) {
    let top = BODY_TOP;
    let background = l.palette.background;
    l.push(Rect::new(0, top, WIDTH, 420), "", Look::Fill(background));
    if ui.param_page == 1 {
        filter_strip(ui, l);
        return;
    }
    for mic in CabMic::ALL {
        mic_panel(ui, l, mic);
    }
}

/// Page 2 (I: the stock `0x81e634` strip, 784 × 107 at 8,0, shows HPF, LPF and OUTPUT
/// VOLUME in 156 × 95 cells with 1 × 78 `#121212` dividers; its page switch is inferred).
fn filter_strip(ui: &UiState, l: &mut Layout) {
    let palette = l.palette;
    let strip = Rect::new(8, BODY_TOP, 784, 107);
    l.panel(strip, palette.panel, 8.0, None);
    for (column, index) in [16, 17, 18].into_iter().enumerate() {
        let x = strip.x + column as i32 * 157;
        if column > 0 {
            l.push(
                Rect::new(x - 1, strip.y + 15, 1, 78),
                "",
                Look::Fill(palette.background),
            );
        }
        if let Some(p) = parameter(ui, index) {
            knob_cell(l, Rect::new(x, strip.y, 156, 107), p, palette.text);
        }
    }
}

fn mic_panel(ui: &UiState, l: &mut Layout, mic: CabMic) {
    let palette = l.palette;
    let base = mic.base();
    let hue = MIC_HUES[mic_index(mic)];
    let px = PANEL_X[mic_index(mic)];
    let top = BODY_TOP;
    l.panel(Rect::new(px, top, 390, 413), palette.panel, 8.0, None);
    let row = top + 361;
    if ui.cab_ir[mic_index(mic)] {
        ir_mode(ui, l, mic);
        ir_selector(ui, l, mic, Rect::new(px + 82, row, 226, 44));
        return;
    }
    for (x, offset) in [2, 99, 196, 293].into_iter().zip(MIC_KNOBS) {
        if let Some(p) = parameter(ui, base + offset) {
            mini_knob(l, Rect::new(px + x, top + 13, 96, 68), p, hue);
        }
    }
    let bypassed = toggle(ui, base);
    match missing_ir(ui, mic) {
        Some(name) => missing_ir_note(l, px, name),
        None => speaker(ui, l, mic, bypassed),
    }
    let phase = Rect::new(px + 8, row, 66, 44);
    l.panel(phase, palette.surface, 8.0, None);
    l.control(
        phase,
        Control::Editor(EditorControl::Phase(mic)),
        true,
        "PHASE",
        Look::Area,
    );
    // Owned phase mark: Ø, white when the phase is inverted.
    l.text(
        phase,
        "Ø",
        Font::new(TextSize::Px24, Weight::Regular),
        if toggle(ui, base + 6) {
            palette.text
        } else {
            palette.muted
        },
        Align::Center,
    );
    ir_selector(ui, l, mic, Rect::new(px + 82, row, 226, 44));
    let power = Rect::new(px + 316, row, 66, 44);
    l.panel(power, palette.surface, 8.0, None);
    l.control(
        power,
        Control::Editor(EditorControl::MicBypass(mic)),
        true,
        if bypassed { "MIC BYPASSED" } else { "MIC ON" },
        Look::Area,
    );
    l.push(
        power,
        "",
        Look::Glyph {
            icon: Icon::Power,
            size: IconSize::Medium,
            color: if bypassed {
                palette.faint
            } else {
                palette.text
            },
        },
    );
}

/// IR mode (`0x8203a4(true)`): the knobs, mic graph, phase and power buttons and the
/// missing note hide; the IR table (390 × 357 at the panel origin, 51 px rows, `0x821c28`)
/// lists the IRs of the cab with the current one selected, and the `ir.png` and close
/// buttons (66 × 44 `#1e1e1e`) take the phase and power places. Owned: the glyphs, the row
/// look (16 px text at x 16, `#1e1e1e` selected row); I: no "SPEAKER:" section rows.
fn ir_mode(ui: &UiState, l: &mut Layout, mic: CabMic) {
    let palette = l.palette;
    let px = PANEL_X[mic_index(mic)];
    let top = BODY_TOP;
    let table = Rect::new(px, top + 4, 390, 353);
    let irs = cab_irs(ui, mic);
    let current = parameter(ui, mic.base() + 1).map(|p| p.value.as_str());
    let list = ListId::CabIrs(mic_index(mic) as u8);
    l.scroll(
        list,
        table,
        irs.len() as u32 * IR_ROW,
        ui.offset(list),
        |l, y| {
            for (index, entry) in irs.iter().enumerate() {
                let row = Rect::new(
                    table.x,
                    y + index as i32 * IR_ROW as i32,
                    table.width,
                    IR_ROW,
                );
                if Some(entry.name.as_str()) == current {
                    l.push(row, "", Look::Fill(palette.surface));
                }
                let control = Control::Editor(EditorControl::IrRow {
                    mic,
                    row: index as u16,
                });
                l.control(row, control, true, entry.name.clone(), Look::Area);
                let text = Rect::new(row.x + 16, row.y, row.width - 32, row.height);
                l.text(text, entry.name.clone(), BODY, palette.text, Align::Left);
            }
        },
    );
    let row = top + 361;
    for (x, control, label, icon) in [
        (8, EditorControl::IrBrowse(mic), "IR", Icon::IrLoader),
        (316, EditorControl::IrClose(mic), "CLOSE", Icon::Close),
    ] {
        let button = Rect::new(px + x, row, 66, 44);
        l.panel(button, palette.surface, 8.0, None);
        l.control(button, Control::Editor(control), true, label, Look::Area);
        let look = Look::Glyph {
            icon,
            size: IconSize::Medium,
            color: palette.text,
        };
        l.push(button, "", look);
    }
}

/// The IRs of `mic`'s cab: media entries of the current IR's folder whose stored selector
/// `<cab>_<mic>` shares its cab part, in list order (I: the stock list `0x8221f8` reads is
/// the model's IR set, not traced). Empty when the current IR is not in the list.
fn cab_irs(ui: &UiState, mic: CabMic) -> Vec<&crate::MediaEntry> {
    let cab = |value: &str| value.rsplit_once('_').map(|(cab, _)| cab.to_owned());
    let irs = (ui.snapshot.media.iter()).filter(|entry| entry.folder.kind() == MediaKind::Ir);
    let Some(current) =
        parameter(ui, mic.base() + 1).and_then(|p| irs.clone().find(|entry| entry.name == p.value))
    else {
        return Vec::new();
    };
    let key = (current.folder, cab(&current.value));
    irs.filter(|entry| (entry.folder, cab(&entry.value)) == key)
        .collect()
}

/// The stored IR of `mic` when the media list does not have it (`0x8221f8` sets `+0x190`):
/// its name, or "DELETED IR" when the name is empty. An empty list (not loaded) is not
/// missing.
fn missing_ir(ui: &UiState, mic: CabMic) -> Option<&str> {
    let stored = parameter(ui, mic.base() + 1)?;
    let media = &ui.snapshot.media;
    let known = (media.iter()).any(|e| e.folder.kind() == MediaKind::Ir && e.name == stored.value);
    match stored.value.as_str() {
        _ if media.is_empty() || known => None,
        "" => Some("DELETED IR"),
        name => Some(name),
    }
}

/// The missing-IR note in place of the speaker (`0x821408`, panel coordinates): the
/// warning icon 24 × 24 at (184, 162), "Missing IR file:" 390 × 21 at (0, 194) in 16 px
/// and the name 390 × 26 at (0, 223) in 20 px, both centred. I: the speaker hides while the
/// note shows. The warning mark is an owned drawing (no stock `warning.png`).
fn missing_ir_note(l: &mut Layout, px: i32, name: &str) {
    let p = l.palette;
    let top = BODY_TOP;
    let mark = Rect::new(px + 184, top + 162, 24, 24);
    l.push(mark, "", Look::Dot { color: p.warning });
    l.text(mark, "!", BODY_BOLD, p.canvas, Align::Center);
    let line = |y, height| Rect::new(px, top + y, 390, height);
    l.text(
        line(194, 21),
        "Missing IR file:",
        BODY,
        p.text,
        Align::Center,
    );
    l.text(line(223, 26), name, LARGE, p.text, Align::Center);
}

/// A 96 × 68 cab knob (`0x820c4c`); the compact face is owned: label, a 34 px dial and the
/// value under it.
pub(super) fn mini_knob(l: &mut Layout, cell: Rect, p: &ParameterView, hue: Rgb) {
    let palette = l.palette;
    l.control(
        cell,
        Control::Editor(EditorControl::Knob(p.index)),
        true,
        p.label.clone(),
        Look::Area,
    );
    l.text(
        Rect::new(cell.x, cell.y, cell.width, 16),
        p.label.clone(),
        CAPTION,
        palette.text,
        Align::Center,
    );
    let dial = Rect::new(cell.x + 31, cell.y + 17, 34, 34);
    let position = match p.kind {
        ParameterKind::Continuous { position }
        | ParameterKind::Centred { position }
        | ParameterKind::Int { position } => position,
        _ => 0.0,
    };
    for (start, sweep, color) in [
        (135.0, 270.0, palette.row_selected),
        (135.0, 270.0 * position.clamp(0.0, 1.0), hue),
    ] {
        if sweep > 0.0 {
            l.push(
                dial,
                "",
                Look::Arc {
                    width: 3.0,
                    start,
                    sweep,
                    color,
                },
            );
        }
    }
    l.text(
        Rect::new(cell.x, cell.y + 52, cell.width, 16),
        p.value.clone(),
        CAPTION,
        palette.muted,
        Align::Center,
    );
    if p.scene {
        l.panel(
            Rect::new(cell.x + 4, cell.y + 20, 7, 7),
            palette.text,
            1.0,
            None,
        );
    }
}

/// The speaker and mic (`0x825f34`): a 240 px black 2 px ring, a 166 px `#161616` cone
/// (black while the mic is bypassed, I) and a 76 px `#1e1e1e` cap; white marks at 30 % and
/// 60 % of the way to the mic, and the mic, all larger as DISTANCE grows.
fn speaker(ui: &UiState, l: &mut Layout, mic: CabMic, bypassed: bool) {
    let p = l.palette;
    let (gx, gy, w, h) = GRAPH;
    let px = PANEL_X[mic_index(mic)];
    let (ox, oy) = ((px + gx) as f32, (BODY_TOP + gy) as f32);
    let (cx, cy) = (ox + w / 2.0, oy + h / 2.0);
    let disc = |l: &mut Layout, x: f32, y: f32, r: f32, color: Rgb| {
        let d = (2.0 * r).round() as u32;
        l.push(
            Rect::new((x - r).round() as i32, (y - r).round() as i32, d, d),
            "",
            Look::Dot { color },
        );
    };
    l.push(
        Rect::new((cx - 121.0) as i32, (cy - 121.0) as i32, 242, 242),
        "",
        Look::Arc {
            width: 2.0,
            start: 0.0,
            sweep: 360.0,
            color: p.canvas,
        },
    );
    let cone = if bypassed { p.canvas } else { p.tone(CONE) };
    disc(l, cx, cy, 83.0, cone);
    disc(l, cx, cy, 38.0, p.surface);
    let base = mic.base();
    let (position, distance) = (position_of(ui, base + 5), position_of(ui, base + 4));
    let reach = w / 2.0 - GRAPH_MARGIN;
    let mx = match mic {
        CabMic::A => cx - position * reach,
        CabMic::B => cx + position * reach,
    };
    let my = cy + distance * (h / 2.0 - GRAPH_MARGIN);
    let grow = ((my - cy) / (h / 2.0)).abs() * 0.5;
    for (at, radius) in [(0.3, 7.0), (0.6, 10.0)] {
        disc(
            l,
            cx + (mx - cx) * at,
            cy + (my - cy) * at,
            radius * (1.0 + grow),
            p.text,
        );
    }
    disc(l, mx, my, 20.0 * (1.0 + grow), p.text);
    l.control(
        Rect::new(ox as i32, oy as i32, w as u32, h as u32),
        Control::Editor(EditorControl::Mic(mic)),
        true,
        match mic {
            CabMic::A => "MIC A",
            CabMic::B => "MIC B",
        },
        Look::Area,
    );
}

/// IR selector (`0x34377c`, 226 × 44): ‹ name ›. The arrows step the mic of this cab; the
/// name opens the IR picker.
fn ir_selector(ui: &UiState, l: &mut Layout, mic: CabMic, bounds: Rect) {
    let palette = l.palette;
    l.panel(bounds, palette.surface, 8.0, None);
    let name = parameter(ui, mic.base() + 1).map_or("-", |p| p.value.as_str());
    let (back, rest) = (
        Rect::new(bounds.x, bounds.y, 44, 44),
        Rect::new(bounds.x + 44, bounds.y, bounds.width - 88, 44),
    );
    let forward = Rect::new(bounds.right() - 44, bounds.y, 44, 44);
    l.control(
        rest,
        Control::Editor(EditorControl::IrList(mic)),
        true,
        name,
        Look::Area,
    );
    // A missing IR's name is grey (`0x8221f8`: `#959595`, else white).
    let color = if missing_ir(ui, mic).is_some() {
        palette.muted
    } else {
        palette.text
    };
    l.text(rest, name, CAPTION, color, Align::Center);
    for (rect, forward_step, icon) in [
        (back, false, Icon::StepBack),
        (forward, true, Icon::StepForward),
    ] {
        l.control(
            rect,
            Control::Editor(EditorControl::IrStep {
                mic,
                forward: forward_step,
            }),
            true,
            if forward_step {
                "NEXT MIC"
            } else {
                "PREVIOUS MIC"
            },
            Look::Glyph {
                icon,
                size: IconSize::Small,
                color: palette.text,
            },
        );
    }
}

/// The cab page 2 encoder table in HAL order (`0x646f4c`, tables at `0x11e8e68`,
/// `0x11e8e98`, `0x11e8ec8`, `0x11e8ef8`; −1 entries are `None`).
fn cab_page_two(model: u32, plugin: bool) -> [Option<u16>; 10] {
    const N: Option<u16> = None;
    let s = Some;
    match model {
        _ if !plugin => [s(16), s(17), s(18), N, N, N, N, N, N, N],
        21007 | 33007 => [s(16), s(17), s(18), N, N, N, N, N, N, N],
        12078 | 32089 | 12115 => [
            s(19),
            s(16),
            s(17),
            s(18),
            s(21),
            s(20),
            s(24),
            N,
            s(25),
            s(22),
        ],
        12079..=12081 | 32078..=32080 => {
            [s(24), s(16), s(17), s(18), s(26), s(25), N, s(23), N, s(27)]
        }
        _ => [s(24), s(16), s(17), s(18), s(26), s(25), N, N, N, s(27)],
    }
}

// --- Actions ----------------------------------------------------------------------------

impl UiState {
    pub(crate) fn model_editor(&self) -> Option<ModelEditor> {
        let position = self.editing_block()?;
        ModelEditor::of(self.snapshot.blocks[position.index()])
    }

    /// A hardware encoder turn (`ModelEditor::encoderUpdate` `0x4f5f38` → the editor's
    /// vtable `+0xb0`). `knob` is the Tetrad index (A–H, DOWN, TEMPO); stock keys its tables by
    /// the HAL element order (A–D 0..3, DOWN 4, E–H 5..8, TEMPO 9). Pattern Tremolo
    /// `0x3b38d0`: A turns STEPS, B moves the scroller one position by the sign
    /// (`0x3ae564`), E–H and TEMPO the five knobs of the page. Cab `0x646f4c`: page 1 A–D
    /// turn the mic A knobs and E–H the mic B knobs (table `0x11e8e38`); page 2 A–C turn HPF,
    /// LPF and OUTPUT (`0x11e8e68`), except plugin cabs other than the Parallax cabs
    /// (21007, 33007): Petrucci (12078, 32089, 12115) `0x11e8e98`, John Mayer (12079..=12081,
    /// 32078..=32080) `0x11e8ec8`, others `0x11e8ef8`. Other encoders do nothing. The EQ,
    /// IR Loader and Neural Capture maps are in their modules.
    pub(crate) fn editor_knob(
        &mut self,
        editor: ModelEditor,
        knob: KnobIndex,
        delta: i32,
    ) -> Vec<UiCommand> {
        let Some(position) = self.editing_block() else {
            return Vec::new();
        };
        let k = usize::from(knob.get());
        let nudge = |parameter: u16, steps: i32| UiCommand::NudgeParameter {
            position,
            parameter,
            steps,
        };
        let index = match editor {
            ModelEditor::Eq { .. } | ModelEditor::GraphicEq { .. } => {
                return (self.block_eq_knob(editor, knob, delta))
                    .filter(|&(index, _)| parameter(self, index).is_some())
                    .map(|(index, steps)| vec![nudge(index, steps)])
                    .unwrap_or_default();
            }
            ModelEditor::IrLoader { .. } | ModelEditor::Capture => {
                self.media_editor_knob(editor, knob)
            }
            ModelEditor::Arpeggio => self.fx_editor_knob(editor, knob),
            ModelEditor::Minivoicer
            | ModelEditor::Multivoicer
            | ModelEditor::Monosynth
            | ModelEditor::Parallax => self.synth_knob(editor, knob),
            ModelEditor::PatternTremolo => match k {
                0 => Some(STEPS),
                1 => {
                    let offset = i32::from(self.step_offset) + delta.signum();
                    self.step_offset = offset.clamp(0, i32::from(LAST_OFFSET)) as u8;
                    return Vec::new();
                }
                4..=7 => pattern_knobs(self).get(k - 4).copied(),
                9 => pattern_knobs(self).get(4).copied(),
                _ => None,
            },
            ModelEditor::Cab { model, plugin } if self.param_page == 1 => {
                // HAL order: A–D 0..3, DOWN 4, E–H 5..8, TEMPO 9.
                let hal = match k {
                    0..=3 => k,
                    4..=7 => k + 1,
                    8 => 4,
                    _ => 9,
                };
                cab_page_two(model, plugin)[hal]
            }
            ModelEditor::Cab { .. } => match k {
                0..=3 => Some(CabMic::A.base() + MIC_KNOBS[k]),
                4..=7 => Some(CabMic::B.base() + MIC_KNOBS[k - 4]),
                _ => None,
            },
        };
        index
            .filter(|&index| parameter(self, index).is_some())
            .map(|parameter| vec![nudge(parameter, delta)])
            .unwrap_or_default()
    }

    /// A tap (release) on an editor control.
    pub(crate) fn editor_control(&mut self, control: EditorControl) -> Vec<UiCommand> {
        let (Some(position), Some(editor)) = (self.editing_block(), self.model_editor()) else {
            return Vec::new();
        };
        match editor {
            ModelEditor::Eq { .. } | ModelEditor::GraphicEq { .. }
                if !matches!(control, EditorControl::Knob(_)) =>
            {
                return self.eq_control(editor, control);
            }
            ModelEditor::IrLoader { .. } | ModelEditor::Capture
                if !matches!(control, EditorControl::Knob(_)) =>
            {
                return self.media_control_tap(position, control);
            }
            ModelEditor::Arpeggio if !matches!(control, EditorControl::Knob(_)) => {
                return self.fx_editor_control(control);
            }
            ModelEditor::Minivoicer
            | ModelEditor::Multivoicer
            | ModelEditor::Monosynth
            | ModelEditor::Parallax
                if !matches!(control, EditorControl::Knob(_)) =>
            {
                return self.synth_control(editor, control);
            }
            _ => {}
        }
        let nudge = |parameter: u16, steps: i32| {
            vec![UiCommand::NudgeParameter {
                position,
                parameter,
                steps,
            }]
        };
        match control {
            EditorControl::Knob(index) => match parameter(self, index).map(|p| p.kind) {
                Some(
                    ParameterKind::Toggle { .. }
                    | ParameterKind::Choice { .. }
                    | ParameterKind::ComboBox { .. },
                ) => nudge(index, 1),
                _ => Vec::new(),
            },
            EditorControl::Step(_) => Vec::new(),
            EditorControl::StepsMinus => nudge(STEPS, -1),
            EditorControl::StepsPlus => nudge(STEPS, 1),
            EditorControl::Phase(mic) => nudge(mic.base() + 6, 1),
            EditorControl::MicBypass(mic) => nudge(mic.base(), 1),
            EditorControl::Scroller | EditorControl::Mic(_) => {
                let (x, y) = self.pointer;
                self.editor_drag(control, x, y)
            }
            EditorControl::IrStep { mic, forward } => self.step_ir(position, mic, forward),
            EditorControl::EqNode(_)
            | EditorControl::EqTab(_)
            | EditorControl::EqType
            | EditorControl::EqBypass
            | EditorControl::Fader(_)
            | EditorControl::IrPower(_)
            | EditorControl::IrPhase(_)
            | EditorControl::IrName(_)
            | EditorControl::IrNext { .. }
            | EditorControl::CaptureDownload
            | EditorControl::Tab(_)
            | EditorControl::Bank(_)
            | EditorControl::Resize
            | EditorControl::Toggle(_)
            | EditorControl::Wave { .. }
            | EditorControl::Point(_) => Vec::new(),
            EditorControl::StepGroupsInfo => {
                self.overlay = Some(Overlay::Info {
                    title: "Step Groups".into(),
                    body: STEP_GROUPS.into(),
                });
                Vec::new()
            }
            EditorControl::IrList(mic) => {
                let open = &mut self.cab_ir[mic_index(mic)];
                *open = !*open;
                Vec::new()
            }
            EditorControl::IrClose(mic) => {
                self.cab_ir[mic_index(mic)] = false;
                Vec::new()
            }
            EditorControl::IrRow { mic, row } => cab_irs(self, mic)
                .get(usize::from(row))
                .map(|entry| UiCommand::SelectText {
                    position,
                    parameter: mic.base() + 1,
                    text: entry.value.clone(),
                })
                .into_iter()
                .collect(),
            EditorControl::IrBrowse(mic) => {
                let index = mic.base() + 1;
                let current = parameter(self, index).map(|p| p.value.clone());
                if let Some(current) = current {
                    self.open_media(
                        MediaPick {
                            position,
                            parameter: index,
                            kind: MediaKind::Ir,
                            folder: None,
                        },
                        &current,
                    );
                }
                Vec::new()
            }
        }
    }

    /// A touch or drag at `(x, y)` on the scroller or a mic graph.
    pub(crate) fn editor_drag(&mut self, control: EditorControl, x: i32, y: i32) -> Vec<UiCommand> {
        if let Some(editor @ (ModelEditor::Eq { .. } | ModelEditor::GraphicEq { .. })) =
            self.model_editor()
        {
            return self.eq_drag(editor, control, x, y);
        }
        match self.model_editor() {
            Some(ModelEditor::Arpeggio) => return self.fx_editor_drag(control, x, y),
            Some(
                editor @ (ModelEditor::Minivoicer
                | ModelEditor::Multivoicer
                | ModelEditor::Monosynth
                | ModelEditor::Parallax),
            ) => return self.synth_drag(editor, control, x, y),
            _ => {}
        }
        match control {
            EditorControl::Scroller => {
                self.step_offset = scroller_offset(x);
                Vec::new()
            }
            EditorControl::Mic(mic) => {
                let Some(position) = self.editing_block() else {
                    return Vec::new();
                };
                let (place, distance) = mic_place(mic, x, y);
                // POSITION and DISTANCE are linear 0..10 (ModelRepo cab models).
                [(5, place), (4, distance)]
                    .into_iter()
                    .filter(|&(offset, value)| {
                        (position_of(self, mic.base() + offset) - value).abs() > 0.005
                    })
                    .map(|(offset, value)| UiCommand::EnterParameter {
                        target: ParameterTarget::Block(position),
                        parameter: mic.base() + offset,
                        text: format!("{:.2}", value * 10.0),
                    })
                    .collect()
            }
            _ => Vec::new(),
        }
    }

    /// The previous or next IR of the same cab ([`cab_irs`]); stops at the ends.
    fn step_ir(&self, position: GridPosition, mic: CabMic, forward: bool) -> Vec<UiCommand> {
        let index = mic.base() + 1;
        let Some(current) = parameter(self, index) else {
            return Vec::new();
        };
        let irs = cab_irs(self, mic);
        let Some(at) = irs.iter().position(|entry| entry.name == current.value) else {
            return Vec::new();
        };
        let next = if forward {
            irs.get(at + 1)
        } else {
            at.checked_sub(1).and_then(|at| irs.get(at))
        };
        next.map(|entry| UiCommand::SelectText {
            position,
            parameter: index,
            text: entry.value.clone(),
        })
        .into_iter()
        .collect()
    }

    /// A sideways drag from step `from` released on step `to` (I: stock `addToGroup` /
    /// `removeFromGroup` follow a drag over the faders; the exact gesture is not traced).
    /// The links between the two steps take the opposite of the first link's state; each
    /// changed link is one CONNECTED flip.
    pub(crate) fn group_steps(&self, from: u8, to: u8) -> Vec<UiCommand> {
        let Some(position) = self.editing_block() else {
            return Vec::new();
        };
        if from == to || to >= STEP_COUNT {
            return Vec::new();
        }
        let links = self.snapshot.step_connected;
        let (low, high) = (from.min(to), from.max(to));
        let first = if to > from { from } else { from - 1 };
        let join = !links[usize::from(first)];
        (low..high)
            .filter(|&link| links[usize::from(link)] != join)
            .map(|link| UiCommand::NudgeParameter {
                position,
                parameter: FIRST_LINK + u16::from(link),
                steps: 1,
            })
            .collect()
    }

    /// The stock parameter index a held editor control assigns to Scenes.
    pub(crate) fn editor_scene_parameter(&self, control: EditorControl) -> Option<&ParameterView> {
        match control {
            EditorControl::Knob(index) => parameter(self, index),
            EditorControl::Step(step) => parameter(self, FIRST_STEP + u16::from(step)),
            _ => None,
        }
    }
}

/// Vertical drag law of a step fader: one value step per 10 px (stock: +dy / 198 of the
/// range, `0x3a6e70`).
pub(crate) const STEP_DRAG_PX: i32 = STEP_PX;
/// The parameter a step fader drag edits.
pub(crate) const fn step_parameter(step: u8) -> u16 {
    FIRST_STEP + step as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `0x3b01e0`: with SMOOTH 0 a level change is a near-vertical edge, and a full-depth
    /// zero step sits on the strip floor.
    #[test]
    fn envelope_steps_between_levels_and_floors_at_full_depth() {
        let mut steps = [1.0; 16];
        steps[1] = 0.0;
        let points = envelope(&steps, 0.0, 1.0, 0.0);
        let width = 583.0 / 16.0;
        // Step 1 starts at x = width, falls from the top (y 2) to the floor (y 51).
        let start = points
            .iter()
            .rposition(|&(x, y)| x == width && y == 2.0)
            .unwrap();
        assert_eq!(points[start], (width, 2.0));
        let (x, y) = points[start + 1];
        assert!(x - width < 2.0, "edge width {}", x - width);
        assert_eq!(y, 51.0);
    }
}
