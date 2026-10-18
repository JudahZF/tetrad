//! Frame goldens: a fixed walk over the screens the QC opens most (Grid, ⋮ menu, I/O
//! Settings, block editor with a dropdown, capture browser, an unsaved preset title) and their
//! live updates. Each frame is hashed and compared with the hash the renderer produced before
//! the speed work (2026-10-17), so a render change that moves any pixel fails here.
//! The same walk times the renderer on the host (`render_timing`, ignored).

use std::sync::Arc;

use crate::tests::{activate, cell, rich};
use crate::*;

/// FNV-1a, 64 bit: stable across Rust versions (std's hasher is not).
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// [`rich`] with a filled Grid, I/O settings, I/O meters and a capture library.
fn fixture() -> UiSnapshot {
    let mut s = rich();
    let blocks = [
        (0, 0, ModelCategory::GuitarOverdrive, 3001, "DRIVE"),
        (0, 1, ModelCategory::GuitarAmp, 1001, "AMP ONE"),
        (0, 2, ModelCategory::GuitarCabMono, 12001, "CAB"),
        (0, 5, ModelCategory::Delay, 6002, "ECHO TWO"),
        (1, 0, ModelCategory::Compressor, 5001, "COMP"),
        (1, 3, ModelCategory::Reverb, 8001, "HALL"),
        (2, 1, ModelCategory::Modulation, 7001, "CHORUS"),
    ];
    for (row, column, category, model, label) in blocks {
        let index = cell(row, column).index();
        s.blocks[index] = BlockInfo {
            category: Some(category),
            model_id: Some(model),
            plugin: None,
        };
        s.grid[index] = GridCell {
            label: label.into(),
            bypassed: column == 5,
        };
    }
    s.rows[0].input = InputPort::Input1;
    s.rows[1].input = InputPort::Input2;
    for (id, value) in [
        (
            SettingId::InputLevel(InputJack::In1),
            SettingValue::Number(-35),
        ),
        (
            SettingId::InputImpedance(InputJack::In1),
            SettingValue::Choice(3),
        ),
        (
            SettingId::InputType(InputJack::In1),
            SettingValue::Choice(0),
        ),
        (
            SettingId::Phantom(InputJack::In1),
            SettingValue::Bool(false),
        ),
        (
            SettingId::InputGroundLift(InputJack::In1),
            SettingValue::Bool(true),
        ),
    ] {
        s.settings.insert(id, value);
    }
    s.io_meters.inputs = [-18.0, -60.0];
    // A stepped cell: the dropdown ▼.
    s.parameters[4].kind = ParameterKind::Choice {
        position: 0.5,
        steps: 3,
    };
    s.parameters[4].value = "MODERN".into();
    let folder = |name: &str, parent| LibraryFolder {
        name: name.into(),
        parent,
    };
    s.capture_library = Arc::new(CaptureLibrary {
        folders: vec![
            folder("Factory Captures V1", None),
            folder("Factory Captures V2", None),
            folder("My Captures", None),
        ],
        captures: (0..40)
            .map(|index| LibraryCapture {
                value: format!("key-{index}"),
                name: format!(
                    "Capture {} {}",
                    (index * 7) % 40,
                    ["Amp", "Drive", "Comp"][index % 3]
                ),
                date: format!("2025-{:02}-{:02}T10:00:00Z", index % 12 + 1, index % 28 + 1),
                device: Some(CaptureDevice::Compressor),
                nc2: index % 2 == 0,
                origin: if index < 30 {
                    CaptureOrigin::Factory
                } else {
                    CaptureOrigin::User
                },
                folders: vec![if index < 30 { 1 } else { 2 }],
            })
            .collect(),
    });
    s
}

type Step = (&'static str, fn(&mut UiState));

fn change(ui: &mut UiState, edit: impl FnOnce(&mut UiSnapshot)) {
    let mut snapshot = ui.snapshot().clone();
    edit(&mut snapshot);
    ui.set_snapshot(snapshot).unwrap();
}

/// The walk: each step changes the UI before one frame, as the device does.
const STEPS: [Step; 14] = [
    ("grid", |_| {}),
    ("menu open", |ui| _ = activate(ui, Control::OpenMainMenu)),
    ("menu close", |ui| _ = ui.handle_input(InputEvent::Back)),
    ("io open", |ui| _ = ui.navigate(Screen::Io(IoTab::Input1))),
    ("io meter", |ui| {
        change(ui, |s| s.io_meters.inputs = [-6.0, -60.0]);
    }),
    ("io toggle", |ui| {
        let id = SettingId::InputType(InputJack::In1);
        change(ui, |s| _ = s.settings.insert(id, SettingValue::Choice(1)));
    }),
    ("io close", |ui| _ = ui.handle_input(InputEvent::Back)),
    ("editor open", |ui| {
        change(ui, |s| s.selected_block = Some(cell(0, 1)))
    }),
    ("editor knob", |ui| {
        change(ui, |s| {
            s.parameters[1].kind = ParameterKind::Continuous { position: 0.9 };
            s.parameters[1].value = "9".into();
        });
    }),
    ("editor close", |ui| change(ui, |s| s.selected_block = None)),
    ("picker open", |ui| {
        _ = activate(ui, Control::SelectCell(cell(0, 4)));
    }),
    ("capture browser", |ui| {
        let group = PickerControl::Group(DeviceGroup::NeuralCapture);
        _ = activate(ui, Control::Picker(group));
    }),
    ("browser close", |ui| {
        _ = activate(ui, Control::Picker(PickerControl::Close));
    }),
    // The unsaved preset title is italic (sheared glyphs).
    ("grid unsaved", |ui| change(ui, |s| s.dirty = true)),
];

/// Frame hashes of [`STEPS`] from the renderer before the speed work, with the Grid mode glyph
/// redrawn and moved to the stock box, the stock block tile corners, the stock Grid header, the
/// block glyphs fitted to the stock images, the stock row pads and the "Tetrad 0.1.0-alpha.1"
/// version line under the grid.
const GOLDEN: [u64; 14] = [
    0x7cf8fecb3e8386d9,
    0x42329940c3cbc4a4,
    0x7cf8fecb3e8386d9,
    0x5962cb401c8355e5,
    0x2e5a5be15edbadeb,
    0x99c0ffaf2c64adc5,
    0x7cf8fecb3e8386d9,
    0x1377eade186805f0,
    0x520cc05e7c925e2d,
    0x7cf8fecb3e8386d9,
    0x69048b73fb7ea096,
    0x13b2fc624f3b10b2,
    0x7cf8fecb3e8386d9,
    0xd26223cfc8587887,
];

fn start() -> UiState {
    let mut ui = UiState::new();
    ui.set_snapshot(fixture()).unwrap();
    ui
}

/// Every step's partial frame equals its full render, and both equal the golden hash. The
/// walk runs twice on one renderer, so the second pass also starts screens from kept frames.
#[test]
fn frames_match_the_golden_hashes() {
    let mut renderer = Renderer::new();
    for pass in 0..2 {
        let mut ui = start();
        let mut hashes = Vec::new();
        for (name, step) in STEPS {
            step(&mut ui);
            renderer.render(&ui);
            let full = ui.render();
            assert!(
                renderer.frame() == &full,
                "{name} (pass {pass}): partial frame differs"
            );
            hashes.push(fnv(full.rgb()));
        }
        let named: Vec<_> = STEPS.iter().map(|(name, _)| *name).zip(&hashes).collect();
        assert_eq!(hashes, GOLDEN, "{named:#x?}");
    }
}

/// The ⋮ menu by touch, twice, drawn after every pointer event as the device does: each partial
/// frame equals the full render. A press outside the open menu closes it at once, as Qt closes
/// a popup (stock: ~50 ms after the press); its release then changes nothing.
#[test]
fn menu_taps_draw_like_full_renders() {
    let mut ui = start();
    let mut renderer = Renderer::new();
    renderer.render(&ui);
    let events = [
        ("press", InputEvent::PointerDown { x: 765, y: 24 }, false),
        ("open", InputEvent::PointerUp { x: 765, y: 24 }, true),
        (
            "press outside",
            InputEvent::PointerDown { x: 765, y: 24 },
            false,
        ),
        ("release", InputEvent::PointerUp { x: 765, y: 24 }, false),
    ];
    // The second round reopens the menu from its kept frame.
    for (name, event, open) in events.iter().chain(&events).cloned() {
        ui.handle_input(event);
        let rects = renderer.render(&ui);
        assert!(
            renderer.frame() == &ui.render(),
            "{name}: partial frame differs"
        );
        assert_eq!(ui.overlay_open(), open, "{name}");
        if name == "release" {
            assert!(rects.is_empty(), "{name}: {rects:?}");
        }
    }
}

/// Host timing of each step: the full render, the walk's own render (from the previous frame
/// and the kept ones, the walk repeated on one renderer), a render with nothing changed, and
/// the layout alone.
/// Timing only: `cargo test -p tetrad-ui --release render_timing -- --ignored --nocapture`.
#[test]
#[ignore = "timing only"]
fn render_timing() {
    use std::time::{Duration, Instant};
    const ROUNDS: u32 = 200;
    // One renderer for every round, as the device keeps one.
    let mut walk = [Duration::ZERO; STEPS.len()];
    let mut renderer = Renderer::new();
    for _ in 0..ROUNDS {
        let mut ui = start();
        for ((_, step), time) in STEPS.iter().zip(&mut walk) {
            step(&mut ui);
            let started = Instant::now();
            std::hint::black_box(renderer.render(&ui));
            *time += started.elapsed();
        }
    }
    let (mut ui, mut renderer, mut frame) = (start(), Renderer::new(), Frame::new());
    for ((name, step), walk) in STEPS.iter().zip(walk) {
        step(&mut ui);
        renderer.render(&ui);
        let time = |run: &mut dyn FnMut()| {
            let started = Instant::now();
            (0..ROUNDS).for_each(|_| run());
            started.elapsed() / ROUNDS
        };
        let full = time(&mut || ui.render_into(&mut frame));
        let same = time(&mut || _ = std::hint::black_box(renderer.render(&ui)));
        let layout = time(&mut || _ = std::hint::black_box(ui.layout()));
        let walk = walk / ROUNDS;
        eprintln!(
            "{name:16} full {full:>9.1?}  walk {walk:>9.1?}  unchanged {same:>8.1?}  layout {layout:>8.1?}"
        );
    }
}
