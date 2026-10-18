//! Caller-resolved ordinary working-grid state. No catalog compiler, QVariant
//! conversion, transport execution, or complete preset comparison is implied.

use super::*;

pub const PARAMETER_CHAINS: usize = 4;
pub const PARAMETER_COLUMNS: usize = 8;
pub const PARAMETER_EDIT_INTERVAL_MS: u64 = 500;
pub const MAX_WORKING_PARAMETERS: usize = 256;
pub const MAX_PARAMETER_LINKS: usize = 32;
pub const MAX_PARAMETER_TEXT_UNITS: usize = 65_536;

/// Raw IEEE-754 storage, including NaN payloads, infinities and signed zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FloatBits(pub u32);

impl FloatBits {
    /// Optional owned DSP-domain gate. Compatibility storage does not call it.
    pub fn finite_dsp_value(self) -> Result<f32> {
        let value = f32::from_bits(self.0);
        if value.is_finite() {
            Ok(value)
        } else {
            Err(WorkflowError::NonFiniteDspValue)
        }
    }
}

/// Live QVariant alternatives, distinct from the file-import ParameterValue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveParameterValue {
    Float(FloatBits),
    Int(i32),
    UInt(u32),
    String(UiText),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainIndex(u8);

impl ChainIndex {
    pub fn new(index: i32) -> Result<Self> {
        if !(0..PARAMETER_CHAINS as i32).contains(&index) {
            return Err(WorkflowError::InvalidParameterIndex);
        }
        Ok(Self(index as u8))
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParameterIndex(u8);

impl ParameterIndex {
    pub fn new(index: i32) -> Result<Self> {
        if !(0..=99).contains(&index) {
            return Err(WorkflowError::InvalidParameterIndex);
        }
        Ok(Self(index as u8))
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParameterAddress {
    pub chain: ChainIndex,
    pub column: BankSlot,
    pub parameter: ParameterIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterRoute {
    /// Explicit caller resolution, never inferred solely from a model ID.
    Ordinary,
    Unresolved,
    Mute,
    Bypass,
    Momentary,
    Selector,
    Coupled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CouplingFacts {
    ResolvedAbsent,
    Present,
    Unresolved,
}

/// Declarative resolved facts, not an allowlist of all known catalog IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParameterModelFacts {
    pub model_id: u32,
    pub category_id: u32,
    pub column_identity: u32,
    /// None is unresolved; runtime kind 1 is outside this slice.
    pub runtime_kind: Option<u32>,
    pub coupling: CouplingFacts,
    pub force: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedParameterPolicy {
    AlternateContext,
    Force,
    Route(ParameterRoute),
    Coupling,
    RuntimeKind,
    SpecialColumn,
    SpecialModel,
    SpecialCategory,
}

/// These kinds describe the control helper's metadata branch. They do not
/// convert exact stored alternatives to Qt toInt/toFloat/toString values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterControlKind {
    Float,
    Int,
    String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParameterControlFacts {
    pub session_supported: bool,
    pub kind: ParameterControlKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParameterMetadata {
    pub route: ParameterRoute,
    pub binding_valid: bool,
    pub visual_target_exists: bool,
    pub model_block_needs_repaint: bool,
    pub control: ParameterControlFacts,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingParameter {
    pub position: ParameterIndex,
    pub primary: [LiveParameterValue; DIALOG_SCENES],
    pub second: [LiveParameterValue; DIALOG_SCENES],
    pub scene_mode: bool,
    pub metadata: ParameterMetadata,
    /// Order and duplicates survive; only this list is traversed by mode actions.
    pub linked_scene_mode: Vec<ParameterAddress>,
    /// Distinct metadata, never expanded as linked-scene targets.
    pub dependencies: Vec<ParameterAddress>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingParameterModel {
    pub chain: ChainIndex,
    pub column: BankSlot,
    pub facts: ParameterModelFacts,
    pub parameters: Vec<WorkingParameter>,
}

/// Bounded working state within ApplicationCore, sharing its scene and revision.
/// Second banks are not assumed to be a save baseline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingParameters {
    pub(super) default_scene: SceneIndex,
    models: Vec<WorkingParameterModel>,
}

impl WorkingParameters {
    // Restricted ordinary projection only; runtime caches remain caller facts.
    pub(super) fn lifecycle_reset_ordinary_banks(&mut self) {
        for model in &mut self.models {
            for parameter in &mut model.parameters {
                parameter.primary.clone_from(&parameter.second);
            }
        }
    }

    pub fn new(default_scene: SceneIndex, models: Vec<WorkingParameterModel>) -> Result<Self> {
        if models.len() > PARAMETER_CHAINS * PARAMETER_COLUMNS {
            return Err(WorkflowError::ResourceLimit);
        }
        let mut parameters = 0_usize;
        let mut text_units = 0_usize;
        for (index, model) in models.iter().enumerate() {
            if models[..index]
                .iter()
                .any(|other| other.chain == model.chain && other.column == model.column)
            {
                return Err(WorkflowError::ParameterAddress);
            }
            parameters = parameters
                .checked_add(model.parameters.len())
                .ok_or(WorkflowError::Overflow)?;
            if parameters > MAX_WORKING_PARAMETERS {
                return Err(WorkflowError::ResourceLimit);
            }
            for (index, parameter) in model.parameters.iter().enumerate() {
                if model.parameters[..index]
                    .iter()
                    .any(|other| other.position == parameter.position)
                {
                    return Err(WorkflowError::ParameterAddress);
                }
                if parameter.linked_scene_mode.len() > MAX_PARAMETER_LINKS
                    || parameter.dependencies.len() > MAX_PARAMETER_LINKS
                {
                    return Err(WorkflowError::ResourceLimit);
                }
                for value in parameter.primary.iter().chain(&parameter.second) {
                    text_units = text_units
                        .checked_add(value.text_units())
                        .ok_or(WorkflowError::Overflow)?;
                    if text_units > MAX_PARAMETER_TEXT_UNITS {
                        return Err(WorkflowError::ResourceLimit);
                    }
                }
            }
        }
        let state = Self {
            default_scene,
            models,
        };
        for model in &state.models {
            for parameter in &model.parameters {
                for &target in parameter
                    .linked_scene_mode
                    .iter()
                    .chain(&parameter.dependencies)
                {
                    state.parameter(target)?;
                }
            }
        }
        Ok(state)
    }

    pub fn default_scene(&self) -> SceneIndex {
        self.default_scene
    }

    pub fn models(&self) -> &[WorkingParameterModel] {
        &self.models
    }

    pub fn parameter(&self, target: ParameterAddress) -> Result<&WorkingParameter> {
        self.model(target)?
            .parameters
            .iter()
            .find(|parameter| parameter.position == target.parameter)
            .ok_or(WorkflowError::ParameterAddress)
    }

    fn model(&self, target: ParameterAddress) -> Result<&WorkingParameterModel> {
        self.models
            .iter()
            .find(|model| model.chain == target.chain && model.column == target.column)
            .ok_or(WorkflowError::ParameterAddress)
    }

    fn parameter_mut(&mut self, target: ParameterAddress) -> Result<&mut WorkingParameter> {
        self.models
            .iter_mut()
            .find(|model| model.chain == target.chain && model.column == target.column)
            .and_then(|model| {
                model
                    .parameters
                    .iter_mut()
                    .find(|p| p.position == target.parameter)
            })
            .ok_or(WorkflowError::ParameterAddress)
    }

    fn validate_text_budget(&self) -> Result<()> {
        let units = self
            .models
            .iter()
            .flat_map(|model| &model.parameters)
            .flat_map(|parameter| parameter.primary.iter().chain(&parameter.second))
            .try_fold(0_usize, |units, value| {
                units
                    .checked_add(value.text_units())
                    .ok_or(WorkflowError::Overflow)
            })?;
        if units > MAX_PARAMETER_TEXT_UNITS {
            return Err(WorkflowError::ResourceLimit);
        }
        Ok(())
    }
}

impl LiveParameterValue {
    fn text_units(&self) -> usize {
        match self {
            Self::String(text) => text.units().len(),
            _ => 0,
        }
    }
}

/// Explicit synchronous dispatch factor supplied by the caller for this turn.
/// Predicate(false) is stock backend false, not validation failure or rollback.
/// An unresolved predicate is a safe boundary error when +48 must be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloatDispatchPredicate {
    Resolved(bool),
    Unresolved,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterWriteSource {
    /// Recovered source code 1.
    PrimaryOnly,
    /// Recovered ordinary-bank behavior for source 0 and all other source codes.
    BothBanks,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParameterEditTimer {
    pub deadline: Millis,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParameterAction {
    /// Install once; no extra WorkflowInit fields, save, baseline reset or dirty.
    InstallWorkingParameters(WorkingParameters),
    SetDefaultScene(SceneIndex),
    /// Low-level copy: no UI, backend or dirty timer effects.
    SetStoredParameterScene {
        target: ParameterAddress,
        scene: SceneIndex,
        value: LiveParameterValue,
        source: ParameterWriteSource,
    },
    EditorParameterEdit {
        target: ParameterAddress,
        requested: LiveParameterValue,
        float_dispatch: FloatDispatchPredicate,
    },
    EnableParameterSceneMode {
        target: ParameterAddress,
    },
    DisableParameterSceneMode {
        target: ParameterAddress,
        /// Requested target then its direct links, including duplicates.
        float_dispatch: Vec<FloatDispatchPredicate>,
    },
    ParameterEditTimeout {
        chain: ChainIndex,
        timer: ParameterEditTimer,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParameterBank {
    Primary,
    Second,
}

/// Snapshot at emission, before subsequent links/general-setter phases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterObservation {
    pub target: ParameterAddress,
    pub effective: ContextStamp,
    pub selected_scene: SceneIndex,
    pub default_scene: SceneIndex,
    pub scene_mode: bool,
    pub primary: [LiveParameterValue; DIALOG_SCENES],
    pub second: [LiveParameterValue; DIALOG_SCENES],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParameterEffect {
    /// Owned chronology marker, not a recovered Qt signal.
    StoredSceneWritten {
        target: ParameterAddress,
        scene: SceneIndex,
        bank: ParameterBank,
        value: LiveParameterValue,
    },
    VisualFloat48 {
        target: ParameterAddress,
        requested: FloatBits,
    },
    ModelBlockNeedsRepaint {
        target: ParameterAddress,
    },
    /// Records the decision after repaint. Not an accepted value/acknowledgement.
    FloatDispatchEvaluated {
        target: ParameterAddress,
        predicate: bool,
    },
    /// Raw request intent. DSP value validity and transport require separate gates.
    GenericFloatSend {
        target: ParameterAddress,
        requested: FloatBits,
    },
    /// Raw UInt request; unresolved Qt toInt overflow is deliberately not guessed.
    UIntHelper {
        target: ParameterAddress,
        requested: u32,
    },
    VisualString52 {
        target: ParameterAddress,
        requested: UiText,
    },
    StoredCurrentSceneControl {
        observation: Box<ParameterObservation>,
        metadata_kind: ParameterControlKind,
        stored: LiveParameterValue,
    },
    VisualFloat96 {
        target: ParameterAddress,
        requested: FloatBits,
    },
    SceneModeSignal(Box<ParameterObservation>),
    SceneModeControl {
        observation: Box<ParameterObservation>,
        enabled: bool,
    },
    EditTimerStopped {
        chain: ChainIndex,
    },
    EditTimerRestarted {
        chain: ChainIndex,
        timer: ParameterEditTimer,
    },
    ChainDirty {
        chain: ChainIndex,
    },
}

impl ParameterEffect {
    pub(super) fn adapter(&self) -> Adapter {
        match self {
            Self::VisualFloat48 { .. }
            | Self::GenericFloatSend { .. }
            | Self::UIntHelper { .. }
            | Self::VisualString52 { .. } => Adapter::Backend,
            Self::ModelBlockNeedsRepaint { .. } | Self::VisualFloat96 { .. } => Adapter::Renderer,
            Self::StoredCurrentSceneControl { .. } | Self::SceneModeControl { .. } => {
                Adapter::Control
            }
            Self::EditTimerStopped { .. } | Self::EditTimerRestarted { .. } => Adapter::Scheduler,
            Self::StoredSceneWritten { .. }
            | Self::SceneModeSignal(_)
            | Self::FloatDispatchEvaluated { .. }
            | Self::ChainDirty { .. } => Adapter::ApplicationObserver,
        }
    }
}

#[derive(Clone, Copy)]
enum ParameterOperation {
    Edit,
    Enable,
    Disable,
}

impl ApplicationCore {
    pub(super) fn apply_parameter(
        &mut self,
        action: ParameterAction,
        now: Millis,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        // Timeouts can forward into the currently effective context; no bank
        // mutation or alternate editor behavior is implied by timer delivery.
        if let ParameterAction::ParameterEditTimeout { chain, timer } = action {
            let index = usize::from(chain.0);
            if self.state.parameter_edit_timers[index] == Some(timer) && now.0 >= timer.deadline.0 {
                self.state.parameter_edit_timers[index] = None;
                emit(effects, ParameterEffect::ChainDirty { chain })?;
                self.schedule_dirty(now, effects)?;
            }
            return Ok(());
        }
        if self.state.alternate.is_some() {
            return Err(unsupported(UnsupportedParameterPolicy::AlternateContext));
        }
        match action {
            ParameterAction::InstallWorkingParameters(parameters) => {
                if self.state.working_parameters.is_some() {
                    return Err(WorkflowError::ParametersAlreadyInstalled);
                }
                self.bump_revision(PresetContext::Working)?;
                self.state.working_parameters = Some(parameters);
            }
            ParameterAction::SetDefaultScene(scene) => {
                self.parameters_mut()?.default_scene = scene;
                self.bump_revision(PresetContext::Working)?;
            }
            ParameterAction::SetStoredParameterScene {
                target,
                scene,
                value,
                source,
            } => {
                self.validate_parameter(target, ParameterOperation::Edit)?;
                self.bump_revision(PresetContext::Working)?;
                self.write_parameter(target, scene, &value, source)?;
            }
            ParameterAction::EditorParameterEdit {
                target,
                requested,
                float_dispatch,
            } => {
                self.validate_parameter(target, ParameterOperation::Edit)?;
                self.validate_float_dispatch(target, &requested, float_dispatch)?;
                self.general_parameter_setter(target, &requested, float_dispatch, effects)?;
                self.parameter_value_control(target, effects)?;
                let metadata = self.parameters()?.parameter(target)?.metadata;
                if let LiveParameterValue::Float(requested) = requested
                    && metadata.visual_target_exists
                {
                    emit(
                        effects,
                        ParameterEffect::VisualFloat96 { target, requested },
                    )?;
                }
                let index = usize::from(target.chain.0);
                let generation = self.state.parameter_timer_generations[index]
                    .checked_add(1)
                    .ok_or(WorkflowError::Overflow)?;
                let timer = ParameterEditTimer {
                    deadline: Millis(
                        now.0
                            .checked_add(PARAMETER_EDIT_INTERVAL_MS)
                            .ok_or(WorkflowError::Overflow)?,
                    ),
                    generation,
                };
                self.state.parameter_edit_timers[index] = None;
                emit(
                    effects,
                    ParameterEffect::EditTimerStopped {
                        chain: target.chain,
                    },
                )?;
                self.state.parameter_timer_generations[index] = generation;
                self.state.parameter_edit_timers[index] = Some(timer);
                emit(
                    effects,
                    ParameterEffect::EditTimerRestarted {
                        chain: target.chain,
                        timer,
                    },
                )?;
            }
            ParameterAction::EnableParameterSceneMode { target } => {
                let targets = self.scene_mode_targets(target, ParameterOperation::Enable)?;
                for target in targets {
                    self.parameters_mut()?.parameter_mut(target)?.scene_mode = true;
                    self.bump_revision(PresetContext::Working)?;
                    self.parameter_scene_mode_signal(target, now, effects)?;
                    self.parameter_scene_mode_control(target, true, effects)?;
                }
            }
            ParameterAction::DisableParameterSceneMode {
                target,
                float_dispatch,
            } => {
                let targets = self.scene_mode_targets(target, ParameterOperation::Disable)?;
                if float_dispatch.len() != targets.len() {
                    return Err(WorkflowError::ParameterDispatchCount);
                }
                // Audit every direct target and required dispatch factor before
                // mutation. Duplicate turns snapshot again during execution.
                for (&target, &predicate) in targets.iter().zip(&float_dispatch) {
                    let parameters = self.parameters()?;
                    let value = &parameters.parameter(target)?.primary
                        [usize::from(parameters.default_scene.0)];
                    self.validate_float_dispatch(target, value, predicate)?;
                }
                // Each turn emits at least 32 bank-copy markers, one signal
                // and one dirty restart, even with every adapter gate off.
                // Three turns fit even the maximal 40 effects per turn; four
                // cannot fit. Duplicates consume the same budget as other turns.
                if targets.len() > MAX_EFFECTS_PER_ACTION / (4 * DIALOG_SCENES + 2) {
                    return Err(WorkflowError::ResourceLimit);
                }
                for (target, predicate) in targets.into_iter().zip(float_dispatch) {
                    self.parameters_mut()?.parameter_mut(target)?.scene_mode = false;
                    self.bump_revision(PresetContext::Working)?;
                    let parameters = self.parameters()?;
                    let value = parameters.parameter(target)?.primary
                        [usize::from(parameters.default_scene.0)]
                    .clone();
                    self.store_parameter_scenes(target, &value, true, effects)?;
                    self.parameter_scene_mode_signal(target, now, effects)?;
                    self.parameter_scene_mode_control(target, false, effects)?;
                    self.parameter_value_control(target, effects)?;
                    self.general_parameter_setter(target, &value, predicate, effects)?;
                }
            }
            ParameterAction::ParameterEditTimeout { .. } => unreachable!("handled above"),
        }
        self.parameters()?.validate_text_budget()?;
        Ok(())
    }

    fn parameters(&self) -> Result<&WorkingParameters> {
        self.state
            .working_parameters
            .as_ref()
            .ok_or(WorkflowError::ParametersNotInstalled)
    }

    fn parameters_mut(&mut self) -> Result<&mut WorkingParameters> {
        self.state
            .working_parameters
            .as_mut()
            .ok_or(WorkflowError::ParametersNotInstalled)
    }

    fn validate_parameter(
        &self,
        target: ParameterAddress,
        operation: ParameterOperation,
    ) -> Result<()> {
        let parameters = self.parameters()?;
        let facts = parameters.model(target)?.facts;
        let parameter = parameters.parameter(target)?;
        let policy = if facts.force {
            Some(UnsupportedParameterPolicy::Force)
        } else if parameter.metadata.route != ParameterRoute::Ordinary {
            Some(UnsupportedParameterPolicy::Route(parameter.metadata.route))
        } else if facts.coupling != CouplingFacts::ResolvedAbsent {
            Some(UnsupportedParameterPolicy::Coupling)
        } else if facts.runtime_kind.is_none() || facts.runtime_kind == Some(1) {
            Some(UnsupportedParameterPolicy::RuntimeKind)
        } else if matches!(facts.column_identity, 11000 | 23000 | 25000 | 28000 | 10004) {
            Some(UnsupportedParameterPolicy::SpecialColumn)
        } else if facts.category_id == 26 {
            Some(UnsupportedParameterPolicy::SpecialCategory)
        } else {
            let special_model = match operation {
                ParameterOperation::Edit => {
                    (facts.model_id == 27000 && matches!(target.parameter.0, 19 | 20))
                        || (facts.model_id == 26001 && target.parameter.0 == 1)
                }
                ParameterOperation::Enable => matches!(facts.model_id, 27000 | 7040),
                ParameterOperation::Disable => {
                    matches!(facts.model_id, 27000 | 7040 | 18007 | 29001..=29008 | 3008)
                }
            };
            special_model.then_some(UnsupportedParameterPolicy::SpecialModel)
        };
        if let Some(policy) = policy {
            return Err(unsupported(policy));
        }
        Ok(())
    }

    fn scene_mode_targets(
        &self,
        target: ParameterAddress,
        operation: ParameterOperation,
    ) -> Result<Vec<ParameterAddress>> {
        self.validate_parameter(target, operation)?;
        let parameter = self.parameters()?.parameter(target)?;
        let targets: Vec<_> = std::iter::once(target)
            .chain(parameter.linked_scene_mode.iter().copied())
            .collect();
        for &target in &targets {
            self.validate_parameter(target, operation)?;
            // Disable also calls the general editor setter's ordinary routes.
            if matches!(operation, ParameterOperation::Disable) {
                self.validate_parameter(target, ParameterOperation::Edit)?;
            }
        }
        Ok(targets)
    }

    fn validate_float_dispatch(
        &self,
        target: ParameterAddress,
        value: &LiveParameterValue,
        predicate: FloatDispatchPredicate,
    ) -> Result<()> {
        let metadata = self.parameters()?.parameter(target)?.metadata;
        if matches!(value, LiveParameterValue::Float(_))
            && metadata.binding_valid
            && metadata.visual_target_exists
            && predicate == FloatDispatchPredicate::Unresolved
        {
            return Err(WorkflowError::UnresolvedFloatDispatch);
        }
        Ok(())
    }

    fn write_parameter(
        &mut self,
        target: ParameterAddress,
        scene: SceneIndex,
        value: &LiveParameterValue,
        source: ParameterWriteSource,
    ) -> Result<()> {
        let parameter = self.parameters_mut()?.parameter_mut(target)?;
        let index = usize::from(scene.0);
        if source == ParameterWriteSource::BothBanks {
            parameter.second[index] = value.clone();
        }
        parameter.primary[index] = value.clone();
        Ok(())
    }

    fn store_parameter_scenes(
        &mut self,
        target: ParameterAddress,
        value: &LiveParameterValue,
        all_scenes: bool,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        self.bump_revision(PresetContext::Working)?;
        let scenes = if all_scenes {
            0..DIALOG_SCENES as u8
        } else {
            self.state.scene.0..self.state.scene.0 + 1
        };
        for index in scenes {
            let scene = SceneIndex(index);
            // Emit chronology after each individual bank assignment.
            self.parameters_mut()?.parameter_mut(target)?.second[usize::from(index)] =
                value.clone();
            emit(
                effects,
                ParameterEffect::StoredSceneWritten {
                    target,
                    scene,
                    bank: ParameterBank::Second,
                    value: value.clone(),
                },
            )?;
            self.parameters_mut()?.parameter_mut(target)?.primary[usize::from(index)] =
                value.clone();
            emit(
                effects,
                ParameterEffect::StoredSceneWritten {
                    target,
                    scene,
                    bank: ParameterBank::Primary,
                    value: value.clone(),
                },
            )?;
        }
        Ok(())
    }

    fn general_parameter_setter(
        &mut self,
        target: ParameterAddress,
        requested: &LiveParameterValue,
        float_dispatch: FloatDispatchPredicate,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        let parameter = self.parameters()?.parameter(target)?;
        let all_scenes = !parameter.scene_mode;
        let metadata = parameter.metadata;
        self.store_parameter_scenes(target, requested, all_scenes, effects)?;
        if !metadata.binding_valid {
            return Ok(());
        }
        match requested {
            LiveParameterValue::Float(requested) => {
                if metadata.visual_target_exists {
                    emit(
                        effects,
                        ParameterEffect::VisualFloat48 {
                            target,
                            requested: *requested,
                        },
                    )?;
                }
                if metadata.model_block_needs_repaint {
                    emit(effects, ParameterEffect::ModelBlockNeedsRepaint { target })?;
                }
                let predicate = if !metadata.visual_target_exists {
                    true
                } else if let FloatDispatchPredicate::Resolved(value) = float_dispatch {
                    value
                } else {
                    return Err(WorkflowError::UnresolvedFloatDispatch);
                };
                emit(
                    effects,
                    ParameterEffect::FloatDispatchEvaluated { target, predicate },
                )?;
                if predicate {
                    emit(
                        effects,
                        ParameterEffect::GenericFloatSend {
                            target,
                            requested: *requested,
                        },
                    )?;
                }
            }
            LiveParameterValue::UInt(requested) => {
                emit(
                    effects,
                    ParameterEffect::UIntHelper {
                        target,
                        requested: *requested,
                    },
                )?;
            }
            LiveParameterValue::String(requested) if metadata.visual_target_exists => {
                emit(
                    effects,
                    ParameterEffect::VisualString52 {
                        target,
                        requested: requested.clone(),
                    },
                )?;
            }
            LiveParameterValue::Int(_) | LiveParameterValue::String(_) => {}
        }
        Ok(())
    }

    fn parameter_observation(&self, target: ParameterAddress) -> Result<ParameterObservation> {
        let parameters = self.parameters()?;
        let parameter = parameters.parameter(target)?;
        Ok(ParameterObservation {
            target,
            effective: self.state.effective_stamp(),
            selected_scene: self.state.scene,
            default_scene: parameters.default_scene,
            scene_mode: parameter.scene_mode,
            primary: parameter.primary.clone(),
            second: parameter.second.clone(),
        })
    }

    fn parameter_value_control(
        &self,
        target: ParameterAddress,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        let control = self.parameters()?.parameter(target)?.metadata.control;
        if control.session_supported {
            let observation = self.parameter_observation(target)?;
            let stored = observation.primary[usize::from(self.state.scene.0)].clone();
            emit(
                effects,
                ParameterEffect::StoredCurrentSceneControl {
                    observation: Box::new(observation),
                    metadata_kind: control.kind,
                    stored,
                },
            )?;
        }
        Ok(())
    }

    fn parameter_scene_mode_signal(
        &mut self,
        target: ParameterAddress,
        now: Millis,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        emit(
            effects,
            ParameterEffect::SceneModeSignal(Box::new(self.parameter_observation(target)?)),
        )?;
        // Resolved synchronous listener delivery into the same shared dirty timer.
        self.schedule_dirty(now, effects)
    }

    fn parameter_scene_mode_control(
        &self,
        target: ParameterAddress,
        enabled: bool,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        if self
            .parameters()?
            .parameter(target)?
            .metadata
            .control
            .session_supported
        {
            emit(
                effects,
                ParameterEffect::SceneModeControl {
                    observation: Box::new(self.parameter_observation(target)?),
                    enabled,
                },
            )?;
        }
        Ok(())
    }
}

fn unsupported(policy: UnsupportedParameterPolicy) -> WorkflowError {
    WorkflowError::UnsupportedParameterPolicy(policy)
}

fn emit(effects: &mut Vec<Effect>, effect: ParameterEffect) -> Result<()> {
    if effects.len() >= MAX_EFFECTS_PER_ACTION {
        return Err(WorkflowError::ResourceLimit);
    }
    effects.push(Effect::Parameter(effect));
    Ok(())
}

#[cfg(test)]
mod tests;
