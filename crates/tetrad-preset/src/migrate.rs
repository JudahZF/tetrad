//! Load-time import that ZenUI applies (`0x00681d44` and `0x00680614`), as a
//! stored-form rewrite: after [`Preset::migrate`], the preset holds the state
//! stock holds after it loads the file. Recovered parameter, splitter and
//! sidechain rules live in tetrad-core; model metadata is an injected [`ModelCatalog`].
//! Fields stock ignores on load (`Param.index`, `is_led`, `dynamic_*`,
//! coordinates, update notifications, unknown fields) are kept.

use tetrad_core::{
    ImportVersions, LegacySplitterPlan, MigrationFlags, ParameterControls, ParameterImportPlan,
    ParameterValue, ResolvedParameterMetadata, ResolvedSplitterType, RuntimeParameter,
    SourceLayout, SplitterType, StoredParameterScenes, Version,
};

use crate::{
    COLUMNS, Error, MAX_STOMP_INDEX, ParamValue, ROWS, Result, SCENES, Slot,
    edit::DEFAULT_SCENE_COLORS,
    model::{
        ColBypass, ExpressionBypassInfo, MidiMessage, Model, Param, Preset, SceneValue,
        StompAssignment, StompRole,
    },
};

/// Output and input lane controls; their expression bypass data is special.
pub const OUTPUT_CONTROL_ID: u32 = 23000;
pub const INPUT_CONTROL_ID: u32 = 28000;
pub const TEMPO_MODEL_ID: u32 = 25000;
pub const MIXER_ID: u32 = 11000;
/// Stock clamps legacy BPM values to this range before mapping (`0x0067c750`).
pub const TEMPO_RANGE: std::ops::RangeInclusive<u32> = 40..=240;
pub const MIDI_MESSAGES: usize = 12;
pub const MIDI_GENERAL_GROUPS: usize = 10;
pub const MIDI_GENERAL_SLOTS: usize = 12;
/// Lane-control expression bypass entry seen in every 4.x save (inferred default).
const CONTROL_BYPASS_INFO: ExpressionBypassInfo = ExpressionBypassInfo {
    kind: 2,
    invert: false,
    delay_ms: 600,
    latch_emulation: false,
    unknown: crate::model::Unknown::EMPTY,
};

/// Caller-resolved model metadata (tetrad-catalog implements this in wave 2).
pub trait ModelCatalog {
    fn model(&self, model_id: u32) -> Option<CatalogModel>;

    /// Stored TYPE value of splitter 10004 for a logical selector. Stock
    /// converts it through metadata at VA `0x6e4170`.
    fn splitter_type(&self, selector: SplitterType) -> Option<f32>;

    /// Stored TEMPO value (model 25000, parameter 0) for a BPM in
    /// [`TEMPO_RANGE`]. Stock maps it with `0x0064b8f8(bpm, min, max, skew, 1)`.
    fn tempo_value(&self, bpm: u32) -> Option<f32>;

    /// True only when the catalog is the full device ModelRepo. Then a grid
    /// model it lacks is emptied, as stock does; otherwise the block is kept.
    fn is_complete(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogParam {
    /// Stored-form default.
    pub default: ParamValue,
    /// XML `expAssignable` (stock default true).
    pub expression_assignable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CatalogModel {
    /// Every runtime parameter except BYPASS: the XML parameters, any dynamic
    /// ones (cabinet data) and MUTE last. Stock saves exactly these.
    pub params: Vec<CatalogParam>,
    /// Position of the XML-designated sidechain parameter.
    pub sidechain_parameter: Option<usize>,
}

/// A grid block whose model ID the catalog lacks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnknownBlock {
    pub slot: Slot,
    pub model_id: u32,
}

/// What [`Preset::migrate`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationReport {
    pub versions: ImportVersions,
    pub imported_models: usize,
    pub resized_models: usize,
    /// Grid blocks whose ID the catalog lacks. Stock empties them silently
    /// (VA 0x680678 → 0x680bf4); this happens only with a complete catalog.
    pub unknown_blocks: Vec<UnknownBlock>,
    /// The unknown blocks were emptied ([`ModelCatalog::is_complete`]).
    /// Otherwise they are kept unchanged: not imported, not migrated.
    pub unknown_blocks_emptied: bool,
    /// Lane-control lists (23000/28000) without exactly one entry that took
    /// the catalog default (`0x00681d44`, size test `== 1`).
    pub default_controls: usize,
    pub converted_splitters: usize,
    pub converted_tempo: bool,
    /// Legacy stomp entries consumed (out-of-grid entries are dropped).
    pub converted_stomps: usize,
    pub converted_midi: bool,
    pub sidechain_parameters: usize,
    /// Pair layout code k is not a single stored value; stock marks the pair
    /// for relayout (VA 0x68493c). The graph compiler owns the new code.
    pub relayout: [bool; 2],
}

/// Stock version parse (`0x006eaecc`): text before the first '-', split on
/// '.', each part through `toInt(&ok, 0)` with failures as 0. Missing parts are
/// 0 here; stock orders a shorter equal prefix first (edge case not kept).
pub fn parse_version(text: &str) -> Version {
    let core = text.split('-').next().unwrap_or_default();
    let mut parts = core.split('.').map(|part| {
        let part = part.trim();
        let (digits, radix) =
            if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
                (hex, 16)
            } else if part.len() > 1 && part.starts_with('0') {
                (&part[1..], 8)
            } else {
                (part, 10)
            };
        i32::from_str_radix(digits, radix).map_or(0, |value| value as u32)
    });
    let mut next = || parts.next().unwrap_or(0);
    Version {
        major: next(),
        minor: next(),
        patch: next(),
    }
}

fn to_core(value: &ParamValue) -> ParameterValue {
    match value {
        ParamValue::Int(v) => ParameterValue::Int(*v),
        ParamValue::Float(v) => ParameterValue::Float(*v),
        ParamValue::String(v) => ParameterValue::String(v.clone()),
    }
}

fn from_core(value: ParameterValue) -> ParamValue {
    match value {
        ParameterValue::Int(v) => ParamValue::Int(v),
        ParameterValue::Float(v) => ParamValue::Float(v),
        ParameterValue::String(v) => ParamValue::String(v),
    }
}

/// Stored scene values. An empty oneof reads as Float 0.0 (stock logs
/// "Unknown preset param value type"). Other than eight values is refused:
/// stock leaves invalid variants or writes past its scene vectors.
fn stored_scenes(param: &Param) -> Result<StoredParameterScenes> {
    if param.values.len() != SCENES {
        return Err(Error::Layout("a stored parameter needs eight scene values"));
    }
    Ok(StoredParameterScenes {
        scenes: std::array::from_fn(|scene| {
            Some(
                param.values[scene]
                    .value
                    .as_ref()
                    .map_or(ParameterValue::Float(0.0), to_core),
            )
        }),
    })
}

/// Runtime default controls: no expression, range 0..1, scene mode off.
const DEFAULT_CONTROLS: ParameterControls = ParameterControls {
    expression_assignment: 0,
    expression_min: 0.0,
    expression_max: 1.0,
    scene_mode: false,
};

/// Controls stage (VA 0x6806f4..0x680754): positional, without the scene
/// rules' shifts. Only an `expAssignable` parameter (`beq 0x680748` at
/// 0x6806fc) reads a present expression and, when it is nonzero, the raw
/// min/max. Scene mode is always read.
fn imported_controls(stored: &Param, assignable: bool) -> ParameterControls {
    let mut controls = DEFAULT_CONTROLS;
    if assignable && let Some(expression) = stored.expression {
        controls.expression_assignment = expression;
        if expression != 0 {
            controls.expression_min = stored.expression_min.unwrap_or(0.0);
            controls.expression_max = stored.expression_max.unwrap_or(0.0);
        }
    }
    controls.scene_mode = stored.scene_mode.unwrap_or(false);
    controls
}

/// Writes runtime state into a stored parameter in the stock saver shape.
/// Scene values that keep their bits keep their value messages and unknowns.
fn store(param: &mut Param, runtime: &RuntimeParameter) {
    param.expression = Some(runtime.controls.expression_assignment);
    param.expression_min = Some(runtime.controls.expression_min);
    param.expression_max = Some(runtime.controls.expression_max);
    param.scene_mode = Some(runtime.controls.scene_mode);
    param.values.resize_with(SCENES, SceneValue::default);
    for (slot, value) in param.values.iter_mut().zip(&runtime.scenes) {
        let value = from_core(value.clone());
        let unchanged = match (&slot.value, &value) {
            (Some(ParamValue::Float(a)), ParamValue::Float(b)) => a.to_bits() == b.to_bits(),
            (Some(a), b) => a == b,
            (None, _) => false,
        };
        if !unchanged {
            *slot = SceneValue {
                value: Some(value),
                ..SceneValue::default()
            };
        }
    }
}

fn default_runtime(param: &CatalogParam) -> RuntimeParameter {
    RuntimeParameter {
        scenes: std::array::from_fn(|_| to_core(&param.default)),
        controls: DEFAULT_CONTROLS,
    }
}

/// Positional import of one model into the catalog's stored parameter shape.
fn import_model(
    model: &mut Model,
    metadata: &CatalogModel,
    modified: Version,
) -> Result<Vec<RuntimeParameter>> {
    let id = model.hash.unwrap_or(0);
    let xml_count = metadata
        .params
        .len()
        .checked_sub(1)
        .ok_or(Error::Unsupported("a catalog model needs at least MUTE"))?;
    let plan = ParameterImportPlan::new(id, xml_count, model.params.len(), modified)?;
    let stored = model
        .params
        .iter()
        .map(stored_scenes)
        .collect::<Result<Vec<_>>>()?;
    let mut runtime = Vec::with_capacity(metadata.params.len());
    let mut params = Vec::with_capacity(metadata.params.len());
    for (destination, catalog_param) in metadata.params.iter().enumerate() {
        let base = model.params.get(destination);
        let mut initialized = default_runtime(catalog_param);
        if let Some(base) = base {
            initialized.controls = imported_controls(base, catalog_param.expression_assignable);
        }
        let sidechain = metadata.sidechain_parameter == Some(destination);
        let imported = plan.import_parameter(
            destination,
            &initialized,
            &stored,
            ResolvedParameterMetadata { sidechain },
        )?;
        let mut param = base
            .cloned()
            .unwrap_or_else(|| Param::new(catalog_param.default.clone()));
        store(&mut param, &imported);
        runtime.push(imported);
        params.push(param);
    }
    model.params = params;
    // VA 0x681424..0x681478: ordinary models repair a zero-width bypass range.
    if !matches!(id, OUTPUT_CONTROL_ID | INPUT_CONTROL_ID)
        && let Some(bypass) = model.bypass_expression.first_mut()
        && bypass.expression_min == 0.0
        && bypass.expression_max == 0.0
    {
        bypass.expression_max = 1.0;
    }
    Ok(runtime)
}

/// A model built from catalog defaults in the stock control-model shape.
pub(crate) fn default_model(id: u32, catalog: &impl ModelCatalog) -> Result<Model> {
    let metadata = catalog.model(id).ok_or(Error::UnknownModel(id))?;
    Ok(Model {
        hash: Some(id),
        params: metadata
            .params
            .iter()
            .map(|param| Param::new(param.default.clone()))
            .collect(),
        bypass_expression: vec![crate::model::Expression {
            expression_max: 1.0,
            ..Default::default()
        }],
        ..Model::default()
    })
}

/// Lane control 23000 (two bypass-info entries) or 28000 (one) in the shape
/// of every 4.x save; stock builds them with `0x00550030` / `0x00519cc0`.
pub(crate) fn lane_control(id: u32, catalog: &impl ModelCatalog) -> Result<Model> {
    let entries = if id == OUTPUT_CONTROL_ID { 2 } else { 1 };
    Ok(Model {
        expression_bypass_info: vec![CONTROL_BYPASS_INFO; entries],
        sidechain_source_flag: (id == INPUT_CONTROL_ID).then_some(false),
        ..default_model(id, catalog)?
    })
}

impl Preset {
    /// Stored versions with the stock import default (absent created → 1.0.3).
    /// Present created metadata without both peers is refused: stock reads an
    /// empty repeated field there (VA 0x681e3c..0x681eec).
    pub fn import_versions(&self) -> Result<ImportVersions> {
        let version = |field: &[String]| field.first().map(|text| parse_version(text));
        Ok(ImportVersions::resolve(
            version(&self.created_version),
            version(&self.modified_version),
            version(&self.oldest_compatible_version),
        )?)
    }

    /// True for the stored state that only [`Preset::migrate`],
    /// [`Preset::new_default`] or a stock save writes: both sidechain flags set
    /// (fields 36/37; only the stock saver sets them, VA 0x68036c) and no
    /// legacy tempo pair or legacy stomp data. Grid edits and
    /// [`Preset::stamp_save`] need it (owned policy).
    pub fn is_loaded_form(&self) -> bool {
        self.side_chain_follow_exists == Some(true)
            && self.zsk_7182_applied == Some(true)
            && (self.tempo.is_none() || self.scene_tempo.is_empty())
            && self.legacy_stomp_mode_stomp_data.is_empty()
    }

    /// Applies the recovered load-time import. Run it through
    /// [`crate::PresetDocument::modify`] so that a refusal changes nothing.
    pub fn migrate(&mut self, catalog: &impl ModelCatalog) -> Result<MigrationReport> {
        self.check_stock_loadable()?;
        let versions = self.import_versions()?;
        if self.created_version.is_empty() {
            let legacy = "1.0.3".to_owned();
            self.created_version = vec![legacy.clone()];
            self.modified_version = vec![legacy.clone()];
            self.oldest_compatible_version = vec![legacy];
        }
        let mut report = MigrationReport {
            versions,
            imported_models: 0,
            resized_models: 0,
            unknown_blocks: Vec::new(),
            unknown_blocks_emptied: catalog.is_complete(),
            default_controls: 0,
            converted_splitters: 0,
            converted_tempo: false,
            converted_stomps: self.legacy_stomp_mode_stomp_data.len(),
            converted_midi: false,
            sidechain_parameters: 0,
            relayout: [self.layout_code_1.len() != 1, self.layout_code_2.len() != 1],
        };
        let mut import = |model: &mut Model| -> Result<()> {
            let id = model.hash.unwrap_or(0);
            let metadata = catalog.model(id).ok_or(Error::UnknownModel(id))?;
            let before = model.params.len();
            import_model(model, &metadata, versions.modified)?;
            report.imported_models += 1;
            report.resized_models += usize::from(before != model.params.len());
            Ok(())
        };
        for (row, chain) in self.chains.iter_mut().enumerate() {
            for (column, model) in chain.models.iter_mut().enumerate() {
                if model.is_empty() {
                    continue;
                }
                let model_id = model.hash.unwrap_or(0);
                if catalog.model(model_id).is_none() {
                    report.unknown_blocks.push(UnknownBlock {
                        slot: Slot { row, column },
                        model_id,
                    });
                    if report.unknown_blocks_emptied {
                        // VA 0x680678 → 0x680bf4: unknown IDs become empty slots.
                        *model = Model::empty_slot();
                        if let Some(record) = self
                            .bypass
                            .get_mut(row)
                            .and_then(|bypass| bypass.col_bypass.get_mut(column))
                        {
                            *record = ColBypass::empty_slot();
                        }
                    }
                    continue;
                }
                import(model)?;
            }
            // Even rows own the pair's splitter; stock prefers combined_splitter.
            if row % 2 == 0
                && chain.combined_splitter.is_empty()
                && let Some(source) = chain.splitter.first()
            {
                chain
                    .combined_splitter
                    .push(convert_splitter(source, catalog, versions.modified)?);
                report.converted_splitters += 1;
            }
            // 0x00681d44 imports a lane control only from a one-entry list.
            for (controls, id) in [
                (&mut chain.output_control, OUTPUT_CONTROL_ID),
                (&mut chain.input_control, INPUT_CONTROL_ID),
            ] {
                if controls.len() != 1 {
                    *controls = vec![lane_control(id, catalog)?];
                    report.default_controls += 1;
                } else if !controls[0].is_empty() {
                    import(&mut controls[0])?;
                }
            }
            for model in chain
                .mixer
                .iter_mut()
                .chain(&mut chain.combined_splitter)
                .filter(|model| !model.is_empty())
            {
                import(model)?;
            }
        }
        if self.tempo.is_some() && !self.scene_tempo.is_empty() {
            self.migrate_tempo(catalog)?;
            report.converted_tempo = true;
        } else if let Some(model) = self.tempo_program_data.first_mut() {
            import(model)?;
            self.tempo_program_data.truncate(1);
        } else {
            self.tempo_program_data = vec![default_model(TEMPO_MODEL_ID, catalog)?];
        }
        self.migrate_stomps();
        report.converted_midi = self.migrate_midi();
        if self.scene_colors.len() < SCENES {
            let stored = self.scene_colors.len();
            self.scene_colors
                .extend_from_slice(&DEFAULT_SCENE_COLORS[stored..]);
        }
        report.sidechain_parameters = self.migrate_sidechains(catalog)?;
        Ok(report)
    }

    /// Refuses inputs where the stock loader reads or writes out of bounds.
    fn check_stock_loadable(&self) -> Result<()> {
        if self.chains.len() != ROWS
            || self
                .chains
                .iter()
                .any(|chain| chain.models.len() != COLUMNS)
        {
            return Err(Error::Layout("import needs four chains of eight slots"));
        }
        if self.default_scene.unwrap_or(0) as usize >= SCENES {
            return Err(Error::Unsupported("default scene above 7"));
        }
        if self.scene_colors.len() > SCENES {
            return Err(Error::Unsupported("more than eight scene colors"));
        }
        if self.scene_labels.len() < SCENES {
            return Err(Error::Unsupported("fewer than eight scene labels"));
        }
        if !self.description.is_empty() && self.cloud_id.is_empty() {
            return Err(Error::Unsupported("description without cloud ID"));
        }
        Ok(())
    }

    /// Legacy tempo (VA 0x683fb8, `0x0067c750`): a default tempo model whose
    /// TEMPO parameter takes the per-scene BPMs when eight or more differ,
    /// otherwise `tempo` in every scene. Both legacy fields are consumed.
    fn migrate_tempo(&mut self, catalog: &impl ModelCatalog) -> Result<()> {
        let mut model = default_model(TEMPO_MODEL_ID, catalog)?;
        let per_scene = self.scene_tempo.len() >= SCENES
            && self
                .scene_tempo
                .iter()
                .any(|&bpm| bpm != self.scene_tempo[0]);
        let encode = |bpm: u32| {
            catalog
                .tempo_value(bpm.clamp(*TEMPO_RANGE.start(), *TEMPO_RANGE.end()))
                .ok_or(Error::Unsupported("catalog lacks the tempo mapping"))
        };
        let param = model.param_mut(0)?;
        param.scene_mode = Some(per_scene);
        for (scene, value) in param.values.iter_mut().enumerate() {
            let bpm = if per_scene {
                self.scene_tempo[scene]
            } else {
                self.tempo.unwrap_or(0)
            };
            value.value = Some(ParamValue::Float(encode(bpm)?));
        }
        self.tempo_program_data = vec![model];
        self.tempo = None;
        self.scene_tempo.clear();
        Ok(())
    }

    /// Legacy stomp data becomes primary assignments at its list position, then
    /// stored assignments override per (row, column, role). Stock drops indices
    /// above 10 and legacy entries outside the grid (VA 0x6835f4, 0x683798).
    fn migrate_stomps(&mut self) {
        let mut merged: Vec<StompAssignment> = Vec::new();
        let legacy = std::mem::take(&mut self.legacy_stomp_mode_stomp_data);
        let legacy = legacy
            .into_iter()
            .enumerate()
            .filter(|(_, slot)| (slot.row as usize) < ROWS && (slot.column as usize) < COLUMNS)
            .map(|(position, slot)| StompAssignment {
                row: slot.row,
                column: slot.column,
                stomp_index: position as u32,
                role: Some(StompRole::Primary),
                ..StompAssignment::default()
            });
        let stored = std::mem::take(&mut self.stomp_mode_assignments)
            .into_iter()
            .filter(|assignment| assignment.stomp_index <= MAX_STOMP_INDEX);
        for mut assignment in legacy.chain(stored) {
            let role = assignment.role.unwrap_or(StompRole::Primary);
            assignment.role = Some(role);
            let key = |a: &StompAssignment| (a.row, a.column, a.role);
            match merged.iter_mut().find(|a| key(a) == key(&assignment)) {
                Some(existing) => *existing = assignment,
                None => merged.push(assignment),
            }
        }
        merged.sort_by_key(|a| (a.row, a.column, a.role.map(StompRole::raw)));
        self.stomp_mode_assignments = merged;
    }

    /// Fixed MIDI tables (VA 0x683d78, 0x6847c4..0x6849e0): 12 load messages;
    /// a 10×12 general table from v2, or from v1 into slot 0 of each group when
    /// v2 is empty. The v1 branch of `0x681d44` copies channel and params only:
    /// the type stays 0 (None). Message types are 8-bit. v1 then mirrors slot 0
    /// of each group, as the saver writes it. Returns whether v1 was converted.
    fn migrate_midi(&mut self) -> bool {
        let byte_type = |mut message: MidiMessage| {
            message.kind &= 0xff;
            message
        };
        let mut load: Vec<_> = std::mem::take(&mut self.midi_messages)
            .into_iter()
            .take(MIDI_MESSAGES)
            .map(byte_type)
            .collect();
        load.resize_with(MIDI_MESSAGES, MidiMessage::default);
        self.midi_messages = load;
        let table = MIDI_GENERAL_GROUPS * MIDI_GENERAL_SLOTS;
        let converted = self.midi_messages_general_v2.is_empty();
        let mut general: Vec<MidiMessage> = if converted {
            let mut general = vec![MidiMessage::default(); table];
            for (group, message) in std::mem::take(&mut self.midi_messages_general)
                .into_iter()
                .take(MIDI_GENERAL_GROUPS)
                .enumerate()
            {
                general[group * MIDI_GENERAL_SLOTS] = MidiMessage { kind: 0, ..message };
            }
            general
        } else {
            std::mem::take(&mut self.midi_messages_general_v2)
                .into_iter()
                .take(table)
                .collect()
        };
        general.resize_with(table, MidiMessage::default);
        self.midi_messages_general_v2 = general.into_iter().map(byte_type).collect();
        self.midi_messages_general = (0..MIDI_GENERAL_GROUPS)
            .map(|group| self.midi_messages_general_v2[group * MIDI_GENERAL_SLOTS].clone())
            .collect();
        converted
    }

    /// Runs both recovered sidechain passes on every designated parameter
    /// (VA 0x684190..0x684588), then sets both flags as the stock saver does.
    fn migrate_sidechains(&mut self, catalog: &impl ModelCatalog) -> Result<usize> {
        let flags = MigrationFlags {
            side_chain_follow_exists: self.side_chain_follow_exists.unwrap_or(false),
            zsk_7182_applied: self.zsk_7182_applied.unwrap_or(false),
        };
        let mut migrated = 0;
        if !(flags.side_chain_follow_exists && flags.zsk_7182_applied) {
            let models = std::array::from_fn(|row| {
                std::array::from_fn(|column| self.chains[row].models[column].hash.unwrap_or(0))
            });
            let mixer = |row: usize| {
                self.chains[row]
                    .split_control_points
                    .first()
                    .and_then(|points| usize::try_from(points.mix).ok())
            };
            let layout = SourceLayout::new(models, [mixer(0), mixer(2)])?;
            for (row, chain) in self.chains.iter_mut().enumerate() {
                for (column, model) in chain.models.iter_mut().enumerate() {
                    let Some(position) = catalog
                        .model(model.hash.unwrap_or(0))
                        .and_then(|m| m.sidechain_parameter)
                        .filter(|_| !model.is_empty())
                    else {
                        continue;
                    };
                    let param = model.param_mut(position)?;
                    // Stock reads `toFloat` and truncates: NaN gives k = 0 and the
                    // value stays (C). A non-float value is kept too (O). 0.0 also
                    // has k = 0, so it stands in for both in the tetrad-core passes.
                    let stored = stored_scenes(param)?.scenes;
                    let kept = stored.clone().map(|value| match value {
                        Some(ParameterValue::Float(value)) if !value.is_nan() => None,
                        other => other,
                    });
                    let scenes = stored.map(|value| match value {
                        Some(ParameterValue::Float(value)) if !value.is_nan() => value,
                        _ => 0.0,
                    });
                    let passed = layout.migrate_scenes(row, column, scenes, flags)?;
                    let result: [ParameterValue; SCENES] = std::array::from_fn(|scene| {
                        kept[scene]
                            .clone()
                            .unwrap_or(ParameterValue::Float(passed[scene]))
                    });
                    let controls = imported_controls(param, true);
                    store(
                        param,
                        &RuntimeParameter {
                            scenes: result,
                            controls,
                        },
                    );
                    migrated += 1;
                }
            }
        }
        self.side_chain_follow_exists = Some(true);
        self.zsk_7182_applied = Some(true);
        Ok(migrated)
    }
}

/// Legacy 10000/10001/10002 → combined splitter 10004 (tetrad-core mapping).
fn convert_splitter(
    source: &Model,
    catalog: &impl ModelCatalog,
    modified: Version,
) -> Result<Model> {
    let source_id = source.hash.unwrap_or(0);
    let plan = LegacySplitterPlan::new(source_id)?;
    let metadata = catalog
        .model(source_id)
        .ok_or(Error::UnknownModel(source_id))?;
    let runtime = import_model(&mut source.clone(), &metadata, modified)?;
    let target_id = LegacySplitterPlan::TARGET_MODEL_ID;
    let mut target = default_model(target_id, catalog)?;
    let defaults: Vec<_> = catalog
        .model(target_id)
        .ok_or(Error::UnknownModel(target_id))?
        .params
        .iter()
        .map(default_runtime)
        .collect();
    let encoded_value = catalog
        .splitter_type(plan.selector())
        .ok_or(Error::Unsupported(
            "catalog lacks the splitter TYPE encoding",
        ))?;
    let converted = plan.apply(
        &runtime,
        &defaults,
        ResolvedSplitterType {
            selector: plan.selector(),
            encoded_value,
        },
    )?;
    for (param, runtime) in target.params.iter_mut().zip(&converted) {
        store(param, runtime);
    }
    Ok(target)
}
