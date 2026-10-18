//! Grid (1) pushes of a QC-side scene recall and preset load, and the runtime fields stock fills
//! into the working preset at deploy. Checked against the stock S7 capture (records 1647–1666 and
//! 1750–1773).

use tetrad_control::codecs::ControlMessage;
use tetrad_engine::{Column, Layout, Pair, Row, SidechainSource, Site, Slot};
use tetrad_preset::{Chain, Model, Param, Preset};
use tetrad_ui::ModelCategory;

use super::super::State;
use super::super::preset::stored_site_model;
use super::grid::{bypass_echo, grid_update, one_param, site_preset};

/// Pair junction models the recall echoes (`0x5129a4`): the mixer and the combined splitter.
const MIXER: u32 = 11_000;
const COMBINED_SPLITTER: u32 = 10_004;

/// The tail of the stock scene recall `0x527248`: per grid row, each occupied block
/// (`0x5129a4`), then on the master rows 0 and 2 the splitter and the mixer (`0x2e10ec`,
/// `+0x108`). A block echoes every parameter in scene mode (`0x2ea908`) that no active
/// expression pedal drives (`0x322178`), in index order, through `0x25d51c`; its bypass is
/// the last index there and goes out as `0x25d364` with the scene value (`0x2ecd4c`). A
/// junction echoes its scene-mode parameters through `0x25e55c`, only for models 11000 and
/// 10004. Stock sends them at once (the hold flag `+0x44` is set only by Grid receivers),
/// and every scene-mode value goes out, changed or not (`[1651]`–`[1659]`). Not built: the
/// sidechain list push `0x25c8dc` after an echoed source parameter (no dirty-cell copy).
pub(super) fn recall_echoes(state: &State) -> Vec<ControlMessage> {
    let Some(current) = state.current.as_ref() else {
        return Vec::new();
    };
    let preset = &current.preset;
    let scene = current.deployment.scene().index();
    let expression = &state.expression;
    let driven = |site: Site, index: usize| expression.values.contains_key(&(site, index as u32));
    let scene_values = |site: Site, model: &Model| -> Vec<ControlMessage> {
        (model.params.iter().enumerate())
            .filter(|(index, param)| param.scene_mode == Some(true) && !driven(site, *index))
            .filter_map(|(index, param)| {
                let value = param.value(scene)?.clone();
                Some(grid_update(site_preset(
                    site,
                    one_param(index as u32, value),
                )))
            })
            .collect()
    };
    let mut echoes = Vec::new();
    for slot in slots() {
        let site = Site::Slot(slot);
        if let Some(model) = stored_site_model(preset, site).filter(|model| occupied(model)) {
            echoes.extend(scene_values(site, model));
            let bypass = (preset.bypass.get(slot.row.index()))
                .and_then(|row| row.col_bypass.get(slot.column.index()))
                .filter(|column| column.scene_mode == Some(true))
                .filter(|_| !expression.bypasses.contains_key(&site));
            if let Some(column) = bypass {
                let bypassed = column.scene_bypass.get(scene).is_some_and(|s| s.bypass);
                echoes.push(bypass_echo(slot, bypassed));
            }
        }
        // After the last column of a master row: its pair's splitter, then its mixer.
        let pair = Pair::ALL.into_iter().find(|pair| pair.master() == slot.row);
        if let Some(pair) = pair.filter(|_| slot.column.index() == 7) {
            for site in [Site::Splitter(pair), Site::Mixer(pair)] {
                if let Some(model) = stored_site_model(preset, site)
                    .filter(|model| matches!(model.hash, Some(MIXER | COMBINED_SPLITTER)))
                {
                    echoes.extend(scene_values(site, model));
                }
            }
        }
    }
    echoes
}

/// The 4×8 grid in row-major order.
fn slots() -> impl Iterator<Item = Slot> {
    (0..4u8).flat_map(|row| {
        (0..8u8).map(move |column| Slot {
            row: Row::new(row).expect("grid row"),
            column: Column::new(column).expect("grid column"),
        })
    })
}

fn occupied(model: &Model) -> bool {
    model.hash.is_some_and(|hash| hash != 0)
}

/// The Grid sender slot `updateSideChainSourceIcons` (`0x2607c0`, connected to DSPController
/// signal 28 by `0x6e0714`): rows 0..=3, each with `input_control[0]` holding only its
/// `sidechain_source_flag` (`0x2ea698`, written as a presence field). Inferred: the flags
/// are the stored ones; stock recomputes them in `0x519d0c` (a cell's followed lane,
/// `+0x2c8`), which Tetrad does not keep, and the two agree for every unedited preset in the
/// S7 capture.
pub(super) fn input_sources(state: &State) -> Option<ControlMessage> {
    let preset = &state.current.as_ref()?.preset;
    let chains = (0..4u8)
        .map(|row| {
            let flag = (preset.chains.get(usize::from(row)))
                .and_then(|chain| chain.input_control.first())
                .and_then(|gate| gate.sidechain_source_flag)
                .unwrap_or(false);
            Chain {
                row: Some(u32::from(row)),
                input_control: vec![Model {
                    sidechain_source_flag: Some(flag),
                    ..Default::default()
                }],
                ..Default::default()
            }
        })
        .collect();
    Some(grid_update(Preset {
        chains,
        ..Default::default()
    }))
}

/// How often a scene recall emits signal 28: the recall writes every parameter of every
/// block that no pedal drives (`0x526cc0`), and the write of a sidechain source parameter
/// re-routes (`0x52570c`: `0x5843a4`, `0x52e820`, then `0x519d0c`, which emits it). The
/// signal is queued, so its pushes follow the recall's own echoes (stock `[1661]`, `[1662]`
/// for the two sinks of Factory 1A).
pub(super) fn recalled_sources(state: &State) -> usize {
    let (Some(current), Some(specs)) = (state.current.as_ref(), state.specs.as_ref()) else {
        return 0;
    };
    let layout = current.deployment.layout();
    slots()
        .filter_map(|slot| Some((slot, layout.instance(Site::Slot(slot))?)))
        .filter(|(slot, block)| {
            specs
                .model(block.model.0)
                .and_then(|spec| spec.sidechain)
                .is_some_and(|sidechain| {
                    !(state.expression.values)
                        .contains_key(&(Site::Slot(*slot), sidechain.source_param))
                })
        })
        .count()
}

/// The fixed steps of every sidechain selector (`_INIT_123` `0x48b5c`): Off, Follow Input,
/// then the twelve external inputs, with their `icons/IO` glyphs (stock `[1753]`).
const FIXED_STEPS: [(&str, &str); 14] = [
    ("Off", "icons/IO/off.png"),
    ("Follow Input", "icons/IO/input.png"),
    ("Input 1", "icons/IO/input.png"),
    ("Input 2", "icons/IO/input.png"),
    ("Input 1/2", "icons/IO/stereo2.png"),
    ("Return 1", "icons/IO/return.png"),
    ("Return 2", "icons/IO/return.png"),
    ("Return 1/2", "icons/IO/return.png"),
    ("USB input 5", "icons/IO/USB.png"),
    ("USB input 6", "icons/IO/USB.png"),
    ("USB input 7", "icons/IO/USB.png"),
    ("USB input 8", "icons/IO/USB.png"),
    ("USB input 5/6", "icons/IO/USB.png"),
    ("USB input 7/8", "icons/IO/USB.png"),
];

/// The runtime fields of the working preset that stock holds after a deploy and sends in
/// every RecallPreset push and reply (`0x67df1c`), which the stored file lacks:
/// - `hash`: FNV-1a of the device path ([`tetrad_core::PresetId`]); checked against the S7
///   capture for Factory 1A and 1B (137189267, 4204141790). A path without `.pb` (a stored
///   Downloads audition) keeps the stored hash (inferred);
/// - per sidechain sink, the selector parameter's `dynamic_steps`, `dynamic_icons` and
///   `dynamic_metadata` (`SidechainController::setupSidechainSourceParameters` →
///   `0x52ebf4`): [`FIXED_STEPS`], then per preceding block its name (`0x2ea1f4`), its grid
///   glyph (`0x2eb99c`, [`device_icon`]) and `R%1 C%2`; and `sidechain_sink_flag` = the
///   source step at the playing scene is not Off (`0x52e53c`);
/// - `sidechain_source_flag` of a block = some sink names it at the playing scene.
///
/// Inferred: the block name is the catalog name; stock shows a cab's IR name there.
pub(super) fn deploy_fields(preset: &mut Preset, state: &State) {
    let (Some(current), Some(specs)) = (state.current.as_ref(), state.specs.as_ref()) else {
        return;
    };
    if let Some(path) = current.path.as_deref().filter(|path| path.ends_with(".pb")) {
        preset.hash = Some(tetrad_core::PresetId::from_identifier(path).0);
    }
    let layout = current.deployment.layout();
    let Ok(routing) = tetrad_engine::route(layout, specs) else {
        return;
    };
    let scene = current.deployment.scene();
    let named: Vec<Slot> = (routing.sidechains.iter())
        .filter_map(|route| match route.scenes[scene.index()] {
            SidechainSource::Block(source) => Some(source),
            _ => None,
        })
        .collect();
    for slot in slots() {
        let (Some(block), Some(model)) = (
            layout.instance(Site::Slot(slot)),
            (preset.chains.get_mut(slot.row.index()))
                .and_then(|chain| chain.models.get_mut(slot.column.index())),
        ) else {
            continue;
        };
        if named.contains(&slot) {
            model.sidechain_source_flag = Some(true);
        }
        let Some(sidechain) = specs.model(block.model.0).and_then(|spec| spec.sidechain) else {
            continue;
        };
        let route = routing.sidechains.iter().find(|route| route.sink == slot);
        let source = route.map_or(SidechainSource::Off, |route| route.scenes[scene.index()]);
        model.sidechain_sink_flag = Some(source != SidechainSource::Off);
        if let Some(param) = model.params.get_mut(sidechain.source_param as usize) {
            source_lists(param, &layout.sidechain_sources(slot), layout, specs);
        }
    }
}

fn source_lists(
    param: &mut Param,
    sources: &[Slot],
    layout: &Layout,
    specs: &tetrad_catalog::Specs,
) {
    param.dynamic_steps.clear();
    param.dynamic_icons.clear();
    param.dynamic_metadata.clear();
    for (step, icon) in FIXED_STEPS {
        param.dynamic_steps.push(step.to_owned());
        param.dynamic_icons.push(icon.to_owned());
        param.dynamic_metadata.push(String::new());
    }
    for &slot in sources {
        let Some(block) = layout.instance(Site::Slot(slot)) else {
            continue;
        };
        let model = block.model.0;
        let name = specs
            .model(model)
            .map_or_else(String::new, |spec| spec.name.clone());
        param.dynamic_steps.push(name);
        param.dynamic_icons.push(device_icon(model).to_owned());
        let (row, column) = (slot.row.index() + 1, slot.column.index() + 1);
        param.dynamic_metadata.push(format!("R{row} C{column}"));
    }
}

/// The glyph file of a block (`0x2eb99c`, the cases of [`ModelCategory::model_icon`];
/// file names from the strings it loads). Inferred: a Neural Capture gives `NC.png`, as
/// Tetrad does not project the capture device type.
fn device_icon(model: u32) -> &'static str {
    use ModelCategory as C;
    let Some(category) = ModelCategory::of_model(model) else {
        return "Invalid";
    };
    match (category, model) {
        (C::FxLoop, 13_000 | 13_001 | 13_006) => "icons/virtual devices/send.png",
        (C::FxLoop, 13_002 | 13_003 | 13_007) => "icons/virtual devices/return.png",
        (C::FxLoop, _) => "icons/virtual devices/loop.png",
        (C::NeuralCapture, _) => "icons/virtual devices/NC.png",
        (C::Morph, 26_001) => "icons/virtual devices/freeze.png",
        (C::Morph, 26_003 | 26_004) => "icons/virtual devices/BitCrusher.png",
        (C::Utility, 16_008 | 16_013) => "icons/virtual devices/blend.png",
        (C::Utility, 16_011 | 16_012) => "icons/virtual devices/doubler.png",
        (C::Pitch, 18_012) => "icons/virtual devices/multivoicer.png",
        (_, 1184) => "icons/virtual devices/NC_ampcab.png",
        (C::GuitarOverdrive | C::BassOverdrive, _) => "icons/virtual devices/overdrive.png",
        (C::GuitarAmp | C::BassAmp, _) => "icons/virtual devices/AMP.png",
        (C::Eq, _) => "icons/virtual devices/EQ.png",
        (C::Compressor, _) => "icons/virtual devices/comp.png",
        (C::Delay, _) => "icons/virtual devices/delay.png",
        (C::Modulation, _) => "icons/virtual devices/modulation.png",
        (C::Reverb, _) => "icons/virtual devices/reverb.png",
        (C::Wah, _) => "icons/virtual devices/wah.png",
        (C::GuitarCabMono | C::BassCabMono | C::GuitarCabStereo | C::BassCabStereo, _) => {
            "icons/virtual devices/cabsim.png"
        }
        (C::Tuner | C::Utility, _) => "icons/virtual devices/utility.png",
        (C::Pitch, _) => "icons/virtual devices/pitch.png",
        (C::Filter, _) => "icons/virtual devices/filter.png",
        (C::Morph, _) => "icons/virtual devices/morph.png",
        (C::Looper, _) => "icons/looper/looper_icon.png",
        (C::IrLoader, _) => "icons/virtual devices/ir.png",
        (C::Synth, _) => "icons/virtual devices/synth.png",
        (C::Splitter | C::Mixer, _) => "Invalid",
    }
}
