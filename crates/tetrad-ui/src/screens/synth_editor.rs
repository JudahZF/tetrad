//! The voicer, Parallax and synth block editors:
//! Minivoicer (`MinivoicerModelEditor` `0x61d080`, 18007), Multivoicer
//! (`MultivoicerModelEditor` `0x62a024`, 18012), Overlord Synth (`MonosynthModelEditor`
//! `0x632684`, 30001) and Parallax (`ParallaxModelEditor` `0x639444`, 3008). Stock geometry,
//! knob tables and encoder tables; the graphs, buttons and icons are owned drawings.

use tetrad_assets::{Icon, IconSize, Palette, Rgb, TextSize, Weight, block};

use super::fx_editor::{knob, point, position, tab_bar};
use super::model_editor::{BODY_TOP, ModelEditor, mini_knob, parameter};
use crate::layout::{BODY, CAPTION, Layout, Look};
use crate::render::{Align, Font, WIDTH};
use crate::state::DOUBLE_TAP_MS;
use crate::{Control, EditorControl, KnobIndex, POSITION_SCALE, Rect, UiCommand, UiState};

/// Knob columns of a 784 px card (`0x60c43c`, `0x630140`).
const COLUMNS: [i32; 5] = [8, 165, 322, 479, 636];
const KNOB_W: u32 = 157;
const KNOB_H: u32 = 96;
/// `−1` in a stock table.
const NONE: Option<u16> = None;

fn on(ui: &UiState, index: u16) -> bool {
    position(ui, index) > 0.5
}

/// The stock HAL slot of a Tetrad encoder (A–D 0..3, DOWN 4, E–H 5..8, TEMPO 9).
fn hal(knob: KnobIndex) -> usize {
    match knob.get() {
        k @ 0..=3 => usize::from(k),
        8 => 4,
        k @ 4..=7 => usize::from(k) + 1,
        _ => 9,
    }
}

fn card(l: &mut Layout, x: i32, y: i32, height: u32, radius: f32) {
    let panel = l.palette.panel;
    l.panel(Rect::new(x, BODY_TOP + y, 784, height), panel, radius, None);
}

/// A row of knobs at body `y` in the stock columns.
fn knob_row(
    ui: &UiState,
    l: &mut Layout,
    y: i32,
    knobs: &[Option<u16>],
    hue: Rgb,
    live: &dyn Fn(u16) -> bool,
) {
    for (&x, index) in COLUMNS.iter().zip(knobs) {
        if let Some(p) = index.and_then(|index| parameter(ui, index)) {
            let bounds = Rect::new(x, BODY_TOP + y, KNOB_W, KNOB_H);
            knob(l, bounds, p, hue, live(p.index));
        }
    }
}

/// A button that flips parameter `index` (or does another editor action).
fn button(l: &mut Layout, bounds: Rect, label: &str, control: EditorControl, lit: bool, fill: Rgb) {
    let p = l.palette;
    l.panel(bounds, if lit { fill } else { p.surface }, 8.0, None);
    l.control(bounds, Control::Editor(control), true, label, Look::Area);
    l.text(
        bounds,
        label,
        BODY,
        if lit { p.text } else { p.muted },
        Align::Center,
    );
}

pub(super) fn body(ui: &UiState, l: &mut Layout, editor: ModelEditor, hue: Rgb) {
    let p = l.palette;
    l.push(
        Rect::new(0, BODY_TOP, WIDTH, 420),
        "",
        Look::Fill(p.background),
    );
    match editor {
        ModelEditor::Minivoicer => minivoicer(ui, l, hue),
        ModelEditor::Multivoicer => multivoicer(ui, l, hue),
        ModelEditor::Monosynth => monosynth(ui, l),
        ModelEditor::Parallax => parallax(ui, l),
        _ => {}
    }
}

// --- Minivoicer ------------------------------------------------------------------------

const MINI_MODE: u16 = 2;
const MINI_MIDI: u16 = 15;

/// Pages in HAL slot order: slots 0..4 the top row, 5..9 the bottom row (`0x6220a8`,
/// `0x60b5ac`: slot = HAL + 10 · page).
const MINI_PAGES: [[Option<u16>; 10]; 2] = [
    [
        Some(0),
        Some(5),
        Some(6),
        Some(7),
        Some(8),
        Some(4),
        Some(9),
        Some(10),
        Some(11),
        Some(12),
    ],
    [
        Some(1),
        Some(2),
        Some(3),
        Some(13),
        Some(14),
        Some(15),
        Some(22),
        Some(16),
        Some(17),
        NONE,
    ],
];

/// MODE Chromatic (the last of five) swaps V1 / V2 INTERVAL for SEMIT (`0x623220`); MIDI On
/// puts the read-only V1 / V2 NOTE text in those places ([`mini_note`]).
fn mini_swap(ui: &UiState, index: u16) -> Option<u16> {
    let chromatic = position(ui, MINI_MODE) > 0.875;
    match index {
        7 | 11 if on(ui, MINI_MIDI) => None,
        7 if chromatic => Some(20),
        11 if chromatic => Some(21),
        _ => Some(index),
    }
}

/// ROOT needs a non-Chromatic MODE and MIDI Off; MIDI On dims ROOT, MODE, QUANTIZE and both
/// ACTIVE; MIDI CH needs MIDI On (`0x623220`).
fn mini_live(ui: &UiState, index: u16) -> bool {
    let midi = on(ui, MINI_MIDI);
    match index {
        1 => !midi && position(ui, MINI_MODE) <= 0.875,
        2 | 4 | 5 | 9 => !midi,
        22 => midi,
        _ => true,
    }
}

fn mini_page(ui: &UiState) -> usize {
    ui.param_page.min(1)
}

/// Body 800 × 224 (`0x60c43c`): two `#2e2e2e` radius 8 cards at (8, 0) and (8, 109), knobs
/// at y 7 and 116.
fn minivoicer(ui: &UiState, l: &mut Layout, hue: Rgb) {
    let p = l.palette;
    card(l, 8, 0, 107, 8.0);
    card(l, 8, 109, 107, 8.0);
    let page = MINI_PAGES[mini_page(ui)].map(|index| index.and_then(|i| mini_swap(ui, i)));
    let live = |index| mini_live(ui, index);
    knob_row(ui, l, 7, &page[..5], hue, &live);
    knob_row(ui, l, 116, &page[5..], hue, &live);
    // `0x61d080`, `0x61dfbc`: V1 / V2 NOTE on page 0 with MIDI On, in the INTERVAL places.
    if mini_page(ui) == 0 && on(ui, MINI_MIDI) {
        for ((y, label), note) in [(8, "V1 NOTE"), (117, "V2 NOTE")]
            .into_iter()
            .zip(ui.snapshot.voice_notes)
        {
            let text = |dy: i32, text: String| (Rect::new(489, BODY_TOP + y + dy, 80, 21), text);
            for (bounds, text) in [text(0, label.into()), text(69, mini_note(note))] {
                l.text(bounds, text, BODY, p.text, Align::Left);
            }
        }
    }
}

/// A voice note as `0x61d994` writes it: the pitch class and `note / 12`; a silent voice
/// is "-". The label reads the driver's voices, not the stored NOTE DISPLAY 23 / 24.
fn mini_note(note: Option<u8>) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    note.map_or_else(
        || "-".into(),
        |note| format!("{}{}", NAMES[usize::from(note % 12)], note / 12),
    )
}

// --- Multivoicer -----------------------------------------------------------------------

const MULTI_SCALE: u16 = 2;
const MULTI_VIEW: u16 = 48;
const MULTI_MIDI_CH: u16 = 50;
const MULTI_TABS: [&str; 5] = ["SETTINGS", "VOICE 1", "VOICE 2", "VOICE 3", "VOICE 4"];
/// Graph (`0x623850`): 760 px across, 196 px up from y 13 + 196; the voice stems are
/// `#ffd236`, the DI stem `#ff7000`; points `#ababab` on, `#616161` off (`0x3b61d8`).
const GRAPH_W: f32 = 760.0;
const GRAPH_H: f32 = 196.0;
const STEM_VOICE: Rgb = block::PITCH;
const STEM_DI: Rgb = block::DRIVE;
const POINT_ON: Rgb = Rgb(0xab, 0xab, 0xab);

fn level_pan(ui: &UiState) -> bool {
    on(ui, MULTI_VIEW)
}

fn multi_midi(ui: &UiState) -> bool {
    position(ui, MULTI_MIDI_CH) > 0.0
}

/// Voice `v` (0..4): ACTIVE, INTERVAL, LEVEL, DETUNE, DELAY, PAN at 5 + 6v; SEMITONES 40 + v.
fn voice_knobs(ui: &UiState, v: u16) -> [Option<u16>; 4] {
    let base = 5 + 6 * v;
    let pitch = if position(ui, MULTI_SCALE) < 0.125 {
        40 + v
    } else {
        base + 2
    };
    if level_pan(ui) {
        [Some(base), Some(pitch), Some(base + 1), Some(base + 5)]
    } else {
        [Some(base), Some(pitch), Some(base + 3), Some(base + 4)]
    }
}

/// The card of a Normal tab (`0x627e80`, `0x628160`): SETTINGS VOICE VIEW, LEVEL / DETUNE,
/// PAN / DELAY and, in Level/Pan, DI LEVEL.
fn multi_tab_knobs(ui: &UiState, tab: usize) -> [Option<u16>; 4] {
    match (tab, level_pan(ui)) {
        (0, true) => [Some(48), Some(44), Some(45), Some(0)],
        (0, false) => [Some(48), Some(46), Some(47), NONE],
        (tab, _) => voice_knobs(ui, tab as u16 - 1),
    }
}

/// Global banks (`0x11e76d8`): bank 0 upper and lower rows, bank 1 MIDI CH.
const MULTI_GLOBAL: [[Option<u16>; 10]; 2] = [
    [
        Some(1),
        Some(2),
        Some(4),
        Some(3),
        Some(49),
        Some(51),
        Some(34),
        Some(33),
        Some(31),
        Some(35),
    ],
    [
        Some(50),
        NONE,
        NONE,
        NONE,
        NONE,
        NONE,
        NONE,
        NONE,
        NONE,
        NONE,
    ],
];

/// ACTIVE off dims its voice; KEY and VOICING dim in Chromatic or with a MIDI channel; a
/// MIDI channel also dims SCALE and every ACTIVE; UNISON 0 dims SOURCE (`0x625dc0`,
/// `0x626d2c`).
fn multi_live(ui: &UiState, index: u16) -> bool {
    let midi = multi_midi(ui);
    match index {
        5 | 11 | 17 | 23 => !midi,
        6..=28 => on(ui, 5 + 6 * ((index - 5) / 6)),
        40..=43 => on(ui, 5 + 6 * (index - 40)),
        1 | 49 => !midi && position(ui, MULTI_SCALE) >= 0.125,
        2 => !midi,
        51 => position(ui, 33) > 0.0,
        _ => true,
    }
}

fn multi_tab(ui: &UiState) -> usize {
    usize::from(ui.editor_tab).min(MULTI_TABS.len() - 1)
}

/// Graph coordinates `(x, y)` (screen) of voice `v` or the DI (4), and its stem colour.
fn multi_place(ui: &UiState, v: u16) -> (f32, f32) {
    let unit = |u: f32| u.clamp(0.0, 1.0);
    let (x, y) = if v == 4 {
        (0.5, position(ui, 0))
    } else if level_pan(ui) {
        let base = 5 + 6 * v;
        let pan = (position(ui, base + 5) - 0.5) * position(ui, 45) + 0.5;
        (pan, position(ui, base + 1) * position(ui, 44))
    } else {
        let base = 5 + 6 * v;
        let detune = (position(ui, base + 3) - 0.5) * position(ui, 46) + 0.5;
        (position(ui, base + 4) * position(ui, 47), detune)
    };
    let sx = 20.0 + GRAPH_W * unit(x);
    let sy = BODY_TOP as f32 + 33.0 + GRAPH_H * (1.0 - unit(y));
    (sx, sy)
}

/// Normal (page 1): the graph (800 × 257), the voice tab bar 761 × 56 at (16, 249) and the
/// tab card at (8, 305). Global (page 2, I: stock switches with a host button): bank tabs
/// 136 × 52 at (647, 145), upper and lower cards at y 197 and 305.
fn multivoicer(ui: &UiState, l: &mut Layout, hue: Rgb) {
    let p = l.palette;
    if ui.param_page == 1 {
        let bank = usize::from(ui.editor_bank.min(1));
        for (index, label) in ["1", "2"].into_iter().enumerate() {
            let bounds = Rect::new(647 + index as i32 * 70, BODY_TOP + 145, 66, 52);
            let control = EditorControl::Bank(index as u8);
            button(l, bounds, label, control, index == bank, p.panel);
        }
        card(l, 8, 197, 107, 6.0);
        card(l, 8, 305, 107, 6.0);
        let knobs = MULTI_GLOBAL[bank];
        let live = |index| multi_live(ui, index);
        knob_row(ui, l, 204, &knobs[..5], hue, &live);
        knob_row(ui, l, 312, &knobs[5..], hue, &live);
        return;
    }
    let lines: &[i32] = if level_pan(ui) {
        &[53, 106, 159, 212]
    } else {
        &[64, 129, 194]
    };
    for &y in lines {
        l.push(
            Rect::new(8, BODY_TOP + y, 784, 1),
            "",
            Look::Fill(p.surface),
        );
    }
    let tab = multi_tab(ui);
    let selected = tab.checked_sub(1);
    let voices = if level_pan(ui) { 0..5 } else { 0..4 };
    for v in voices {
        let (x, y) = multi_place(ui, v);
        let live = v == 4 || on(ui, 5 + 6 * v);
        let baseline = if level_pan(ui) {
            (BODY_TOP + 242) as f32
        } else {
            (BODY_TOP + 129 + 20) as f32
        };
        let stem = if v == 4 { STEM_DI } else { STEM_VOICE };
        let (top, bottom) = (y.min(baseline), y.max(baseline));
        let bar = Rect::new(x as i32 - 8, top as i32, 16, (bottom - top).max(1.0) as u32);
        l.push(bar, "", Look::Fill(if live { stem } else { p.faint }));
        let label = if v == 4 {
            "DI".to_owned()
        } else {
            (v + 1).to_string()
        };
        let dot = Rect::new(x as i32 - 20, y as i32 - 20, 40, 40);
        let fill = if live { p.tone(POINT_ON) } else { p.faint };
        l.push(dot, "", Look::Dot { color: fill });
        l.text(dot, label.clone(), BODY, p.canvas, Align::Center);
        if Some(usize::from(v)) == selected {
            point(l, (x as i32, y as i32), &label, true, live);
        }
        let control = Control::Editor(EditorControl::Point(v as u8));
        l.control(dot, control, true, format!("VOICE {label}"), Look::Area);
    }
    tab_bar(l, Rect::new(16, BODY_TOP + 249, 761, 56), &MULTI_TABS, tab);
    card(l, 8, 305, 107, 6.0);
    let live = |index| multi_live(ui, index);
    let knobs = multi_tab_knobs(ui, tab);
    knob_row(ui, l, 313, &knobs, hue, &live);
}

// --- Overlord Synth --------------------------------------------------------------------

const SYNTH_RED: Rgb = block::SYNTH;
const SYNTH_PURPLE: Rgb = block::CAB;
const CATEGORIES: [&str; 4] = ["GLOBAL & AMP", "ARPEGGIATOR", "OSCILLATOR", "FILTER"];
/// Knobs per category in HAL order (`0x11e7b10`, `0x11e7b38`, `0x11e7b60`, `0x11e7b88`);
/// slot s sits at (8 + 157 · (s % 5), 125 or 233) (`0x630140`).
const SYNTH_KNOBS: [[Option<u16>; 10]; 4] = [
    [
        Some(0),
        Some(1),
        Some(2),
        Some(34),
        Some(37),
        Some(36),
        Some(35),
        Some(32),
        Some(33),
        Some(51),
    ],
    [
        Some(38),
        Some(5),
        Some(6),
        Some(39),
        Some(40),
        Some(49),
        Some(50),
        Some(48),
        Some(3),
        NONE,
    ],
    [
        Some(12),
        Some(13),
        Some(19),
        Some(18),
        Some(17),
        Some(11),
        Some(22),
        Some(20),
        Some(21),
        Some(16),
    ],
    [
        Some(23),
        Some(24),
        Some(25),
        Some(27),
        Some(28),
        Some(29),
        Some(30),
        Some(31),
        NONE,
        NONE,
    ],
];
/// Knob colours: category 0 slots 7, 8 red; arpeggiator all red; oscillator slots 0, 1, 5
/// red and 3, 4, 9 purple; filter slots 0..4 red; others white (`0x155e800`…).
fn synth_hue(p: &Palette, category: usize, slot: usize) -> Rgb {
    match (category, slot) {
        (0, 7 | 8) | (1, _) | (2, 0 | 1 | 5) | (3, 0..=4) => SYNTH_RED,
        (2, 3 | 4 | 9) => SYNTH_PURPLE,
        _ => p.text,
    }
}

/// An oscillator off dims LEVEL, STEPS and DETUNE, a noise wave STEPS and DETUNE
/// (`0x62dbbc`); FILTER off dims its knobs; a MIDI channel dims SENSITIVITY, GLIDE, GATE and
/// S/C SOURCE (`0x62e3f8`); ARPEGGIATOR off dims its knobs but TUNING, SYNC picks FREE RATE
/// or SYNC NOTE (`0x62e2c4`).
fn synth_live(ui: &UiState, index: u16) -> bool {
    let noise = |wave: u16| position(ui, wave) > 4.5 / 6.0;
    let arp = on(ui, 38);
    match index {
        11 => on(ui, 9),
        12 | 13 => on(ui, 9) && !noise(10),
        16 => on(ui, 14),
        17 | 18 => on(ui, 14) && !noise(15),
        24 | 25 | 27..=31 => on(ui, 23),
        1 | 34 | 36 | 37 => position(ui, 51) <= 0.0,
        5 | 6 | 39 | 40 | 49 => arp,
        48 => arp && !on(ui, 49),
        50 => arp && on(ui, 49),
        _ => true,
    }
}

fn synth_category(ui: &UiState) -> usize {
    usize::from(ui.editor_tab).min(CATEGORIES.len() - 1)
}

/// The envelope graph (`0x4fd588`) at (10, 2), 782 × 108; its curve child sits 6 px lower.
fn envelope_graph() -> Rect {
    Rect::new(10, BODY_TOP + 2, 782, 108)
}

/// The filter graph (`MoogLadderFilterGraph` `0x4fb528`) at (8, 2), 784 × 110.
fn filter_graph() -> Rect {
    Rect::new(8, BODY_TOP + 2, 784, 110)
}

/// Curve height at `x` of a curve `width` × `height` for ENV. DCY/ATCK `u` (`0x4fdaa4`): an
/// attack rise for u ≥ 0.5, a decay fall below. Stock divides by zero at 0 and 0.5; Tetrad
/// keeps the divisor at least 1 px.
fn envelope_y(u: f32, x: f32, width: f32, height: f32) -> f32 {
    if u >= 0.5 {
        height * (-x / (width * (u - 0.5)).max(1.0)).exp()
    } else {
        height * (1.0 - (-x / (width * u).max(1.0)).exp())
    }
}

/// Envelope graph: the red stroke-4 curve, the ATTACK / DECAY caption (40 px, `#2e2e2e`) beside
/// the dragger, and the 96 × 114 dragger at x = −32 + u (W − 32), y 1 (`0x4fde3c`, `0x4fd240`).
fn envelope(ui: &UiState, l: &mut Layout) {
    let Some(p) = parameter(ui, 32) else {
        return;
    };
    let palette = l.palette;
    let graph = envelope_graph();
    let u = position(ui, 32);
    let (width, height) = (graph.width as f32, graph.height as f32 - 6.0);
    let at = |x: f32| {
        let y = envelope_y(u, x, width, height);
        (graph.x as f32 + x, graph.y as f32 + 6.0 + y)
    };
    let mut previous = at(0.0);
    for step in 1..=100 {
        let next = at(width * step as f32 / 100.0);
        l.line(previous, next, 4.0, SYNTH_RED);
        previous = next;
    }
    let x = (-32.0 + u * (width - 32.0)) as i32;
    let (caption, caption_x) = if u >= 0.5 {
        ("ATTACK", x - 144)
    } else {
        ("DECAY", x + 80)
    };
    l.text(
        Rect::new(graph.x + caption_x, graph.y + 27, 160, 52),
        caption,
        Font::new(TextSize::Px40, Weight::Medium),
        palette.panel,
        Align::Left,
    );
    let dragger = Rect::new(graph.x + x, graph.y + 1, 96, 114);
    l.push(
        Rect::new(dragger.x + 28, dragger.y, 40, dragger.height),
        "",
        Look::Fill(palette.canvas),
    );
    l.panel(
        Rect::new(dragger.x + 32, dragger.y + 4, 32, dragger.height - 8),
        palette.surface,
        8.0,
        None,
    );
    // Owned grip in place of `env_dragger.png`.
    for dx in [44.0, 50.0] {
        let (gx, gy) = (dragger.x as f32 + dx, dragger.y as f32 + 47.0);
        l.line((gx, gy), (gx, gy + 20.0), 2.0, palette.muted);
    }
    let control = Control::Editor(EditorControl::Point(0));
    l.control(dragger, control, true, p.label.clone(), Look::Area);
}

fn filter_x(graph: Rect, hz: f32) -> f32 {
    graph.x as f32 + (graph.width as f32 - 9.0) * (hz / 20.0).log10() / 3.0
}

/// CUTOFF 25 in Hz: normalized `((f − 20) / 19980)^0.3` (`0x62ef40`, ModelRepo skew 0.3).
fn cutoff_hz(u: f32) -> f32 {
    20.0 + 19_980.0 * u.clamp(0.0, 1.0).powf(1.0 / 0.3)
}

/// I: an owned ladder low-pass, `(1 + k) / ((1 + jf/fc)^n + k)` with n poles for SLOPE
/// 6 / 12 / 18 / 24 dB per octave and k from RESONANCE (stock `max(1e−5, 4u)`, scaled by
/// n / 4 and kept under the n = 4 limit). Stock evaluates two biquads from pole tables
/// (`0x4fbf80`, `0x4faf08`, `0x4fb200`) that are not recovered.
fn ladder_db(hz: f32, cutoff: f32, poles: u8, resonance: f32) -> f32 {
    let k = 0.95 * resonance.clamp(0.0, 1.0) * f32::from(poles);
    let omega = hz / cutoff;
    let (mut re, mut im) = (1.0f32, 0.0f32);
    for _ in 0..poles {
        (re, im) = (re - im * omega, im + re * omega);
    }
    20.0 * ((1.0 + k) / (re + k).hypot(im)).log10()
}

/// Filter graph (`0x4fb528`, `0x4fc378`): grid lines (100 / 1k / 10k 2 px `#252525` with
/// 12 px `#404040` labels, others 1 px `#1e1e1e`), the 0 dB line, the red stroke-4 response
/// over −24..24 dB and the white CUTOFF point at 0 dB.
fn filter(ui: &UiState, l: &mut Layout) {
    let Some(p) = parameter(ui, 25) else {
        return;
    };
    let palette = l.palette;
    let graph = filter_graph();
    let (top, bottom) = (graph.y as f32, graph.bottom() as f32);
    for decade in [10.0f32, 100.0, 1_000.0, 10_000.0] {
        for step in 1..10 {
            let hz = decade * step as f32;
            if !(20.0..=20_000.0).contains(&hz) {
                continue;
            }
            let x = filter_x(graph, hz);
            let major = step == 1;
            let (width, color) = if major {
                (2.0, palette.row_selected)
            } else {
                (1.0, palette.surface)
            };
            l.line((x, top), (x, bottom), width, color);
            if major {
                let label = match decade as u32 {
                    100 => "100",
                    1_000 => "1k",
                    _ => "10k",
                };
                let bounds = Rect::new(x as i32 + 8, graph.y, 32, 16);
                l.text(bounds, label, CAPTION, palette.section, Align::Left);
            }
        }
    }
    let middle = top + graph.height as f32 / 2.0;
    l.line(
        (graph.x as f32, middle),
        (graph.right() as f32, middle),
        1.0,
        palette.surface,
    );
    let cutoff = cutoff_hz(position(ui, 25));
    let poles = (position(ui, 24) * 3.0).round() as u8 + 1;
    let resonance = position(ui, 27);
    let at = |x: f32| {
        let hz = 20.0 * 1000f32.powf(x / (graph.width as f32 - 9.0));
        let db = ladder_db(hz, cutoff, poles, resonance).clamp(-24.0, 24.0);
        (graph.x as f32 + x, middle - db * graph.height as f32 / 48.0)
    };
    let mut previous = at(0.0);
    for step in 1..=100 {
        let next = at((graph.width - 1) as f32 * step as f32 / 100.0);
        l.line(previous, next, 4.0, SYNTH_RED);
        previous = next;
    }
    let live = synth_live(ui, 25);
    let x = filter_x(graph, cutoff) as i32;
    let dot = Rect::new(x - 20, middle as i32 - 20, 40, 40);
    let color = if live { palette.text } else { palette.faint };
    l.push(dot, "", Look::Dot { color });
    let hit = Rect::new(x - 27, middle as i32 - 27, 54, 54);
    let control = Control::Editor(EditorControl::Point(1));
    l.control(hit, control, live, p.label.clone(), Look::Area);
}

/// Body (`0x632684`): the category content over two cards at (8, 118) and (8, 226), the
/// knobs, and the four category buttons 192 × 72 at y 340 (`0x62f80c`). GLOBAL shows the
/// envelope graph, FILTER the filter graph.
fn monosynth(ui: &UiState, l: &mut Layout) {
    let palette = l.palette;
    let category = synth_category(ui);
    card(l, 8, 118, 106, 6.0);
    card(l, 8, 226, 106, 6.0);
    for (slot, index) in SYNTH_KNOBS[category].iter().enumerate() {
        if let Some(p) = index.and_then(|index| parameter(ui, index)) {
            let y = if slot < 5 { 125 } else { 233 };
            let bounds = Rect::new(COLUMNS[slot % 5], BODY_TOP + y, KNOB_W, 95);
            knob(
                l,
                bounds,
                p,
                synth_hue(&palette, category, slot),
                synth_live(ui, p.index),
            );
        }
    }
    match category {
        1 => {
            // NOTES header and the eight note buttons (`0x630c4c`); ROOT stays on.
            card(l, 8, 0, 24, 8.0);
            let header = Rect::new(16, BODY_TOP, 200, 24);
            l.text(header, "NOTES", BODY, palette.text, Align::Left);
            for k in 0..8u16 {
                let bounds = Rect::new(8 + 100 * k as i32, BODY_TOP + 48, 84, 46);
                let (label, lit) = match k {
                    0 => ("ROOT".to_owned(), true),
                    k => ((k + 1).to_string(), on(ui, 40 + k)),
                };
                let control = EditorControl::Toggle(if k == 0 { u16::MAX } else { 40 + k });
                button(l, bounds, &label, control, lit, SYNTH_RED);
            }
        }
        2 => {
            // Oscillator grids (`0x6312e4`): item 0 the OSC button, 1..7 the waves.
            const WAVES: [&str; 8] = ["", "SIN", "TRI", "SAW", "SQR", "PUL", "WHT", "PNK"];
            l.push(
                Rect::new(400, BODY_TOP, 1, 110),
                "",
                Look::Fill(palette.panel),
            );
            for (osc, base_x, fill) in [(0u8, 8, SYNTH_RED), (1, 405, SYNTH_PURPLE)] {
                let (active, wave) = if osc == 0 { (9, 10) } else { (14, 15) };
                let current = (position(ui, wave) * 6.0).round() as usize + 1;
                for (item, name) in WAVES.into_iter().enumerate() {
                    let x = base_x + 99 * (item & 3) as i32;
                    let y = if item < 4 { 0 } else { 59 };
                    let bounds = Rect::new(x, BODY_TOP + y, 91, 51);
                    if item == 0 {
                        // `0x630f84`: "OSC n" at (11, 5), the wave readout at (11, 27).
                        let lit = on(ui, active);
                        l.panel(bounds, if lit { fill } else { palette.surface }, 8.0, None);
                        let label = format!("OSC {}", osc + 1);
                        let control = Control::Editor(EditorControl::Toggle(active));
                        l.control(bounds, control, true, label.clone(), Look::Area);
                        let top = Rect::new(x + 11, BODY_TOP + y + 5, 70, 21);
                        l.text(
                            top,
                            label,
                            BODY,
                            if lit { palette.text } else { palette.muted },
                            Align::Left,
                        );
                        let readout = Rect::new(x + 11, BODY_TOP + y + 27, 70, 18);
                        let text = if lit { WAVES[current] } else { "OFF" };
                        l.text(readout, text, CAPTION, palette.text, Align::Left);
                    } else {
                        let lit = on(ui, active) && item == current;
                        let control = EditorControl::Wave {
                            osc,
                            wave: item as u8,
                        };
                        button(l, bounds, name, control, lit, fill);
                    }
                }
            }
        }
        0 => envelope(ui, l),
        _ => filter(ui, l),
    }
    for (index, label) in CATEGORIES.iter().enumerate() {
        let bounds = Rect::new(8 + 198 * index as i32, BODY_TOP + 340, 192, 72);
        l.panel(
            bounds,
            if index == category {
                SYNTH_RED
            } else {
                palette.panel
            },
            8.0,
            None,
        );
        let control = Control::Editor(EditorControl::Tab(index as u8));
        l.control(bounds, control, true, *label, Look::Area);
        let text = Rect::new(bounds.x + 8, bounds.y + 8, 176, 21);
        l.text(text, *label, BODY, palette.text, Align::Left);
    }
}

// --- Parallax --------------------------------------------------------------------------

const PARALLAX_BLUE: Rgb = block::FILTER;
/// Page 0 encoders (`0x153f670`, E is the GAIN REDUCTION meter), page 1 (`0x153f6c0`),
/// page 2 (`0x153f6f0`), HAL order.
const PARALLAX_KNOBS: [[Option<u16>; 10]; 3] = [
    [
        Some(0),
        Some(1),
        Some(3),
        Some(5),
        Some(6),
        NONE,
        Some(2),
        Some(4),
        Some(7),
        NONE,
    ],
    [
        Some(19),
        Some(18),
        Some(16),
        Some(17),
        NONE,
        Some(27),
        Some(26),
        Some(24),
        Some(25),
        NONE,
    ],
    [
        Some(8),
        Some(9),
        Some(10),
        Some(11),
        Some(12),
        Some(13),
        NONE,
        NONE,
        NONE,
        NONE,
    ],
];
/// The graph (`0x4fe9c8`): 800 × 149, 800 × 348 in resize mode (`0x63625c`);
/// x = (W − 8) · log₁₀₀₀(f / 10) (`0x4fe610`).
fn parallax_graph_h(ui: &UiState) -> f32 {
    if ui.editor_expanded { 348.0 } else { 149.0 }
}

/// Resize mode encoders (`0x63507c`): B, C, D turn COMPRESSION, MID DRIVE, HIGH DRIVE.
const PARALLAX_EXPANDED: [u16; 3] = [1, 3, 5];

/// I: the band filter magnitudes, second order: L a low-pass at its frequency (stock type 2),
/// M a band-pass at 400 Hz with Q 0.707 (type 7), H a high-pass (type 1). Stock evaluates
/// the shared EQ filter types (`0x4fe148`), whose code is not traced.
fn parallax_power(band: u16, hz: f32, corner: f32) -> f32 {
    let r = hz / corner;
    match band {
        0 => 1.0 / (1.0 + r.powi(4)),
        1 => {
            let s = (r / std::f32::consts::FRAC_1_SQRT_2).powi(2);
            s / ((1.0 - r * r).powi(2) + s)
        }
        _ => 1.0 / (1.0 + r.powi(-4)),
    }
}

/// Response curves (`0x4fe148`, `0x3b4cf4`): band power P = |H|² · g with g = 10^(level/20),
/// y = 1 − 20 log₁₀(P) · H / 60 (−60..0 dB). On bands draw `#616161`, their sum white.
fn parallax_curves(ui: &UiState, l: &mut Layout, height: f32) {
    let p = l.palette;
    let bands: Vec<(u16, f32, f32)> = (0..3u16)
        .filter(|&band| on(ui, 34 + band))
        .map(|band| {
            let (hz, gain) = parallax_band(ui, band);
            (band, hz, 10f32.powf((-48.0 + 48.0 * gain) / 20.0))
        })
        .collect();
    let y = |power: f32| {
        let db = 20.0 * power.max(1e-6).log10();
        BODY_TOP as f32 + (1.0 - db * height / 60.0).clamp(0.0, height)
    };
    let x_at = |step: usize| (WIDTH - 1) as f32 * step as f32 / 100.0;
    let curve = |l: &mut Layout, power: &dyn Fn(f32) -> f32, color: Rgb| {
        let point = |step| {
            let x = x_at(step);
            (x, y(power(parallax_hz(x))))
        };
        let mut previous = point(0);
        for step in 1..=100 {
            let next = point(step);
            l.line(previous, next, 2.0, color);
            previous = next;
        }
    };
    for &(band, corner, gain) in &bands {
        curve(l, &|hz| parallax_power(band, hz, corner) * gain, p.faint);
    }
    if !bands.is_empty() {
        let sum = |hz: f32| {
            (bands.iter())
                .map(|&(band, corner, gain)| parallax_power(band, hz, corner) * gain)
                .sum()
        };
        curve(l, &sum, p.text);
    }
}

/// The resize button 66 × 44 at (723, 97), (723, 296) in resize mode, with owned arrows in
/// place of `resize_arrows_out.png` / `_in.png`.
fn parallax_resize(ui: &UiState, l: &mut Layout) {
    let p = l.palette;
    let expanded = ui.editor_expanded;
    let bounds = Rect::new(723, BODY_TOP + if expanded { 296 } else { 97 }, 66, 44);
    l.panel(bounds, p.surface, 8.0, None);
    let control = Control::Editor(EditorControl::Resize);
    l.control(bounds, control, true, "RESIZE", Look::Area);
    let (x, y) = (bounds.x as f32 + 21.0, bounds.y as f32 + 10.0);
    l.line((x + 4.0, y + 20.0), (x + 20.0, y + 4.0), 2.0, p.text);
    // Arrow heads at the ends (out) or at the middle (in).
    let heads = if expanded {
        [(x + 10.0, y + 14.0, 1.0), (x + 14.0, y + 10.0, -1.0)]
    } else {
        [(x + 4.0, y + 20.0, -1.0), (x + 20.0, y + 4.0, 1.0)]
    };
    for (hx, hy, sign) in heads {
        l.line((hx, hy), (hx - 7.0 * sign, hy), 2.0, p.text);
        l.line((hx, hy), (hx, hy + 7.0 * sign), 2.0, p.text);
    }
}

/// A resize mode control (stock presentation mode 2, not traced): owned label, value and
/// a level bar; a section off dims it (`0x63411c`) but it still turns.
fn compact_knob(ui: &UiState, l: &mut Layout, cell: Rect, p: &crate::ParameterView) {
    let palette = l.palette;
    let live = parallax_live(ui, p.index);
    let control = Control::Editor(EditorControl::Knob(p.index));
    l.control(cell, control, true, p.label.clone(), Look::Area);
    let line = Rect::new(cell.x + 10, cell.y + 4, cell.width - 20, 22);
    l.text(
        line,
        p.label.clone(),
        BODY,
        if live { palette.text } else { palette.faint },
        Align::Left,
    );
    l.text(line, p.value.clone(), BODY, palette.muted, Align::Right);
    let track = Rect::new(line.x, cell.y + 36, line.width, 8);
    l.panel(track, palette.surface, 4.0, None);
    let fill = (position(ui, p.index).clamp(0.0, 1.0) * track.width as f32).round() as u32;
    if fill > 0 {
        let bar = Rect::new(track.x, track.y, fill, track.height);
        l.panel(
            bar,
            if live { PARALLAX_BLUE } else { palette.faint },
            4.0,
            None,
        );
    }
}

/// GAIN REDUCTION (41, −40..0 dB) in the first column of the second row, `#87daff`
/// (`0x6373d0`). The device polls word 41 while the editor shows (`0x635fa0`) and
/// `0x636054` sets the meter only: u = 1 − |dB / 40|, 1 while LOW COMPRESSION is off.
/// I: 1 (no reduction) before the first reply; owned bar.
fn parallax_meter(ui: &UiState, l: &mut Layout) {
    let Some(p) = parameter(ui, 41) else {
        return;
    };
    let palette = l.palette;
    let live = on(ui, 34);
    let level = match ui.snapshot.editor_meters.get(&41) {
        Some(&level) if live => level,
        _ => 1.0,
    };
    let cell = Rect::new(COLUMNS[0], BODY_TOP + 313, KNOB_W, KNOB_H);
    let color = if live { palette.text } else { palette.faint };
    l.text(
        Rect::new(cell.x + 10, cell.y, 147, 23),
        p.label.clone(),
        BODY,
        color,
        Align::Left,
    );
    let track = Rect::new(cell.x + 12, cell.y + 44, 133, 12);
    l.panel(track, palette.surface, 4.0, None);
    let fill = (level.clamp(0.0, 1.0) * track.width as f32).round() as u32;
    if fill > 0 {
        let bar = Rect::new(track.x, track.y, fill, track.height);
        l.panel(
            bar,
            if live { PARALLAX_BLUE } else { palette.faint },
            4.0,
            None,
        );
    }
}

fn parallax_x(hz: f32) -> f32 {
    (WIDTH as f32 - 8.0) * (hz / 10.0).log10() / 3.0
}

fn parallax_hz(x: f32) -> f32 {
    10.0 * 1000f32.powf(x / (WIDTH as f32 - 8.0))
}

/// Band b (L, M, H): Hz and gain position (`0x635d8c`: L 70..400 Hz from 0, level 2; M at
/// 400 Hz, level 4; H 100..2000 Hz from 6, level 7).
fn parallax_band(ui: &UiState, band: u16) -> (f32, f32) {
    match band {
        0 => (70.0 + 330.0 * position(ui, 0), position(ui, 2)),
        1 => (400.0, position(ui, 4)),
        _ => (100.0 + 1900.0 * position(ui, 6), position(ui, 7)),
    }
}

/// LOW, MID, HIGH section switches (`0x153f6b0`).
const SECTIONS: [(u16, &str, i32, u32); 3] = [
    (34, "LOW", 8, 304),
    (35, "MID", 330, 139),
    (36, "HIGH", 487, 304),
];

/// A section off dims its knobs (`0x63411c`): LOW 0..2, MID 3, 4, HIGH 5..7.
fn parallax_live(ui: &UiState, index: u16) -> bool {
    match index {
        0..=2 => on(ui, 34),
        3 | 4 => on(ui, 35),
        5..=7 => on(ui, 36),
        _ => true,
    }
}

/// Pages (I: the header page tabs stand for the stock top bar Distortion, Cab, EQ):
/// Distortion the graph with its curves and L, M, H points, the section buttons, the resize
/// button and two knob rows at y 204 and 313 (resize mode: the tall graph and three compact
/// knobs); Cab the two mic panels' knobs, phase and power; EQ six faders 124 × 404.
fn parallax(ui: &UiState, l: &mut Layout) {
    let palette = l.palette;
    match ui.param_page.min(2) {
        0 => {
            let height = parallax_graph_h(ui);
            for decade in [100.0, 1_000.0, 10_000.0] {
                let x = parallax_x(decade);
                let line = Rect::new(x as i32, BODY_TOP, 1, height as u32);
                l.push(line, "", Look::Fill(palette.surface));
            }
            parallax_curves(ui, l, height);
            for (band, label) in ["L", "M", "H"].into_iter().enumerate() {
                let (hz, gain) = parallax_band(ui, band as u16);
                let at = (
                    parallax_x(hz) as i32,
                    BODY_TOP + ((1.0 - gain) * height) as i32,
                );
                let lit = on(ui, 34 + band as u16);
                point(l, at, label, false, lit);
                let hit = Rect::new(at.0 - 26, at.1 - 26, 52, 52);
                let control = Control::Editor(EditorControl::Point(band as u8));
                l.control(hit, control, true, format!("BAND {label}"), Look::Area);
            }
            parallax_resize(ui, l);
            if ui.editor_expanded {
                // Three compact controls 249 × 56 at y 360, dividers at (265, 369), (532, 369).
                for (k, index) in PARALLAX_EXPANDED.into_iter().enumerate() {
                    if let Some(p) = parameter(ui, index) {
                        let cell = Rect::new(8 + 267 * k as i32, BODY_TOP + 360, 249, 56);
                        compact_knob(ui, l, cell, p);
                    }
                }
                for x in [265, 532] {
                    let divider = Rect::new(x, BODY_TOP + 369, 2, 22);
                    l.push(divider, "", Look::Fill(palette.surface));
                }
                return;
            }
            for x in [320, 476] {
                let divider = Rect::new(x, BODY_TOP + 162, 2, 22);
                l.push(divider, "", Look::Fill(palette.surface));
            }
            for (index, label, x, width) in SECTIONS {
                let bounds = Rect::new(x, BODY_TOP + 153, width, 40);
                let control = EditorControl::Toggle(index);
                button(l, bounds, label, control, on(ui, index), palette.panel);
            }
            card(l, 8, 197, 106, 6.0);
            card(l, 8, 306, 106, 6.0);
            let live = |index| parallax_live(ui, index);
            let knobs = PARALLAX_KNOBS[0];
            knob_row(ui, l, 204, &knobs[..5], PARALLAX_BLUE, &live);
            knob_row(
                ui,
                l,
                313,
                &[NONE, knobs[6], knobs[7], knobs[8]],
                PARALLAX_BLUE,
                &live,
            );
            parallax_meter(ui, l);
        }
        1 => {
            for (x, knobs, phase, power) in [
                (8, [19, 18, 16, 17], 20, 14),
                (401, [27, 26, 24, 25], 28, 22),
            ] {
                l.panel(Rect::new(x, BODY_TOP, 390, 413), palette.panel, 8.0, None);
                for (k, index) in knobs.into_iter().enumerate() {
                    if let Some(p) = parameter(ui, index) {
                        let cell = Rect::new(x + 2 + 97 * k as i32, BODY_TOP + 13, 96, 68);
                        mini_knob(l, cell, p, palette.text);
                    }
                }
                let row = BODY_TOP + 361;
                button(
                    l,
                    Rect::new(x + 8, row, 66, 44),
                    "Ø",
                    EditorControl::Toggle(phase),
                    on(ui, phase),
                    palette.surface,
                );
                // Power (`0x820ac4`): the mic is on while its bypass is off.
                let bounds = Rect::new(x + 316, row, 66, 44);
                l.panel(bounds, palette.surface, 8.0, None);
                let control = Control::Editor(EditorControl::Toggle(power));
                l.control(bounds, control, true, "MIC ON", Look::Area);
                let color = if on(ui, power) {
                    palette.faint
                } else {
                    palette.text
                };
                let glyph = Look::Glyph {
                    icon: Icon::Power,
                    size: IconSize::Medium,
                    color,
                };
                l.push(bounds, "", glyph);
            }
        }
        _ => {
            for (k, index) in (8..=13u16).enumerate() {
                let Some(p) = parameter(ui, index) else {
                    continue;
                };
                let bounds = Rect::new(8 + 132 * k as i32, BODY_TOP + 8, 124, 404);
                l.panel(bounds, palette.panel, 8.0, None);
                let track = Rect::new(bounds.x + 52, bounds.y + 29, 20, 326);
                l.panel(track, palette.surface, 4.0, None);
                let fill = (position(ui, index) * 326.0) as u32;
                let bar = Rect::new(track.x, track.bottom() - fill as i32, 20, fill.max(1));
                l.panel(bar, palette.text, 4.0, None);
                l.control(
                    bounds,
                    Control::Editor(EditorControl::Knob(index)),
                    true,
                    p.label.clone(),
                    Look::Area,
                );
                l.text(
                    Rect::new(bounds.x, bounds.y + 360, 124, 20),
                    p.label.clone(),
                    CAPTION,
                    palette.text,
                    Align::Center,
                );
                l.text(
                    Rect::new(bounds.x, bounds.y + 380, 124, 20),
                    p.value.clone(),
                    CAPTION,
                    palette.muted,
                    Align::Center,
                );
            }
        }
    }
}

// --- Actions ----------------------------------------------------------------------------

/// Whether a knob of the editor takes input.
type Live = fn(&UiState, u16) -> bool;

impl UiState {
    /// Encoders. Minivoicer: slot = HAL + 10 · page (`0x60b5ac`). Multivoicer Normal: E–H
    /// turn the tab card (HAL − 5, `0x625638`); Global: the bank in HAL order, a MIDI channel
    /// blocks HAL 0, 1, 2, 4. Overlord Synth and Parallax: the page table in HAL order
    /// (`0x62cad8`, `0x63507c`). Dimmed knobs take no turn, except on Parallax (its handler
    /// does not check the dim flag).
    pub(super) fn synth_knob(&self, editor: ModelEditor, knob: KnobIndex) -> Option<u16> {
        let slot = hal(knob);
        let ui = self;
        let (index, live): (Option<u16>, Live) = match editor {
            ModelEditor::Minivoicer => (
                MINI_PAGES[mini_page(ui)][slot].and_then(|i| mini_swap(ui, i)),
                mini_live,
            ),
            ModelEditor::Multivoicer if ui.param_page == 1 => {
                let blocked = multi_midi(ui) && matches!(slot, 0 | 1 | 2 | 4);
                let index = MULTI_GLOBAL[usize::from(ui.editor_bank.min(1))][slot];
                (index.filter(|_| !blocked), multi_live)
            }
            ModelEditor::Multivoicer => (
                slot.checked_sub(5)
                    .and_then(|s| multi_tab_knobs(ui, multi_tab(ui)).get(s).copied().flatten()),
                multi_live,
            ),
            ModelEditor::Monosynth => (SYNTH_KNOBS[synth_category(ui)][slot], synth_live),
            ModelEditor::Parallax if ui.param_page == 0 && ui.editor_expanded => (
                slot.checked_sub(1)
                    .and_then(|s| PARALLAX_EXPANDED.get(s).copied()),
                |_, _| true,
            ),
            ModelEditor::Parallax => (PARALLAX_KNOBS[ui.param_page.min(2)][slot], |_, _| true),
            _ => return None,
        };
        index.filter(|&index| live(ui, index))
    }

    /// Taps: tabs and category buttons, the Multivoicer global banks, parameter buttons, the
    /// synth waves, and graph points (Multivoicer: select the voice's tab, the DI SETTINGS;
    /// a double tap flips ACTIVE without a MIDI channel, `0x625268`).
    pub(super) fn synth_control(
        &mut self,
        editor: ModelEditor,
        control: EditorControl,
    ) -> Vec<UiCommand> {
        let Some(position) = self.editing_block() else {
            return Vec::new();
        };
        let flip = |parameter| UiCommand::NudgeParameter {
            position,
            parameter,
            steps: 1,
        };
        match control {
            EditorControl::Tab(tab) => self.editor_tab = tab,
            EditorControl::Bank(bank) => self.editor_bank = bank,
            EditorControl::Resize => self.editor_expanded = !self.editor_expanded,
            EditorControl::Toggle(index) if parameter(self, index).is_some() => {
                return vec![flip(index)];
            }
            EditorControl::Wave { osc, wave } if (1..=7).contains(&wave) => {
                let (active, index) = if osc == 0 { (9, 10) } else { (14, 15) };
                let mut commands: Vec<UiCommand> = (!on(self, active))
                    .then(|| flip(active))
                    .into_iter()
                    .collect();
                commands.push(UiCommand::SetPosition {
                    position,
                    parameter: index,
                    value: (f32::from(wave - 1) / 6.0 * f32::from(POSITION_SCALE)).round() as u16,
                });
                return commands;
            }
            EditorControl::Point(v) if editor == ModelEditor::Multivoicer && v <= 4 => {
                let double = self.node_tap.is_some_and(|(last, at)| {
                    last == v && self.now.saturating_sub(at) <= DOUBLE_TAP_MS
                });
                self.editor_tab = if v == 4 { 0 } else { v + 1 };
                if double && v < 4 && !multi_midi(self) {
                    self.node_tap = None;
                    return vec![flip(5 + 6 * u16::from(v))];
                }
                self.node_tap = Some((v, self.now));
            }
            EditorControl::Point(band) if editor == ModelEditor::Parallax && band < 3 => {
                let double = self.node_tap.is_some_and(|(last, at)| {
                    last == band && self.now.saturating_sub(at) <= DOUBLE_TAP_MS
                });
                if double {
                    self.node_tap = None;
                    return vec![flip(34 + u16::from(band))];
                }
                self.node_tap = Some((band, self.now));
            }
            _ => {}
        }
        Vec::new()
    }

    /// Point drags. Multivoicer (`0x624e28`, Normal only): Level/Pan sets PAN (x) and LEVEL
    /// (y), Detune/Dly DELAY (x) and DETUNE (y), through the global scales; the DI sets DI
    /// LEVEL (y). I: the point follows the finger (stock adds relative steps of
    /// dx / max(760 s, 100) and dy / max(217 s, 100)). Parallax (`0x4f84e0`, `0x635d8c`): L
    /// sets 0 and 2, M 4, H 6 and 7 from the graph's frequency and gain scales.
    pub(super) fn synth_drag(
        &mut self,
        editor: ModelEditor,
        control: EditorControl,
        x: i32,
        y: i32,
    ) -> Vec<UiCommand> {
        let (Some(block), EditorControl::Point(v)) = (self.editing_block(), control) else {
            return Vec::new();
        };
        self.node_tap = None;
        let v = u16::from(v);
        let set = |parameter: u16, value: f32| UiCommand::SetPosition {
            position: block,
            parameter,
            value: (value.clamp(0.0, 1.0) * f32::from(POSITION_SCALE)).round() as u16,
        };
        let mut writes = Vec::new();
        match editor {
            ModelEditor::Multivoicer if self.param_page == 0 && v <= 4 => {
                let gx = (x as f32 - 20.0) / GRAPH_W;
                let gy = 1.0 - (y as f32 - BODY_TOP as f32 - 33.0) / GRAPH_H;
                let unscale = |u: f32, scale: u16, centred: bool| {
                    let s = position(self, scale).max(0.01);
                    if centred { (u - 0.5) / s + 0.5 } else { u / s }
                };
                let base = 5 + 6 * v;
                if v == 4 {
                    writes.push(set(0, gy));
                } else if level_pan(self) {
                    writes.push(set(base + 5, unscale(gx, 45, true)));
                    writes.push(set(base + 1, unscale(gy, 44, false)));
                } else {
                    writes.push(set(base + 4, unscale(gx, 47, false)));
                    writes.push(set(base + 3, unscale(gy, 46, true)));
                }
            }
            ModelEditor::Monosynth if v == 0 && synth_category(self) == 0 => {
                let graph = envelope_graph();
                writes.push(set(32, (x - graph.x) as f32 / graph.width as f32));
            }
            ModelEditor::Monosynth if v == 1 && synth_category(self) == 3 => {
                // `0x4fabcc`, `0x62ef40`: the sample's Hz to CUTOFF only, while it is live.
                let graph = filter_graph();
                let sample = ((x - graph.x) as f32).clamp(0.0, graph.width as f32 - 1.0);
                let hz = 20.0 * 1000f32.powf(sample / (graph.width as f32 - 9.0));
                if synth_live(self, 25) {
                    writes.push(set(25, ((hz - 20.0) / 19_980.0).clamp(0.0, 1.0).powf(0.3)));
                }
            }
            ModelEditor::Parallax if self.param_page == 0 && v < 3 => {
                let hz = parallax_hz((x as f32).clamp(0.0, WIDTH as f32 - 1.0));
                let gain = 1.0 - (y - BODY_TOP) as f32 / parallax_graph_h(self);
                match v {
                    0 => writes.extend([set(0, (hz - 70.0) / 330.0), set(2, gain)]),
                    1 => writes.push(set(4, gain)),
                    _ => writes.extend([set(6, (hz - 100.0) / 1900.0), set(7, gain)]),
                }
            }
            _ => {}
        }
        writes
            .into_iter()
            .filter(|command| {
                matches!(command, UiCommand::SetPosition { parameter, .. } if parameter_exists(self, *parameter))
            })
            .collect()
    }
}

fn parameter_exists(ui: &UiState, index: u16) -> bool {
    parameter(ui, index).is_some()
}
