//! Pixel-fidelity checks against the stock QC shots (gap census A10).

use crate::*;
use tetrad_assets::Palette;

fn ui_with(snapshot: UiSnapshot) -> UiState {
    let mut ui = UiState::new();
    ui.set_snapshot(snapshot).unwrap();
    ui
}

/// Pointer angle in degrees (3 o'clock is 0, clockwise) of the Tempo VOLUME knob at `raw`
/// tenths of dB.
fn tempo_volume_angle(raw: i32) -> f32 {
    let mut snapshot = UiSnapshot::default();
    snapshot
        .settings
        .insert(SettingId::MetronomeLevel, SettingValue::Number(raw));
    let mut ui = ui_with(snapshot);
    ui.navigate(Screen::Tempo);
    let position = ui
        .layout()
        .items
        .iter()
        .find_map(|item| match item.look {
            crate::layout::Look::Knob {
                kind: ParameterKind::Continuous { position },
                ..
            } if item.label == "VOLUME" => Some(position),
            _ => None,
        })
        .expect("Tempo VOLUME knob");
    135.0 + 270.0 * position
}

/// Stock Tempo VOLUME is −60…9 dB with skew 3.5 (`ModelRepo.xml`, model 25000). The pointer is
/// ((dB + 60) / 69)^3.5 of the sweep: 0.0 dB sits at 300.5°, and stock shot mi4 measures 299.8°.
#[test]
fn tempo_volume_pointer_follows_the_stock_skew() {
    assert!((tempo_volume_angle(-600) - 135.0).abs() < 0.01);
    assert!((tempo_volume_angle(0) - 300.5).abs() < 1.5);
    assert!((tempo_volume_angle(90) - 405.0).abs() < 0.01);
}

/// Position of the knob of `id` on `screen` at `raw`.
fn knob_at(screen: Screen, id: SettingId, raw: i32) -> f32 {
    let label = id.label();
    let mut snapshot = UiSnapshot::default();
    snapshot.settings.insert(id, SettingValue::Number(raw));
    let mut ui = ui_with(snapshot);
    ui.navigate(screen);
    ui.layout()
        .items
        .iter()
        .find_map(|item| match item.look {
            crate::layout::Look::Knob {
                kind: ParameterKind::Continuous { position } | ParameterKind::Centred { position },
                ..
            } if item.label == label => Some(position),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{label} knob"))
}

/// Stock knobs show the catalog proportion `((v − min) / (max − min))^skew`. A Global EQ FREQ
/// is ModelRepo 4004 20…20 000 Hz with skew 0.3: 1 kHz sits at 0.405 of the sweep (Tetrad drew
/// the graph's log axis, 0.566). Shot 009 measures the 50 Hz pointer at 177–180° (law 173°, log
/// 171°). GAIN, Q and the I/O levels are linear over their bounds.
#[test]
fn settings_knobs_follow_the_catalog_skew() {
    let band = EqBand::new(0).unwrap();
    let eq = Screen::GlobalEq(EqTab::Band(band));
    let freq = |hz| knob_at(eq, SettingId::EqFreq(band), hz);
    assert!((freq(1000) - 0.405).abs() < 0.002, "{}", freq(1000));
    assert!((freq(50) - 0.142).abs() < 0.002, "{}", freq(50));
    assert_eq!(freq(20_000), 1.0);
    assert_eq!(knob_at(eq, SettingId::EqGain(band), 0), 0.5);
    let input = SettingId::InputLevel(InputJack::In1);
    assert_eq!(knob_at(Screen::Io(IoTab::Input1), input, 240), 0.5);
}

/// The Input Gate meter over a row input port (`stock-home-20261015.png`, `0x5a43e8`): a 24 × 4
/// bar at port (10, 7). Orange `0xff7000` is the gain reduction, from the open px to the end;
/// at 2 px open a 1 px mark starts it and the black bar shows left of it (In 1, x 18–19). The
/// meter rounds as `0x5a163c`: the full bar from 23.5 px. A row without a meter has none.
#[test]
fn grid_input_port_draws_the_input_gate_meter() {
    assert_eq!(GateMeter::from_level(0.97).open(), 23);
    assert_eq!(GateMeter::from_level(0.98).open(), 24);
    let mut snapshot = UiSnapshot::default();
    snapshot.rows[0].input = InputPort::Input1;
    snapshot.rows[1].input = InputPort::Input2;
    snapshot.rows[2].input = InputPort::Return1And2;
    snapshot.gate_meters[0] = Some(GateMeter::from_level(2.5 / 24.0));
    snapshot.gate_meters[2] = Some(GateMeter::from_level(0.0));
    let frame = ui_with(snapshot).render();
    let orange = |x: u32, y: u32| frame.pixel(x, y) == tetrad_assets::Rgb(0xff, 0x70, 0x00);
    let lit = |x: u32, y: u32| matches!(frame.pixel(x, y), tetrad_assets::Rgb(200.., _, ..50));
    // Ret 1/2 (port at y 297): the whole bar, x 18–41, y 304–307, with soft ends.
    assert!((19..=40).all(|x| orange(x, 305) && orange(x, 306)));
    assert!((20..=39).all(|x| orange(x, 304) && orange(x, 307)));
    assert!(lit(18, 305) && lit(41, 305));
    assert!(!lit(17, 305) && !lit(42, 305) && !lit(30, 303) && !lit(30, 308));
    // In 1 (port at y 109): 2 px open, the black bar left of the mark.
    assert!((116..=119).all(|y| orange(20, y)));
    assert!((21..=40).all(|x| orange(x, 117)));
    let pad = frame.pixel(30, 140);
    assert!(frame.pixel(18, 117).0 < pad.0 && frame.pixel(19, 117) == Palette::STOCK.canvas);
    // In 2: no meter.
    assert!((203..=279).all(|y| (8..=51).all(|x| !lit(x, y))));
}
