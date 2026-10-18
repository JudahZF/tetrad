//! Tuner, Looper X and tempo/metronome. The Neural Capture wizard is in `capture.rs`.

use tetrad_assets::{
    ACCENT, ACCENT_FILL, BACKGROUND, CANVAS, DANGER, FAINT, Icon, MUTED, PANEL, Rgb, SECTION,
    SURFACE, TEXT, TUNER_IN, TextSize, WARNING, Weight,
};

use crate::layout::{
    BODY, BODY_BOLD, ButtonStyle, CAPTION, LARGE, LARGE_BOLD, Layout, Look, MEDIUM, MEDIUM_BOLD,
    TileMark,
};
use crate::render::{Align, Font, HEIGHT, WIDTH};
use crate::state::LooperSwap;
use crate::widgets::{CARD, CLOSE_BUTTON, PAGE, page_header, panel_cells};
use crate::*;

/// Tempo BPM (shot 008, `0x37f28c`).
const BPM: Font = Font::new(TextSize::Px96, Weight::Regular);
/// Tuner note (shot 037).
const NOTE: Font = Font::new(TextSize::Px70, Weight::Regular);
/// Tuner cents value and in-tune chevrons.
const CENTS: Font = Font::new(TextSize::Px40, Weight::Regular);
/// Tuner previous/next note names.
const SIDE_NOTE: Font = Font::new(TextSize::Px32, Weight::Regular);

/// One cell of the Tuner/Tempo settings panel. Cell n is encoder n, as in
/// [`crate::widgets::parameter_panel`].
#[derive(Clone, Copy)]
enum Cell {
    /// TEMPO knob: the BPM itself.
    Bpm,
    Knob(&'static str, SettingId),
    /// Vertical two-state switch with its two option names.
    Switch(&'static str, SettingId, [&'static str; 2]),
    /// Dropdown; an empty label centres the field (Tuner INPUT).
    Dropdown(&'static str, SettingId),
    /// Action button labelled `on`/`off`; `alarm` fills it red while on (MUTED).
    Toggle {
        id: SettingId,
        on: &'static str,
        off: &'static str,
        alarm: bool,
    },
    Empty,
}

/// Shot 037: FREQ knob, INPUT dropdown, MUTED button, LIVE TUNER switch.
const TUNER_PANEL: [Cell; 5] = [
    Cell::Knob("FREQ [Hz]", SettingId::TunerReference),
    Cell::Dropdown("", SettingId::TunerInput),
    Cell::Toggle {
        id: SettingId::TunerMute,
        on: "MUTED",
        off: "MUTE",
        alarm: true,
    },
    Cell::Switch("LIVE TUNER", SettingId::LiveTuner, ["Yes", "No"]),
    Cell::Empty,
];

/// Shot 008, two rows.
const TEMPO_PANEL: [Cell; KNOB_COUNT] = [
    Cell::Bpm,
    Cell::Switch("MODE", SettingId::TempoMode, ["Global", "Preset"]),
    Cell::Switch("TEMPO LED", SettingId::TempoLed, ["On", "Off"]),
    Cell::Knob("VOLUME", SettingId::MetronomeLevel),
    Cell::Toggle {
        id: SettingId::Metronome,
        on: "MUTE",
        off: "UNMUTE",
        alarm: false,
    },
    Cell::Knob("PAN", SettingId::MetronomePan),
    Cell::Dropdown("T/SIGNATURE", SettingId::TimeSignature),
    Cell::Dropdown("SUBDIVISIONS", SettingId::Subdivision),
    Cell::Dropdown("SOUND", SettingId::MetronomeSound),
    Cell::Dropdown("ROUTING", SettingId::MetronomeRouting),
];

/// What an encoder turns on the Tuner and Tempo pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanelKnob {
    Bpm,
    Setting(SettingId),
}

pub(crate) fn panel_knob(screen: Screen, knob: KnobIndex) -> Option<PanelKnob> {
    let cells: &[Cell] = match screen {
        Screen::Tuner => &TUNER_PANEL,
        Screen::Tempo => &TEMPO_PANEL,
        _ => return None,
    };
    match cells.get(usize::from(knob.get()))? {
        Cell::Bpm => Some(PanelKnob::Bpm),
        Cell::Knob(_, id) | Cell::Switch(_, id, _) | Cell::Dropdown(_, id) => {
            Some(PanelKnob::Setting(*id))
        }
        Cell::Toggle { id, .. } => Some(PanelKnob::Setting(*id)),
        Cell::Empty => None,
    }
}

/// Stock divider colours: Tuner (037) is green-black, Tempo (008) is black.
const TUNER_DIVIDER: Rgb = Rgb(0x18, 0x1c, 0x18);
const TEMPO_DIVIDER: Rgb = Rgb(0x10, 0x10, 0x10);

fn panel(
    ui: &UiState,
    l: &mut Layout,
    screen: Screen,
    bounds: Rect,
    gap: Rgb,
    divider: Rgb,
    cells: &[Cell],
) {
    let rows = cells.len().div_ceil(KNOB_COUNT / 2) as u32;
    for (cell, b) in cells
        .iter()
        .zip(panel_cells(l, bounds, rows, gap, divider, &[]))
    {
        match cell {
            // Stock has no cell there (037: four cells), so no divider before it.
            Cell::Empty => l.push(Rect::new(b.x - 1, b.y, 1, b.height), "", Look::Fill(CARD)),
            cell => panel_cell(ui, l, screen, b, *cell),
        }
    }
}

fn panel_cell(ui: &UiState, l: &mut Layout, screen: Screen, b: Rect, cell: Cell) {
    let s = &ui.snapshot;
    // Tempo (008) switches sit 8 px and its MUTE button 6 px lower than the Tuner's (037).
    let tempo = screen == Screen::Tempo;
    let knob = |value: String, kind: ParameterKind| Look::Knob {
        value,
        kind,
        color: ACCENT,
        focused: false,
    };
    match cell {
        Cell::Bpm => l.control(
            b,
            Control::Tempo(crate::TempoControl::Knob),
            true,
            "TEMPO",
            Look::Knob {
                value: format!("{:.0} BPM", s.tempo.bpm),
                kind: ParameterKind::Continuous {
                    position: (s.tempo.bpm - 40.0) / 200.0,
                },
                color: tempo_color(ui),
                focused: false,
            },
        ),
        Cell::Knob(label, id) => {
            let value = s.setting(id);
            // Stock FREQ shows one decimal; the unit is in the label (037).
            let text = match (id, value) {
                (SettingId::TunerReference, Some(SettingValue::Number(hz))) => format!("{hz}.0"),
                _ => id.format(value),
            };
            // Stock centres only these two cells (`0x3f09b4`, `0x382bbc` cell 5).
            let centred = matches!(id, SettingId::TunerReference | SettingId::MetronomePan);
            let position = match value {
                Some(SettingValue::Number(raw)) => id.knob_position(*raw),
                // No value: an empty arc.
                _ if centred => 0.5,
                _ => 0.0,
            };
            let kind = if centred {
                ParameterKind::Centred { position }
            } else {
                ParameterKind::Continuous { position }
            };
            l.control(
                b,
                Control::Setting(SettingControl::Open(id)),
                value.is_some(),
                label,
                knob(text, kind),
            );
        }
        Cell::Switch(label, id, options) => {
            let value = s.setting(id);
            let active = match value {
                Some(SettingValue::Bool(on)) => Some(usize::from(!on)),
                Some(SettingValue::Choice(index)) => Some(usize::from(*index)),
                _ => None,
            };
            let enabled = active.is_some();
            l.control(
                b,
                Control::Setting(SettingControl::Step(id, 1)),
                enabled,
                label,
                Look::Area,
            );
            cell_label(l, b, label, enabled);
            if id == SettingId::TempoMode {
                info_mark(l, b);
            }
            // Stock switch (shot 037): black slot, green ring at the active end.
            let top = b.y + b.height as i32 / 2 - 15 + if tempo { 8 } else { 0 };
            let track = Rect::new(b.x + 16, top, 22, 46);
            l.panel(track, CANVAS, 11.0, None);
            for (index, option) in options.into_iter().enumerate() {
                let y = track.y + index as i32 * 23;
                let on = active == Some(index);
                l.text(
                    Rect::new(track.right() + 16, y, b.width.saturating_sub(60), 23),
                    option,
                    BODY,
                    if on { TEXT } else { FAINT },
                    Align::Left,
                );
                if on {
                    l.push(
                        Rect::new(track.x + 1, y + 2, 19, 19),
                        "",
                        Look::Dot { color: ACCENT },
                    );
                    l.push(
                        Rect::new(track.x + 4, y + 5, 13, 13),
                        "",
                        Look::Dot { color: CANVAS },
                    );
                }
            }
        }
        // Tuner INPUT (037): a 137 × 37 black field 36 px below the cell top, an
        // upper-case 18 px value and a white ▼ (the shared dropdown is `#121212`).
        Cell::Dropdown("", id) => {
            let value = s.setting(id);
            let known = value.is_some();
            let field = Rect::new(b.x + 10, b.y + 36, 137, 37);
            l.control(
                field,
                Control::Setting(SettingControl::Open(id)),
                known,
                id.label(),
                Look::Area,
            );
            l.panel(field, CANVAS, 4.0, None);
            let ink = if known { TEXT } else { FAINT };
            let arrow = Rect::new(field.right() - 22, field.y + 15, 9, 7);
            super::settings::triangle(l, arrow, true, ink);
            l.text(
                Rect::new(
                    field.x + 8,
                    field.y,
                    (arrow.x - field.x - 14) as u32,
                    field.height,
                ),
                id.format(value).to_uppercase(),
                MEDIUM,
                ink,
                Align::Left,
            );
        }
        // Tempo panel: label on top, a field with a 14 px value at the bottom (008).
        Cell::Dropdown(label, id) => {
            let value = s.setting(id);
            let enabled = value.is_some();
            cell_label(l, b, label, enabled);
            let field = Rect::new(b.x + 10, b.bottom() - 48, b.width - 20, 36);
            l.control(
                field,
                Control::Setting(SettingControl::Open(id)),
                enabled,
                label,
                Look::Dropdown {
                    value: String::new(),
                },
            );
            l.text(
                Rect::new(field.x + 8, field.y, field.width - 38, field.height),
                id.format(value),
                CAPTION,
                if enabled { TEXT } else { FAINT },
                Align::Left,
            );
        }
        Cell::Toggle { id, on, off, alarm } => {
            let value = s.setting(id);
            let lit = value == Some(&SettingValue::Bool(true));
            let top = b.y + (b.height as i32 - 38) / 2 + if tempo { 6 } else { 0 };
            let button = Rect::new(b.x + 10, top, b.width - 20, 38);
            l.panel(
                button,
                if lit && alarm { DANGER } else { CANVAS },
                6.0,
                None,
            );
            l.button(
                button,
                Control::Setting(SettingControl::Step(id, 1)),
                value.is_some(),
                if lit { on } else { off },
                ButtonStyle::Ghost,
            );
        }
        Cell::Empty => {}
    }
}

/// Stock ⓘ (008 MODE): a grey disc with a dark "i", 8 px inside the cell's top right.
/// Drawn only; its help text is not recovered.
fn info_mark(l: &mut Layout, b: Rect) {
    let disc = Rect::new(b.right() - 30, b.y + 8, 22, 22);
    l.push(disc, "", Look::Dot { color: MUTED });
    l.text(disc, "i", BODY_BOLD, PANEL, Align::Center);
}

fn cell_label(l: &mut Layout, b: Rect, label: &str, enabled: bool) {
    l.text(
        Rect::new(b.x + 10, b.y + 6, b.width.saturating_sub(20), 28),
        label,
        BODY,
        if enabled { TEXT } else { FAINT },
        Align::Left,
    );
}

/// `D#/Eb`: the stock sharp/flat pair, or the natural name.
fn note_name(pitch_class: u8) -> String {
    let note = Note {
        pitch_class: pitch_class % 12,
        octave: 0,
    };
    match note.flat_name() {
        Some(flat) => format!("{}/{flat}", note.name()),
        None => note.name().to_owned(),
    }
}

/// Tuner (shot 037): cents row with ±50, a white line with the ball, the note
/// between its neighbours, then the one-row settings panel.
pub(super) fn tuner(ui: &UiState, l: &mut Layout) {
    const LINE_Y: i32 = 189;
    const BALL: i32 = 41;
    let t = &ui.snapshot.tuner;
    l.push(Rect::new(0, 0, WIDTH, HEIGHT), "", Look::Fill(PAGE));
    page_header(l, "Tuner", Control::Back, Icon::Check);
    l.text(Rect::new(25, 76, 100, 30), "-50", LARGE, MUTED, Align::Left);
    l.text(
        Rect::new(672, 76, 100, 30),
        "+50",
        LARGE,
        MUTED,
        Align::Right,
    );
    l.line((26.0, LINE_Y as f32), (774.0, LINE_Y as f32), 2.0, TEXT);
    let Some(note) = t.note else {
        l.text(
            Rect::new(250, 255, 300, 90),
            "-",
            NOTE,
            FAINT,
            Align::Center,
        );
        panel(
            ui,
            l,
            Screen::Tuner,
            Rect::new(8, 366, 784, 107),
            PAGE,
            TUNER_DIVIDER,
            &TUNER_PANEL,
        );
        return;
    };
    // Stock update `0x003eda88`: in tune is |cents| < 2 (`+0x194`), the value has a "+"
    // when sharp (`0xa34460`), and the ball is red, or Qt::green in tune.
    let cents = t.cents.clamp(-50.0, 50.0);
    let in_tune = t.cents.abs() < 2.0;
    // `+ 0.0` turns −0.0 into 0.0.
    let shown = (cents * 10.0).round() / 10.0 + 0.0;
    let sign = if shown > 0.0 { "+" } else { "" };
    l.text(
        Rect::new(250, 66, 300, 50),
        format!("{sign}{shown:.1}"),
        CENTS,
        TEXT,
        Align::Center,
    );
    // Both chevrons always show. Out of tune, the one on the ball's side changes pixmap
    // (`+0x150`/`+0x168`); the other keeps its in-tune pixmap. I: the changed one is red.
    let (left, right) = match () {
        _ if in_tune => (ACCENT, ACCENT),
        _ if shown > 0.0 => (ACCENT, DANGER),
        _ => (DANGER, ACCENT),
    };
    for (x, chevron, color) in [(195, ">", left), (573, "<", right)] {
        l.text(
            Rect::new(x, 62, 30, 50),
            chevron,
            CENTS,
            color,
            Align::Center,
        );
    }
    // Stock ball centre: (cents + 50) · 7.48 + 26, so the travel is the whole line.
    let x = ((cents + 50.0) * 7.48 + 26.0).round() as i32;
    for (radius, color) in [
        (BALL, TEXT),
        (BALL - 2, CANVAS),
        (30, if in_tune { TUNER_IN } else { DANGER }),
    ] {
        l.push(
            Rect::new(
                x - radius,
                LINE_Y - radius,
                2 * radius as u32,
                2 * radius as u32,
            ),
            "",
            Look::Dot { color },
        );
    }
    l.text(
        Rect::new(25, 270, 250, 50),
        note_name(note.pitch_class % 12 + 11),
        SIDE_NOTE,
        MUTED,
        Align::Left,
    );
    l.text(
        Rect::new(250, 255, 300, 90),
        note.name(),
        NOTE,
        TEXT,
        Align::Center,
    );
    l.text(
        Rect::new(525, 270, 250, 50),
        note_name(note.pitch_class % 12 + 1),
        SIDE_NOTE,
        MUTED,
        Align::Right,
    );
    panel(
        ui,
        l,
        Screen::Tuner,
        Rect::new(8, 366, 784, 107),
        PAGE,
        TUNER_DIVIDER,
        &TUNER_PANEL,
    );
}

/// Tempo & Metronome (shot 008): large green BPM (a tap there taps the tempo,
/// I), the red "BPM = …" caption, beat dots and the 2 × 5 settings panel.
/// MIDI Clock Out is not OFF. Stock checks the clock-out mode (`0x32db8c`, I: the field its
/// setter `0x32dbbc` writes) before it paints the Tempo page.
fn midi_clock_out(ui: &UiState) -> bool {
    matches!(ui.snapshot.setting(SettingId::MidiClockOut), Some(SettingValue::Choice(mode)) if *mode != 0)
}

/// The BPM number and TEMPO knob colour: stock red `0xff2727` with MIDI Clock Out on
/// (`0x380924`..`0x3809cc`, seen on the QC 2026-10-15), else the accent green.
fn tempo_color(ui: &UiState) -> Rgb {
    if midi_clock_out(ui) { DANGER } else { ACCENT }
}

pub(super) fn tempo(ui: &UiState, l: &mut Layout) {
    let tempo = &ui.snapshot.tempo;
    // Stock Tempo (008) is black; Tuner (037) and Capture use `PAGE` (#101010).
    l.push(Rect::new(0, 0, WIDTH, HEIGHT), "", Look::Fill(CANVAS));
    page_header(l, "Tempo", Control::Back, Icon::Check);
    // Scene stepper left of ✓, as on stock (seen on the QC 2026-10-15).
    let scenes = Rect::new(544, 8, 132, 44);
    l.panel(scenes, PANEL, 8.0, None);
    super::scene_stepper(ui, l, scenes, 28);
    l.push(Rect::new(684, 14, 1, 32), "", Look::Fill(SECTION));
    let bpm = Rect::new(240, 38, 320, 88);
    // The tap area starts below the header so it does not overlap the scene stepper.
    l.control(
        Rect::new(240, 56, 320, 70),
        Control::Tempo(TempoControl::Tap),
        true,
        "TAP",
        Look::Area,
    );
    l.text(
        bpm,
        format!("{:.0}", tempo.bpm),
        BPM,
        tempo_color(ui),
        Align::Center,
    );
    // Read-only "BPM = QUARTERS / EIGHTHS" (`0x38087c`): the beat unit of the time signature;
    // with MIDI Clock Out on, stock appends " | MIDI Clock Out" (`0x3809d0`).
    let note = SettingId::BpmNote;
    let mut caption = format!("BPM = {}", note.format(ui.snapshot.setting(note)));
    if midi_clock_out(ui) {
        caption.push_str(" | MIDI Clock Out");
    }
    l.text(
        Rect::new(200, 128, 400, 26),
        caption,
        BODY,
        DANGER,
        Align::Center,
    );
    let beats = i32::from(tempo.beats_per_bar.clamp(1, 16));
    let spacing = (640 / beats).min(128);
    let first = WIDTH as i32 / 2 - spacing * (beats - 1) / 2;
    for beat in 0..beats {
        let lit = tempo.beat.is_some_and(|current| i32::from(current) == beat);
        let x = first + beat * spacing;
        let color = if lit { TEXT } else { SECTION };
        l.push(Rect::new(x - 12, 201, 24, 24), "", Look::Dot { color });
        // Marks: 8 px dots 24 px above or below the beat centre (shot 008).
        let accent = tempo
            .accents
            .get(beat as usize)
            .copied()
            .unwrap_or_default();
        let mark = match accent {
            BeatAccent::Dot => None,
            BeatAccent::Black => Some((Rect::new(x - 10, 203, 20, 20), CANVAS)),
            BeatAccent::Above => Some((Rect::new(x - 4, 185, 8, 8), SECTION)),
            BeatAccent::Below => Some((Rect::new(x - 4, 233, 8, 8), SECTION)),
        };
        if let Some((bounds, color)) = mark {
            l.push(bounds, "", Look::Dot { color });
        }
    }
    let bounds = Rect::new(8, 257, 784, 216);
    panel(
        ui,
        l,
        Screen::Tempo,
        bounds,
        CANVAS,
        TEMPO_DIVIDER,
        &TEMPO_PANEL,
    );
}

/// Looper X editor (shot 086, manual §4 "Looper X"): header (⋮, name, Params,
/// scene, ✓), the hint card, then the eight action tiles assigned to A–H.
/// Stock tile origins (`_INIT_519` table `0x155e6cc`) are x 8 + 198·i.
pub(super) fn looper(ui: &UiState, l: &mut Layout) {
    let s = &ui.snapshot;
    let looper = &s.looper;
    let swap = ui.looper_swap;
    l.control(
        Rect::new(8, 8, 44, 44),
        Control::LooperMenu,
        swap.is_none(),
        "MENU",
        Look::IconButton {
            icon: Icon::More,
            fill: PANEL,
        },
    );
    l.text(
        Rect::new(60, 8, 280, 20),
        "LOOPER",
        BODY,
        MUTED,
        Align::Left,
    );
    l.text(
        Rect::new(60, 28, 280, 24),
        "Looper X",
        LARGE,
        ModelCategory::Looper.hue(),
        Align::Left,
    );
    if looper.mini {
        l.control(
            Rect::new(352, 8, 50, 44),
            Control::LooperLock,
            true,
            "LOCK",
            Look::IconButton {
                icon: Icon::Lock,
                fill: if looper.editor_view {
                    ACCENT_FILL
                } else {
                    PANEL
                },
            },
        );
    }
    let looper_block = s
        .blocks
        .iter()
        .any(|block| block.category == Some(ModelCategory::Looper));
    l.control(
        Rect::new(410, 8, 126, 44),
        Control::LooperParams,
        looper_block,
        "Params",
        Look::Button {
            style: ButtonStyle::Secondary,
            icon: Some(Icon::Knob),
            font: None,
        },
    );
    super::scene_stepper(ui, l, Rect::new(544, 8, 132, 44), 28);
    l.push(Rect::new(684, 14, 1, 32), "", Look::Fill(SECTION));
    l.control(
        CLOSE_BUTTON,
        if swap.is_some() {
            Control::LooperSwapDone
        } else {
            Control::Back
        },
        true,
        if swap.is_some() { "DONE" } else { "CLOSE" },
        Look::IconButton {
            icon: Icon::Check,
            fill: PANEL,
        },
    );
    l.panel(Rect::new(8, 60, 784, 208), PANEL, 8.0, None);
    l.panel(Rect::new(8, 168, 784, 100), SURFACE, 8.0, None);
    if swap.is_some() {
        // Swap dialog hint (`0xa30f6c`, `0x37c7ac`).
        l.text(
            Rect::new(16, 62, 760, 26),
            "Tap Looper X actions to reassign them to different footswitches.",
            BODY,
            TEXT,
            Align::Left,
        );
    } else {
        status_lines(l, looper);
    }
    let color = status_color(looper);
    if let Some(seconds) = looper.loop_seconds {
        l.text(
            Rect::new(560, 192, 216, 28),
            format!("LOOP {}", minutes(seconds)),
            MEDIUM_BOLD,
            color,
            Align::Right,
        );
    }
    // Stock 086: no track until a loop exists.
    if looper.loop_seconds.is_some() {
        l.push(
            Rect::new(24, 232, 752, 14),
            "",
            Look::Progress {
                fraction: looper.progress,
                color,
            },
        );
    }
    for (index, action) in looper.tiles.0.into_iter().enumerate() {
        let (title, mark, enabled, active) = match swap {
            // Every tile can be picked; the picked one is lit.
            Some(swap) => {
                let (title, mark, ..) = looper_tile(looper, action);
                (title, mark, true, swap == LooperSwap::Assign(index as u8))
            }
            None => looper_tile(looper, action),
        };
        let (column, row) = (index % 4, index / 4);
        let hue = match action {
            LooperAction::Duplicate | LooperAction::PunchIn | LooperAction::Record => DANGER,
            _ => TEXT,
        };
        l.control(
            Rect::new(8 + column as i32 * 198, 276 + row as i32 * 102, 190, 94),
            Control::Looper(action),
            enabled,
            title,
            Look::ActionTile {
                letter: char::from(b'A' + index as u8),
                title: title.into(),
                mark,
                hue,
                active,
            },
        );
    }
    if let Some(LooperSwap::Assign(_)) = swap {
        assign_prompt(l);
    }
}

/// "Assign footswitch" message (`0x37ab08`): `#121212` panel, the QC text (`0xa30e04`) and
/// CANCEL. The device takes the next footswitch letter.
fn assign_prompt(l: &mut Layout) {
    l.overlay();
    let panel = Rect::new(160, 140, 480, 200);
    l.panel(panel, BACKGROUND, 8.0, None);
    l.text(
        Rect::new(panel.x + 24, panel.y + 16, panel.width - 48, 28),
        "Assign footswitch",
        LARGE_BOLD,
        TEXT,
        Align::Left,
    );
    l.paragraph(
        Rect::new(panel.x + 24, panel.y + 60, panel.width - 48, 60),
        "Press the target footswitch you want to assign the action to",
        MEDIUM,
        MUTED,
    );
    l.button(
        Rect::new(panel.right() - 164, panel.bottom() - 64, 140, 44),
        Control::LooperAssignCancel,
        true,
        "CANCEL",
        ButtonStyle::Secondary,
    );
}

fn minutes(seconds: f32) -> String {
    let total = seconds.max(0.0) as u32;
    format!("{}:{:02}", total / 60, total % 60)
}

fn status_color(looper: &LooperView) -> tetrad_assets::Rgb {
    use LooperState::*;
    match looper.state {
        _ if looper.waiting.is_some() => WARNING,
        Recording => DANGER,
        Overdubbing | Duplicating | Punching => WARNING,
        Playing | PlayingToEnd => ACCENT,
        Empty | Armed | Stopped => MUTED,
    }
}

/// Card lines (`0x614e48`, `0x6153e4`, `0x616690`): the state or the record
/// prompt, "AVAILABLE m:ss", then the close gesture for this product.
fn status_lines(l: &mut Layout, looper: &LooperView) {
    const FIRST: i32 = 62;
    const SECOND: i32 = 101;
    let line = |l: &mut Layout, x: i32, y: i32, text: &str, color| {
        let width = BODY.width(text).ceil() as u32 + 4;
        l.text(Rect::new(x, y, width, 26), text, BODY, color, Align::Left);
        x + width as i32 + 6
    };
    // Inline record dot / UP icon between two words, as the stock hint lines.
    let inline = |l: &mut Layout, y: i32, before: &str, look: Look, after: &str| {
        let x = line(l, 16, y, before, TEXT);
        l.push(Rect::new(x, y + 3, 20, 20), "", look);
        line(l, x + 26, y, after, TEXT);
    };
    match (looper.undo_in_progress, looper.waiting, looper.state) {
        (true, ..) => {
            line(l, 16, FIRST, "UNDO/REDO IN PROGRESS", TEXT);
        }
        (false, Some(wait), _) => {
            line(l, 16, FIRST, wait.label(), WARNING);
        }
        (false, None, LooperState::Empty) => inline(
            l,
            FIRST,
            "USE",
            Look::Dot { color: DANGER },
            "TO START RECORDING",
        ),
        (false, None, state) => {
            line(l, 16, FIRST, state.label(), status_color(looper));
        }
    }
    if let Some(seconds) = looper.available_seconds {
        l.text(
            Rect::new(560, FIRST, 216, 26),
            format!("AVAILABLE {}", minutes(seconds)),
            BODY,
            TEXT,
            Align::Right,
        );
    }
    if looper.mini {
        line(l, 16, SECOND, "USE B+D TO CLOSE THE LOOPER VIEW", TEXT);
    } else {
        inline(
            l,
            SECOND,
            "USE",
            Look::Button {
                style: ButtonStyle::Ghost,
                icon: Some(Icon::Up),
                font: None,
            },
            "TO CLOSE THE LOOPER VIEW",
        );
    }
}

/// Tile title, mark, enabled and engaged. Titles are the stock tile strings
/// (`_INIT_519`). The device drops presses that the gates refuse (`w4-looper-gates`
/// §2.1: Play/Stop and Reverse need a loop, UNDO a loop or a redo), but stock draws
/// those tiles in normal ink (086, empty looper). During undo tiles 0–6 are inactive
/// and UNDO is pressed. Each mark selects an owned large glyph in `draw.rs`
/// (`Icon::Copy` ⊗1, `Icon::Refresh` loop, `Icon::Pedal` glove, `Icon::Back` ◀◀).
fn looper_tile(looper: &LooperView, action: LooperAction) -> (&'static str, TileMark, bool, bool) {
    use LooperState::*;
    use TileMark::{Icon as I, Text as T};
    let running = matches!(
        looper.state,
        Recording | Playing | PlayingToEnd | Overdubbing | Duplicating | Punching
    );
    let (title, mark, enabled, active) = match action {
        LooperAction::Duplicate if looper.state == Duplicating => {
            ("INCREASE", I(Icon::Copy), true, true)
        }
        LooperAction::Duplicate => ("DUPLICATE", I(Icon::Copy), true, false),
        LooperAction::OneShot if looper.one_shot => ("RE-LOOP", I(Icon::Refresh), true, true),
        LooperAction::OneShot => ("ONE SHOT", I(Icon::Refresh), true, false),
        LooperAction::HalfSpeed => ("HALF SPEED", T("1/2"), true, looper.half_speed),
        LooperAction::PunchIn if looper.state == Punching => {
            ("PUNCH OUT", I(Icon::Pedal), true, true)
        }
        LooperAction::PunchIn => ("PUNCH IN", I(Icon::Pedal), true, false),
        LooperAction::Record => match looper.state {
            Armed => ("ARMED", I(Icon::Record), true, true),
            Recording => ("RECORDING", I(Icon::Record), true, true),
            Overdubbing => ("OVERDUB", I(Icon::Record), true, true),
            Playing | PlayingToEnd | Duplicating | Punching => {
                ("OVERDUB", I(Icon::Record), true, false)
            }
            Empty | Stopped => ("RECORD", I(Icon::Record), true, false),
        },
        LooperAction::PlayStop if running => ("PLAYING", I(Icon::Stop), true, true),
        LooperAction::PlayStop => ("PLAY", I(Icon::Play), true, false),
        LooperAction::Reverse => ("REVERSE", I(Icon::Back), true, looper.reverse),
        LooperAction::UndoRedo => {
            let (title, icon) = if looper.redo_available {
                ("REDO", Icon::Redo)
            } else {
                ("UNDO", Icon::Undo)
            };
            return (title, I(icon), true, looper.undo_in_progress);
        }
    };
    (title, mark, enabled && !looper.undo_in_progress, active)
}
