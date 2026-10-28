//! Preset edits and the stock save over a synthetic catalog and a temp library: what a save
//! stores is what the next load deploys. No stock data.

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use tetrad_app::device::actions::LayoutEdit;
use tetrad_app::device::preset::{self, SaveRequest};
use tetrad_app::device::{Current, PresetRef};
use tetrad_catalog::{Catalog, Specs};
use tetrad_engine::{Column, Row, SceneIndex, Scened, Site, Slot, new_instance};
use tetrad_library::sidecar::Creator;
use tetrad_library::stock::{StockLibrary, StockLimits};
use tetrad_preset::Preset;
use tetrad_storage::media::MediaRoot;

/// Lane controls, tempo, the split path head 10003 and a cab clone whose `ir selector`
/// replaces the base's first parameter (the stock cab shape, d-engine §1).
const MODELS: &str = "<Models>\
<Category id='10' name='Splitter' hidden='true'><Model id='10004' name='Splitter'>\
<Parameter name='TYPE' type='switch' min='0' max='2' defaultValue='0'/></Model>\
<Model id='10003' name='LR Crossover' blob='lr' hidden='true'>\
<Parameter name='FREQ' type='float' min='20' max='20000' defaultValue='20'/></Model></Category>\
<Category id='11' name='Mixer' hidden='true'><Model id='11000' name='Mixer'>\
<Parameter name='LEVEL A' type='float' min='MIN_MIXER_DB' max='MAX_MIXER_DB' defaultValue='0'/>\
</Model></Category>\
<Category id='12' name='Cab'><Model id='12000' name='Base Cab' blob='cab'>\
<Parameter name='A' type='float' min='0' max='1' defaultValue='0'/>\
<Parameter name='B' type='float' min='0' max='1' defaultValue='0.5'/></Model>\
<Model id='12001' name='Clone Cab' clones='12000'>\
<Parameter name='ir selector' type='string' defaultValue='IR' replaces='0'/></Model></Category>\
<Category id='23' name='Lane Output' hidden='true'><Model id='23000' name='Lane Output'>\
<Parameter name='VOLUME' type='float' min='0' max='1' defaultValue='0.76923'/>\
<Parameter name='PAN' type='float' min='0' max='1' defaultValue='0.5'/></Model></Category>\
<Category id='25' name='Tempo' hidden='true'><Model id='25000' name='Tempo'>\
<Parameter name='TEMPO' type='float' min='40' max='240' defaultValue='120'/></Model></Category>\
<Category id='28' name='Input Gate' hidden='true'><Model id='28000' name='Input Gate'>\
<Parameter name='THRESHOLD' type='float' min='-96' max='0' defaultValue='-60'/></Model></Category>\
</Models>";

const REPORT: &str = "<ModuleReport>\
<Kernel core='0' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='1' cpu='2' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='2' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Kernel core='3' cpu='0' dm_heap='0' pm_heap='0' sd_heap='0'/>\
<Module id='12000' name='Base Cab' blob_name='cab' category='12' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='16'>\
<Test name='Stimulus High' avg='10'/></Module>\
<Module id='10003' name='LR Crossover' blob_name='lr' category='10' dm_heap='100' pm_heap='10' \
sd_heap='0' dma='0' dm='10' pm='0' sw='100' nw='0' Latency='0'>\
<Test name='Stimulus High' avg='1'/></Module>\
</ModuleReport>";

const IR: &str = "T1_Cab_Mic";

/// A temp `/media/p4` with one setlist, removed on drop.
struct Root(PathBuf);

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn library(test: &str) -> (Root, StockLibrary) {
    let root =
        std::env::temp_dir().join(format!("tetrad-app-preset-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("Presets")).unwrap();
    let mut library = StockLibrary::open(MediaRoot::new(&root), StockLimits::default()).unwrap();
    library.create_setlist("Live").unwrap();
    (Root(root), library)
}

fn specs() -> Specs {
    Specs::resolve(&Catalog::parse(MODELS, REPORT).unwrap()).unwrap()
}

/// The new-preset shape of an empty slot, deployed.
fn new_current(library: &StockLibrary, specs: &Specs) -> Current {
    let read = preset::read(library, None, specs).unwrap();
    let prepared = preset::prepare(read, specs, false, None, Default::default()).unwrap();
    Current {
        path: None,
        preset: prepared.preset,
        migration: prepared.migration,
        deployment: prepared.deployment,
    }
}

fn save(
    library: &mut StockLibrary,
    current: &mut Current,
    specs: &Specs,
    index: u32,
    name: &str,
    unix_seconds: u64,
) -> preset::SavedPreset {
    let request = SaveRequest {
        slot: &PresetRef {
            setlist: None,
            index,
        },
        name,
        now: SystemTime::UNIX_EPOCH + Duration::from_secs(unix_seconds),
        account_name: "",
        account_id: "",
        creator: Creator::Quad,
    };
    preset::save(library, current, specs, &request).unwrap()
}

/// Review w2c-io R1: a grid edit stores string parameters as text and keeps MUTE, so a cab's
/// IR and mute survive save and reload.
#[test]
fn pasted_cab_keeps_ir_selector_and_mute_through_save_and_reload() {
    let specs = specs();
    let (_root, mut library) = library("r1");
    let mut current = new_current(&library, &specs);
    let slot = Slot {
        row: Row::new(0).unwrap(),
        column: Column::new(1).unwrap(),
    };
    let mut block = new_instance(specs.model(12001).unwrap());
    block.texts[0].scenes = vec![Some(IR.into())];
    block.mute = Scened::Fixed(true);
    let paste = LayoutEdit::Paste {
        slot,
        block: Box::new(block),
    };
    preset::prepare_layout_edit(&current, &specs, paste)
        .unwrap()
        .commit(&mut current);

    let saved = save(&mut library, &mut current, &specs, 0, "Cab", 1_600_000_000);
    let reloaded = preset::read(&library, Some(&saved.path), &specs).unwrap();
    let prepared = preset::prepare(reloaded, &specs, false, None, Default::default()).unwrap();
    let cab = prepared
        .deployment
        .layout()
        .instance(Site::Slot(slot))
        .unwrap();
    assert_eq!(cab.texts[0].index, 0);
    assert!(
        cab.texts[0]
            .scenes
            .iter()
            .all(|text| text.as_deref() == Some(IR)),
        "{:?}",
        cab.texts
    );
    assert!(cab.mute.at(SceneIndex::new(0).unwrap()));
}

/// Review w2c-io R2 and R9: a plain Save of a preset whose file stem differs from its name
/// (`Clean_1.pb`, a taken name) keeps the name, and a changed preset restamps its sidecar date.
#[test]
fn plain_save_keeps_the_stored_name_and_restamps_the_sidecar() {
    let specs = specs();
    let (_root, mut library) = library("r2");
    let mut first = new_current(&library, &specs);
    save(&mut library, &mut first, &specs, 0, "Clean", 1_600_000_000);
    let mut current = new_current(&library, &specs);
    let saved = save(
        &mut library,
        &mut current,
        &specs,
        1,
        "Clean",
        1_600_000_000,
    );
    assert!(saved.path.ends_with("/Clean_1.pb"), "{}", saved.path);

    let lane = Site::OutputControl(Row::new(0).unwrap());
    preset::set_parameter(&mut current, &specs, lane, 0, 0.25).unwrap();
    let name = preset::save_name(&current, None).unwrap();
    assert_eq!(name, "Clean");
    save(&mut library, &mut current, &specs, 1, &name, 1_700_000_000);

    let stored = library.load(&saved.path).unwrap();
    let preset = Preset::try_from(&stored.document).unwrap();
    assert_eq!(preset.name.as_deref(), Some("Clean"));
    let sidecar = stored.sidecar.unwrap();
    assert_eq!(sidecar.text("date"), Some("2023-11-14T22:13:20Z"));
    assert_eq!(
        sidecar.text("corosVersion"),
        Some(tetrad_firmware::running().version)
    );
}

/// Save As of a preset that has a file (`0x5ac350`): the edited copy goes to the setlist's
/// first empty slot under a free file name, keeps its stored name, and becomes the loaded
/// preset; the source file keeps its old value.
#[test]
fn save_as_copies_an_edited_preset_into_the_first_free_slot() {
    let specs = specs();
    let (_root, mut library) = library("save-as");
    let mut current = new_current(&library, &specs);
    let source = save(
        &mut library,
        &mut current,
        &specs,
        0,
        "Clean",
        1_600_000_000,
    )
    .path;
    let lane = Site::OutputControl(Row::new(0).unwrap());
    preset::set_parameter(&mut current, &specs, lane, 0, 0.25).unwrap();
    let request = SaveRequest {
        slot: &PresetRef {
            setlist: None,
            index: 0,
        },
        name: "Clean",
        now: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        account_name: "",
        account_id: "",
        creator: Creator::Quad,
    };
    let (saved, slot) = preset::save_as(
        &mut library,
        &mut current,
        &specs,
        &request,
        Some("Live"),
        None,
    )
    .unwrap();
    assert_eq!(slot.index, 1);
    assert!(saved.path.ends_with("/Live/Clean_1.pb"), "{}", saved.path);
    assert_eq!(current.path.as_deref(), Some(saved.path.as_str()));
    assert_eq!(
        library.slots("Live").unwrap()[1].as_deref(),
        Some(saved.path.as_str())
    );

    let volume = |path: &str| {
        let stored = Preset::try_from(&library.load(path).unwrap().document).unwrap();
        assert_eq!(stored.name.as_deref(), Some("Clean"));
        preset::stored_parameter(&stored, lane, 0, 0)
    };
    assert_eq!(volume(&saved.path), Some(0.25));
    assert_ne!(volume(&source), Some(0.25), "the source file is unchanged");
}

/// Directory Multi Select copy (`copy_into`, stock Copy + "Paste in the first empty slot"
/// `0x7c3d94`): the preset lands in the target setlist's first empty slot with its stored
/// name, and the source slot keeps its file.
#[test]
fn copy_into_places_the_preset_in_the_first_free_slot_of_another_setlist() {
    let specs = specs();
    let (_root, mut library) = library("copy-into");
    library.create_setlist("Gig").unwrap();
    let mut current = new_current(&library, &specs);
    let source = save(
        &mut library,
        &mut current,
        &specs,
        0,
        "Clean",
        1_600_000_000,
    )
    .path;
    assert_eq!(
        library.slots("Live").unwrap()[0].as_deref(),
        Some(source.as_str())
    );

    let preset::Copied { slot, path, .. } =
        preset::copy_into(&mut library, "Live", 0, "Gig", None).unwrap();
    assert_eq!(slot, 0);
    assert!(path.contains("/Gig/"), "{path}");
    assert_eq!(
        library.slots("Gig").unwrap()[0].as_deref(),
        Some(path.as_str())
    );
    assert_eq!(
        library.slots("Live").unwrap()[0].as_deref(),
        Some(source.as_str())
    );
    let stored = Preset::try_from(&library.load(&path).unwrap().document).unwrap();
    assert_eq!(stored.name.as_deref(), Some("Clean"));
}

/// Stock history contract (`0x00648f1c`): edits under 500 ms apart share a frame, a frame
/// equal to the newest is not pushed, a new edit clears redo, and each stack keeps 20.
#[test]
fn history_merges_dedupes_clears_redo_and_keeps_twenty_frames() {
    let specs = specs();
    let (_root, library) = library("history");
    let mut current = new_current(&library, &specs);
    let lane = Site::OutputControl(Row::new(0).unwrap());
    let mut history = preset::History::default();
    let edit = |history: &mut preset::History, current: &mut Current, ms: u64, value| {
        history.record(current, Duration::from_millis(ms), preset::UndoPath::View);
        preset::set_parameter(current, &specs, lane, 0, value).unwrap();
    };
    edit(&mut history, &mut current, 0, 0.1);
    edit(&mut history, &mut current, 300, 0.2);
    assert!(history.step(&current, false).is_some());
    assert!(
        !history.can_undo() && history.can_redo(),
        "one frame for both edits"
    );
    edit(&mut history, &mut current, 1_000, 0.3);
    assert!(!history.can_redo(), "a new edit clears redo");

    for step in 0..25u64 {
        edit(
            &mut history,
            &mut current,
            2_000 + step * 600,
            step as f32 / 100.0,
        );
    }
    let mut undone = 0;
    while history.step(&current, false).is_some() {
        undone += 1;
    }
    assert_eq!(undone, preset::HISTORY_DEPTH);
}

struct DirectoryPresetSettings;
impl tetrad_settings::SettingsStorage for DirectoryPresetSettings {
    fn read(&self, _: tetrad_settings::SettingsFileId) -> std::io::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    fn write(&mut self, _: tetrad_settings::SettingsFileId, _: &[u8]) -> std::io::Result<()> {
        panic!("unexpected settings write")
    }
    fn remove(&mut self, _: tetrad_settings::SettingsFileId) -> std::io::Result<()> {
        panic!("unexpected settings remove")
    }
}

fn directory_preset_state(library: StockLibrary, current: Current) -> tetrad_app::device::State {
    let (settings, _) = tetrad_settings::DeviceSettings::load(
        &DirectoryPresetSettings,
        tetrad_settings::Hardware::QuadCortex,
    )
    .unwrap();
    let mut state = tetrad_app::device::State::new(
        tetrad_hal::platform::Product::QuadCortex,
        tetrad_hal::platform::Platform::QcOriginal,
        settings,
    );
    state.library = Some(tetrad_app::device::Library {
        presets: library,
        database: None,
    });
    *state.current.get_mut() = Some(current);
    state
}

#[test]
fn directory_setlist_edits_persist_and_keep_loaded_paths_valid() {
    use tetrad_app::device::{Action, Effect};
    use tetrad_ui::DirectoryRequest as R;
    let specs = specs();
    let (_root, mut library) = library("directory-setlist");
    let mut current = new_current(&library, &specs);
    let saved = save(&mut library, &mut current, &specs, 0, "Lead", 1_600_000_000);
    let mut state = directory_preset_state(library, current);
    state
        .settings
        .update(|settings| settings.last_preset.setlist = Some("/media/p4/Presets/Live".into()));
    tetrad_app::device::actions::reduce(Action::CreateSetlist("Extra".into()), &mut state).unwrap();
    let request = R::RenameSetlist {
        from: "Live".into(),
        to: "Tour".into(),
    };
    let effects =
        tetrad_app::device::actions::reduce(Action::Directory(request.clone()), &mut state)
            .unwrap();
    assert!(
        effects
            .iter()
            .any(|effect| matches!(effect, Effect::Directory(edit) if *edit == request))
    );
    tetrad_app::device::media::edit_directory(
        request,
        &mut state,
        None,
        &tetrad_app::device::bringup::RunOptions::STOCK,
    )
    .unwrap();
    let expected = saved.path.replace("/Live/", "/Tour/");
    assert_eq!(
        state.current.as_ref().unwrap().path.as_deref(),
        Some(expected.as_str())
    );
    assert_eq!(
        state.settings.settings().last_preset.setlist.as_deref(),
        Some("/media/p4/Presets/Tour")
    );
    assert!(
        state
            .library
            .as_ref()
            .unwrap()
            .presets
            .load(&expected)
            .is_ok()
    );
    tetrad_app::device::media::edit_directory(
        R::DeleteSetlist {
            name: "Tour".into(),
        },
        &mut state,
        None,
        &tetrad_app::device::bringup::RunOptions::STOCK,
    )
    .unwrap();
    assert!(state.current.as_ref().unwrap().path.is_none());
    assert_eq!(
        state
            .library
            .as_ref()
            .unwrap()
            .presets
            .setlists()
            .unwrap()
            .iter()
            .map(|setlist| setlist.name.as_str())
            .collect::<Vec<_>>(),
        ["Extra"]
    );
}

#[test]
fn directory_row_move_tracks_the_loaded_preset_and_last_slot_without_loading_audio() {
    use tetrad_ui::DirectoryRequest as R;
    let specs = specs();
    let (_root, mut library) = library("directory-move");
    let mut current = new_current(&library, &specs);
    let saved = save(&mut library, &mut current, &specs, 0, "Lead", 1_600_000_000);
    let mut state = directory_preset_state(library, current);
    let effects = tetrad_app::device::media::edit_directory(
        R::MovePreset {
            setlist: "Live".into(),
            from: 0,
            to: 255,
            overwrite: false,
        },
        &mut state,
        None,
        &tetrad_app::device::bringup::RunOptions::STOCK,
    )
    .unwrap();
    assert!(effects.iter().all(|effect| !matches!(
        effect,
        tetrad_app::device::Effect::LoadPreset(_) | tetrad_app::device::Effect::Dsp(_)
    )));
    assert_eq!(
        state.current.as_ref().unwrap().path.as_deref(),
        Some(saved.path.as_str())
    );
    assert_eq!(state.performance.footswitch.current_preset, 255);
    assert_eq!(state.settings.settings().last_preset.preset_index, 255);
    assert_eq!(
        state
            .library
            .as_ref()
            .unwrap()
            .presets
            .slots("Live")
            .unwrap()[255]
            .as_deref(),
        Some(saved.path.as_str())
    );
}

/// Gap #3: row ports go to `Chain.in_portid`/`out_portid` and split/mix points to the master
/// chain's `SplitControlPoints` (`0x51a090` → `0x2e2fb4`), so a save and reload keeps them.
#[test]
fn row_ports_and_split_mix_points_survive_save_and_reload() {
    use tetrad_engine::{InputPort, OutputPort, Pair};
    let specs = specs();
    let (_root, mut library) = library("routing");
    let mut current = new_current(&library, &specs);
    let row = Row::new(2).unwrap();
    let edits = vec![
        LayoutEdit::Input {
            row,
            port: InputPort::Input2,
        },
        LayoutEdit::Output {
            row,
            port: OutputPort::Xlr1,
        },
        LayoutEdit::Split {
            pair: Pair::Upper,
            column: Some(Column::new(2).unwrap()),
        },
        LayoutEdit::Mix {
            pair: Pair::Upper,
            column: Some(Column::new(5).unwrap()),
        },
    ];
    preset::prepare_layout_edits(&current, &specs, edits)
        .unwrap()
        .commit(&mut current);

    let saved = save(
        &mut library,
        &mut current,
        &specs,
        0,
        "Routes",
        1_600_000_000,
    );
    let reloaded = preset::read(&library, Some(&saved.path), &specs).unwrap();
    let layout = preset::prepare(reloaded, &specs, false, None, Default::default())
        .unwrap()
        .deployment
        .layout()
        .clone();
    let lane = layout.lane(row);
    assert_eq!(
        (lane.input, lane.output),
        (InputPort::Input2, OutputPort::Xlr1)
    );
    let pair = layout.pair(Pair::Upper);
    assert_eq!(
        (pair.split, pair.merge),
        (Some(Column::new(2).unwrap()), Some(Column::new(5).unwrap()))
    );
}

/// A split point on a pair whose stored splitter is empty takes the new-preset junction
/// (combined splitter 10004, inferred from `0x0051a370`) in the preset and the deployment
/// instead of refusing.
#[test]
fn a_split_without_a_stored_splitter_takes_the_new_preset_default() {
    use tetrad_engine::Pair;
    let specs = specs();
    let (_root, library) = library("junction");
    let mut read = preset::read(&library, None, &specs).unwrap();
    read.chains[0].combined_splitter[0] = tetrad_preset::Model::empty_slot();
    let prepared = preset::prepare(read, &specs, false, None, Default::default()).unwrap();
    let current = Current {
        path: None,
        preset: prepared.preset,
        migration: prepared.migration,
        deployment: prepared.deployment,
    };
    assert!(
        current
            .deployment
            .layout()
            .pair(Pair::Upper)
            .splitter
            .is_none()
    );
    let split = LayoutEdit::Split {
        pair: Pair::Upper,
        column: Some(Column::new(3).unwrap()),
    };
    let change = preset::prepare_layout_edit(&current, &specs, split).unwrap();
    assert_eq!(
        change.preset().chains[0].combined_splitter[0].hash,
        Some(10004)
    );
    let deployed = change.deployment().layout().pair(Pair::Upper);
    assert_eq!(
        deployed.splitter.as_ref().map(|splitter| splitter.model.0),
        Some(10004)
    );
}

/// `0x519cc0` → `0x668354`: a new junction takes the user default model preset (`0x1f7e1c`:
/// the `default_model_presets` row, its values over a fresh block) in the stored preset and
/// the deployment, not the catalog default.
#[test]
fn a_new_mixer_takes_the_user_default_model_preset() {
    use tetrad_engine::Pair;
    use tetrad_preset::ParamValue;
    use tetrad_storage::stock::{DbLimits, ExpSwitch, ParamEntry, StockDb};
    let specs = specs();
    let (root, library) = library("junction-default");
    let mut database = StockDb::create(&root.0.join("zenui.db"), DbLimits::default()).unwrap();
    let switch = ExpSwitch {
        switch_type: 0,
        inverted: false,
        latch_emulation: false,
        delay_ms: 0,
    };
    let level = ParamEntry {
        index: 0,
        value: "0.25".into(),
        exp: 0,
        exp_trim_max: 1.0,
        exp_trim_min: 0.0,
    };
    (database.transaction(|write| {
        let id = write.create_model_preset("Quiet", 11000, switch, &[level])?;
        write.set_default_model_preset(11000, Some((id, false)))
    }))
    .unwrap();
    let defaults = preset::JunctionDefaults::read(&specs, &database).unwrap();
    let mut read = preset::read(&library, None, &specs).unwrap();
    read.chains[0].mixer[0] = tetrad_preset::Model::empty_slot();
    let prepared = preset::prepare(read, &specs, false, None, Default::default()).unwrap();
    let current = Current {
        path: None,
        preset: prepared.preset,
        migration: prepared.migration,
        deployment: prepared.deployment,
    };
    let mix = LayoutEdit::Mix {
        pair: Pair::Upper,
        column: Some(Column::new(5).unwrap()),
    };
    let change = preset::prepare_junction_edits(&current, &specs, vec![mix], &defaults).unwrap();
    let stored = &change.preset().chains[0].mixer[0];
    assert_eq!(
        (stored.hash, stored.params[0].value(0)),
        (Some(11000), Some(&ParamValue::Float(0.25)))
    );
    let deployed = change.deployment().layout().pair(Pair::Upper);
    let mixer = deployed.mixer.as_ref().unwrap();
    assert_eq!(mixer.params[0].at(SceneIndex::new(0).unwrap()), 0.25);
}

/// `0x51898c` (review r14 P-4): a save inverts the stored bypass of each block of a momentary
/// stomp whose footswitch is in the held mask: every scene, or only the playing scene when
/// the bypass is in scene mode. A momentary stomp that is not held saves as stored, and the
/// loaded preset keeps its live records.
#[test]
fn held_momentary_stomps_save_their_blocks_inverted() {
    let specs = specs();
    let (_root, mut library) = library("momentary");
    let mut current = new_current(&library, &specs);
    // The rule reads the stored preset only; the synthetic fit holds one cab.
    let at = |row| tetrad_preset::Slot { row, column: 1 };
    let stored = &mut current.preset;
    for row in 0..=2 {
        let cab =
            tetrad_preset::Model::block(12000, vec![tetrad_preset::ParamValue::Float(0.0); 3]);
        stored
            .insert_block(at(row), cab, &preset::SpecsCatalog(&specs))
            .unwrap();
    }
    // Stomp A (HAL 0) is held and holds rows 0 and 1; stomp B (HAL 1) is not held.
    for (stomp, row) in [(0, 0), (0, 1), (1, 2)] {
        stored
            .stomp_mode_assignments
            .push(tetrad_preset::StompAssignment {
                row,
                column: 1,
                stomp_index: stomp,
                ..Default::default()
            });
    }
    stored.stomp_is_momentary.insert(0, true);
    stored.stomp_is_momentary.insert(1, true);
    stored
        .block_bypass_mut(at(1))
        .unwrap()
        .set_scene_mode(true, 0)
        .unwrap();
    let live = current.preset.clone();
    let held = 1 << 0;

    let saved = preset::save_resting(
        held,
        &[],
        &specs,
        |_| false,
        &mut current,
        |current| save(&mut library, current, &specs, 0, "Held", 1_600_000_000),
    );
    let mut reloaded = preset::read(&library, Some(&saved.path), &specs).unwrap();
    let mut scenes = |row| {
        let record = reloaded.block_bypass_mut(at(row)).unwrap();
        (0..8)
            .map(|scene| record.bypassed(scene).unwrap())
            .collect::<Vec<_>>()
    };
    assert_eq!(scenes(0), [true; 8]);
    assert_eq!(
        scenes(1),
        [true, false, false, false, false, false, false, false]
    );
    assert_eq!(scenes(2), [false; 8]);
    assert_eq!(
        current.preset.bypass, live.bypass,
        "the live records come back"
    );
}

/// Cortex Control COPY into a target index (`0x24b254` → `0x1e8a70` → `0x1e8290` with the
/// force flag): an empty slot takes the copy, an occupied one is overwritten (the old file
/// goes when no other slot names it), and the source slot itself is refused.
#[test]
fn copy_into_a_target_slot_overwrites_its_preset() {
    let specs = specs();
    let (_root, mut library) = library("copy-slot");
    library.create_setlist("Gig").unwrap();
    let mut current = new_current(&library, &specs);
    save(
        &mut library,
        &mut current,
        &specs,
        0,
        "Clean",
        1_600_000_000,
    );
    let mut current = new_current(&library, &specs);
    save(&mut library, &mut current, &specs, 1, "Lead", 1_600_000_000);

    let copied = preset::copy_into(&mut library, "Live", 0, "Gig", Some(3)).unwrap();
    let (slot, first) = (copied.slot, copied.path);
    assert_eq!(slot, 3);
    let second = (preset::copy_into(&mut library, "Live", 1, "Gig", Some(3)).unwrap()).path;
    let gig = library.slots("Gig").unwrap();
    assert_eq!(gig[3].as_deref(), Some(second.as_str()));
    assert_eq!(gig.iter().flatten().count(), 1);
    assert!(
        library.load(&first).is_err(),
        "the overwritten file is gone"
    );
    assert!(matches!(
        preset::copy_into(&mut library, "Live", 0, "Live", Some(0)),
        Err(preset::PresetError::Unsupported(_))
    ));
}

/// Save As into a chosen slot (Control preset CREATE with `index`, `0x24e874`): the copy takes
/// that slot, not the first empty one; the loaded preset's own slot is refused.
#[test]
fn save_as_into_a_chosen_slot() {
    let specs = specs();
    let (_root, mut library) = library("save-as-slot");
    let mut current = new_current(&library, &specs);
    save(
        &mut library,
        &mut current,
        &specs,
        0,
        "Clean",
        1_600_000_000,
    );
    let request = SaveRequest {
        slot: &PresetRef {
            setlist: None,
            index: 0,
        },
        name: "Copy",
        now: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        account_name: "",
        account_id: "",
        creator: Creator::Quad,
    };
    let own = preset::save_as(&mut library, &mut current, &specs, &request, None, Some(0));
    assert!(matches!(own, Err(preset::PresetError::Unsupported(_))));
    let (saved, slot) =
        preset::save_as(&mut library, &mut current, &specs, &request, None, Some(5)).unwrap();
    assert_eq!(slot.index, 5);
    assert_eq!(
        library.slots("Live").unwrap()[5].as_deref(),
        Some(saved.path.as_str())
    );
}

/// Review r20 G2: a Cortex Control COPY or MOVE onto the loaded preset's slot is refused, as
/// Save As: the overwrite would delete the file the next device save writes. The loaded file
/// and its slot stay.
#[test]
fn copy_or_move_onto_the_loaded_slot_is_refused() {
    use tetrad_app::device::Action;
    use tetrad_app::device::actions::{ActionError, reduce};
    let specs = specs();
    let (_root, mut library) = library("copy-onto-loaded");
    let mut current = new_current(&library, &specs);
    save(&mut library, &mut current, &specs, 0, "Lead", 1_600_000_000);
    let mut other = new_current(&library, &specs);
    save(&mut library, &mut other, &specs, 1, "Lead", 1_600_000_000);
    let loaded = current.path.clone();
    let mut state = directory_preset_state(library, current);
    for remove in [false, true] {
        let copy = Action::CopyPresetTo {
            from: "Live".into(),
            slot: 1,
            to: "Live".into(),
            to_slot: Some(0),
            remove,
        };
        assert!(matches!(
            reduce(copy, &mut state),
            Err(ActionError::Unsupported(_))
        ));
    }
    let library = &state.library.as_ref().unwrap().presets;
    assert_eq!(library.slots("Live").unwrap()[0], loaded);
    assert!(library.load(loaded.as_deref().unwrap()).is_ok());
}

/// Review r20 G4: an overwrite empties the target slot only after every check that can refuse
/// the new preset. A Save As refused by the migration gate (kept unknown blocks with a stored
/// modified version below 3.0.0) into an occupied slot keeps that slot's preset.
#[test]
fn a_refused_save_as_keeps_the_occupied_target_slot() {
    let specs = specs();
    let (_root, mut library) = library("refused-overwrite");
    let mut first = new_current(&library, &specs);
    let kept = save(&mut library, &mut first, &specs, 0, "Clean", 1_600_000_000).path;
    let mut gated = new_current(&library, &specs);
    gated
        .migration
        .unknown_blocks
        .push(tetrad_preset::UnknownBlock {
            slot: tetrad_preset::Slot { row: 0, column: 1 },
            model_id: 99_999,
        });
    gated.migration.unknown_blocks_emptied = false;
    gated.migration.versions.modified = tetrad_preset::Version {
        major: 2,
        minor: 0,
        patch: 0,
    };
    let request = SaveRequest {
        slot: &PresetRef {
            setlist: None,
            index: 0,
        },
        name: "Lead",
        now: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        account_name: "",
        account_id: "",
        creator: Creator::Quad,
    };
    let refused = preset::save_as(&mut library, &mut gated, &specs, &request, None, Some(0));
    assert!(matches!(refused, Err(preset::PresetError::Unsupported(_))));
    assert_eq!(
        library.slots("Live").unwrap()[0].as_deref(),
        Some(kept.as_str())
    );
    assert!(library.load(&kept).is_ok());
}

/// Review r20 G4 (with `StockLibrary::replace`): a copy over an occupied slot whose new file
/// cannot be written (a directory at the new sidecar path) fails and keeps the old preset in
/// its slot.
#[test]
fn a_failed_copy_write_keeps_the_occupied_target_slot() {
    let specs = specs();
    let (_root, mut library) = library("failed-copy-write");
    let mut clean = new_current(&library, &specs);
    let kept = save(&mut library, &mut clean, &specs, 0, "Clean", 1_600_000_000).path;
    let mut lead = new_current(&library, &specs);
    save(&mut library, &mut lead, &specs, 1, "Lead", 1_600_000_000);
    let live = library.setlists().unwrap().remove(0).path;
    let sidecar = library
        .media()
        .resolve(&format!("{live}/Lead_1.json"))
        .unwrap();
    std::fs::create_dir(sidecar).unwrap();
    assert!(preset::copy_into(&mut library, "Live", 1, "Live", Some(0)).is_err());
    assert_eq!(
        library.slots("Live").unwrap()[0].as_deref(),
        Some(kept.as_str())
    );
    assert!(library.load(&kept).is_ok());
}

fn account_request<'a>(slot: &'a PresetRef, name: &'a str, account: &'a str) -> SaveRequest<'a> {
    SaveRequest {
        slot,
        name,
        now: SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        account_name: account,
        account_id: "id-of-account",
        creator: Creator::Quad,
    }
}

/// r50 (B2 on the QC): Save As of a Factory Library preset (`0x5b7438`, the loaded folder is
/// read-only, `0x7054f0`) stamps the account as author over the factory author, drops the
/// factory tags (`0x2e2318`), and writes the stock record: `creator` "quad", empty
/// `cloudId` and `description`.
#[test]
fn a_save_as_of_a_factory_preset_stamps_the_account_and_the_product() {
    let specs = specs();
    let (root, library) = library("factory-save-as");
    let factory = root.0.join("factory");
    let mut library = library.with_factory(MediaRoot::factory(&factory));
    let mut current = new_current(&library, &specs);
    let user = save(
        &mut library,
        &mut current,
        &specs,
        0,
        "Plexi",
        1_600_000_000,
    )
    .path;
    let mut stock = Preset::try_from(&library.load(&user).unwrap().document).unwrap();
    stock.author_name = Some("Factory Author".into());
    stock.author_id = Some("factory-id".into());
    stock.tags = vec!["Crunch".into()];
    std::fs::create_dir_all(factory.join("Factory Library")).unwrap();
    std::fs::write(factory.join("Factory Library/Plexi.pb"), stock.to_bytes()).unwrap();
    let path = "/opt/neuraldsp/Factory Library/Plexi.pb";
    let read = preset::read(&library, Some(path), &specs).unwrap();
    let prepared = preset::prepare(read, &specs, false, None, Default::default()).unwrap();
    let mut current = Current {
        path: Some(path.into()),
        preset: prepared.preset,
        migration: prepared.migration,
        deployment: prepared.deployment,
    };

    let slot = PresetRef {
        setlist: None,
        index: 0,
    };
    let request = account_request(&slot, "Plexi", "Test Player");
    let target = (Some("Live"), Some(3));
    let guitar = tetrad_ui::PreferredInstrument::Guitar;
    let saved = preset::save_as_with(&mut library, &mut current, &specs, &request, target, guitar);
    let stored = library.load(&saved.unwrap().0.path).unwrap();
    let sidecar = stored.sidecar.unwrap();
    for (key, value) in [
        ("author", "Test Player"),
        ("authorId", "id-of-account"),
        ("creator", "quad"),
        ("cloudId", ""),
        ("description", ""),
        ("instrument_type", "guitar"),
        ("name", "Plexi"),
    ] {
        assert_eq!(sidecar.text(key), Some(value), "{key}");
    }
    assert!(sidecar.texts("tags").is_empty());
    let preset = Preset::try_from(&stored.document).unwrap();
    assert_eq!(preset.author_name.as_deref(), Some("Test Player"));
    assert!(preset.tags.is_empty());
}

/// r50: a plain Save over a user preset writes a new record (`0x5b7438` → `0x7c4db8`, the
/// folder copies it whole, `0x1eb11c`): the preset's own author and tags stay, the slot's
/// instrument stays (`0x5b9078`), `cloudId`, `description` and unknown keys go.
#[test]
fn a_plain_save_keeps_the_author_and_instrument_and_drops_cloud_fields() {
    let specs = specs();
    let (_root, mut library) = library("plain-save-record");
    let mut current = new_current(&library, &specs);
    current.preset.tags = vec!["Lead".into()];
    let slot = PresetRef {
        setlist: None,
        index: 0,
    };
    let first = account_request(&slot, "Lead", "First Player");
    let bass = tetrad_ui::PreferredInstrument::Bass;
    let path = preset::save_with(&mut library, &mut current, &specs, &first, bass)
        .unwrap()
        .path;
    let mut stock = library.load(&path).unwrap();
    let sidecar = stock.sidecar.as_mut().unwrap();
    assert_eq!(sidecar.text("author"), Some("First Player"), "a new preset");
    sidecar.set_text("cloudId", "cloud-1");
    sidecar.set_text("description", "notes");
    sidecar.set_text("extra", "kept by no save");
    library.save(&mut stock).unwrap();

    let again = account_request(&slot, "Lead", "Other Player");
    preset::save(&mut library, &mut current, &specs, &again).unwrap();
    let sidecar = library.load(&path).unwrap().sidecar.unwrap();
    for (key, value) in [
        ("author", "First Player"),
        ("creator", "quad"),
        ("cloudId", ""),
        ("description", ""),
        ("instrument_type", "bass"),
    ] {
        assert_eq!(sidecar.text(key), Some(value), "{key}");
    }
    assert_eq!(sidecar.texts("tags"), ["Lead"]);
    assert_eq!(sidecar.text("extra"), None);
}

/// r50: the save author is the account display name (`ui_username`, `0x2ff44c`), not the
/// sign-in name, and the id is the access token's `sub` (`0x2ff3f8`); logged out, both are
/// empty. Invented values.
#[test]
fn the_save_account_is_the_display_name_and_the_token_subject() {
    use tetrad_app::device::preset::SaveAccount;
    use tetrad_settings::account::AccountTokens;
    use tetrad_settings::device::{Hardware, Settings};
    let mut settings = Settings::defaults(Hardware::QuadCortex);
    assert_eq!(SaveAccount::of(&settings), SaveAccount::default());
    settings.account.username = "player@example.com".into();
    settings.account.ui_username = "Test Player".into();
    settings.tokens = AccountTokens {
        // `{"sub":"id-test-7","exp":4102444800}`, unsigned.
        access_token: tetrad_settings::Secret::new(
            "e30.eyJzdWIiOiJpZC10ZXN0LTciLCJleHAiOjQxMDI0NDQ4MDB9.sig",
        ),
        refresh_token: tetrad_settings::Secret::new("refresh-invented"),
    };
    let account = SaveAccount::of(&settings);
    assert_eq!(account.name, "Test Player");
    assert_eq!(account.id, "id-test-7");
}
