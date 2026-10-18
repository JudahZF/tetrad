//! Pure application state and ordered intents from the pinned UI contracts.
//!
//! Effects do not execute adapters. Bounds and transaction atomicity are owned
//! Rust policies; they are not claims about stock error handling or Qt delivery.

mod lifecycle;
pub use lifecycle::*;

mod parameters;
pub use parameters::*;

mod stomps;
pub use stomps::*;

/// Owned resource limits, not recovered storage cardinality or bank limits.
pub const MAX_SCENE_STORAGE_SLOTS: usize = 256;
pub const MAX_TEXT_UNITS: usize = 4096;
pub const MAX_ALLOWED_MODES: usize = 32;
pub const MAX_EFFECTS_PER_ACTION: usize = 128;
pub const DIALOG_SCENES: usize = 8;
pub const DIRTY_INTERVAL_MS: u64 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum WorkflowError {
    #[error("scene or bank slot must be in 0..=7")]
    InvalidSlot,
    #[error("numeric mode must be in 0..=9")]
    InvalidMode,
    #[error("owned resource limit exceeded")]
    ResourceLimit,
    #[error("scene storage index {index} is outside cardinality {count}")]
    StorageIndex { index: usize, count: usize },
    #[error("cycling requires a nonempty allowed-mode list (owned safety policy)")]
    EmptyModeCycle,
    #[error("scene dialog is not open")]
    NoDialog,
    #[error("scene dialog is already open")]
    DialogAlreadyOpen,
    #[error("clock moved backwards")]
    ClockRegression,
    #[error("clock, context revision or bank arithmetic overflow")]
    Overflow,
    #[error("semantic comparison is for a stale context or another baseline")]
    StaleComparison,
    #[error("adapter {0:?} is unsupported")]
    UnsupportedAdapter(Adapter),
    #[error("parameter position is outside the owned ordinary grid")]
    InvalidParameterIndex,
    #[error("working parameter state is not installed")]
    ParametersNotInstalled,
    #[error("working parameter state is already installed")]
    ParametersAlreadyInstalled,
    #[error("parameter or model address is missing or duplicated")]
    ParameterAddress,
    #[error("parameter route {0:?} is unsupported")]
    UnsupportedParameterPolicy(UnsupportedParameterPolicy),
    #[error("a synchronous Float dispatch predicate is required")]
    UnresolvedFloatDispatch,
    #[error("ordered dispatch facts must match requested target and every direct link")]
    ParameterDispatchCount,
    #[error("nonfinite Float is not a finite DSP value; storage remains unrestricted")]
    NonFiniteDspValue,
    #[error("stomp edits in the alternate preset context are unsupported")]
    AlternateStompContext,
}

type Result<T> = std::result::Result<T, WorkflowError>;

/// Literal UTF-16, including unpaired surrogates. Constructors do not normalize
/// whitespace or locale. The length cap is an owned allocation policy.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UiText(Vec<u16>);

impl UiText {
    pub fn from_units(units: Vec<u16>) -> Result<Self> {
        if units.len() > MAX_TEXT_UNITS {
            return Err(WorkflowError::ResourceLimit);
        }
        Ok(Self(units))
    }

    pub fn from_literal(text: &str) -> Result<Self> {
        Self::from_units(text.encode_utf16().take(MAX_TEXT_UNITS + 1).collect())
    }

    pub fn units(&self) -> &[u16] {
        &self.0
    }

    /// Explicit pinned Qt 4.8.7 content policy, not a live Qt call or QString
    /// nullness model. Trims/collapses its whitespace set without decoding UTF-16.
    pub fn simplified_pinned_qt48(mut self) -> Self {
        let mut written = 0;
        let mut separator = false;
        for read in 0..self.0.len() {
            let unit = self.0[read];
            if matches!(
                unit,
                0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x180e
                    | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000
            ) {
                separator = written != 0;
            } else {
                if separator {
                    self.0[written] = 0x0020;
                    written += 1;
                    separator = false;
                }
                self.0[written] = unit;
                written += 1;
            }
        }
        self.0.truncate(written);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SceneIndex(u8);

impl SceneIndex {
    pub fn new(index: i32) -> Result<Self> {
        if !(0..=7).contains(&index) {
            return Err(WorkflowError::InvalidSlot);
        }
        Ok(Self(index as u8))
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BankSlot(u8);

impl BankSlot {
    pub fn new(index: i32) -> Result<Self> {
        if !(0..=7).contains(&index) {
            return Err(WorkflowError::InvalidSlot);
        }
        Ok(Self(index as u8))
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

/// The caller has checked library/folder policy. No stock bank maximum is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallerValidatedBank(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PresetIndex(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BankPosition {
    pub bank: CallerValidatedBank,
    pub slot: BankSlot,
}

impl BankPosition {
    pub fn absolute_index(self) -> Result<PresetIndex> {
        self.bank
            .0
            .checked_mul(8)
            .and_then(|bank| bank.checked_add(u64::from(self.slot.0)))
            .map(PresetIndex)
            .ok_or(WorkflowError::Overflow)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeId(u8);

impl ModeId {
    pub fn new(value: u8) -> Result<Self> {
        if value > 9 {
            return Err(WorkflowError::InvalidMode);
        }
        Ok(Self(value))
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

/// Order and duplicates are preserved. Empty set is allowed for the setter;
/// cycling an empty list fails under the owned Rust policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AllowedModes(Vec<ModeId>);

impl AllowedModes {
    pub fn new(modes: Vec<ModeId>) -> Result<Self> {
        if modes.len() > MAX_ALLOWED_MODES {
            return Err(WorkflowError::ResourceLimit);
        }
        Ok(Self(modes))
    }

    pub fn as_slice(&self) -> &[ModeId] {
        &self.0
    }
}

impl Default for AllowedModes {
    fn default() -> Self {
        Self(vec![ModeId(0), ModeId(1), ModeId(2)])
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneEntry {
    pub label: UiText,
    pub raw_color: u32,
}

/// Caller-provided storage cardinality, independent of the fixed eight-entry
/// dialog. This is only scene fields, not a complete preset/equality model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneStorage(Vec<SceneEntry>);

impl SceneStorage {
    pub fn new(entries: Vec<SceneEntry>) -> Result<Self> {
        if entries.len() > MAX_SCENE_STORAGE_SLOTS {
            return Err(WorkflowError::ResourceLimit);
        }
        Ok(Self(entries))
    }

    pub fn entries(&self) -> &[SceneEntry] {
        &self.0
    }

    fn entry_mut(&mut self, index: usize) -> Result<&mut SceneEntry> {
        let count = self.0.len();
        self.0
            .get_mut(index)
            .ok_or(WorkflowError::StorageIndex { index, count })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetContext {
    Working,
    Alternate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextStamp {
    pub context: PresetContext,
    pub revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaselineId(pub u64);

/// A caller-supplied semantic result over the complete effective preset, not
/// wire equality or equality of this module's scene-field subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SemanticComparison {
    pub effective: ContextStamp,
    pub baseline: BaselineId,
    pub differs: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Millis(pub u64);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SceneDialogDraft {
    pub labels: [UiText; DIALOG_SCENES],
    pub colors: [u32; DIALOG_SCENES],
    pub selected: SceneIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyObservation {
    pub effective: ContextStamp,
    pub ui_dirty: bool,
    pub last_notified_dirty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BankObservation {
    pub active: bool,
    pub position: BankPosition,
    pub title: UiText,
    pub requested: Option<PresetIndex>,
    pub loaded: Option<PresetIndex>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BankRestore {
    position: BankPosition,
    title: UiText,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowState {
    pub scene: SceneIndex,
    pub previous_scene: SceneIndex,
    pub mode: ModeId,
    pub previous_mode: ModeId,
    pub allowed_modes: AllowedModes,
    pub page: u32,
    pub working: SceneStorage,
    pub alternate: Option<SceneStorage>,
    pub dialog: Option<SceneDialogDraft>,
    pub baseline: BaselineId,
    pub ui_dirty: bool,
    pub last_notified_dirty: bool,
    pub dirty_deadline: Option<Millis>,
    pub bank: BankObservation,
    pub working_parameters: Option<WorkingParameters>,
    pub stomps: StompMaps,
    pub parameter_edit_timers: [Option<ParameterEditTimer>; PARAMETER_CHAINS],
    parameter_timer_generations: [u64; PARAMETER_CHAINS],
    working_revision: u64,
    alternate_revision: u64,
    bank_restore: Option<BankRestore>,
    last_clock: Option<Millis>,
}

impl WorkflowState {
    pub fn effective_stamp(&self) -> ContextStamp {
        match self.alternate {
            Some(_) => ContextStamp {
                context: PresetContext::Alternate,
                revision: self.alternate_revision,
            },
            None => ContextStamp {
                context: PresetContext::Working,
                revision: self.working_revision,
            },
        }
    }

    pub fn effective_scenes(&self) -> &SceneStorage {
        self.alternate.as_ref().unwrap_or(&self.working)
    }

    fn dirty_observation(&self) -> DirtyObservation {
        DirtyObservation {
            effective: self.effective_stamp(),
            ui_dirty: self.ui_dirty,
            last_notified_dirty: self.last_notified_dirty,
        }
    }
}

/// Initialization facts. Mode, page, dirty flags, bank/title and baseline
/// lifecycle are supplied by the caller; their stock initialization/reset
/// policies remain unresolved. Selected/previous scene zero is the recovered
/// controller initialization, not a default scene in preset storage.
#[derive(Debug, Clone)]
pub struct WorkflowInit {
    pub working: SceneStorage,
    pub alternate: Option<SceneStorage>,
    pub baseline: BaselineId,
    pub mode: ModeId,
    pub previous_mode: ModeId,
    pub page: u32,
    pub ui_dirty: bool,
    pub last_notified_dirty: bool,
    pub position: BankPosition,
    pub title: UiText,
    pub requested: Option<PresetIndex>,
    pub loaded: Option<PresetIndex>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Parameter(ParameterAction),
    Stomp(StompAction),
    /// Low-level selection: bypasses the front bank-active guard.
    SelectScene {
        scene: SceneIndex,
        force: bool,
    },
    /// The recovered front action always uses force=false.
    FrontSelectScene(SceneIndex),
    SetAllowedModes(AllowedModes),
    SetMode {
        requested: i32,
        persist: bool,
    },
    ModeShortRelease,
    ModeVisualPress {
        control_color: u32,
    },
    ModeVisualRelease,
    SetSceneLabel {
        index: usize,
        label: UiText,
    },
    SetSceneColor {
        index: usize,
        raw_color: u32,
    },
    /// Colors are observed getter results supplied by the adapter. The read
    /// context of that getter is not guessed from the working-color setter.
    OpenSceneDialog {
        observed_colors: [u32; DIALOG_SCENES],
    },
    SelectDraftScene(SceneIndex),
    SetDraftLabelLiteral(UiText),
    SetDraftColor(u32),
    /// Unasserted/live Qt normalization remains an explicit unsupported adapter.
    NormalizeDraftKeyboard(UiText),
    /// Explicit pinned Qt 4.8.7 UTF-16 content policy; edits only the draft.
    NormalizeDraftKeyboardPinnedQt48(UiText),
    ConfirmSceneDialog,
    CloseSceneDialog,
    ScheduleDirty,
    /// Timer delivery before the current deadline is ignored (restart-safe).
    DirtyTimeout(SemanticComparison),
    ForceDirtyEvaluation(SemanticComparison),
    /// Use after externally observed effective-preset mutations not modeled
    /// here. The caller must do this before supplying another comparison.
    InvalidateEffectiveComparison,
    BeginBank {
        special_folder: bool,
    },
    /// Explicit caller-validated navigation fact. Complete bank-choice policy
    /// is external; this does not request or load a preset.
    SetPreviewBank(CallerValidatedBank),
    SelectPreviewSlot {
        slot: BankSlot,
        resolved_title: UiText,
        empty_title_fallback: UiText,
    },
    CancelBank,
    /// End with restore=false; intentionally has no active guard.
    EndBank,
    /// Records a completed external load's identity only; does not load or
    /// reset baseline/content. Those operations require separate integration.
    RecordLoadedIdentity(Option<PresetIndex>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Adapter {
    Renderer,
    Backend,
    Control,
    Persistence,
    Scheduler,
    ApplicationObserver,
    KeyboardNormalization,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterSupport {
    Unsupported(Adapter),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RendererRequest {
    RefreshScenes,
    RefreshTitle,
    RouteViews,
    ModeButtonColor(u32),
    ShowSceneDialog,
    HideSceneDialog,
    RefreshStomps,
    RefreshDirtyTitle(DirtyObservation),
    BeginBankVisuals,
    EndBankVisuals,
    PreviewTitle(UiText),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendRequest {
    PrepareSceneSelection,
    PrepareModeChange,
    BeginBankHelpers,
    EndBankHelpers,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlRequest {
    SceneLabel {
        index: usize,
        context: PresetContext,
        label: UiText,
    },
    SceneColor {
        index: usize,
        raw_color: u32,
    },
    Mode(ModeId),
    Dirty(DirtyObservation),
    /// Label notification `0x299d04`, then group `0x25fdbc` or single `0x260130`.
    StompLabel {
        stomp: u8,
        grouped: bool,
    },
    /// Momentary-map notification `0x26055c`.
    StompMomentary(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistenceRequest {
    ModeSetting(ModeId),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObserverEvent {
    SceneSelected {
        current: SceneIndex,
        previous: SceneIndex,
    },
    ModeUpdated {
        current: ModeId,
        previous: ModeId,
    },
    PageUpdated(u32),
    DirtyChanged(DirtyObservation),
    BankStarted(BankObservation),
    PresetChanged(BankObservation),
    BankStopped(BankObservation),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    Parameter(ParameterEffect),
    Stomp(StompEffect),
    Renderer(RendererRequest),
    Backend(BackendRequest),
    Control(ControlRequest),
    Persistence(PersistenceRequest),
    Observer(ObserverEvent),
    DirtyTimerRestarted {
        deadline: Millis,
    },
    DirtyTimerStopped,
    /// Explicit chronology marker, after DirtyChanged and control notification.
    DirtyNotificationCacheStored(bool),
    PreviewRestored(BankObservation),
    PreviewEnded(BankObservation),
}

impl Effect {
    /// No adapter implementation is included in this crate. An intent is not
    /// renderer/backend/control execution or a durable persistence result.
    pub fn adapter_support(&self) -> AdapterSupport {
        AdapterSupport::Unsupported(match self {
            Self::Parameter(effect) => effect.adapter(),
            Self::Stomp(_) => Adapter::Backend,
            Self::Renderer(_) => Adapter::Renderer,
            Self::Backend(_) => Adapter::Backend,
            Self::Control(_) => Adapter::Control,
            Self::Persistence(_) => Adapter::Persistence,
            Self::DirtyTimerRestarted { .. } | Self::DirtyTimerStopped => Adapter::Scheduler,
            Self::Observer(_)
            | Self::DirtyNotificationCacheStored(_)
            | Self::PreviewRestored(_)
            | Self::PreviewEnded(_) => Adapter::ApplicationObserver,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationCore {
    state: WorkflowState,
    lifecycle: Option<LifecycleState>,
}

impl ApplicationCore {
    pub fn new(init: WorkflowInit) -> Self {
        Self {
            lifecycle: None,
            state: WorkflowState {
                scene: SceneIndex(0),
                previous_scene: SceneIndex(0),
                mode: init.mode,
                previous_mode: init.previous_mode,
                allowed_modes: AllowedModes::default(),
                page: init.page,
                working: init.working,
                alternate: init.alternate,
                dialog: None,
                baseline: init.baseline,
                ui_dirty: init.ui_dirty,
                last_notified_dirty: init.last_notified_dirty,
                dirty_deadline: None,
                bank: BankObservation {
                    active: false,
                    position: init.position,
                    title: init.title,
                    requested: init.requested,
                    loaded: init.loaded,
                },
                working_revision: 0,
                alternate_revision: 0,
                bank_restore: None,
                last_clock: None,
                working_parameters: None,
                stomps: StompMaps::default(),
                parameter_edit_timers: [None; PARAMETER_CHAINS],
                parameter_timer_generations: [0; PARAMETER_CHAINS],
            },
        }
    }

    pub fn state(&self) -> &WorkflowState {
        &self.state
    }

    /// Executes on a bounded candidate state. Errors return no effects and do
    /// not mutate the core, including its clock, draft or timer. This owned
    /// atomicity is stronger than stock's separate dialog callback emissions.
    pub fn dispatch(&mut self, action: Action, now: Millis) -> Result<Vec<Effect>> {
        if self.state.last_clock.is_some_and(|last| now.0 < last.0) {
            return Err(WorkflowError::ClockRegression);
        }
        let mut candidate = self.clone();
        let mut effects = Vec::new();
        candidate.apply(action, now, &mut effects)?;
        if effects.len() > MAX_EFFECTS_PER_ACTION {
            return Err(WorkflowError::ResourceLimit);
        }
        candidate.state.last_clock = Some(now);
        *self = candidate;
        Ok(effects)
    }

    fn apply(&mut self, action: Action, now: Millis, effects: &mut Vec<Effect>) -> Result<()> {
        match action {
            Action::Parameter(action) => self.apply_parameter(action, now, effects)?,
            Action::Stomp(action) => self.apply_stomp(action, now, effects)?,
            Action::SelectScene { scene, force } => self.select_scene(scene, force, effects),
            Action::FrontSelectScene(scene) => {
                if !self.state.bank.active {
                    self.select_scene(scene, false, effects);
                }
            }
            Action::SetAllowedModes(modes) => self.state.allowed_modes = modes,
            Action::SetMode { requested, persist } => self.set_mode(requested, persist, effects),
            Action::ModeShortRelease => {
                let modes = &self.state.allowed_modes.0;
                if modes.is_empty() {
                    return Err(WorkflowError::EmptyModeCycle);
                }
                let next = modes
                    .iter()
                    .position(|mode| *mode == self.state.mode)
                    .map_or(0, |position| (position + 1) % modes.len());
                self.set_mode(i32::from(modes[next].0), true, effects);
            }
            Action::ModeVisualPress { control_color } => effects.push(Effect::Renderer(
                RendererRequest::ModeButtonColor(control_color),
            )),
            Action::ModeVisualRelease => effects.push(Effect::Renderer(
                RendererRequest::ModeButtonColor(0x001e_1e1e),
            )),
            Action::SetSceneLabel { index, label } => self.set_label(index, label, now, effects)?,
            Action::SetSceneColor { index, raw_color } => {
                self.set_color(index, raw_color, now, effects)?;
            }
            Action::OpenSceneDialog { observed_colors } => {
                if self.state.dialog.is_some() {
                    return Err(WorkflowError::DialogAlreadyOpen);
                }
                let entries = self.state.effective_scenes().entries();
                if entries.len() < DIALOG_SCENES {
                    return Err(WorkflowError::StorageIndex {
                        index: DIALOG_SCENES - 1,
                        count: entries.len(),
                    });
                }
                self.state.dialog = Some(SceneDialogDraft {
                    labels: std::array::from_fn(|index| entries[index].label.clone()),
                    colors: observed_colors,
                    selected: self.state.scene,
                });
                effects.push(Effect::Renderer(RendererRequest::ShowSceneDialog));
            }
            Action::SelectDraftScene(scene) => self.draft_mut()?.selected = scene,
            Action::SetDraftLabelLiteral(label) => {
                let draft = self.draft_mut()?;
                draft.labels[usize::from(draft.selected.0)] = label;
            }
            Action::SetDraftColor(color) => {
                let draft = self.draft_mut()?;
                draft.colors[usize::from(draft.selected.0)] = color;
            }
            Action::NormalizeDraftKeyboard(_) => {
                return Err(WorkflowError::UnsupportedAdapter(
                    Adapter::KeyboardNormalization,
                ));
            }
            Action::NormalizeDraftKeyboardPinnedQt48(label) => {
                let draft = self.draft_mut()?;
                draft.labels[usize::from(draft.selected.0)] = label.simplified_pinned_qt48();
            }
            Action::ConfirmSceneDialog => {
                let draft = self.state.dialog.clone().ok_or(WorkflowError::NoDialog)?;
                for (index, label) in draft.labels.into_iter().enumerate() {
                    self.set_label(index, label, now, effects)?;
                    if draft.colors[index] != 0 {
                        self.set_color(index, draft.colors[index], now, effects)?;
                    }
                }
                self.state.dialog = None;
                effects.push(Effect::Renderer(RendererRequest::HideSceneDialog));
            }
            Action::CloseSceneDialog => {
                if self.state.dialog.take().is_some() {
                    effects.push(Effect::Renderer(RendererRequest::HideSceneDialog));
                }
            }
            Action::ScheduleDirty => self.schedule_dirty(now, effects)?,
            Action::DirtyTimeout(comparison) => {
                self.validate_comparison(comparison)?;
                if self
                    .state
                    .dirty_deadline
                    .is_some_and(|deadline| now.0 >= deadline.0)
                {
                    self.evaluate_dirty(comparison, effects)?;
                }
            }
            Action::ForceDirtyEvaluation(comparison) => self.evaluate_dirty(comparison, effects)?,
            Action::InvalidateEffectiveComparison => {
                self.bump_revision(self.state.effective_stamp().context)?;
            }
            Action::BeginBank { special_folder } => {
                if !self.state.bank.active && !special_folder {
                    self.state.bank.active = true;
                    effects.push(Effect::Renderer(RendererRequest::BeginBankVisuals));
                    effects.push(Effect::Backend(BackendRequest::BeginBankHelpers));
                    self.state.bank_restore = Some(BankRestore {
                        position: self.state.bank.position,
                        title: self.state.bank.title.clone(),
                    });
                    effects.push(Effect::Observer(ObserverEvent::BankStarted(
                        self.state.bank.clone(),
                    )));
                }
            }
            Action::SetPreviewBank(bank) => self.state.bank.position.bank = bank,
            Action::SelectPreviewSlot {
                slot,
                resolved_title,
                empty_title_fallback,
            } => {
                let position = BankPosition {
                    slot,
                    ..self.state.bank.position
                };
                self.state.bank.requested = Some(position.absolute_index()?);
                self.state.bank.position.slot = slot;
                if self.state.bank.active {
                    effects.push(Effect::Observer(ObserverEvent::PresetChanged(
                        self.state.bank.clone(),
                    )));
                }
                let title = if resolved_title.0.is_empty() {
                    empty_title_fallback
                } else {
                    resolved_title
                };
                self.state.bank.title = title.clone();
                effects.push(Effect::Renderer(RendererRequest::PreviewTitle(title)));
            }
            Action::CancelBank => {
                if self.state.bank.active {
                    let restore = self
                        .state
                        .bank_restore
                        .as_ref()
                        .expect("active bank has snapshot");
                    let requested = restore.position.absolute_index()?;
                    self.end_bank(true, effects);
                    self.state.bank.requested = Some(requested);
                    effects.push(Effect::PreviewRestored(self.state.bank.clone()));
                    effects.push(Effect::Renderer(RendererRequest::RouteViews));
                }
            }
            Action::EndBank => {
                self.end_bank(false, effects);
                effects.push(Effect::PreviewEnded(self.state.bank.clone()));
                effects.push(Effect::Renderer(RendererRequest::RouteViews));
            }
            Action::RecordLoadedIdentity(loaded) => self.state.bank.loaded = loaded,
        }
        Ok(())
    }

    fn select_scene(&mut self, scene: SceneIndex, force: bool, effects: &mut Vec<Effect>) {
        if self.state.scene != scene || force {
            self.state.previous_scene = self.state.scene;
            self.state.scene = scene;
            effects.push(Effect::Backend(BackendRequest::PrepareSceneSelection));
            effects.push(Effect::Observer(ObserverEvent::SceneSelected {
                current: scene,
                previous: self.state.previous_scene,
            }));
        }
    }

    fn set_mode(&mut self, requested: i32, persist: bool, effects: &mut Vec<Effect>) {
        let Some(first) = self.state.allowed_modes.0.first().copied() else {
            return;
        };
        let resolved = self
            .state
            .allowed_modes
            .0
            .iter()
            .copied()
            .find(|mode| i32::from(mode.0) == requested)
            .unwrap_or(first);
        if resolved == self.state.mode {
            return;
        }
        effects.push(Effect::Backend(BackendRequest::PrepareModeChange));
        self.state.previous_mode = self.state.mode;
        self.state.mode = resolved;
        if persist {
            effects.push(Effect::Persistence(PersistenceRequest::ModeSetting(
                resolved,
            )));
        }
        effects.push(Effect::Observer(ObserverEvent::ModeUpdated {
            current: resolved,
            previous: self.state.previous_mode,
        }));
        effects.push(Effect::Observer(ObserverEvent::PageUpdated(
            self.state.page,
        )));
        effects.push(Effect::Control(ControlRequest::Mode(resolved)));
    }

    fn draft_mut(&mut self) -> Result<&mut SceneDialogDraft> {
        self.state.dialog.as_mut().ok_or(WorkflowError::NoDialog)
    }

    fn bump_revision(&mut self, context: PresetContext) -> Result<()> {
        let revision = match context {
            PresetContext::Working => &mut self.state.working_revision,
            PresetContext::Alternate => &mut self.state.alternate_revision,
        };
        *revision = revision.checked_add(1).ok_or(WorkflowError::Overflow)?;
        Ok(())
    }

    fn set_label(
        &mut self,
        index: usize,
        label: UiText,
        now: Millis,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        let context = self.state.effective_stamp().context;
        match self.state.alternate.as_mut() {
            Some(alternate) => alternate.entry_mut(index)?.label = label.clone(),
            None => self.state.working.entry_mut(index)?.label = label.clone(),
        }
        self.bump_revision(context)?;
        effects.push(Effect::Control(ControlRequest::SceneLabel {
            index,
            context,
            label,
        }));
        effects.push(Effect::Renderer(RendererRequest::RefreshScenes));
        self.schedule_dirty(now, effects)?;
        effects.push(Effect::Renderer(RendererRequest::RefreshTitle));
        Ok(())
    }

    fn set_color(
        &mut self,
        index: usize,
        raw_color: u32,
        now: Millis,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        self.state.working.entry_mut(index)?.raw_color = raw_color;
        self.bump_revision(PresetContext::Working)?;
        effects.push(Effect::Control(ControlRequest::SceneColor {
            index,
            raw_color,
        }));
        effects.push(Effect::Renderer(RendererRequest::RefreshScenes));
        self.schedule_dirty(now, effects)?;
        effects.push(Effect::Renderer(RendererRequest::RouteViews));
        effects.push(Effect::Renderer(RendererRequest::RefreshTitle));
        Ok(())
    }

    fn schedule_dirty(&mut self, now: Millis, effects: &mut Vec<Effect>) -> Result<()> {
        if effects.len() >= MAX_EFFECTS_PER_ACTION {
            return Err(WorkflowError::ResourceLimit);
        }
        let deadline = Millis(
            now.0
                .checked_add(DIRTY_INTERVAL_MS)
                .ok_or(WorkflowError::Overflow)?,
        );
        self.state.dirty_deadline = Some(deadline);
        effects.push(Effect::DirtyTimerRestarted { deadline });
        Ok(())
    }

    fn evaluate_dirty(
        &mut self,
        comparison: SemanticComparison,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        self.validate_comparison(comparison)?;
        self.state.dirty_deadline = None;
        effects.push(Effect::DirtyTimerStopped);
        self.state.ui_dirty = comparison.differs;
        let observation = self.state.dirty_observation();
        effects.push(Effect::Renderer(RendererRequest::RefreshDirtyTitle(
            observation,
        )));
        if comparison.differs != self.state.last_notified_dirty {
            effects.push(Effect::Observer(ObserverEvent::DirtyChanged(observation)));
            effects.push(Effect::Control(ControlRequest::Dirty(observation)));
        }
        self.state.last_notified_dirty = comparison.differs;
        effects.push(Effect::DirtyNotificationCacheStored(comparison.differs));
        Ok(())
    }

    fn validate_comparison(&self, comparison: SemanticComparison) -> Result<()> {
        if comparison.effective != self.state.effective_stamp()
            || comparison.baseline != self.state.baseline
        {
            return Err(WorkflowError::StaleComparison);
        }
        Ok(())
    }

    fn end_bank(&mut self, restore: bool, effects: &mut Vec<Effect>) {
        self.state.bank.active = false;
        effects.push(Effect::Renderer(RendererRequest::EndBankVisuals));
        effects.push(Effect::Backend(BackendRequest::EndBankHelpers));
        effects.push(Effect::Observer(ObserverEvent::BankStopped(
            self.state.bank.clone(),
        )));
        if restore && let Some(saved) = self.state.bank_restore.take() {
            self.state.bank.position = saved.position;
            self.state.bank.title = saved.title;
        }
        self.state.bank_restore = None;
    }
}

#[cfg(test)]
mod tests;
