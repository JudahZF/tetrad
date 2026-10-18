//! `PresetSummary` → [`Layout`] for the deployment compiler.
//!
//! Structure is validated by the same pass as [`crate::import_summary`] (positional
//! addressing, coordinates, duplicates, unknown fields), with sidechains and
//! splitter/mixer bypass admitted. Parameters follow the modern positional path of
//! the stock loader (`0x681d44` → `0x680614`): stored parameter `i` feeds XML
//! parameter `i`, missing scenes and missing trailing parameters take catalog
//! defaults, and sidechain selector floats above 1.0 clamp to 1.0 (`0x6811f8`).
//! Legacy migrations (`tetrad-core::ParameterImportPlan`) are not repeated here:
//! a stored count that is neither the XML count nor XML + MUTE + BYPASS is refused.

use tetrad_catalog::{ModelRole, ModelSpec, ParameterType, Specs, ids};
use tetrad_preset::{ChainSummary, ModelSummary, ParamValue, PresetSummary, SceneBypassSummary};

use crate::import::{is_empty_hash, list, unsupported, validate_summary};
use crate::layout::collapse;
use crate::{
    AbsentScalars, BypassPolarity, Column, ImportError, ImportPolicy, InputPort, Instance, Layout,
    ModelId, OutputPort, Pair, Row, SCENES, Scened, ShadowedPort, SidechainFlags, Site, Slot,
    TextParam,
};

/// A layout plus what the import filled in or dropped.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedLayout {
    pub layout: Layout,
    /// Absent scalars filled under [`AbsentScalars::Proto3Default`].
    pub defaulted: Vec<String>,
    /// Slave ports the pair's split or merge replaces.
    pub shadowed: Vec<ShadowedPort>,
    /// Instances whose stored parameter list was shorter than the XML list.
    pub padded: Vec<Site>,
    /// Hidden or meter parameters whose stored value was not finite and was
    /// replaced by the catalog default (stock would forward the raw float).
    pub sanitized: Vec<(Site, u32)>,
    /// `BinaryPreset.side_chain_follow_exists` (root field 36).
    pub side_chain_follow_exists: bool,
    /// Slave-chain routing that stock ignores ([`crate::Imported::ignored`]).
    pub ignored: Vec<String>,
    /// Legacy splitters (10000–10002) converted to the combined 10004.
    pub migrated: Vec<Pair>,
}

/// Builds a deployable layout. Every model must exist in `specs` with a role
/// that fits its site; unknown models are refused, never skipped.
pub fn import_layout(
    summary: &PresetSummary,
    specs: &Specs,
    absent: AbsentScalars,
) -> Result<ImportedLayout, ImportError> {
    // The stored bypass bool is copied to the BYPASS parameter verbatim
    // (`0x2ecca4`), so no audio polarity decision is needed here.
    let policy = ImportPolicy {
        absent,
        bypass: BypassPolarity::StoredTrueBypasses,
    };
    let validated = validate_summary(summary, policy, true)?;
    let mut import = LayoutImport {
        specs,
        layout: Layout::default(),
        padded: Vec::new(),
        sanitized: Vec::new(),
        migrated: Vec::new(),
    };
    for (index, chain) in summary.chains.iter().enumerate() {
        import.lane(Row(index as u8), chain, &validated.grid)?;
    }
    for pair in Pair::ALL {
        import.pair(
            pair,
            &summary.chains[pair.master().index()],
            &validated.grid,
        )?;
    }
    Ok(ImportedLayout {
        layout: import.layout,
        defaulted: validated.defaulted,
        shadowed: validated.shadowed,
        padded: import.padded,
        sanitized: import.sanitized,
        side_chain_follow_exists: summary.side_chain_follow_exists == Some(true),
        ignored: validated.ignored,
        migrated: import.migrated,
    })
}

struct LayoutImport<'a> {
    specs: &'a Specs,
    layout: Layout,
    padded: Vec<Site>,
    sanitized: Vec<(Site, u32)>,
    migrated: Vec<Pair>,
}

impl LayoutImport<'_> {
    fn lane(
        &mut self,
        row: Row,
        chain: &ChainSummary,
        grid: &crate::Grid,
    ) -> Result<(), ImportError> {
        let path = format!("chains[{}]", chain.position);
        let ports = grid.ports(row);
        let lane = self.layout.lane_mut(row);
        lane.input = port(
            &path,
            "in_portid",
            ports.input.map(|p| p.0),
            InputPort::from_raw,
        )?
        .unwrap_or(InputPort::Empty);
        lane.output = port(
            &path,
            "out_portid",
            ports.output.map(|p| p.0),
            OutputPort::from_raw,
        )?
        .unwrap_or(OutputPort::Empty);
        for (site, name, controls, role) in [
            (
                Site::InputControl(row),
                "input_control",
                &chain.input_control,
                ModelRole::InputGate,
            ),
            (
                Site::OutputControl(row),
                "output_control",
                &chain.output_control,
                ModelRole::LaneOutput,
            ),
        ] {
            let path = format!("{path}.{name}[0]");
            let instance = self.instance(site, &path, &controls[0], role, None)?;
            self.layout.set_instance(site, instance);
        }
        for (index, model) in chain.models.iter().enumerate() {
            let slot = Slot {
                row,
                column: Column(index as u8),
            };
            // Validation guarantees a bypass record for every occupied slot.
            let bypass = grid.block(slot).map(|block| block.bypass);
            let path = format!("{path}.models[{index}]");
            let instance =
                self.instance(Site::Slot(slot), &path, model, ModelRole::Block, bypass)?;
            self.layout.set_instance(Site::Slot(slot), instance);
        }
        Ok(())
    }

    fn pair(
        &mut self,
        pair: Pair,
        chain: &ChainSummary,
        grid: &crate::Grid,
    ) -> Result<(), ImportError> {
        let path = format!("chains[{}]", chain.position);
        let route = grid.route(pair);
        let (name, splitter) = match chain.combined_splitter.first() {
            Some(model) => ("combined_splitter", model),
            None => ("splitter", &chain.splitter[0]),
        };
        let split_bypass = scene_bypass(&chain.split_bypass);
        let mix_bypass = scene_bypass(&chain.mix_bypass);

        let splitter = self.instance(
            Site::Splitter(pair),
            &format!("{path}.{name}[0]"),
            splitter,
            ModelRole::Splitter,
            split_bypass,
        )?;
        let mixer = self.instance(
            Site::Mixer(pair),
            &format!("{path}.mixer[0]"),
            &chain.mixer[0],
            ModelRole::Mixer,
            mix_bypass,
        )?;
        let splitter = match splitter {
            Some(legacy) if (ids::SPLITTER_AB..=ids::SPLITTER_AB + 2).contains(&legacy.model.0) => {
                let converted = self.legacy_splitter(&legacy);
                if converted.is_some() {
                    self.migrated.push(pair);
                }
                Some(converted.unwrap_or(legacy))
            }
            other => other,
        };
        let layout = self.layout.pair_mut(pair);
        layout.split = route.split.map(|junction| junction.column);
        layout.merge = route.merge.map(|junction| junction.column);
        layout.splitter = splitter;
        layout.mixer = mixer;
        Ok(())
    }

    /// Stock converts a legacy splitter into the combined splitter 10004 when
    /// it loads a preset without `combined_splitter` (`tetrad-core`
    /// `LegacySplitterPlan`; TYPE through the 10004 metadata, `0x6e4170`, in
    /// all eight scenes). Unmapped parameters take their 10004 defaults.
    /// `None` when the catalog has no 10004.
    fn legacy_splitter(&self, legacy: &Instance) -> Option<Instance> {
        let plan = tetrad_core::LegacySplitterPlan::new(legacy.model.0).ok()?;
        let target = self.specs.model(ids::SPLITTER)?;
        let mut instance = crate::new_instance(target);
        let selector = match plan.selector() {
            tetrad_core::SplitterType::Type0 => 0.0,
            tetrad_core::SplitterType::Type1 => 1.0,
            tetrad_core::SplitterType::Type2 => 2.0,
        };
        let encoded = target.parameter(0)?.scale.to_normalized(selector).ok()?;
        instance.params[0] = Scened::Fixed(encoded);
        for &(from, to) in plan.mappings() {
            if let (Some(value), Some(slot)) =
                (legacy.params.get(from), instance.params.get_mut(to))
            {
                *slot = *value;
                instance.scene_mode[to] = legacy.scene_mode.get(from).copied().unwrap_or(false);
            }
        }
        instance.bypass = legacy.bypass;
        instance.bypass_scene_mode = legacy.bypass_scene_mode;
        instance.mute = legacy.mute;
        Some(instance)
    }

    fn instance(
        &mut self,
        site: Site,
        path: &str,
        model: &ModelSummary,
        role: ModelRole,
        bypass: Option<Scened<bool>>,
    ) -> Result<Option<Instance>, ImportError> {
        let Some(id) = model.hash.filter(|&hash| !is_empty_hash(hash)) else {
            return Ok(None);
        };
        let spec = self
            .specs
            .model(id)
            .ok_or_else(|| ImportError::UnknownModel {
                path: path.to_owned(),
                model: ModelId(id),
            })?;
        if spec.role != role {
            return Err(ImportError::ModelRole {
                path: path.to_owned(),
                model: ModelId(id),
                expected: role,
                actual: spec.role,
            });
        }
        let (params, texts, mute, sanitized) = params(path, spec, model)?;
        let scene_mode = (0..spec.parameters.len())
            .map(|index| {
                model
                    .params
                    .get(index)
                    .is_some_and(|p| p.scene_mode == Some(true))
            })
            .collect();
        let bypass = bypass.unwrap_or(Scened::Fixed(false));
        self.sanitized
            .extend(sanitized.into_iter().map(|index| (site, index)));
        if model.params.len() < spec.parameters.len() {
            self.padded.push(site);
        }
        Ok(Some(Instance {
            model: ModelId(id),
            params,
            texts,
            // The grid import keeps per-scene storage exactly when the record
            // is in scene mode (`0x67c5d4`).
            bypass_scene_mode: matches!(bypass, Scened::PerScene(_)),
            bypass,
            scene_mode,
            mute,
            sidechain: SidechainFlags {
                sink: model.sidechain_sink_flag == Some(true),
                source: model.sidechain_source_flag == Some(true),
            },
        }))
    }
}

fn port<T>(
    path: &str,
    field: &'static str,
    raw: Option<u32>,
    parse: fn(u32) -> Option<T>,
) -> Result<Option<T>, ImportError> {
    raw.map(|raw| {
        parse(raw).ok_or_else(|| ImportError::Port {
            path: format!("{path}.{field}"),
            raw,
        })
    })
    .transpose()
}

/// Stored split/mix bypass list; empty means "no stored state".
fn scene_bypass(scenes: &[SceneBypassSummary]) -> Option<Scened<bool>> {
    if scenes.is_empty() {
        return None;
    }
    let mut values = [false; SCENES];
    for (value, scene) in values.iter_mut().zip(scenes) {
        *value = scene.bypass == Some(true);
    }
    Some(collapse(values))
}

/// Stock positional parameters (modern path, `tetrad-core::ParameterImportPlan`):
/// stored `i` feeds XML parameter `i`; stored `XML` is MUTE. BYPASS is never
/// read from the list; bypass records and split/mix bypass lists supply it.
/// Entries after MUTE (for example the 20 extra cabsim entries) are not read.
/// Missing parameters and scenes take the normalized catalog default. A
/// non-finite value in a hidden or meter parameter (stock factory presets
/// store NaN in `NOTIFICATION_START`) is replaced by its default and reported;
/// any other non-finite value is refused.
#[allow(clippy::type_complexity)]
fn params(
    path: &str,
    spec: &ModelSpec,
    model: &ModelSummary,
) -> Result<(Vec<Scened<f32>>, Vec<TextParam>, Scened<bool>, Vec<u32>), ImportError> {
    let xml = spec.parameters.len();
    let sidechain = spec.sidechain.map(|s| s.source_param as usize);
    let stored = |index: usize| -> Result<&[Option<ParamValue>], ImportError> {
        let values = model.params.get(index).map_or(&[][..], |p| &p.values[..]);
        if values.len() > SCENES {
            let path = format!("{path}.params[{index}]");
            return Err(list(&path, values.len(), "at most 8 scene values"));
        }
        Ok(values)
    };
    let mut texts = Vec::new();
    let mut sanitized = Vec::new();
    let mut params = Vec::with_capacity(xml);
    for (index, parameter) in spec.parameters.iter().enumerate() {
        let path = format!("{path}.params[{index}]");
        let values = stored(index)?;
        if parameter.kind == ParameterType::String {
            let scenes = values
                .iter()
                .map(|value| match value {
                    None => Ok(None),
                    Some(ParamValue::String(text)) => Ok(Some(text.clone())),
                    Some(_) => Err(unsupported(&path, "number in a string parameter")),
                })
                .collect::<Result<Vec<_>, _>>()?;
            texts.push(TextParam {
                index: index as u32,
                scenes,
            });
            params.push(Scened::Fixed(0.0));
            continue;
        }
        let default = parameter.default_normalized();
        let mut scenes = [default; SCENES];
        for (value, scene) in scenes.iter_mut().zip(values) {
            *value = match scene {
                None => continue,
                Some(ParamValue::Float(v)) if sidechain == Some(index) && *v > 1.0 => 1.0,
                Some(ParamValue::Float(v)) if v.is_finite() => *v,
                Some(ParamValue::Float(_)) if parameter.kind.is_meter() || parameter.hidden => {
                    if sanitized.last() != Some(&(index as u32)) {
                        sanitized.push(index as u32);
                    }
                    default
                }
                Some(ParamValue::Int(v)) => *v as f32,
                Some(ParamValue::Float(_)) => {
                    return Err(unsupported(&path, "non-finite parameter value"));
                }
                Some(ParamValue::String(_)) => {
                    return Err(unsupported(&path, "string in a numeric parameter"));
                }
            };
        }
        params.push(collapse(scenes));
    }
    let mut mute = [false; SCENES];
    for (flag, value) in mute.iter_mut().zip(stored(xml)?) {
        *flag = match value {
            None => false,
            Some(ParamValue::Float(v)) => *v != 0.0,
            Some(ParamValue::Int(v)) => *v != 0,
            Some(ParamValue::String(_)) => {
                let path = format!("{path}.params[{xml}]");
                return Err(unsupported(&path, "string MUTE value"));
            }
        };
    }
    Ok((params, texts, collapse(mute), sanitized))
}
