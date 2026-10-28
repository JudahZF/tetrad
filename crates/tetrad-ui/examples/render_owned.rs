//! Render a caller-owned synthetic state to binary PPM on stdout, or write the README
//! images (`readme <dir>`) or a few views in every built-in theme (`themes <dir>`) as PNG. All names and values are synthetic; nothing comes from
//! stock firmware or presets.
use std::error::Error;
use std::io::{self, Write};
use std::path::Path;

use tetrad_ui::*;

const VIEWS: &str = "preset-grid|dir-filter|dir-folder-menu|dir-gain|numeric|params-tabs|params-bypassed|picker-pin|device-info|licences|licence|diagnostics|diag-dsp|diag-fs|diag-usb|send-report|dir-link|dir-media-delete|dir-media-remove|vd-overwrite|vd-delete|dir-category|dir-captures|dir-irs|dir-list|dir-multi|dir-menu|dir-search|dir-recent|favorites|saveas|savedest|copyscene|gigpreset|grid|scenes|gig|presets|draft|params|picker|media|menu|settings|wifi|updates|io|tuner|looper|looper-empty|tempo|capture|keyboard|block|routing|confirm|choice|stomp|diagram|groups|update-ready|update-confirm|update-install|backups|storage|process|ab|picker-amp|vd-presets|vd-menu|expression|media-capture|capture-tab2|capture-save|power|locked|volume|modes|modes-hybrid|modes-add|brightness|bypass|scene-bypass|factory|about|recovery|resetting|io-out|io-usb|io-ret|io-hp|global-eq|midi-out|midi-edit|assign|edit-stomp|notice|gate|exp-range|account|stomp-bar|nc2|nc2-lost|nc2-ab|eq-presets|eq-preset-menu|io-presets|splitter|nc2-saved|pattern|pattern-2|cab|cab-2|cab-missing|restoring|eq|eq-out|eq-type|geq|geq-2|ir|ir-2|nc|nc-missing|dsp-error|synth-filter|parallax-expanded|mini-midi|storage-quarantine|dir-cloud|dir-cloud-empty|cloud-offline|whats-new|backup-name|backup-menu|paste-option|paste-bar|dir-cloud-multi|preset-metadata|upload-overwrite|capture-overwrite|capture-upload-overwrite|upload-failed|upload-done|add-to-grid|dir-media-edit|dir-ir-exists|download-setlist|download-option|upload-status|upload-batch|upload-batch-ask|details|dir-cloud-ir-multi|cpu-status|eq-off|eq-gate-off|extensions|ext-empty|ext-detail|ext-blocks|ext-detail-mid|ext-detail-end|ext-fault|ext-remove|ext-sources|ext-trust|map-menu|map-list|map-global|map-detail|map-learn|map-send|map-actions|map-block|map-toggle|map-values|appearance|remote|custom-gig|custom-edit|custom-actions|custom-copy|custom-preset|custom-picker|picker-ext";

fn fixture() -> Result<UiSnapshot, UiError> {
    let mut s = UiSnapshot {
        preset_id: Some(42),
        preset_title: "STUDIO CRUNCH".into(),
        preset_slot: Some(BankSlot { bank: 0, slot: 1 }),
        selected_scene: SceneIndex::new(1)?,
        footswitch_mode: Some(FootswitchMode::Scene),
        status: "OWNED FIXTURE - NO DSP".into(),
        dirty: true,
        can_undo: true,
        preset_file: true,
        favorite_presets: ["Deep Clean", "Lead Boost", "Studio Crunch"]
            .into_iter()
            .enumerate()
            .map(|(id, title)| PresetItem {
                id: id as u64,
                title: title.into(),
            })
            .collect(),
        presets: [
            "Deep Clean",
            "Studio Crunch",
            "Glass Chorus",
            "Lead Boost",
            "Ambient Swell",
            "Fuzz Wall",
            "Dry Funk",
            "Bass DI",
            "Slapback",
            "Shimmer",
        ]
        .into_iter()
        .cycle()
        .take(20)
        .zip(0..)
        .map(|(title, id)| PresetItem {
            id,
            title: title.into(),
        })
        .collect(),
        setlists: ["My Presets", "Live Set", "Studio"]
            .into_iter()
            .enumerate()
            .map(|(id, title)| SetlistItem {
                id: id as u64,
                title: title.into(),
            })
            .collect(),
        active_setlist: Some(0),
        ..UiSnapshot::default()
    };
    s.bank.presets = s.presets[..8].to_vec();
    for (scene, label) in s.scenes.iter_mut().zip([
        "Intro", "Verse", "Chorus", "Lead", "Bridge", "Clean", "Drive", "Outro",
    ]) {
        scene.label = label.into();
    }
    let blocks = [
        (0, 0, "Gate", ModelCategory::Compressor, false),
        (0, 1, "Green Drive", ModelCategory::GuitarOverdrive, false),
        (0, 2, "Crunch Amp", ModelCategory::GuitarAmp, false),
        (0, 3, "Cab 4x12", ModelCategory::GuitarCabMono, false),
        (0, 5, "Tape Echo", ModelCategory::Delay, true),
        (0, 6, "Plate Verb", ModelCategory::Reverb, false),
        (1, 2, "Chorus", ModelCategory::Modulation, false),
        (1, 4, "Octaver", ModelCategory::Pitch, false),
    ];
    for (row, column, label, category, bypassed) in blocks {
        let position = GridPosition::new(row, column)?;
        let index = usize::from(position.row()) * GRID_COLUMNS + usize::from(position.column());
        s.grid[index] = GridCell {
            label: label.into(),
            bypassed,
        };
        s.blocks[index] = BlockInfo {
            category: Some(category),
            model_id: Some(u32::from(category as u8) * 1000 + u32::from(column)),
            plugin: None,
        };
    }
    // An unlicensed plugin model shows the licence lock (review M11).
    s.locked_models
        .insert(u32::from(ModelCategory::Pitch as u8) * 1000 + 4);
    s.rows[0] = RowView {
        input: InputPort::Input1,
        output: OutputPort::MultipleOuts,
        ..RowView::default()
    };
    s.rows[1] = RowView {
        input: InputPort::PrevRow,
        output: OutputPort::Xlr1And2,
        ..RowView::default()
    };
    s.pairs[0] = PairView {
        split: Some(2),
        mix: Some(5),
    };
    for (index, (label, category, engaged)) in [
        ("Drive", ModelCategory::GuitarOverdrive, true),
        ("Echo", ModelCategory::Delay, false),
        ("Verb", ModelCategory::Reverb, true),
        ("", ModelCategory::Utility, false),
        ("Chorus", ModelCategory::Modulation, false),
        ("Boost", ModelCategory::GuitarOverdrive, true),
        ("Octave", ModelCategory::Pitch, false),
        ("Wah", ModelCategory::Wah, false),
    ]
    .into_iter()
    .enumerate()
    {
        s.stomps[index] = StompView {
            label: label.into(),
            engaged,
            category: Some(category),
            multiple: false,
            position: None,
            momentary: false,
            blocks: Vec::new(),
        };
    }
    s.models = ModelCategory::PICKER
        .into_iter()
        .flat_map(|category| {
            (0..6).map(move |n| ModelEntry {
                id: u32::from(category as u8) * 1000 + n,
                name: format!("{} Model {}", category.label(), n + 1),
                category,
                extension: false,
                plugin: None,
            })
        })
        .collect::<Vec<_>>()
        .into();
    s.parameters = [
        "GAIN", "BASS", "MID", "TREBLE", "PRESENCE", "MASTER", "BRIGHT", "VOICING",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, label)| ParameterView {
        entry: None,
        scene: index == 2,
        index: index as u16,
        label: label.into(),
        value: format!("{:.1}", index as f32 * 1.3 + 2.0),
        kind: match index {
            6 => ParameterKind::Toggle { on: true },
            7 => ParameterKind::Choice {
                position: 0.0,
                steps: 0,
            },
            _ => ParameterKind::Continuous {
                position: 0.15 + index as f32 * 0.14,
            },
        },
    })
    .collect();
    s.parameters[7].value = "MODERN".into();
    for (id, value) in [
        (SettingId::MidiChannel, SettingValue::Choice(1)),
        (SettingId::MidiOverUsb, SettingValue::Bool(true)),
        (SettingId::IgnoreDuplicatePc, SettingValue::Bool(false)),
        (SettingId::MidiClockOut, SettingValue::Choice(3)),
        (SettingId::MidiClockIn, SettingValue::Bool(false)),
        (SettingId::MidiThru, SettingValue::Bool(true)),
        (SettingId::ReloadPreset, SettingValue::Bool(false)),
        (SettingId::ReloadScene, SettingValue::Bool(true)),
        (SettingId::InternetCheck, SettingValue::Bool(true)),
        (
            SettingId::InputLevel(InputJack::In1),
            SettingValue::Number(-35),
        ),
        (
            SettingId::InputLevel(InputJack::In2),
            SettingValue::Number(0),
        ),
        (
            SettingId::InputImpedance(InputJack::In1),
            SettingValue::Choice(7),
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
        (SettingId::TunerReference, SettingValue::Number(440)),
        (SettingId::TunerMute, SettingValue::Bool(true)),
        (SettingId::LiveTuner, SettingValue::Bool(false)),
        (SettingId::TunerInput, SettingValue::Choice(0)),
        (SettingId::Metronome, SettingValue::Bool(true)),
        (SettingId::TempoMode, SettingValue::Choice(0)),
        (SettingId::BpmNote, SettingValue::Choice(0)),
        (SettingId::TimeSignature, SettingValue::Choice(2)),
        (SettingId::Subdivision, SettingValue::Choice(0)),
        (SettingId::TempoLed, SettingValue::Bool(true)),
        (SettingId::MetronomeSound, SettingValue::Choice(0)),
        (SettingId::MetronomeRouting, SettingValue::Choice(1)),
        (SettingId::MetronomeLevel, SettingValue::Number(-60)),
        (SettingId::MetronomePan, SettingValue::Number(0)),
    ] {
        s.settings.insert(id, value);
    }
    s.tempo = TempoView {
        bpm: 128.0,
        metronome: true,
        beat: Some(1),
        beats_per_bar: 4,
        // Shot 008: a mark above beat 1 and below beat 3.
        accents: std::array::from_fn(|beat| match beat {
            0 => BeatAccent::Above,
            2 => BeatAccent::Below,
            _ => BeatAccent::Dot,
        }),
    };
    s.tuner = TunerView {
        note: Some(Note {
            pitch_class: 9,
            octave: 2,
        }),
        // A2 is 110 Hz: 1200 × log2(108.6 / 110) ≈ −22.2 cents.
        cents: -22.2,
        frequency_hz: Some(108.6),
    };
    s.looper = LooperView {
        state: LooperState::Overdubbing,
        progress: 0.62,
        loop_seconds: Some(14.0),
        undo_available: true,
        reverse: true,
        ..LooperView::default()
    };
    s.capture = CaptureView {
        step: CaptureStep::Calibration,
        input_levels: [0.55, 0.82],
        ..CaptureView::default()
    };
    s.wifi = WifiView {
        status: WifiStatus::InternetConnected,
        networks: [
            ("Studio 5G", 5, true, true),
            ("Rehearsal", 3, true, false),
            ("Open Cafe", 1, false, false),
        ]
        .into_iter()
        .map(|(ssid, signal, secured, connected)| WifiNetwork {
            ssid: ssid.into(),
            signal,
            secured,
            connected,
            saved: connected,
            bssid: "02:00:00:00:00:01".into(),
            frequency_mhz: 2437,
            rssi_dbm: -40 - 8 * i32::from(5 - signal.min(5)),
        })
        .collect(),
        ip_address: Some("192.0.2.20".into()),
        alert: None,
        joining: None,
    };
    s.updater = UpdaterView {
        current_version: "4.1.0".into(),
        available_version: Some("4.2.0".into()),
        state: UpdaterState::DownloadingData,
        status: UpdaterStatus::NewVersionAvailable,
        progress: Some(0.35),
        changelog: "Synthetic changelog.".into(),
        online: true,
        channel: UpdateChannel::Official,
        stock_after_install: true,
    };
    s.account.username = Some("player@example.com".into());
    s.backups.items = [
        ("My Rig 001", "2025-08-29 17:06", Some("2025-08-29 17:11")),
        ("My Backup", "2024-11-15 19:22", None),
        ("Tour 2025", "2023-08-16 15:38", None),
    ]
    .into_iter()
    .enumerate()
    .map(|(id, (title, detail, downloaded))| BackupItem {
        id: id as u64,
        title: title.into(),
        detail: detail.into(),
        downloaded: downloaded.map(Into::into),
    })
    .collect();
    s.storage = StorageView {
        presets: StorageUse {
            current: 412,
            maximum: 2048,
        },
        captures: StorageUse {
            current: 930,
            maximum: 1000,
        },
        impulse_responses: StorageUse {
            current: 64,
            maximum: 1024,
        },
    };
    settings_extras(&mut s);
    s.appearance.themes = theme_rows("stock");
    Ok(s)
}

/// Device settings, I/O, Global EQ and Preset MIDI Out values for the settings views.
fn settings_extras(s: &mut UiSnapshot) {
    let band = |n| EqBand::new(n).expect("band");
    let mut values = vec![
        (SettingId::ScreenBrightness, SettingValue::Number(50)),
        (SettingId::LedBrightness, SettingValue::Number(50)),
        (SettingId::DimmedLedBrightness, SettingValue::Number(6)),
        (SettingId::PresetDimmed, SettingValue::Bool(true)),
        (SettingId::SceneDimmed, SettingValue::Bool(true)),
        (SettingId::StompDimmed, SettingValue::Bool(false)),
        (SettingId::SceneBypassBehavior, SettingValue::Choice(1)),
        (
            SettingId::OutputPair(OutputJack::Out12),
            SettingValue::Bool(true),
        ),
        (
            SettingId::OutputPair(OutputJack::Out34),
            SettingValue::Bool(false),
        ),
        (
            SettingId::OutputLevel(OutputJack::Out12),
            SettingValue::Number(0),
        ),
        (
            SettingId::OutputGroundLift(OutputJack::Out12),
            SettingValue::Bool(false),
        ),
        (
            SettingId::OutputMute(OutputJack::Out12),
            SettingValue::Bool(false),
        ),
        (
            SettingId::ReturnLevel(LoopJack::One),
            SettingValue::Number(0),
        ),
        (
            SettingId::ReturnGroundLift(LoopJack::One),
            SettingValue::Bool(false),
        ),
        (
            SettingId::ReturnLevel(LoopJack::Two),
            SettingValue::Number(60),
        ),
        (
            SettingId::ReturnGroundLift(LoopJack::Two),
            SettingValue::Bool(true),
        ),
        (SettingId::UsbLevel, SettingValue::Number(0)),
        (SettingId::HeadphoneSource, SettingValue::Choice(3)),
        (SettingId::UsbRouting, SettingValue::Choice(0)),
        (SettingId::EqEnabled, SettingValue::Bool(true)),
        (SettingId::EqOutput, SettingValue::Number(0)),
        (
            SettingId::EqAssign(OutputJack::Out12),
            SettingValue::Bool(true),
        ),
        (
            SettingId::EqAssign(OutputJack::Out34),
            SettingValue::Bool(false),
        ),
    ];
    for row in 0..4 {
        let row = Row::new(row).expect("row");
        values.push((
            SettingId::GlobalBypassCab(row),
            SettingValue::Bool(row.get() == 1),
        ));
        values.push((SettingId::GlobalBypassIr(row), SettingValue::Bool(false)));
    }
    // Shot 009 curve: HI PASS 50 Hz, then four bells/shelves.
    for (n, (kind, gain, hz, q)) in [
        (0, 0, 50, 70),
        (4, 25, 220, 70),
        (4, -20, 1_300, 200),
        (3, 0, 4_000, 70),
        (2, 15, 5_000, 70),
    ]
    .into_iter()
    .enumerate()
    {
        let b = band(n as u8);
        values.extend([
            (SettingId::EqType(b), SettingValue::Choice(kind)),
            (SettingId::EqGain(b), SettingValue::Number(gain)),
            (SettingId::EqFreq(b), SettingValue::Number(hz)),
            (SettingId::EqQ(b), SettingValue::Number(q)),
            (SettingId::EqBypass(b), SettingValue::Bool(false)),
        ]);
    }
    s.settings.extend(values);
    let cc = |channel, cc, value| MidiOutMessage {
        kind: MidiMessageKind::Cc,
        channel,
        values: [Some(cc), Some(value), None],
    };
    s.preset_midi.on_load = vec![
        MidiOutMessage {
            kind: MidiMessageKind::Pc,
            channel: 1,
            values: [None, None, Some(12)],
        },
        cc(1, 7, 100),
    ];
    s.preset_midi.footswitches[1] = vec![MidiOutMessage {
        kind: MidiMessageKind::CcToggle,
        channel: 2,
        values: [Some(64), Some(0), Some(127)],
    }];
    s.preset_midi.expression[0] = vec![cc(3, 11, 0)];
}

/// Scene colours of a new preset (`0xAARRGGBB`, tetrad-preset `DEFAULT_SCENE_COLORS`).
fn gig_colours(snapshot: &mut UiSnapshot) {
    let colours = [
        0xffff_2727,
        0xff0a_74e0,
        0xffff_d236,
        0xffff_02c2,
        0xff45_f862,
        0xffff_7000,
        0xff69_54ff,
        0xff00_ffdd,
    ];
    for (scene, colour) in snapshot.scenes.iter_mut().zip(colours) {
        scene.raw_color = colour;
    }
}

/// Activates a menu item; the ⋮ menu scrolls (stock 4.1.0), so items below its fold need
/// a drag first.
fn menu_item(ui: &mut UiState, action: MenuAction) {
    let control = Control::Menu(action);
    for _ in 0..3 {
        if ui
            .hit_regions()
            .iter()
            .any(|region| region.control == control)
        {
            break;
        }
        ui.handle_input(InputEvent::PointerDown { x: 600, y: 400 });
        ui.handle_input(InputEvent::PointerMove { x: 600, y: 40 });
        ui.handle_input(InputEvent::PointerUp { x: 600, y: 40 });
    }
    ui.handle_input(InputEvent::Activate(control));
}

/// The README images: file name in `docs/images/` and view. `boot-selector` is added.
const README: [(&str, &str); 5] = [
    ("grid", "grid"),
    ("block-editor", "params"),
    ("gig", "gig"),
    ("presets", "presets"),
    ("looper", "looper"),
];

/// Views `themes <dir>` draws in each built-in theme.
const THEMED: [&str; 6] = ["grid", "params", "gig", "presets", "settings", "appearance"];

fn usage() -> Box<dyn Error> {
    format!(
        "usage: render_owned [{VIEWS}] [out.png] | render_owned readme <dir> | render_owned themes <dir>"
    )
    .into()
}

/// Settings → Appearance rows: the built-in themes, a theme file, one that fails its checks,
/// and an extension theme.
fn theme_rows(selected: &str) -> Vec<ThemeRow> {
    use tetrad_theme::{BUILTINS, ThemeId, builtin};
    let mut rows: Vec<ThemeRow> = (BUILTINS.iter())
        .map(|theme| ThemeRow {
            id: ThemeId::Builtin(theme.id.into()),
            name: theme.name.into(),
            source: "Built-in".into(),
            palette: Ok(theme.palette),
            selected: theme.id == selected,
        })
        .collect();
    let mut midnight = builtin("slate").map(|t| t.palette).unwrap_or_default();
    midnight.background = tetrad_theme::Rgb::hex(0x0d1020);
    midnight.accent = tetrad_theme::Rgb::hex(0x7aa2f7);
    rows.push(ThemeRow {
        id: ThemeId::File("midnight".into()),
        name: "Midnight".into(),
        source: "Theme file midnight.json".into(),
        palette: Ok(midnight),
        selected: false,
    });
    rows.push(ThemeRow {
        id: ThemeId::File("daylight".into()),
        name: "Daylight".into(),
        source: "Theme file daylight.json".into(),
        palette: Err("background must be dark: Tetrad themes are dark only".into()),
        selected: false,
    });
    let mut dusk = builtin("ember").map(|t| t.palette).unwrap_or_default();
    dusk.accent = tetrad_theme::Rgb::hex(0xff8fab);
    rows.push(ThemeRow {
        id: ThemeId::from("ext:JudahZF/tetrad-extensions:judah.dusk-pack/dusk".to_owned()),
        name: "Dusk".into(),
        source: "Dusk Themes".into(),
        palette: Ok(dusk),
        selected: false,
    });
    rows
}

/// Writes [`THEMED`] in each built-in theme to `dir` as `<theme>-<view>.png`.
fn themes(dir: &Path) -> Result<(), Box<dyn Error>> {
    for theme in &tetrad_theme::BUILTINS {
        for view in THEMED {
            let mut snapshot = UiSnapshot {
                dirty: false,
                ..fixture()?
            };
            snapshot.appearance = AppearanceView {
                palette: theme.palette,
                themes: theme_rows(theme.id),
            };
            let frame = render(view, snapshot)?;
            let image = png(WIDTH, HEIGHT, PngColour::Rgb, frame.rgb()).ok_or("bad frame size")?;
            std::fs::write(dir.join(format!("{}-{view}.png", theme.id)), image)?;
        }
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let view = match args.as_slice() {
        [] => "grid",
        [view] => view.as_str(),
        [mode, dir] if mode == "readme" => return readme(Path::new(dir)),
        [mode, dir] if mode == "themes" => return themes(Path::new(dir)),
        // One view as PNG, for looking at a page.
        [view, file] if file.ends_with(".png") => {
            let frame = render(view, fixture()?)?;
            let image = png(WIDTH, HEIGHT, PngColour::Rgb, frame.rgb()).ok_or("bad frame size")?;
            std::fs::write(file, image)?;
            return Ok(());
        }
        _ => return Err(usage()),
    };
    let snapshot = fixture()?;
    // Control Screenshot (25): the RGBA grid over a 16 px checkerboard, 800 × 384.
    if view == "preset-grid" {
        let rgba = UiState::preset_grid(snapshot)?;
        let mut out = io::stdout().lock();
        write!(out, "P6\n{PRESET_GRID_WIDTH} {PRESET_GRID_HEIGHT}\n255\n")?;
        for (index, pixel) in rgba.chunks_exact(4).enumerate() {
            let (x, y) = (
                index as u32 % PRESET_GRID_WIDTH,
                index as u32 / PRESET_GRID_WIDTH,
            );
            let back: u16 = if (x / 16 + y / 16) % 2 == 0 {
                0x60
            } else {
                0x90
            };
            let alpha = u16::from(pixel[3]);
            let mix = |c: u8| ((u16::from(c) * alpha + back * (255 - alpha)) / 255) as u8;
            out.write_all(&[mix(pixel[0]), mix(pixel[1]), mix(pixel[2])])?;
        }
        return Ok(());
    }
    render(view, snapshot)?.write_ppm(io::stdout().lock())?;
    Ok(())
}

/// Writes the README images to `dir` as PNG: the boot selector, and the [`README`] views
/// without the unsaved-changes mark.
fn readme(dir: &Path) -> Result<(), Box<dyn Error>> {
    let mut frames = vec![(
        "boot-selector",
        BootMenu {
            default: BootChoice::Tetrad,
            seconds_left: 4,
            version: concat!("Tetrad ", env!("CARGO_PKG_VERSION")),
        }
        .render(),
    )];
    for (name, view) in README {
        let snapshot = UiSnapshot {
            dirty: false,
            ..fixture()?
        };
        frames.push((name, render(view, snapshot)?));
    }
    for (name, frame) in frames {
        let image = png(WIDTH, HEIGHT, PngColour::Rgb, frame.rgb()).ok_or("bad frame size")?;
        std::fs::write(dir.join(format!("{name}.png")), image)?;
    }
    Ok(())
}

fn render(view: &str, mut snapshot: UiSnapshot) -> Result<Frame, Box<dyn Error>> {
    if matches!(view, "remote" | "send-report") {
        snapshot.remote = RemoteView {
            enabled: true,
            address: Some("http://192.168.1.20".into()),
            pin: Some("4821".into()),
            status: Some("Added V30 SM57".into()),
        };
    }
    if matches!(
        view,
        "params" | "assign" | "stomp-bar" | "numeric" | "params-tabs" | "params-bypassed"
    ) {
        snapshot.selected_block = Some(GridPosition::new(0, 2)?);
    }
    match view {
        "numeric" => {
            snapshot.parameters[0].entry = Some(NumericRange {
                value: 2.0,
                min: 0.0,
                max: 10.0,
                integer: false,
            });
        }
        // A bypassed block with three parameters (stock `sl-1`).
        "params-bypassed" => {
            snapshot.parameters.truncate(3);
            snapshot.grid[2].bypassed = true;
        }
        // Three TabBar pages (23 parameters).
        "params-tabs" => {
            let first = snapshot.parameters.clone();
            for page in 1..3u16 {
                snapshot
                    .parameters
                    .extend(first.iter().map(|parameter| ParameterView {
                        index: parameter.index + page * 8,
                        ..parameter.clone()
                    }));
            }
            snapshot.parameters.truncate(23);
        }
        "picker-ext" => {
            let blocks = [
                ("NAM A2 Plexi", ModelCategory::GuitarAmp),
                ("NAM A1 Rectifier", ModelCategory::GuitarAmp),
                ("Sweep Synth", ModelCategory::Synth),
                ("Bass Sweep Synth", ModelCategory::Synth),
            ];
            let mut models = snapshot.models.to_vec();
            models.extend(
                blocks
                    .into_iter()
                    .zip(800..)
                    .map(|((name, category), id)| ModelEntry {
                        id,
                        name: name.into(),
                        category,
                        extension: true,
                        plugin: None,
                    }),
            );
            snapshot.models = models.into();
        }
        "picker-pin" => {
            snapshot.pinned_models = snapshot
                .models
                .iter()
                .filter(|model| model.category == ModelCategory::GuitarAmp)
                .skip(2)
                .map(|model| model.id)
                .take(1)
                .collect();
        }
        "device-info" => {
            snapshot.about = AboutView {
                device_name: Some("Owned Rig".into()),
                serial: Some("SYNTH0001".into()),
                mac: Some("00:00:5e:00:53:01".into()),
                zen_boards: true,
                coros: Some("4.1.0".into()),
                kernel: Some("synthetic".into()),
                uboot: Some("U-Boot synthetic".into()),
                wireless_fw: Some("00112233445566778899aabbccddeeff".into()),
                zenjack_app: Some("1a2b".into()),
                ..AboutView::default()
            };
        }
        // Synthetic DSP Diagnostics: cores 1 and 3 report, the others show "-1".
        "diag-dsp" => {
            let core = DspCoreView {
                dropped_messages: 3,
                fp_exceptions: 0,
                cpu: [41.25, 57.5, 3.75],
                emdma: 12.345,
                heap: [42.5, 18.75, 2.25],
            };
            snapshot.dsp_diagnostics = [Some(core), None, Some(core), None];
        }
        view if view.starts_with("custom-") => {
            snapshot.mappings = mappings_fixture()?;
            snapshot.custom = Some(custom_fixture()?);
            snapshot.footswitch_mode = Some(FootswitchMode::Custom);
        }
        view if view.starts_with("map-") => {
            snapshot.mappings = mappings_fixture()?;
            if view == "map-learn" {
                snapshot.mappings.learning = Some((MappingScope::Preset, MappingId::Tetrad(0)));
            }
        }
        view if view.starts_with("ext") => {
            if view != "ext-empty" {
                snapshot.extensions = extensions_fixture();
            }
            if view == "ext-trust" {
                snapshot.extensions.pending_trust = Some(PendingTrust {
                    repository: "night-owl/footswitch-tools".into(),
                    fingerprint: "3f2a 9c01 77be 04d2 e1a9 5b6c 0d83 f4e7".into(),
                });
            }
        }
        "licence" => {
            snapshot.licence = Some(LicenceView {
                package: "binutils".into(),
                text: Some("Owned placeholder text for the licence viewer.\n\nThe device reads the real notice from its system image.".into()),
            });
        }
        _ => {}
    }
    if matches!(view, "assign" | "edit-stomp" | "stomp-bar") {
        snapshot.footswitch_mode = Some(FootswitchMode::Stomp);
        snapshot.selected_stomp = (view == "assign").then_some(0);
        snapshot.stomps[0].blocks = vec![GridPosition::new(0, 2)?];
        snapshot.stomps[0].momentary = true;
    }
    if view == "notice" {
        snapshot.notice = Some(NoticeView {
            id: 1,
            title: "Preset".into(),
            body: "Virtual Device Preset 8: 1 invalid values left out".into(),
            button: "OK",
        });
    }
    // Directory upload result dialogs (tetrad-app `Ui::upload_end`).
    let upload_end = match view {
        "upload-failed" => Some((
            "Failed to upload Preset",
            "This Preset could not be uploaded as one or more Neural Captures are missing.",
        )),
        "upload-done" => Some((
            "Preset uploaded",
            "Preset Synthetic Lead was uploaded to your Cortex Cloud account.",
        )),
        _ => None,
    };
    if let Some((title, body)) = upload_end {
        snapshot.notice = Some(NoticeView {
            id: 1,
            title: title.into(),
            body: body.into(),
            button: "OK",
        });
    }
    // Splitter editor (`SMParamsEditor`, M092): TYPE and the quick split buttons.
    if [
        "pattern", "cab", "eq", "geq", "ir", "nc", "arp", "mini", "multi", "synth", "parallax",
    ]
    .iter()
    .any(|prefix| view.starts_with(prefix))
    {
        model_editor(&mut snapshot, view)?;
    }
    // Mic B's IR is not in the media list: the missing note (`0x821408`).
    if view == "cab-missing" {
        snapshot.media = vec![MediaEntry {
            folder: MediaFolder::FactoryIrs,
            name: "Synthetic Cab 1 Mic A".into(),
            value: "Synthetic Cab 1_Mic A".into(),
        }]
        .into();
    }
    // Mic A in IR mode (`0x8203a4`): the table lists the IRs of its cab.
    if view == "cab-ir" {
        let mics = [
            "Dynamic 57",
            "Ribbon 121",
            "Condenser 414",
            "Dynamic 421",
            "Ribbon 160",
        ];
        snapshot.media = mics
            .iter()
            .map(|mic| MediaEntry {
                folder: MediaFolder::FactoryIrs,
                name: format!("Synthetic Cab 1 {mic}"),
                value: format!("SYN_Cab 1_{mic}"),
            })
            .collect::<Vec<_>>()
            .into();
        if let Some(p) = snapshot.parameters.iter_mut().find(|p| p.index == 1) {
            p.value = "Synthetic Cab 1 Ribbon 121".into();
        }
    }
    // A restore's loading overlay (`0x81b74c`) over the Grid.
    snapshot.restoring = view == "restoring";
    snapshot.dsp_error = view == "dsp-error";
    if view == "splitter" {
        snapshot.selected_site = Some(EditorSite::Splitter(RowPair::Upper));
        snapshot.parameters = vec![ParameterView {
            index: 0,
            label: "TYPE".into(),
            value: "A/B".into(),
            kind: ParameterKind::Choice {
                position: 0.0,
                steps: 0,
            },
            entry: None,
            scene: false,
        }];
    }
    if view == "gate" {
        let row = Row::new(0)?;
        snapshot.selected_site = Some(EditorSite::InputGate(row));
        snapshot.parameters = ["THRESHOLD", "RELEASE", "INPUT GAIN", "GATE"]
            .into_iter()
            .enumerate()
            .map(|(index, label)| ParameterView {
                index: index as u16,
                label: label.into(),
                value: ["-60.0 dB", "50 ms", "0.0 dB", "Off"][index].into(),
                kind: if index == 3 {
                    ParameterKind::Toggle { on: false }
                } else {
                    ParameterKind::Continuous { position: 0.4 }
                },
                entry: None,
                scene: false,
            })
            .collect();
    }
    if view == "account" {
        snapshot.account.username = Some("player@example.com".into());
    }
    if view == "media" {
        snapshot.selected_block = Some(GridPosition::new(0, 3)?);
        snapshot.parameters[0] = ParameterView {
            entry: None,
            scene: false,
            index: 20,
            label: "MIC A".into(),
            value: "Synthetic Cab 2 Mic B".into(),
            kind: ParameterKind::Media(MediaKind::Ir),
        };
        snapshot.media = (1..=3)
            .flat_map(|cab| {
                ["A", "B", "C"].map(|mic| MediaEntry {
                    folder: MediaFolder::FactoryIrs,
                    name: format!("Synthetic Cab {cab} Mic {mic}"),
                    value: format!("SYN_Synthetic Cab {cab}_Mic {mic}"),
                })
            })
            .chain((1..=2).map(|n| MediaEntry {
                folder: MediaFolder::MyIrs,
                name: format!("My Room IR {n}"),
                value: format!("CIR_{n:x}"),
            }))
            .collect::<Vec<_>>()
            .into();
    }
    // Global EQ (4004) and I/O (31000) presets: "Bright" matches the Global EQ page.
    if matches!(view, "eq-presets" | "eq-preset-menu" | "io-presets") {
        let rows = [
            (4004, "Flat", true),
            (4004, "Bright", true),
            (4004, "Warm Low End", true),
            (4004, "Stage PA", false),
            (4004, "Studio Monitors", false),
            (31_000, "Gig Rig", false),
            (31_000, "Home Studio", false),
        ];
        snapshot.device_presets = (0..)
            .zip(rows)
            .map(|(id, (model_id, name, factory))| DevicePresetItem {
                id,
                model_id,
                name: name.into(),
                factory,
                default: id == 0,
            })
            .collect::<Vec<_>>()
            .into();
        snapshot.settings_preset_match = SettingsPresetMatch {
            global_eq: Some(1),
            io: None,
        };
    }
    if matches!(
        view,
        "vd-presets" | "vd-menu" | "vd-overwrite" | "vd-delete" | "expression" | "exp-range"
    ) {
        snapshot.selected_block = Some(GridPosition::new(0, 2)?);
        let factory = [
            "Owned Default",
            "Balanced Crunch",
            "Basic Clean",
            "Bright Clean",
            "Punk Drive",
            "Tube Drive",
        ];
        snapshot.device_presets = factory
            .into_iter()
            .map(|name| (name, true))
            .chain([("Lead Tone", false), ("Low Gain", false)])
            .enumerate()
            .map(|(id, (name, factory))| DevicePresetItem {
                id: id as u64,
                model_id: 1002,
                name: name.into(),
                factory,
                default: id == 0,
            })
            .collect::<Vec<_>>()
            .into();
        snapshot.expression = vec![
            ExpressionLink {
                pedal: ExpressionPedal::Exp1,
                target: ExpressionTarget::Bypass,
            },
            ExpressionLink {
                pedal: ExpressionPedal::Exp1,
                target: ExpressionTarget::Parameter(0),
            },
        ];
        snapshot.device_preset = Some(1);
        snapshot.expression_switch = ExpressionSwitchView {
            delay_ms: 600,
            ..ExpressionSwitchView::default()
        };
    }
    if view == "media-capture" {
        snapshot.selected_block = Some(GridPosition::new(0, 2)?);
        snapshot.parameters[0] = ParameterView {
            entry: None,
            scene: false,
            index: 30,
            label: "CAPTURE".into(),
            value: "Synthetic Capture 2".into(),
            kind: ParameterKind::Media(MediaKind::Capture),
        };
        snapshot.media = (1..=9)
            .map(|n| MediaEntry {
                folder: MediaFolder::MyCaptures,
                name: format!("Synthetic Capture {n}"),
                value: format!("cap{n}"),
            })
            .collect::<Vec<_>>()
            .into();
    }
    if view.starts_with("dir-") {
        // Synthetic Directory library: captures by two authors' names, IRs in two folders.
        let captures = [
            "Brit Crunch",
            "4-Comp Custom",
            "Clean Combo",
            "Fuzz Face",
            "Plexi Lead",
        ];
        snapshot.media = captures
            .iter()
            .flat_map(|name| {
                (1..=3).map(move |n| MediaEntry {
                    folder: MediaFolder::MyCaptures,
                    name: format!("{name} {n}"),
                    value: format!("cap_{name}_{n}"),
                })
            })
            .chain((15..=21).rev().map(|n| MediaEntry {
                folder: MediaFolder::MyIrs,
                name: format!("IR{n}"),
                value: format!("CIR_{n:x}"),
            }))
            .chain((1..=3).map(|n| MediaEntry {
                folder: MediaFolder::FactoryIrs,
                name: format!("Synthetic Cab {n}"),
                value: format!("SYN_Cab {n}"),
            }))
            .collect::<Vec<_>>()
            .into();
    }
    if view.starts_with("dir-media")
        || matches!(
            view,
            "dir-link" | "dir-filter" | "dir-folder-menu" | "dir-gain"
        )
    {
        // Capture folders "Live" and "Studio" under the library root; one factory capture.
        let folder = |id, name: &str, root| DirectoryFolder {
            id,
            kind: MediaKind::Capture,
            name: name.into(),
            parent: if root { 0 } else { 1 },
            read_only: false,
            root,
        };
        std::sync::Arc::make_mut(&mut snapshot.directory).folders = vec![
            folder(1, "Captures Library", true),
            folder(2, "Live", false),
            folder(3, "Studio", false),
        ];
        std::sync::Arc::make_mut(&mut snapshot.directory).media = snapshot
            .media
            .iter()
            .filter(|entry| entry.folder == MediaFolder::MyCaptures)
            .enumerate()
            // Synthetic metadata: device codes and gains cycle; "Studio" saves new captures.
            .map(|(n, entry)| DirectoryMedia {
                value: entry.value.clone(),
                author: String::new(),
                date: String::new(),
                read_only: n == 1,
                folders: if n < 4 { vec![2] } else { Vec::new() },
                capture: Some(CaptureMeta {
                    device: Some((n % 9) as u8),
                    gain: Some((n * 3 % 11) as u8),
                    version: if n % 2 == 0 { "2" } else { "1" }.into(),
                    instrument: None,
                }),
            })
            .collect();
        std::sync::Arc::make_mut(&mut snapshot.directory).saving_destinations = vec![3];
    }
    let mut ui = UiState::new();
    ui.set_snapshot(snapshot)?;
    let activate = |ui: &mut UiState, control| ui.handle_input(InputEvent::Activate(control));
    let category = |ui: &mut UiState, index| {
        activate(ui, Control::Navigate(Screen::Presets));
        activate(ui, Control::Library(LibraryControl::Category));
        activate(ui, Control::Choose(index));
    };
    match view {
        "grid" | "params" | "params-tabs" | "params-bypassed" => {}
        "map-menu" => {
            activate(&mut ui, Control::OpenMainMenu);
        }
        view if view.starts_with("custom-") => {
            ui.show(Screen::Gig);
            if view != "custom-gig" {
                activate(&mut ui, Control::Custom(CustomControl::Edit));
                activate(&mut ui, Control::Custom(CustomControl::Select(3)));
            }
            match view {
                "custom-actions" | "custom-picker" => {
                    activate(&mut ui, Control::Custom(CustomControl::Actions));
                }
                "custom-copy" => {
                    activate(&mut ui, Control::Custom(CustomControl::Copy));
                }
                "custom-preset" => {
                    activate(
                        &mut ui,
                        Control::Custom(CustomControl::Scope(LayoutScope::Preset)),
                    );
                }
                _ => {}
            }
            if view == "custom-picker" {
                for row in [1, 4] {
                    activate(&mut ui, Control::Mapping(MappingControl::Pick(row)));
                }
            }
        }
        view if view.starts_with("map-") => {
            activate(&mut ui, Control::OpenMainMenu);
            menu_item(&mut ui, MenuAction::Mappings);
            let map = |ui: &mut UiState, control| {
                activate(ui, Control::Mapping(control));
            };
            match view {
                "map-global" => map(&mut ui, MappingControl::Tab(MappingScope::Global)),
                "map-send" => map(&mut ui, MappingControl::Open(0)),
                "map-detail" | "map-learn" | "map-actions" | "map-block" => {
                    map(&mut ui, MappingControl::Open(2));
                }
                // Stomp B and CC 4 are below the fold: scroll the list, then open the row.
                "map-toggle" | "map-values" => {
                    ui.handle_input(InputEvent::PointerDown { x: 400, y: 440 });
                    ui.handle_input(InputEvent::PointerMove { x: 400, y: 100 });
                    ui.handle_input(InputEvent::PointerUp { x: 400, y: 100 });
                    match view {
                        "map-toggle" => map(&mut ui, MappingControl::Open(7)),
                        _ => {
                            map(&mut ui, MappingControl::Open(8));
                            map(&mut ui, MappingControl::Values);
                        }
                    }
                }
                _ => {}
            }
            if matches!(view, "map-actions" | "map-block") {
                map(&mut ui, MappingControl::Actions);
            }
            if view == "map-block" {
                // BLOCKS is below the fold: scroll the menu, then open Crunch Amp.
                ui.handle_input(InputEvent::PointerDown { x: 600, y: 400 });
                ui.handle_input(InputEvent::PointerMove { x: 600, y: 100 });
                ui.handle_input(InputEvent::PointerUp { x: 600, y: 100 });
                map(&mut ui, MappingControl::Pick(11));
            }
        }
        "remote" => {
            ui.show(Screen::Settings(SettingsPage::Remote));
        }
        "send-report" => {
            ui.show(Screen::Settings(SettingsPage::SendReport));
        }
        view if view.starts_with("ext") => {
            ui.show(Screen::Settings(SettingsPage::Extensions));
            let open = |ui: &mut UiState, index| {
                activate(ui, Control::Extension(ExtensionControl::Open(index)));
            };
            match view {
                "ext-detail" => open(&mut ui, 0),
                "ext-blocks" => open(&mut ui, 4),
                "ext-detail-mid" => {
                    open(&mut ui, 0);
                    ui.handle_input(InputEvent::PointerDown { x: 500, y: 440 });
                    ui.handle_input(InputEvent::PointerMove { x: 500, y: 200 });
                    ui.handle_input(InputEvent::PointerUp { x: 500, y: 200 });
                }
                "ext-fault" => open(&mut ui, 2),
                "ext-detail-end" | "ext-remove" => {
                    open(&mut ui, 0);
                    // REMOVE is at the end of the page: scroll it into view.
                    for _ in 0..3 {
                        ui.handle_input(InputEvent::PointerDown { x: 500, y: 440 });
                        ui.handle_input(InputEvent::PointerMove { x: 500, y: 130 });
                        ui.handle_input(InputEvent::PointerUp { x: 500, y: 130 });
                    }
                    if view == "ext-remove" {
                        activate(&mut ui, Control::Extension(ExtensionControl::Remove));
                    }
                }
                "ext-sources" | "ext-trust" => {
                    activate(&mut ui, Control::Extension(ExtensionControl::Sources));
                }
                _ => {}
            }
        }
        "numeric" => {
            activate(
                &mut ui,
                Control::Param(ParamControl::Value(KnobIndex::new(0)?)),
            );
            for key in [
                NumericKey::Digit(7),
                NumericKey::Point,
                NumericKey::Digit(5),
            ] {
                activate(&mut ui, Control::Numeric(key));
            }
        }
        "picker-pin" => {
            activate(&mut ui, Control::SelectCell(GridPosition::new(0, 4)?));
            activate(
                &mut ui,
                Control::Picker(PickerControl::Group(DeviceGroup::Amp)),
            );
        }
        "device-info" => {
            ui.show(Screen::Settings(SettingsPage::DeviceInformation));
        }
        "licences" | "licence" => {
            ui.show(Screen::Settings(SettingsPage::Licenses));
            if view == "licence" {
                activate(
                    &mut ui,
                    Control::Setting(SettingControl::Licence("binutils")),
                );
            }
        }
        "diagnostics" | "diag-dsp" | "diag-fs" | "diag-usb" => {
            ui.show(Screen::Settings(SettingsPage::Diagnostics));
            let page = match view {
                "diag-dsp" => Some(DiagnosticsPage::Dsp),
                "diag-fs" => Some(DiagnosticsPage::Footswitch),
                "diag-usb" => Some(DiagnosticsPage::Usb),
                _ => None,
            };
            if let Some(page) = page {
                if page == DiagnosticsPage::Footswitch {
                    for pressed in [true, false, true] {
                        ui.handle_input(InputEvent::Footswitch {
                            switch: Footswitch::A,
                            pressed,
                        });
                    }
                }
                activate(&mut ui, Control::Setting(SettingControl::Diagnostics(page)));
            }
        }
        "scenes" => {
            activate(&mut ui, Control::SceneMenu);
        }
        // The scene editor opens from Gig View (EDIT SCENE on the playing scene).
        "draft" => {
            ui.show(Screen::Gig);
            activate(&mut ui, Control::EditScenes);
        }
        "gig" => {
            let mut snapshot = ui.snapshot().clone();
            gig_colours(&mut snapshot);
            ui.set_snapshot(snapshot)?;
            ui.show(Screen::Gig);
        }
        "presets" => {
            activate(&mut ui, Control::Navigate(Screen::Presets));
        }
        "favorites" => category(&mut ui, 0),
        "dir-category" => {
            activate(&mut ui, Control::Navigate(Screen::Presets));
            activate(&mut ui, Control::Library(LibraryControl::Category));
        }
        "dir-captures" => category(&mut ui, 2),
        // Census #4: CLOUD CAPTURES with a page out, an empty folder, and the offline dialog.
        "dir-cloud" | "dir-cloud-empty" | "cloud-offline" => {
            let mut snapshot = ui.snapshot().clone();
            let online = view != "cloud-offline";
            snapshot.wifi.status = match online {
                true => WifiStatus::InternetConnected,
                false => WifiStatus::Idle,
            };
            snapshot
                .wifi
                .networks
                .iter_mut()
                .for_each(|n| n.connected &= online);
            snapshot.account.username = online.then(|| "player@example.com".into());
            let rows = match view {
                "dir-cloud" => (1..=9)
                    .map(|n| CloudItem {
                        id: format!("synthetic-{n}"),
                        name: format!("Synthetic Capture {n}"),
                        author: "player".into(),
                        stored: false,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            snapshot.cloud = vec![CloudFolderView {
                kind: ItemKind::Capture,
                scope: CloudScope::Cloud,
                rows,
                more: true,
                fetching: view == "dir-cloud",
                error: None,
            }]
            .into();
            ui.set_snapshot(snapshot)?;
            category(&mut ui, 2);
            activate(&mut ui, Control::Library(LibraryControl::Cloud));
        }
        "whats-new" => ui.show_update_notes(
            "4.1.1",
            "New Features\n- Synthetic release note one.\n- Synthetic release note two.".into(),
        ),
        "dir-irs" => {
            category(&mut ui, 3);
            activate(
                &mut ui,
                Control::Library(LibraryControl::Folder(MediaFolder::MyIrs)),
            );
        }
        "dir-list" => {
            activate(&mut ui, Control::Navigate(Screen::Presets));
            activate(&mut ui, Control::Library(LibraryControl::Sort));
            activate(&mut ui, Control::Choose(1));
        }
        "dir-multi" => {
            activate(&mut ui, Control::Navigate(Screen::Presets));
            activate(&mut ui, Control::Library(LibraryControl::MultiSelect));
            for id in [1, 2] {
                activate(&mut ui, Control::Library(LibraryControl::Check(id)));
            }
        }
        // Preset ⋮, and its Copy page with an occupied slot of another setlist tapped.
        "preset-menu" | "preset-copy" | "preset-copy-conflict" => {
            let mut snapshot = ui.snapshot().clone();
            let titles = snapshot.presets.iter().map(|preset| preset.title.clone());
            let presets = (0..12).zip(titles).map(|(slot, title)| DirectoryPreset {
                setlist: if slot < 8 { 0 } else { 1 },
                slot: slot % 8 * 2,
                title,
            });
            std::sync::Arc::make_mut(&mut snapshot.directory).presets = presets.collect();
            ui.set_snapshot(snapshot)?;
            let library = Control::Library;
            activate(&mut ui, Control::Navigate(Screen::Presets));
            activate(&mut ui, Control::PresetMenu(2));
            if view != "preset-menu" {
                activate(&mut ui, library(LibraryControl::Item(ItemAction::Copy)));
                activate(&mut ui, library(LibraryControl::TargetSetlist(1)));
            }
            if view == "preset-copy-conflict" {
                activate(&mut ui, library(LibraryControl::TargetSlot(2)));
            }
        }
        "paste-option" | "paste-bar" => {
            let library = Control::Library;
            activate(&mut ui, Control::Navigate(Screen::Presets));
            activate(&mut ui, library(LibraryControl::MultiSelect));
            for id in [1, 2] {
                activate(&mut ui, library(LibraryControl::Check(id)));
            }
            activate(&mut ui, library(LibraryControl::Bulk(BulkAction::Copy)));
            activate(&mut ui, Control::Back);
            activate(&mut ui, library(LibraryControl::Bulk(BulkAction::Paste)));
            if view == "paste-bar" {
                let chosen = LibraryControl::PasteChoice(PasteChoice::FromChosen);
                activate(&mut ui, library(chosen));
                activate(&mut ui, library(LibraryControl::ConfirmPaste));
                activate(&mut ui, library(LibraryControl::Check(4)));
            }
        }
        // re-r31 §1: a Directory capture tap, then the cells the device fitted.
        "add-to-grid" => {
            let mut snapshot = ui.snapshot().clone();
            snapshot.media = vec![MediaEntry {
                folder: MediaFolder::MyCaptures,
                name: "Synthetic Plexi".into(),
                value: "cap_plexi".into(),
            }]
            .into();
            ui.set_snapshot(snapshot)?;
            category(&mut ui, 2);
            activate(&mut ui, Control::Library(LibraryControl::Media(0)));
            let mut snapshot = ui.snapshot().clone();
            let mut cells = [[CellFit::Add; GRID_COLUMNS]; GRID_ROWS];
            for (row, column) in [
                (0, 0),
                (0, 1),
                (0, 2),
                (0, 3),
                (0, 5),
                (0, 6),
                (1, 2),
                (1, 4),
            ] {
                cells[row][column] = CellFit::Replace;
            }
            cells[3][4..].fill(CellFit::NoFit);
            snapshot.add_to_grid = Some(cells);
            ui.set_snapshot(snapshot)?;
        }
        // re-r31 §2: ⋮ Edit Name of a capture opens its save page in edit mode.
        "dir-media-edit" => {
            category(&mut ui, 2);
            activate(&mut ui, Control::Library(LibraryControl::MediaMenu(0)));
            activate(
                &mut ui,
                Control::Library(LibraryControl::Item(ItemAction::EditName)),
            );
        }
        // re-r31 §3: an IR renamed onto another user IR name.
        "dir-ir-exists" => {
            let mut snapshot = ui.snapshot().clone();
            std::sync::Arc::make_mut(&mut snapshot.directory).media = (snapshot.media.iter())
                .filter(|entry| entry.folder == MediaFolder::MyIrs)
                .map(|entry| DirectoryMedia {
                    value: entry.value.clone(),
                    author: String::new(),
                    date: String::new(),
                    read_only: false,
                    folders: Vec::new(),
                    capture: None,
                })
                .collect();
            ui.set_snapshot(snapshot)?;
            category(&mut ui, 3);
            let row = (ui.snapshot().media.iter()).position(|entry| entry.name == "IR15");
            let row = row.ok_or("no IR15")? as u16;
            activate(&mut ui, Control::Library(LibraryControl::MediaMenu(row)));
            activate(
                &mut ui,
                Control::Library(LibraryControl::Item(ItemAction::EditName)),
            );
            ui.handle_input(InputEvent::TextChanged("IR16".into()));
            activate(&mut ui, Control::Key(Key::Done));
        }
        // re-r31 §5: DOWNLOAD of two cloud presets: the setlist pick, then the option.
        // r33-ui: "download-overwrite" picks two occupied slots and taps DOWNLOAD.
        "download-setlist" | "download-option" | "download-overwrite" => {
            let mut snapshot = ui.snapshot().clone();
            snapshot.wifi.status = WifiStatus::InternetConnected;
            snapshot.account.username = Some("player@example.com".into());
            snapshot.cloud = vec![CloudFolderView {
                kind: ItemKind::Preset,
                scope: CloudScope::Cloud,
                rows: (1..=5)
                    .map(|n| CloudItem {
                        id: format!("synthetic-{n}"),
                        name: format!("Synthetic Preset {n}"),
                        author: "player".into(),
                        stored: false,
                    })
                    .collect(),
                more: false,
                fetching: false,
                error: None,
            }]
            .into();
            ui.set_snapshot(snapshot)?;
            activate(&mut ui, Control::Navigate(Screen::Presets));
            activate(&mut ui, Control::Library(LibraryControl::Cloud));
            activate(&mut ui, Control::Library(LibraryControl::MultiSelect));
            for index in [0, 1] {
                activate(&mut ui, Control::Library(LibraryControl::Check(index)));
            }
            activate(
                &mut ui,
                Control::Library(LibraryControl::Bulk(BulkAction::Download)),
            );
            if view != "download-setlist" {
                activate(&mut ui, Control::Library(LibraryControl::Setlist(1)));
                let mut snapshot = ui.snapshot().clone();
                snapshot.active_setlist = Some(1);
                ui.set_snapshot(snapshot)?;
            }
            if view == "download-overwrite" {
                activate(&mut ui, Control::Library(LibraryControl::ConfirmPaste));
                for slot in [2, 3] {
                    activate(&mut ui, Control::Library(LibraryControl::Check(slot)));
                }
                activate(
                    &mut ui,
                    Control::Library(LibraryControl::Bulk(BulkAction::Paste)),
                );
            }
        }
        // Review r31 D2: the upload status while Services uploads.
        "upload-status" => {
            activate(&mut ui, Control::Navigate(Screen::Presets));
            let mut snapshot = ui.snapshot().clone();
            snapshot.uploading = Some("Presets");
            ui.set_snapshot(snapshot)?;
        }
        // re-r38 §2: a batch upload (`0x785dcc`, the ProgressDialog `0x81b74c` with Cancel),
        // then its batch question over it (`0x465178`, OVERWRITE / SKIP).
        "upload-batch" | "upload-batch-ask" => {
            activate(&mut ui, Control::Navigate(Screen::Presets));
            let mut snapshot = ui.snapshot().clone();
            snapshot.upload_progress = Some(UploadProgress {
                noun: "Neural Captures",
                handled: 2,
                total: 5,
            });
            if view == "upload-batch-ask" {
                snapshot.upload_batch = true;
                snapshot.capture_upload_overwrite = Some("Synthetic Plexi".into());
            } else {
                snapshot.uploading = Some("Neural Captures");
            }
            ui.set_snapshot(snapshot)?;
        }
        // Gap I3: Edit Details has the metadata button of the preset save dialog.
        "details" => {
            activate(&mut ui, Control::OpenMainMenu);
            menu_item(&mut ui, MenuAction::EditDetails);
        }
        // Gap C1: Multi Select DOWNLOAD of cloud IRs.
        "dir-cloud-ir-multi" => {
            let mut snapshot = ui.snapshot().clone();
            snapshot.wifi.status = WifiStatus::InternetConnected;
            snapshot.account.username = Some("player@example.com".into());
            snapshot.cloud = vec![CloudFolderView {
                kind: ItemKind::Ir,
                scope: CloudScope::Cloud,
                rows: (1..=6)
                    .map(|n| CloudItem {
                        id: format!("synthetic-ir-{n}"),
                        name: format!("Synthetic Room {n}"),
                        author: "player".into(),
                        stored: n % 3 == 0,
                    })
                    .collect(),
                more: false,
                fetching: false,
                error: None,
            }]
            .into();
            ui.set_snapshot(snapshot)?;
            category(&mut ui, 3);
            activate(&mut ui, Control::Library(LibraryControl::Cloud));
            activate(&mut ui, Control::Library(LibraryControl::MultiSelect));
            for index in [0, 1] {
                activate(&mut ui, Control::Library(LibraryControl::Check(index)));
            }
        }
        "dir-cloud-multi" => {
            let mut snapshot = ui.snapshot().clone();
            snapshot.wifi.status = WifiStatus::InternetConnected;
            snapshot.account.username = Some("player@example.com".into());
            snapshot.cloud = vec![CloudFolderView {
                kind: ItemKind::Capture,
                scope: CloudScope::Downloads,
                rows: (1..=6)
                    .map(|n| CloudItem {
                        id: format!("synthetic-{n}"),
                        name: format!("Synthetic Capture {n}"),
                        author: "player".into(),
                        stored: n % 3 == 0,
                    })
                    .collect(),
                more: false,
                fetching: false,
                error: None,
            }]
            .into();
            ui.set_snapshot(snapshot)?;
            category(&mut ui, 2);
            activate(&mut ui, Control::Library(LibraryControl::Downloads));
            activate(&mut ui, Control::Library(LibraryControl::MultiSelect));
            for index in [0, 1] {
                activate(&mut ui, Control::Library(LibraryControl::Check(index)));
            }
        }
        "dir-filter" => {
            category(&mut ui, 2);
            activate(&mut ui, Control::Library(LibraryControl::Filter));
            activate(&mut ui, Control::Library(LibraryControl::TypeFilter(1)));
            activate(
                &mut ui,
                Control::Library(LibraryControl::Version(CaptureVersion::V2)),
            );
        }
        "dir-folder-menu" => {
            category(&mut ui, 2);
            activate(
                &mut ui,
                Control::Library(LibraryControl::DatabaseFolderMenu(3)),
            );
        }
        "dir-gain" => {
            category(&mut ui, 2);
            activate(&mut ui, Control::Library(LibraryControl::Sort));
            activate(&mut ui, Control::Choose(4));
        }
        "dir-link" | "dir-media-delete" | "dir-media-remove" => {
            category(&mut ui, 2);
            if view == "dir-media-remove" {
                activate(&mut ui, Control::Library(LibraryControl::DatabaseFolder(2)));
            }
            activate(&mut ui, Control::Library(LibraryControl::MultiSelect));
            // Rows of "Brit Crunch 1" and "Brit Crunch 2" (the second is read-only).
            for index in [0, 1] {
                activate(&mut ui, Control::Library(LibraryControl::Check(index)));
            }
            let bulk = if view == "dir-link" {
                BulkAction::Copy
            } else {
                BulkAction::Delete
            };
            activate(&mut ui, Control::Library(LibraryControl::Bulk(bulk)));
        }
        "dir-menu" => {
            activate(&mut ui, Control::Navigate(Screen::Presets));
            activate(&mut ui, Control::PresetMenu(2));
        }
        "dir-search" | "dir-recent" => {
            activate(&mut ui, Control::Navigate(Screen::Presets));
            activate(&mut ui, Control::Library(LibraryControl::Search));
            for key in "cr".chars() {
                activate(&mut ui, Control::Key(Key::Char(key)));
            }
            activate(&mut ui, Control::Key(Key::Done));
            activate(
                &mut ui,
                Control::Library(LibraryControl::Kind(ItemKind::Capture)),
            );
            if view == "dir-recent" {
                activate(&mut ui, Control::Library(LibraryControl::ClearSearch));
                activate(&mut ui, Control::Library(LibraryControl::Search));
                activate(&mut ui, Control::Key(Key::Cancel));
            }
        }
        "saveas" | "savedest" | "preset-metadata" => {
            activate(&mut ui, Control::OpenMainMenu);
            menu_item(&mut ui, MenuAction::SaveAs);
            if view == "savedest" {
                activate(&mut ui, Control::Key(Key::Done));
            }
            if view == "preset-metadata" {
                activate(
                    &mut ui,
                    Control::PresetMetadata(PresetMetadataControl::Open),
                );
                activate(
                    &mut ui,
                    Control::PresetMetadata(PresetMetadataControl::Instrument(
                        PreferredInstrument::Bass,
                    )),
                );
            }
        }
        "copyscene" => {
            ui.show(Screen::Gig);
            activate(
                &mut ui,
                Control::SceneTool {
                    scene: SceneIndex::new(2)?,
                    tool: SceneTool::Copy,
                },
            );
        }
        "gigpreset" => {
            let mut snapshot = ui.snapshot().clone();
            snapshot.footswitch_mode = Some(FootswitchMode::Preset);
            snapshot.preset_id = Some(0);
            snapshot.preset_slot = Some(BankSlot { bank: 0, slot: 0 });
            ui.set_snapshot(snapshot)?;
            ui.show(Screen::Gig);
        }
        "picker" => {
            activate(&mut ui, Control::SelectCell(GridPosition::new(1, 3)?));
            activate(
                &mut ui,
                Control::Picker(PickerControl::Category(ModelCategory::GuitarAmp)),
            );
        }
        "picker-ext" => {
            activate(&mut ui, Control::SelectCell(GridPosition::new(1, 3)?));
            // The Extensions row is after the stock categories: scroll to it.
            for _ in 0..5 {
                ui.handle_input(InputEvent::PointerDown { x: 600, y: 400 });
                ui.handle_input(InputEvent::PointerMove { x: 600, y: 60 });
                ui.handle_input(InputEvent::PointerUp { x: 600, y: 60 });
            }
            activate(
                &mut ui,
                Control::Picker(PickerControl::Group(DeviceGroup::Extensions)),
            );
        }
        "picker-amp" => {
            activate(&mut ui, Control::SelectCell(GridPosition::new(0, 4)?));
            activate(
                &mut ui,
                Control::Picker(PickerControl::Group(DeviceGroup::Amp)),
            );
        }
        "vd-overwrite" | "vd-delete" => {
            activate(&mut ui, Control::SelectCell(GridPosition::new(0, 2)?));
            activate(&mut ui, Control::Picker(PickerControl::Presets(1002)));
            activate(
                &mut ui,
                Control::Picker(PickerControl::PresetTab { user: true }),
            );
            activate(&mut ui, Control::Picker(PickerControl::PresetMenu(6)));
            let item = if view == "vd-delete" {
                DevicePresetMenu::Delete
            } else {
                DevicePresetMenu::Overwrite
            };
            activate(
                &mut ui,
                Control::Picker(PickerControl::PresetMenuItem(item)),
            );
        }
        "vd-presets" | "vd-menu" => {
            activate(&mut ui, Control::SelectCell(GridPosition::new(0, 2)?));
            activate(&mut ui, Control::Picker(PickerControl::Presets(1002)));
            if view == "vd-menu" {
                activate(
                    &mut ui,
                    Control::Picker(PickerControl::PresetTab { user: true }),
                );
                activate(&mut ui, Control::Picker(PickerControl::PresetMenu(6)));
            } else {
                activate(&mut ui, Control::Picker(PickerControl::Preset(1)));
            }
        }
        "expression" => {
            // Editor ⋮ → "Assign expression pedal".
            activate(&mut ui, Control::Param(ParamControl::Menu));
            menu_item(&mut ui, MenuAction::AssignExpression);
        }
        "exp-range" => {
            let mut snapshot = ui.snapshot().clone();
            let parameter = snapshot.parameters[0].index;
            snapshot.expression = vec![ExpressionLink {
                pedal: ExpressionPedal::Exp1,
                target: ExpressionTarget::Parameter(parameter),
            }];
            snapshot.expression_ranges = vec![ExpressionRangeView {
                parameter,
                min: 20,
                max: 85,
            }];
            ui.set_snapshot(snapshot)?;
            activate(&mut ui, Control::Param(ParamControl::Menu));
            menu_item(&mut ui, MenuAction::AssignExpression);
            activate(
                &mut ui,
                Control::Expression(ExpressionControl::Select(parameter)),
            );
        }
        "assign" => {
            activate(&mut ui, Control::Param(ParamControl::Stomp));
        }
        "edit-stomp" => {
            ui.show(Screen::Gig);
            activate(&mut ui, Control::Stomp(StompEdit::Open(0)));
        }
        "notice" | "gate" | "stomp-bar" | "splitter" | "pattern" | "cab" | "cab-missing"
        | "restoring" | "eq" | "geq" | "ir" | "nc" | "nc-missing" | "arp" | "dsp-error"
        | "upload-failed" | "upload-done" => {}
        // The upload overwrite dialog (`0x46485c`) opens when the device asks.
        "upload-overwrite" => {
            let mut asking = ui.snapshot().clone();
            asking.upload_overwrite = Some("Synthetic Lead".into());
            ui.set_snapshot(asking)?;
        }
        // re-r34 §2, §1: "Neural Capture already exists" (`0x465178` modes 1 and 0).
        "capture-overwrite" | "capture-upload-overwrite" => {
            let mut asking = ui.snapshot().clone();
            if view == "capture-overwrite" {
                asking.capture_overwrite = Some(CaptureQuestion {
                    name: "Synthetic Plexi".into(),
                    existing: "My Plexi".into(),
                });
            } else {
                asking.capture_upload_overwrite = Some("Synthetic Plexi".into());
            }
            ui.set_snapshot(asking)?;
        }
        "corrupt" => ui.show_corrupt_captures(vec![
            "Synthetic Plexi Crunch".into(),
            "Synthetic Bass DI".into(),
        ]),
        "cpu-status" => ui.set_global_eq_inhibited(true, true),
        "eq-off" | "eq-gate-off" => {
            ui.set_global_eq_inhibited(true, view == "eq-gate-off");
            activate(&mut ui, Control::GlobalEqStatus);
        }
        "cab-ir" => {
            activate(&mut ui, Control::Editor(EditorControl::IrList(CabMic::A)));
        }
        "arp-tap2" => {
            activate(&mut ui, Control::Editor(EditorControl::Tab(2)));
        }
        "eq-out" => {
            activate(&mut ui, Control::Editor(EditorControl::EqTab(5)));
        }
        "eq-type" => {
            activate(&mut ui, Control::Editor(EditorControl::EqTab(1)));
            activate(&mut ui, Control::Editor(EditorControl::EqType));
        }
        "mini" | "mini-midi" | "multi" | "synth" | "parallax" => {}
        "synth-arp" => {
            activate(&mut ui, Control::Editor(EditorControl::Tab(1)));
        }
        "synth-osc" => {
            activate(&mut ui, Control::Editor(EditorControl::Tab(2)));
        }
        "synth-filter" => {
            activate(&mut ui, Control::Editor(EditorControl::Tab(3)));
        }
        "parallax-expanded" => {
            activate(&mut ui, Control::Editor(EditorControl::Resize));
        }
        "multi-v2" => {
            activate(&mut ui, Control::Editor(EditorControl::Tab(2)));
        }
        "parallax-3" => {
            activate(&mut ui, Control::Param(ParamControl::Page(2)));
        }
        "pattern-2" | "cab-2" | "geq-2" | "ir-2" | "mini-2" | "multi-2" | "parallax-2" => {
            activate(&mut ui, Control::Param(ParamControl::Page(1)));
        }
        "account" => {
            ui.show(Screen::Settings(SettingsPage::MyAccount));
        }
        "media-capture" => {
            activate(
                &mut ui,
                Control::Param(ParamControl::Knob(KnobIndex::new(0)?)),
            );
        }
        "media" => {
            activate(
                &mut ui,
                Control::Param(ParamControl::Knob(KnobIndex::new(0)?)),
            );
        }
        "menu" => {
            activate(&mut ui, Control::OpenMainMenu);
        }
        "settings" => {
            activate(&mut ui, Control::OpenMainMenu);
            menu_item(&mut ui, MenuAction::Settings);
            activate(&mut ui, Control::Setting(SettingControl::Groups));
            activate(&mut ui, Control::Choose(2));
            // Drag the settings menu up so the MIDI entry becomes visible.
            for _ in 0..3 {
                for input in [
                    InputEvent::PointerDown { x: 120, y: 460 },
                    InputEvent::PointerMove { x: 120, y: 120 },
                    InputEvent::PointerUp { x: 120, y: 120 },
                ] {
                    ui.handle_input(input);
                }
            }
            activate(
                &mut ui,
                Control::Setting(SettingControl::Page(SettingsPage::Midi)),
            );
        }
        "wifi" | "updates" | "update-ready" | "update-confirm" | "update-install" | "backups"
        | "storage" | "storage-quarantine" | "groups" | "backup-name" | "backup-menu"
        | "backup-update" | "tetrad-update" | "tetrad-updating" => {
            let mut snapshot = ui.snapshot().clone();
            match view {
                "update-ready" | "update-install" => {
                    snapshot.updater.state = UpdaterState::Idle;
                    snapshot.updater.status = UpdaterStatus::NewVersionDownloaded;
                }
                "update-confirm" => {
                    snapshot.updater.state = UpdaterState::Idle;
                    snapshot.updater.channel = UpdateChannel::Beta;
                }
                "tetrad-update" | "tetrad-updating" => {
                    snapshot.tetrad_update = TetradUpdateView {
                        current_version: "0.1.0-alpha.3".into(),
                        state: if view == "tetrad-update" {
                            TetradUpdateState::Available {
                                version: "0.1.0-alpha.4".into(),
                                usb: false,
                            }
                        } else {
                            TetradUpdateState::Installing {
                                version: "0.1.0-alpha.4".into(),
                                progress: Some(0.4),
                            }
                        },
                        online: true,
                    };
                }
                "storage-quarantine" => {
                    snapshot.quarantine = vec![
                        QuarantineRow {
                            id: "4f1c9a0e".into(),
                            downloads: false,
                            reason: "rawdata does not decode".into(),
                        },
                        QuarantineRow {
                            id: "cloud-2-17".into(),
                            downloads: true,
                            reason: "metadata is not JSON".into(),
                        },
                    ];
                }
                _ => {}
            }
            ui.set_snapshot(snapshot)?;
            activate(&mut ui, Control::OpenMainMenu);
            menu_item(&mut ui, MenuAction::Settings);
            let page = match view {
                "wifi" => SettingsPage::Connection,
                "backups" | "groups" | "backup-name" | "backup-menu" | "backup-update" => {
                    SettingsPage::Backups
                }
                "storage" | "storage-quarantine" => SettingsPage::DeviceStorage,
                "tetrad-update" | "tetrad-updating" => SettingsPage::TetradUpdates,
                _ => SettingsPage::Updates,
            };
            if page.group() == SettingsGroup::System {
                activate(&mut ui, Control::Setting(SettingControl::Groups));
                activate(&mut ui, Control::Choose(1));
            }
            activate(&mut ui, Control::Setting(SettingControl::Page(page)));
            match view {
                "update-confirm" => {
                    activate(&mut ui, Control::Updater(UpdaterCommand::CheckAndDownload));
                }
                "update-install" => {
                    activate(&mut ui, Control::Updater(UpdaterCommand::Install));
                }
                "groups" => {
                    activate(&mut ui, Control::Setting(SettingControl::Groups));
                }
                "backup-name" => {
                    activate(&mut ui, Control::Backup(BackupControl::Create));
                }
                "backup-menu" | "backup-update" => {
                    let id = ui.snapshot().backups.items[0].id;
                    activate(&mut ui, Control::Backup(BackupControl::Item(id)));
                    if view == "backup-update" {
                        menu_item(&mut ui, MenuAction::UpdateBackup);
                    }
                }
                _ => {}
            }
        }
        // Stock 4.1.0: a swipe down on the Grid (no ⋮ row).
        "io" => {
            ui.show(Screen::Io(IoTab::default()));
        }
        "tuner" => {
            ui.show(Screen::Tuner);
        }
        // Stock 4.1.0: a tap on the Looper X block (no ⋮ row).
        "looper" => {
            ui.show(Screen::Looper);
        }
        // Shot 086: an empty Looper X with free record time.
        "looper-empty" => {
            let mut snapshot = ui.snapshot().clone();
            snapshot.looper = LooperView {
                available_seconds: Some(278.0),
                ..LooperView::default()
            };
            ui.set_snapshot(snapshot)?;
            activate(&mut ui, Control::OpenMainMenu);
            menu_item(&mut ui, MenuAction::Looper);
        }
        "tempo" => {
            ui.show(Screen::Tempo);
        }
        "capture" | "capture-tab2" | "capture-save" => {
            let mut snapshot = ui.snapshot().clone();
            for (id, value) in [
                (
                    SettingId::InputType(InputJack::In2),
                    SettingValue::Choice(1),
                ),
                (SettingId::HeadphoneLevel, SettingValue::Number(0)),
                (
                    SettingId::InputGroundLift(InputJack::In2),
                    SettingValue::Bool(true),
                ),
                (
                    SettingId::Phantom(InputJack::In2),
                    SettingValue::Bool(false),
                ),
                (
                    SettingId::OutputGroundLift(OutputJack::Out12),
                    SettingValue::Bool(false),
                ),
            ] {
                snapshot.settings.insert(id, value);
            }
            if view == "capture-save" {
                snapshot.capture.step = CaptureStep::Audition;
            }
            ui.set_snapshot(snapshot)?;
            activate(&mut ui, Control::OpenMainMenu);
            menu_item(&mut ui, MenuAction::NewNeuralCapture);
            match view {
                "capture-tab2" => {
                    activate(
                        &mut ui,
                        Control::CaptureUi(CaptureControl::Tab(CalibrationTab::Grounding)),
                    );
                }
                "capture-save" => {
                    activate(&mut ui, Control::SaveCapture);
                    for ch in "crunch rig".chars() {
                        let key = if ch == ' ' { Key::Space } else { Key::Char(ch) };
                        activate(&mut ui, Control::Key(key));
                    }
                    activate(&mut ui, Control::Key(Key::Done));
                    activate(
                        &mut ui,
                        Control::CaptureUi(CaptureControl::Device(CaptureDevice::Amp)),
                    );
                    activate(
                        &mut ui,
                        Control::CaptureUi(CaptureControl::Instrument(CaptureInstrument::Guitar)),
                    );
                }
                _ => {}
            }
        }
        "keyboard" => {
            activate(&mut ui, Control::OpenMainMenu);
            menu_item(&mut ui, MenuAction::SaveAs);
            activate(&mut ui, Control::Key(Key::Shift));
        }
        "block" => {
            ui.handle_input(InputEvent::Activate(Control::SelectCell(
                GridPosition::new(0, 3)?,
            )));
            let (x, y) = (357, 147);
            ui.handle_input(InputEvent::PointerDown { x, y });
            ui.handle_input(InputEvent::Tick { now_ms: 800 });
            ui.handle_input(InputEvent::PointerUp { x, y });
        }
        "routing" => {
            activate(
                &mut ui,
                Control::Port {
                    row: Row::new(1)?,
                    output: false,
                },
            );
            menu_item(&mut ui, MenuAction::Routing);
        }
        "confirm" => {
            activate(&mut ui, Control::OpenMainMenu);
            menu_item(&mut ui, MenuAction::DeletePreset);
        }
        "choice" => {
            ui.show(Screen::Tempo);
            activate(
                &mut ui,
                Control::Setting(SettingControl::Open(SettingId::TimeSignature)),
            );
        }
        "stomp" => {
            let mut snapshot = ui.snapshot().clone();
            snapshot.footswitch_mode = Some(FootswitchMode::Hybrid);
            gig_colours(&mut snapshot);
            // Live tuner bar: a note 12 cents flat.
            snapshot.tuner.note = Some(Note {
                pitch_class: 9,
                octave: 2,
            });
            snapshot.tuner.cents = -12.0;
            ui.set_snapshot(snapshot)?;
            ui.show(Screen::Gig);
        }
        "diagram" | "process" | "ab" => {
            let mut snapshot = ui.snapshot().clone();
            snapshot.capture.step = match view {
                "diagram" => CaptureStep::ConnectionDiagram,
                "process" => CaptureStep::Training,
                _ => CaptureStep::Audition,
            };
            snapshot.capture.diagram_step = 3;
            snapshot.capture.progress = Some(0.3);
            snapshot.capture.audition = AuditionSource::Capture;
            snapshot
                .settings
                .insert(SettingId::HeadphoneLevel, SettingValue::Number(0));
            ui.set_snapshot(snapshot)?;
            activate(&mut ui, Control::OpenMainMenu);
            menu_item(&mut ui, MenuAction::NewNeuralCapture);
        }
        // The NC2 dialog over the Grid: in progress, lost connection, A/B test.
        "nc2" | "nc2-lost" | "nc2-ab" | "nc2-saved" => {
            let mut snapshot = ui.snapshot().clone();
            snapshot.capture.nc2 = Some(match view {
                "nc2" => Nc2Dialog::InProgress,
                "nc2-lost" => Nc2Dialog::LostConnection,
                "nc2-saved" => Nc2Dialog::Saved,
                _ => Nc2Dialog::Ready,
            });
            snapshot.capture.audition = AuditionSource::Capture;
            snapshot
                .settings
                .insert(SettingId::HeadphoneLevel, SettingValue::Number(0));
            ui.set_snapshot(snapshot)?;
        }
        "power" | "locked" => {
            ui.handle_input(InputEvent::PowerButton { pressed: true });
            if view == "locked" {
                activate(&mut ui, Control::Power(PowerControl::Lock));
            }
        }
        "volume" => {
            let mut snapshot = ui.snapshot().clone();
            for (id, on) in [
                (SettingId::MasterVolumeOut12, true),
                (SettingId::MasterVolumeOut34, false),
                (SettingId::MasterVolumeSend12, false),
                (SettingId::MasterVolumeHeadphones, true),
            ] {
                snapshot.settings.insert(id, SettingValue::Bool(on));
            }
            ui.set_snapshot(snapshot)?;
            ui.handle_input(InputEvent::MasterVolume { percent: 75 });
        }
        "modes" | "modes-hybrid" | "modes-add" => {
            let mut snapshot = ui.snapshot().clone();
            snapshot.allowed_modes = match view {
                "modes" => vec![
                    FootswitchMode::Preset,
                    FootswitchMode::Scene,
                    FootswitchMode::Stomp,
                ],
                "modes-hybrid" => vec![FootswitchMode::Preset, FootswitchMode::Hybrid],
                _ => vec![FootswitchMode::Scene],
            };
            snapshot.hybrid = HybridRows {
                upper: FootswitchMode::Scene,
                lower: FootswitchMode::Stomp,
            };
            ui.set_snapshot(snapshot)?;
            activate(&mut ui, Control::OpenMainMenu);
            menu_item(&mut ui, MenuAction::ModesConfiguration);
            if view == "modes-add" {
                activate(&mut ui, Control::Mode(ModeControl::Add));
            }
        }
        "brightness" | "bypass" | "scene-bypass" | "factory" | "about" | "appearance" => {
            let page = match view {
                "brightness" => SettingsPage::Brightness,
                "appearance" => SettingsPage::Appearance,
                "bypass" => SettingsPage::GlobalBypass,
                "scene-bypass" => SettingsPage::SceneBypass,
                "factory" => SettingsPage::FactoryReset,
                _ => SettingsPage::AboutContact,
            };
            ui.show(Screen::Settings(page));
        }
        "recovery" | "resetting" => {
            ui.show(Screen::Recovery);
            if view == "resetting" {
                activate(&mut ui, Control::Action(DeviceAction::FactoryReset));
                activate(&mut ui, Control::Dialog(DialogButton::Primary));
            }
        }
        "io-out" | "io-usb" | "io-ret" => {
            ui.show(Screen::Io(match view {
                "io-out" => IoTab::Out12,
                "io-usb" => IoTab::Usb,
                _ => IoTab::Return,
            }));
        }
        "io-hp" => {
            let mut snapshot = ui.snapshot().clone();
            snapshot.rows[2].output = OutputPort::Send1;
            snapshot.rows[3].output = OutputPort::Xlr1And2;
            snapshot
                .settings
                .insert(SettingId::HeadphoneLevel, SettingValue::Number(0));
            for (feed, tenths) in [
                (HpFeed::MultiOut, 60),
                (HpFeed::Out12, 60),
                (HpFeed::Send1, -35),
            ] {
                snapshot
                    .settings
                    .insert(SettingId::HeadphoneFeed(feed), SettingValue::Number(tenths));
            }
            snapshot.io_meters.headphones = [-12.0, -14.5];
            snapshot.io_meters.hp_feed = [-9.0, -11.0, -40.0, -40.0, -20.0, -40.0];
            ui.set_snapshot(snapshot)?;
            ui.show(Screen::Io(IoTab::Headphones));
        }
        "global-eq" => {
            ui.show(Screen::Io(IoTab::Input1));
            activate(
                &mut ui,
                Control::Navigate(Screen::GlobalEq(EqTab::Band(EqBand::new(0).expect("band")))),
            );
        }
        "eq-presets" | "eq-preset-menu" => {
            ui.show(Screen::Io(IoTab::Input1));
            activate(
                &mut ui,
                Control::Navigate(Screen::GlobalEq(EqTab::Band(EqBand::new(0).expect("band")))),
            );
            let open = SettingsPresetControl::Open(SettingsPresetPage::GlobalEq);
            activate(&mut ui, Control::SettingsPreset(open));
            if view == "eq-preset-menu" {
                activate(
                    &mut ui,
                    Control::SettingsPreset(SettingsPresetControl::Menu(3)),
                );
            }
        }
        "io-presets" => {
            ui.show(Screen::Io(IoTab::Input1));
            let open = SettingsPresetControl::Open(SettingsPresetPage::Io);
            activate(&mut ui, Control::SettingsPreset(open));
        }
        "midi-out" | "midi-edit" => {
            ui.show(Screen::MidiOut);
            if view == "midi-edit" {
                activate(
                    &mut ui,
                    Control::MidiOut(MidiOutControl::Target(MidiOutTarget::OnLoad)),
                );
            }
        }
        _ => return Err(usage()),
    }
    Ok(ui.render())
}

/// Pattern Tremolo (7040) and a mono cab at row 1, column 3, with their parameter lists.
fn model_editor(snapshot: &mut UiSnapshot, view: &str) -> Result<(), UiError> {
    let position = GridPosition::new(1, 3)?;
    let index = usize::from(position.row()) * GRID_COLUMNS + usize::from(position.column());
    let continuous = |index: u16, label: &str, value: &str, position: f32| ParameterView {
        index,
        label: label.into(),
        value: value.into(),
        kind: ParameterKind::Continuous { position },
        entry: None,
        scene: false,
    };
    let toggle = |index: u16, label: &str, on: bool| ParameterView {
        index,
        label: label.into(),
        value: if on { "On" } else { "Off" }.into(),
        kind: ParameterKind::Toggle { on },
        entry: None,
        scene: false,
    };
    let numeric =
        |index: u16, label: &str, value: f32, text: String, min: f32, max: f32| ParameterView {
            entry: Some(NumericRange {
                value,
                min,
                max,
                integer: false,
            }),
            ..continuous(index, label, &text, (value - min) / (max - min))
        };
    let mut block = |label: &str, category: ModelCategory, model: u32| {
        snapshot.grid[index].label = label.into();
        snapshot.blocks[index] = BlockInfo {
            category: Some(category),
            model_id: Some(model),
            plugin: None,
        };
    };
    if view.starts_with("eq") {
        // Shot 009 values: band 1 HI PASS 50 Hz, Q 0.10.
        block("Output Equalizer", ModelCategory::Eq, 4004);
        let bands = [
            (0.25, 0.0, 50.0, 0.1),
            (0.0, 4.0, 160.0, 0.5),
            (0.0, -4.0, 900.0, 2.0),
            (0.75, 2.0, 3_500.0, 0.7),
            (0.75, 1.0, 4_500.0, 0.7),
        ];
        let mut parameters = Vec::new();
        for (band, &(kind, gain, hz, q)) in bands.iter().enumerate() {
            let base = 5 * band as u16;
            let n = band + 1;
            let log = |hz: f32| (hz / 20.0).log10() / 3.0;
            parameters.extend([
                numeric(
                    base,
                    &format!("{n} GAIN"),
                    gain,
                    format!("{gain:.1} dB"),
                    -12.0,
                    12.0,
                ),
                ParameterView {
                    kind: ParameterKind::Continuous { position: log(hz) },
                    ..numeric(
                        base + 1,
                        &format!("{n} FREQ"),
                        hz,
                        format!("{hz:.0} Hz"),
                        20.0,
                        20_000.0,
                    )
                },
                numeric(base + 2, &format!("{n} Q"), q, format!("{q:.2}"), 0.1, 10.0),
                continuous(base + 3, &format!("{n} TYPE"), "", kind),
                toggle(base + 4, &format!("{n} BYPASS"), true),
            ]);
        }
        parameters.push(numeric(25, "OUTPUT", 0.0, "0.0 dB".into(), -20.0, 20.0));
        snapshot.parameters = parameters;
    } else if view.starts_with("geq") {
        block("Graphic-9", ModelCategory::Eq, 4005);
        let gains = [3.0, 1.5, 0.0, -2.0, -4.0, 0.0, 2.5, 4.0, 6.0];
        let names = [
            "65Hz", "125Hz", "250Hz", "500Hz", "1kHz", "2kHz", "4kHz", "8kHz", "16kHz",
        ];
        let mut parameters = vec![numeric(0, "HPF", 80.0, "80 Hz".into(), 19.0, 500.0)];
        for (band, (gain, name)) in gains.into_iter().zip(names).enumerate() {
            parameters.push(numeric(
                1 + band as u16,
                name,
                gain,
                format!("{gain:.1} dB"),
                -12.0,
                12.0,
            ));
        }
        parameters.extend([
            numeric(10, "LPF", 9_000.0, "9000 Hz".into(), 1_000.0, 20_001.0),
            numeric(12, "OUTPUT", 0.0, "0.0 dB".into(), -20.0, 20.0),
        ]);
        snapshot.parameters = parameters;
    } else if view.starts_with("ir") {
        block("Dual (M)", ModelCategory::IrLoader, 29003);
        let mut parameters = Vec::new();
        for (base, name) in [(0u16, "Synthetic Room IR"), (8, "")] {
            parameters.extend([
                toggle(base, "MUTE", base == 8),
                toggle(base + 1, "INVERT", base == 0),
                ParameterView {
                    kind: ParameterKind::Media(MediaKind::Ir),
                    ..continuous(base + 2, "IR PATH", name, 0.0)
                },
                numeric(base + 3, "LEVEL", 0.0, "0.0 dB".into(), -96.0, 12.0),
                numeric(base + 4, "HI PASS", 20.0, "20 Hz".into(), 20.0, 800.0),
                numeric(
                    base + 5,
                    "LOW PASS",
                    20_000.0,
                    "20000 Hz".into(),
                    1_000.0,
                    20_000.0,
                ),
                ParameterView {
                    kind: ParameterKind::Centred { position: 0.5 },
                    ..continuous(base + 6, "PAN", "C", 0.5)
                },
                numeric(base + 7, "DELAY", 0.0, "0.0 ms".into(), 0.0, 25.0),
            ]);
        }
        parameters.extend([
            numeric(16, "ROOM MIX", 20.0, "20 %".into(), 0.0, 100.0),
            numeric(17, "PRE DELAY", 5.0, "5 ms".into(), 1.0, 100.0),
            numeric(18, "REV HI PASS", 80.0, "80 Hz".into(), 20.0, 800.0),
            numeric(
                19,
                "REV LOW PASS",
                6_000.0,
                "6000 Hz".into(),
                800.0,
                20_000.0,
            ),
            ParameterView {
                kind: ParameterKind::Choice {
                    position: 0.0,
                    steps: 0,
                },
                ..continuous(20, "SIZE", "Medium", 0.5)
            },
            numeric(21, "GLOBAL OUTPUT", 0.0, "0.0 dB".into(), -96.0, 12.0),
        ]);
        snapshot.parameters = parameters;
    } else if view.starts_with("nc") {
        block("Capture 2", ModelCategory::NeuralCapture, 14001);
        let mut parameters: Vec<ParameterView> = ["GAIN", "BASS", "MID", "TREBLE", "VOLUME"]
            .into_iter()
            .enumerate()
            .map(|(index, label)| numeric(index as u16, label, 0.0, "0.0 dB".into(), -6.0, 6.0))
            .collect();
        parameters.push(ParameterView {
            kind: ParameterKind::Media(MediaKind::Capture),
            ..continuous(5, "file_name", "Synthetic Plexi Crunch", 0.0)
        });
        snapshot.parameters = parameters;
        let known = if view == "nc-missing" {
            "Other Capture"
        } else {
            "Synthetic Plexi Crunch"
        };
        snapshot.media = vec![MediaEntry {
            folder: MediaFolder::MyCaptures,
            name: known.into(),
            value: known.into(),
        }]
        .into();
    } else if ["mini", "multi", "synth", "parallax"]
        .iter()
        .any(|p| view.starts_with(p))
    {
        // Synthetic values: every parameter a knob "P n" at a spread position; the switches
        // the editors read are toggles (on).
        let (label, category, model, toggles): (&str, ModelCategory, u32, &[u16]) = match &view[..4]
        {
            "mini" => ("Minivoicer", ModelCategory::Pitch, 18007, &[5, 9]),
            "mult" => (
                "Multivoicer",
                ModelCategory::Pitch,
                18012,
                &[5, 11, 17, 23, 48],
            ),
            "synt" => (
                "Overlord Synth",
                ModelCategory::Synth,
                30001,
                &[9, 14, 23, 38, 41, 43, 46],
            ),
            _ => (
                "Parallax",
                ModelCategory::GuitarOverdrive,
                3008,
                &[34, 35, 36, 22],
            ),
        };
        block(label, category, model);
        snapshot.parameters = (0..56u16)
            .map(|index| {
                let position = ((index * 37) % 100) as f32 / 100.0;
                if toggles.contains(&index) {
                    toggle(index, &format!("P {index}"), true)
                } else {
                    continuous(
                        index,
                        &format!("P {index}"),
                        &format!("{:.0}", position * 100.0),
                        position,
                    )
                }
            })
            .collect();
        if view == "mini-midi" {
            // MIDI On (15) and V1 NOTE DISPLAY 60 (C5); V2 has no note (−1).
            snapshot.parameters[15] = toggle(15, "MIDI", true);
            snapshot.parameters[23] = continuous(23, "V1 NOTE DISPLAY", "60", 61.0 / 128.0);
            snapshot.parameters[24] = continuous(24, "V2 NOTE DISPLAY", "-1", 0.0);
        }
    } else if view.starts_with("arp") {
        // Arpeggio Delay (6031) with ModelRepo names; tap 3 off, SYNC off, SCALE Major.
        block("Arpeggio Delay", ModelCategory::Delay, 6031);
        let choice = |index, label: &str, text: &str, position| ParameterView {
            kind: ParameterKind::Choice { position, steps: 5 },
            ..continuous(index, label, text, position)
        };
        let mut parameters = vec![
            continuous(0, "MIX", "80 %", 0.8),
            toggle(1, "SYNC", false),
            choice(2, "KEY", "None", 0.0),
            choice(3, "SCALE", "Major", 0.25),
            continuous(4, "GLISSANDO", "25 ms", 0.05),
        ];
        for (tap, interval, time, on) in [
            (0u16, 14, 0.25, true),
            (1, 19, 0.38, true),
            (2, 24, 0.5, false),
        ] {
            let base = 5 + 7 * tap;
            let n = tap + 1;
            parameters.extend([
                toggle(base, &format!("TAP {n} ACTIVE"), on),
                choice(
                    base + 1,
                    &format!("TAP {n} SEMITONES"),
                    "+2",
                    interval as f32 / 24.0,
                ),
                choice(
                    base + 2,
                    &format!("TAP {n} INTERVAL"),
                    "3rd",
                    interval as f32 / 24.0,
                ),
                continuous(base + 3, &format!("TAP {n} DELAY TIME"), "500 ms", time),
                choice(base + 4, &format!("TAP {n} SYNC NOTE"), "1/8", 0.5),
                continuous(base + 5, &format!("TAP {n} LEVEL"), "0.0 dB", 1.0),
                continuous(base + 6, &format!("TAP {n} PAN"), "C", 0.5),
            ]);
        }
        parameters.extend([
            continuous(26, "FEEDBACK", "50 %", 0.53),
            continuous(27, "FEEDBACK DELAY TIME", "1500 ms", 0.75),
            continuous(29, "HIGH PASS", "120 Hz", 0.13),
            continuous(30, "LOW PASS", "3000 Hz", 0.1),
            toggle(31, "TRAILS", true),
        ]);
        snapshot.parameters = parameters;
    } else if view.starts_with("pattern") {
        snapshot.grid[index].label = "Pattern Tremolo".into();
        snapshot.blocks[index] = BlockInfo {
            category: Some(ModelCategory::Modulation),
            model_id: Some(7040),
            plugin: None,
        };
        let mut parameters = vec![
            toggle(0, "SYNC", false),
            continuous(1, "RATE", "8.00 Hz", 0.55),
            ParameterView {
                kind: ParameterKind::Choice {
                    position: 0.0,
                    steps: 0,
                },
                ..continuous(2, "SYNC NOTE", "1/8", 0.0)
            },
            continuous(3, "SWING", "10 %", 0.2),
            continuous(4, "DEPTH", "100 %", 1.0),
            continuous(5, "SMOOTH", "30 %", 0.3),
            toggle(6, "PING PONG", false),
            toggle(7, "RETRIGGER", true),
            continuous(8, "OUTPUT", "0.0 dB", 0.77),
            continuous(10, "STEPS", "12", 11.0 / 15.0),
        ];
        let values = [20, 15, 20, 10, 20, 5, 20, 3, 12, 12, 12, 0, 20, 0, 20, 0];
        parameters.extend(
            values
                .iter()
                .enumerate()
                .map(|(step, &value)| ParameterView {
                    scene: step == 4,
                    ..continuous(
                        11 + step as u16,
                        &format!("STEP {}", step + 1),
                        &value.to_string(),
                        value as f32 / 20.0,
                    )
                }),
        );
        snapshot.parameters = parameters;
        // Steps 9..=11 form one connected run.
        snapshot.step_connected[8] = true;
        snapshot.step_connected[9] = true;
    } else {
        snapshot.grid[index].label = "412 Brit 8000 G12M".into();
        snapshot.blocks[index] = BlockInfo {
            category: Some(ModelCategory::GuitarCabMono),
            model_id: Some(12004),
            plugin: None,
        };
        let mut parameters = Vec::new();
        for (base, mic, position, distance, bypassed) in
            [(0u16, "A", 0.4, 0.2, false), (8, "B", 0.7, 0.6, true)]
        {
            parameters.extend([
                toggle(base, "bypass", bypassed),
                ParameterView {
                    kind: ParameterKind::Media(MediaKind::Ir),
                    ..continuous(
                        base + 1,
                        "ir selector",
                        &format!("Synthetic Cab 1 Mic {mic}"),
                        0.0,
                    )
                },
                continuous(base + 2, "LEVEL", "0.0 dB", 0.8),
                ParameterView {
                    kind: ParameterKind::Centred { position: 0.5 },
                    ..continuous(base + 3, "PAN", "C", 0.5)
                },
                continuous(
                    base + 4,
                    "DISTANCE",
                    &format!("{:.1}", distance * 10.0),
                    distance,
                ),
                continuous(
                    base + 5,
                    "POSITION",
                    &format!("{:.1}", position * 10.0),
                    position,
                ),
                toggle(base + 6, "phi", base == 8),
            ]);
        }
        parameters.extend([
            continuous(16, "HPF", "OFF", 0.0),
            continuous(17, "LPF", "OFF", 1.0),
            continuous(18, "OUTPUT VOLUME", "0.0 dB", 0.9),
        ]);
        snapshot.parameters = parameters;
    }
    snapshot.selected_block = Some(position);
    Ok(())
}

/// Synthetic Mappings page data on the fixture's blocks: nine preset rows of every trigger type
/// and mode, four global rows and the grid's blocks with their parameters.
fn mappings_fixture() -> Result<MappingsView, UiError> {
    let position = GridPosition::new;
    let label = |kind| match kind {
        ModeKind::OnPress => "On press",
        ModeKind::Follow => "Follow",
        ModeKind::Relative => "Relative",
        ModeKind::Set => "Set",
        ModeKind::Toggle => "Toggle",
        ModeKind::Momentary => "Momentary",
        ModeKind::Step => "Step",
    };
    let mode = |kind, detail: &str| ModeView {
        kind,
        label: label(kind).into(),
        detail: detail.into(),
    };
    let modes = |kinds: &[ModeKind]| kinds.iter().map(|kind| mode(*kind, "")).collect();
    let field = |id, label: &str, text: &str| ValueField {
        id,
        label: label.into(),
        text: text.into(),
    };
    let window = |low: &str, high: &str| {
        [
            field(FieldId::Low, "Low", low),
            field(FieldId::High, "High", high),
        ]
    };
    let row = |id, trigger, action, labels: [&str; 4], mode| MappingRow {
        id,
        trigger,
        action: Some(action),
        trigger_label: labels[0].into(),
        trigger_detail: labels[1].into(),
        action_label: labels[2].into(),
        action_detail: labels[3].into(),
        mode: Some(mode),
        modes: Vec::new(),
        fields: Vec::new(),
        summary: String::new(),
        response: None,
    };
    let switch = [ModeKind::Set, ModeKind::Toggle, ModeKind::Momentary];
    let cc_switch = [ModeKind::OnPress, ModeKind::Follow, ModeKind::Toggle];
    let parameter = [
        ModeKind::Set,
        ModeKind::Toggle,
        ModeKind::Momentary,
        ModeKind::Step,
    ];
    let cc_parameter = [
        ModeKind::Follow,
        ModeKind::Relative,
        ModeKind::Set,
        ModeKind::Toggle,
    ];
    let block = |position, target| MappingAction::Block { position, target };
    let system = MappingAction::System;
    let mapper = "tetrad-dev/extensions:midi-mapper";
    let extension = |source: &str| Trigger::Extension {
        key: mapper.into(),
        trigger: "midi".into(),
        source: source.into(),
    };
    let preset = vec![
        row(
            MappingId::MidiOut(MidiOutTarget::OnLoad),
            Trigger::PresetLoaded,
            MappingAction::SendMidi,
            [
                "Preset loaded",
                "Each time this preset loads",
                "Send MIDI",
                "PC 12 · Ch 1",
            ],
            mode(ModeKind::OnPress, "Sends the messages on each trigger"),
        ),
        MappingRow {
            modes: modes(&switch),
            ..row(
                MappingId::Stomp {
                    stomp: 0,
                    position: position(0, 1)?,
                },
                Trigger::Stomp(0),
                block(position(0, 1)?, BlockTarget::Bypass),
                ["Stomp A", "Footswitch press", "Green Drive", "Bypass"],
                mode(ModeKind::Toggle, "Each press engages or bypasses"),
            )
        },
        MappingRow {
            modes: modes(&parameter),
            fields: vec![field(FieldId::Value, "Value", "7.5")],
            summary: "7.5".into(),
            ..row(
                MappingId::Tetrad(0),
                Trigger::Midi(MidiInput::Note(36)),
                block(position(0, 2)?, BlockTarget::Parameter(0)),
                ["Note 36", "MIDI · QC channel", "Crunch Amp", "Gain"],
                mode(ModeKind::Set, "Each press sets the value"),
            )
        },
        MappingRow {
            modes: modes(&switch),
            ..row(
                MappingId::Tetrad(1),
                Trigger::Stomp(7),
                system(SystemAction::Tuner),
                ["Stomp H", "Footswitch press", "Tuner", "Open and close"],
                mode(ModeKind::Toggle, "Each press opens or closes"),
            )
        },
        MappingRow {
            fields: vec![
                field(FieldId::Min, "Min", "0 %"),
                field(FieldId::Max, "Max", "100 %"),
                field(FieldId::Pickup, "Pickup", "Jump"),
            ],
            summary: "0–100 % · Jump".into(),
            ..row(
                MappingId::Expression {
                    pedal: 1,
                    position: position(0, 5)?,
                    target: BlockTarget::Parameter(2),
                },
                Trigger::Expression(1),
                block(position(0, 5)?, BlockTarget::Parameter(2)),
                ["EXP 1", "Expression pedal", "Tape Echo", "Mix"],
                mode(ModeKind::Follow, "The value follows the pedal"),
            )
        },
        MappingRow {
            modes: modes(&cc_parameter),
            fields: vec![field(FieldId::Encoding, "Encoding", "Offset 64")],
            summary: "Offset 64".into(),
            ..row(
                MappingId::Tetrad(2),
                extension("cc:21"),
                block(position(0, 0)?, BlockTarget::Parameter(0)),
                ["CC 21", "MIDI Mapper · Ch 3", "Gate", "Threshold"],
                mode(ModeKind::Relative, "An endless encoder steps the value"),
            )
        },
        MappingRow {
            fields: window("CC 64", "CC 127").into(),
            summary: "CC 64–127".into(),
            ..row(
                MappingId::Tetrad(3),
                Trigger::Midi(MidiInput::Cc(22)),
                system(SystemAction::Scene(2)),
                ["CC 22", "MIDI · QC channel", "Scene", "Scene C"],
                mode(ModeKind::OnPress, "Runs when the CC enters its window"),
            )
        },
        MappingRow {
            modes: modes(&parameter),
            fields: vec![
                field(FieldId::A, "A", "25 %"),
                field(FieldId::B, "B", "60 %"),
            ],
            summary: "25 % ↔ 60 %".into(),
            ..row(
                MappingId::Tetrad(4),
                Trigger::Stomp(1),
                block(position(0, 6)?, BlockTarget::Parameter(1)),
                ["Stomp B", "Footswitch press", "Plate Verb", "Mix"],
                mode(ModeKind::Toggle, "Each press swaps A and B"),
            )
        },
        MappingRow {
            modes: modes(&cc_parameter),
            fields: [
                window("CC 10", "CC 110").as_slice(),
                &[
                    field(FieldId::Min, "Min", "2.0"),
                    field(FieldId::Max, "Max", "8.0"),
                    field(FieldId::Pickup, "Pickup", "Pickup"),
                ],
            ]
            .concat(),
            summary: "2.0–8.0 · CC 10–110 · Pickup".into(),
            response: Some(ResponseView {
                low: 10,
                high: 110,
                line: Some((200, 800)),
                level: Some(30),
                current: Some(560),
                waiting: true,
            }),
            ..row(
                MappingId::Tetrad(5),
                Trigger::Midi(MidiInput::Cc(4)),
                block(position(0, 1)?, BlockTarget::Parameter(0)),
                ["CC 4", "MIDI · QC channel", "Green Drive", "Drive"],
                mode(ModeKind::Follow, "Low–High maps to Min–Max"),
            )
        },
    ];
    let global = vec![
        MappingRow {
            modes: modes(&cc_switch),
            fields: window("CC 64", "CC 127").into(),
            summary: "CC 64–127".into(),
            ..row(
                MappingId::Tetrad(0),
                Trigger::Midi(MidiInput::Cc(20)),
                system(SystemAction::Tuner),
                ["CC 20", "MIDI · QC channel", "Tuner", "Open and close"],
                mode(ModeKind::Follow, "Open while the CC is in its window"),
            )
        },
        row(
            MappingId::Tetrad(1),
            Trigger::Midi(MidiInput::Note(36)),
            system(SystemAction::TapTempo),
            ["Note 36", "MIDI · QC channel", "Tempo", "Tap tempo"],
            mode(ModeKind::OnPress, "Runs on each press"),
        ),
        MappingRow {
            fields: window("CC 64", "CC 127").into(),
            summary: "CC 64–127".into(),
            ..row(
                MappingId::Tetrad(2),
                extension("cc:64"),
                system(SystemAction::Looper(LooperButton::RecordOverdub)),
                ["CC 64", "MIDI Mapper · Ch 3", "Looper", "Rec / Overdub"],
                mode(ModeKind::OnPress, "Runs when the CC enters its window"),
            )
        },
        row(
            MappingId::Tetrad(3),
            extension("note:60"),
            system(SystemAction::NextPreset),
            ["Note 60", "MIDI Mapper · Ch 3", "Preset", "Next preset"],
            mode(ModeKind::OnPress, "Runs on each press"),
        ),
    ];
    let blocks = [
        (
            0,
            0,
            "Gate",
            "Row 1 slot 1",
            &["Threshold", "Release", "Level"][..],
        ),
        (
            0,
            1,
            "Green Drive",
            "Row 1 slot 2",
            &["Drive", "Tone", "Level"],
        ),
        (
            0,
            2,
            "Crunch Amp",
            "Row 1 slot 3",
            &[
                "Gain", "Bass", "Middle", "Treble", "Master", "Presence", "Depth",
            ],
        ),
        (
            0,
            3,
            "Cab 4x12",
            "Row 1 slot 4",
            &["Mic Position", "Distance", "Level"],
        ),
        (
            0,
            5,
            "Tape Echo",
            "Row 1 slot 6",
            &["Time", "Feedback", "Mix"],
        ),
        (0, 6, "Plate Verb", "Row 1 slot 7", &["Decay", "Mix"]),
        (1, 2, "Chorus", "Row 2 slot 3", &["Rate", "Depth", "Mix"]),
        (1, 4, "Octaver", "Row 2 slot 5", &["Octave", "Mix"]),
    ];
    let blocks = (blocks.into_iter())
        .map(|(row, column, name, location, parameters)| {
            Ok(MappingBlock {
                position: position(row, column)?,
                name: name.into(),
                location: location.into(),
                parameters: (parameters.iter().zip(0..))
                    .map(|(name, index)| MappingParameter {
                        index,
                        name: (*name).into(),
                    })
                    .collect(),
            })
        })
        .collect::<Result<_, UiError>>()?;
    Ok(MappingsView {
        preset_name: "Studio Crunch".into(),
        preset,
        global,
        blocks,
        extension_triggers: vec![ExtensionTriggerChoice {
            key: mapper.into(),
            trigger: "midi".into(),
            label: "MIDI Mapper · Ch 3".into(),
        }],
        focus: None,
        learning: None,
        notice: None,
    })
}

/// Synthetic Extensions page data: a running, a starting and a faulted extension, two
/// catalog entries and two repositories.
fn extensions_fixture() -> ExtensionsView {
    let default = "tetrad-dev/extensions";
    let installed = |key: &str, name: &str, state: ExtensionState| InstalledExtension {
        key: format!("{default}:{key}"),
        name: name.into(),
        version: "1.2.0".into(),
        description: String::new(),
        source: default.into(),
        enabled: state != ExtensionState::Disabled,
        state,
        update: None,
        duplicates: Vec::new(),
        commands: Vec::new(),
        settings: Vec::new(),
        status_lines: Vec::new(),
        blocks: Vec::new(),
        log: Vec::new(),
        themes: Vec::new(),
        footswitch_mode: None,
    };
    let setting = |key: &str, label: &str, value| ExtensionSetting {
        key: key.into(),
        label: label.into(),
        value,
    };
    let mapper = InstalledExtension {
        description: "Maps incoming MIDI notes and CCs to scenes, presets and stomps, \
            with one mapping table per setlist."
            .into(),
        update: Some("1.3.0".into()),
        commands: ["Learn next message", "Clear mappings", "Send test note"]
            .into_iter()
            .enumerate()
            .map(|(n, label)| ExtensionButton {
                id: format!("cmd{n}"),
                label: label.into(),
            })
            .collect(),
        settings: vec![
            setting(
                "channel",
                "Input channel",
                ExtensionSettingValue::Choice {
                    options: (1..=16)
                        .map(|n| n.to_string())
                        .chain(["Omni".into()])
                        .collect(),
                    selected: 16,
                },
            ),
            setting(
                "thru",
                "Pass unmapped messages",
                ExtensionSettingValue::Toggle(true),
            ),
            setting(
                "velocity",
                "Minimum velocity",
                ExtensionSettingValue::Number {
                    value: 12,
                    min: 0,
                    max: 127,
                },
            ),
        ],
        status_lines: vec![
            "14 mappings loaded".into(),
            "Last message: CC 64 = 127".into(),
        ],
        blocks: Vec::new(),
        log: (0..14)
            .map(|n| {
                format!(
                    "12:0{}:{:02} mapped CC {} to scene {}",
                    n % 10,
                    n * 4,
                    60 + n,
                    n % 8 + 1
                )
            })
            .collect(),
        ..installed("midi-mapper", "MIDI Mapper", ExtensionState::Running)
    };
    let mut looper = installed(
        "looper-sync",
        "Looper Sync with an extremely long extension name that cannot fit",
        ExtensionState::Starting,
    );
    looper.source = "night-owl/footswitch-tools".into();
    let mut faulted = installed("setlist-clock", "Setlist Clock", ExtensionState::Faulted(
        "TypeError: cannot read properties of undefined (reading 'tempo') at onScene (main.js:42)"
            .into(),
    ));
    faulted.log = vec!["started".into(), "uncaught TypeError in onScene".into()];
    let block = |name: &str, detail: &str, ready| ExtensionBlock {
        name: name.into(),
        detail: detail.into(),
        ready,
    };
    let nam = InstalledExtension {
        description: "Plays Neural Amp Modeler LSTM models (.nam files) as DSP blocks.".into(),
        blocks: vec![
            block("NAM Plexi Crunch", "Guitar Amplifier · 31% DSP", true),
            block("NAM Bassman", "Guitar Amplifier · 44% DSP", true),
            block(
                "NAM Rectifier",
                "Rectifier.nam is a WaveNet model; this version plays LSTM models only",
                false,
            ),
        ],
        ..installed("nam", "Neural Amp Modeler", ExtensionState::Running)
    };
    ExtensionsView {
        installed: vec![
            mapper,
            looper,
            faulted,
            installed("tap-lights", "Tap Lights", ExtensionState::Disabled),
            nam,
        ],
        available: vec![
            AvailableExtension {
                source: default.into(),
                id: "tuner-strobe".into(),
                name: "Strobe Tuner".into(),
                version: "0.4.1".into(),
                description: "A strobe view for the tuner screen with cents history.".into(),
                duplicates: Vec::new(),
            },
            AvailableExtension {
                source: "night-owl/footswitch-tools".into(),
                id: "double-tap".into(),
                name: "Double Tap Actions".into(),
                version: "2.0.0".into(),
                description: "Assign a second action to a double tap on any footswitch, \
                    with per-preset overrides."
                    .into(),
                duplicates: Vec::new(),
            },
        ],
        sources: vec![
            ExtensionSource {
                repository: default.into(),
                default: true,
                enabled: true,
                fingerprint: "a1b2 c3d4 e5f6 0718 293a 4b5c 6d7e 8f90".into(),
                status: "Up to date".into(),
            },
            ExtensionSource {
                repository: "night-owl/footswitch-tools".into(),
                default: false,
                enabled: true,
                fingerprint: "3f2a 9c01 77be 04d2 e1a9 5b6c 0d83 f4e7".into(),
                status: "Refresh failed: HTTP 404 from api.github.com".into(),
            },
        ],
        pending_trust: None,
        status: Some("Installed MIDI Mapper 1.2.0".into()),
        busy: false,
    }
}

/// A Custom footswitch mode with four pages; page 1 holds one action of each kind.
fn custom_fixture() -> Result<CustomModeView, UiError> {
    let slot = |label: &str, detail: &str, colour, engaged, action| CustomSlotView {
        action: Some(action),
        label: label.into(),
        detail: detail.into(),
        colour,
        engaged,
        available: true,
        ..CustomSlotView::default()
    };
    let scene = |index: u8| SlotAction::System(SystemAction::Scene(index));
    let page = |step| SlotAction::System(SystemAction::CustomPage(step));
    let mut first = [
        slot("Scene A", "Scene", [0x45, 0xf8, 0x62], true, scene(0)),
        slot("Scene B", "Scene", [0x0a, 0x74, 0xe0], false, scene(1)),
        slot("Scene C", "Scene", [0xff, 0x70, 0x00], false, scene(2)),
        slot(
            "Delay",
            "Bypass · momentary",
            [0x87, 0xda, 0xff],
            true,
            SlotAction::Block {
                position: GridPosition::new(0, 2)?,
                target: BlockTarget::Bypass,
            },
        ),
        slot(
            "Worship 4B",
            "Load preset",
            [0xff, 0xd2, 0x36],
            false,
            SlotAction::LoadPreset,
        ),
        slot(
            "Synth",
            "PC 12 · Ch 1",
            [0x69, 0x54, 0xff],
            false,
            SlotAction::SendMidi,
        ),
        slot(
            "Rec / Overdub",
            "Looper",
            [0xff, 0x27, 0x27],
            false,
            SlotAction::System(SystemAction::Looper(LooperButton::RecordOverdub)),
        ),
        slot(
            "Swell",
            "Control Macro",
            [0xff, 0x02, 0xc2],
            false,
            SlotAction::Command {
                key: "tetrad-dev/extensions:control-macro".into(),
                command: "swell".into(),
            },
        ),
        slot(
            "‹ Page",
            "Previous page",
            [255; 3],
            false,
            page(PageStep::Previous),
        ),
        slot("Page ›", "Next page", [255; 3], false, page(PageStep::Next)),
    ];
    first[3].momentary = Some(true);
    first[3].custom_label = Some("Delay".into());
    first[3].swatch = Some(4);
    Ok(CustomModeView {
        name: "Custom".into(),
        extension: "Custom Footswitch Mode".into(),
        pages: 4,
        page: 0,
        active: true,
        slots: vec![
            first,
            Default::default(),
            Default::default(),
            Default::default(),
        ],
        preset_slots: None,
        preset: "Studio Crunch".into(),
        commands: vec![CommandChoice {
            key: "tetrad-dev/extensions:control-macro".into(),
            command: "swell".into(),
            label: "Control Macro · Swell".into(),
        }],
        colours: [
            [0x45, 0xf8, 0x62],
            [0x0a, 0x74, 0xe0],
            [0xff, 0x70, 0x00],
            [0xff, 0x02, 0xc2],
            [0x87, 0xda, 0xff],
            [0x69, 0x54, 0xff],
            [0xff, 0xd2, 0x36],
            [0xff, 0x27, 0x27],
        ],
        notice: None,
    })
}
