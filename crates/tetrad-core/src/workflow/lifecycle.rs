//! Restricted lifecycle transitions. Full snapshots stay in a caller-owned arena.
//! One caller arena per attached lifecycle: each handle names one immutable object.
//! Provenance is metadata/evidence, never an implicit arena namespace.
//! Bounded local checks cannot prove hidden content or cross-call immutability.
//! Handle equality is identity only: no preset comparator or adapter executes here.
//! Resolved facts are caller assertions, not evidence that stock gates are closed.

use super::*;

pub const SAVE_INTERVAL_MS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotHandle(pub u64);

/// Caller capture/schema evidence. This is metadata, not a handle namespace.
/// Synthetic tests use invented provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Provenance(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageIdentity {
    pub identity: u32,
    pub key: u64,
}

/// Metadata of an opaque complete snapshot, never a projection claiming completeness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullSnapshot {
    pub handle: SnapshotHandle,
    pub provenance: Provenance,
    pub default_scene: SceneIndex,
    pub storage: Option<StorageIdentity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BoundSnapshot {
    pub stamp: ContextStamp,
    pub snapshot: FullSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StorageTarget {
    pub folder: u64,
    pub index: i32,
}

/// Independent partial projection into the existing ApplicationCore stores.
/// Full graph, descriptors, caches, labels and assignments remain in the arena.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkingProjection {
    pub scenes: SceneStorage,
    pub parameters: Option<WorkingParameters>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredFact {
    CategoryNormalization,
    EffectiveBankAndRuntimeCacheReset,
    SavePreparation,
    CallbackReentrancy,
    FullSnapshotCapture,
    LoaderFallbackAndMigration,
    StorageExecutionAndDurability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallbackPolicy {
    Unresolved,
    /// Owned synchronous delivery boundary, supplied explicitly by the caller.
    CallerResolvedNonReentrant(Provenance),
}

/// Full output of caller-resolved normalization and effective bank/cache reset.
/// Runtime caches are not inferred from ordinary parameter values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadFacts {
    pub normalized_from: FullSnapshot,
    pub normalized_working: FullSnapshot,
    pub normalized_projection: WorkingProjection,
    pub reset_from: FullSnapshot,
    pub reset_effective: FullSnapshot,
    /// Required only when working is effective; alternate banks are opaque.
    pub reset_working_projection: Option<WorkingProjection>,
    pub normalization_evidence: Provenance,
    pub reset_evidence: Provenance,
    pub callbacks: CallbackPolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadInputs {
    pub selected_target: StorageTarget,
    /// Original installer r1, copied to baseline; may differ from r2.
    pub baseline_input_r1: FullSnapshot,
    /// Original installer r2, copied to temporary then forced into working.
    pub working_input_r2: FullSnapshot,
    pub facts: Option<LoadFacts>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveStamp {
    pub working: ContextStamp,
    pub effective: ContextStamp,
    pub working_snapshot: FullSnapshot,
    pub effective_snapshot: FullSnapshot,
    pub baseline: BaselineId,
    pub selected: SceneIndex,
    pub current_target: StorageTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedCandidate {
    pub copied_from: BoundSnapshot,
    pub selected: SceneIndex,
    /// Caller actually applied helper 0x51898c and selected->defaultScene.
    pub candidate: FullSnapshot,
    pub preparation_evidence: Provenance,
    pub callbacks: CallbackPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveToken {
    pub generation: u64,
    pub captured: SaveStamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InFlightSave {
    pub token: SaveToken,
    pub target: StorageTarget,
    pub candidate: FullSnapshot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveTimer {
    pub generation: u64,
    pub deadline: Millis,
    pub baseline: BaselineId,
    pub target: StorageTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveTimerPolicy {
    Unresolved,
    /// Observed folder policy getter returned zero (0x7054f0).
    CallerObservedZero(Provenance),
    CallerObservedNonZero,
}

/// Successful callback reports the actual saved object, including returned key.
/// Storage may have transformed it; it is not assumed to equal the candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportedSavedSnapshot {
    pub target: StorageTarget,
    pub saved: FullSnapshot,
    /// Caller-provided FINAL effective capture after both success updates.
    pub effective_after_default: FullSnapshot,
    /// Caller-provided FINAL working capture after identity and effective-default
    /// updates. With working effective, both fields name the SAME final capture,
    /// not two distinct intermediate stages. With alternate effective, working
    /// retains its defaultScene. No external store is rolled back on refusal.
    pub working_after_identity: FullSnapshot,
    pub callbacks: CallbackPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageReport {
    ReportedFailure { code: std::num::NonZeroU32 },
    ReportedSuccess(ReportedSavedSnapshot),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleAction {
    Attach {
        working: FullSnapshot,
        alternate: Option<FullSnapshot>,
        baseline: FullSnapshot,
        current_target: StorageTarget,
    },
    /// Refresh an opaque snapshot at the exact current context stamp. A binding
    /// still at that stamp cannot change known semantic defaultScene. Advance
    /// the revision by an ordinary edit or InvalidateEffectiveComparison first
    /// for such a change. New handles alone do not prove changed content.
    BindSnapshot(BoundSnapshot),
    Load(Box<LoadInputs>),
    BeginSave {
        target: StorageTarget,
        prepared: Option<PreparedCandidate>,
    },
    StorageResult {
        token: SaveToken,
        report: StorageReport,
    },
    /// Owned abandonment only; no cancellation or rollback of external storage.
    AbandonSave(SaveToken),
    ScheduleSave(SaveTimerPolicy),
    SaveTimeout(SaveTimer),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleEffect {
    Core(Effect),
    BaselineCopied {
        baseline: BaselineId,
        snapshot: FullSnapshot,
    },
    ForcedWorkingInstalled {
        source_r2: SnapshotHandle,
        normalized: FullSnapshot,
    },
    CategoryNormalizationReported {
        evidence: Provenance,
    },
    EffectiveSecondToPrimaryResetReported {
        context: PresetContext,
        evidence: Provenance,
    },
    Required {
        fact: RequiredFact,
        snapshot: FullSnapshot,
    },
    CandidatePrepared {
        source: BoundSnapshot,
        selected: SceneIndex,
        candidate: FullSnapshot,
        preparation_evidence: Provenance,
    },
    StorageRequested(InFlightSave),
    StorageFailureObserved {
        code: std::num::NonZeroU32,
    },
    StorageSuccessObserved {
        saved: FullSnapshot,
    },
    WorkingIdentityUpdated(StorageIdentity),
    EffectiveDefaultSceneUpdated {
        context: PresetContext,
        scene: SceneIndex,
    },
    OrdinaryLoaderRequired {
        target: StorageTarget,
        saved: FullSnapshot,
    },
    SaveTimerRestarted(SaveTimer),
    /// Request capture/preparation at current core revisions; this carries no
    /// claim that cached opaque snapshots still describe those revisions.
    SaveRequested {
        target: StorageTarget,
        working: ContextStamp,
        effective: ContextStamp,
        baseline: BaselineId,
        selected: SceneIndex,
    },
    SaveAbandoned(SaveToken),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LifecycleError {
    #[error(transparent)]
    Core(#[from] WorkflowError),
    #[error("lifecycle is not attached")]
    NotAttached,
    #[error("lifecycle is already attached")]
    AlreadyAttached,
    #[error("required caller fact is unresolved: {0:?}")]
    Unresolved(RequiredFact),
    #[error("snapshot, result or timer is stale")]
    Stale,
    #[error("caller projection or transition facts contradict metadata")]
    InconsistentFacts,
    #[error("a save request is already in flight")]
    ReentrantSave,
    #[error("load during save requires unresolved callback/order policy")]
    LoadDuringSave,
}

type LifecycleResult<T> = std::result::Result<T, LifecycleError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleState {
    pub working: BoundSnapshot,
    pub alternate: Option<BoundSnapshot>,
    /// Identity is WorkflowState::baseline, not another independently tracked ID.
    pub baseline_snapshot: FullSnapshot,
    pub current_target: StorageTarget,
    pub in_flight: Option<InFlightSave>,
    pub save_timer: Option<SaveTimer>,
    save_generation: u64,
    timer_generation: u64,
}

impl LifecycleState {
    pub fn effective(&self) -> BoundSnapshot {
        self.alternate.unwrap_or(self.working)
    }
}

impl ApplicationCore {
    pub fn lifecycle(&self) -> Option<&LifecycleState> {
        self.lifecycle.as_ref()
    }

    pub fn save_stamp(&self) -> LifecycleResult<SaveStamp> {
        let lifecycle = self.lifecycle.as_ref().ok_or(LifecycleError::NotAttached)?;
        self.validate_bound(lifecycle.working)?;
        self.validate_bound(lifecycle.effective())?;
        Ok(SaveStamp {
            working: self.context_stamp(PresetContext::Working),
            effective: self.state.effective_stamp(),
            working_snapshot: lifecycle.working.snapshot,
            effective_snapshot: lifecycle.effective().snapshot,
            baseline: self.state.baseline,
            selected: self.state.scene,
            current_target: lifecycle.current_target,
        })
    }

    /// Atomic errors are this bounded core's owned policy, not a stock transaction.
    /// Outputs are inert. Executing callbacks while consuming them is unsupported.
    pub fn dispatch_lifecycle(
        &mut self,
        action: LifecycleAction,
        now: Millis,
    ) -> LifecycleResult<Vec<LifecycleEffect>> {
        if self.state.last_clock.is_some_and(|last| now.0 < last.0) {
            return Err(WorkflowError::ClockRegression.into());
        }
        validate_snapshot_aliases(self.lifecycle.as_ref(), &action)?;
        let mut candidate = self.clone();
        let mut effects = Vec::new();
        candidate.apply_lifecycle(action, now, &mut effects)?;
        if effects.len() > MAX_EFFECTS_PER_ACTION {
            return Err(WorkflowError::ResourceLimit.into());
        }
        candidate.state.last_clock = Some(now);
        *self = candidate;
        Ok(effects)
    }

    fn context_stamp(&self, context: PresetContext) -> ContextStamp {
        ContextStamp {
            context,
            revision: match context {
                PresetContext::Working => self.state.working_revision,
                PresetContext::Alternate => self.state.alternate_revision,
            },
        }
    }

    fn apply_lifecycle(
        &mut self,
        action: LifecycleAction,
        now: Millis,
        effects: &mut Vec<LifecycleEffect>,
    ) -> LifecycleResult<()> {
        if let LifecycleAction::Attach {
            working,
            alternate,
            baseline,
            current_target,
        } = action
        {
            if self.lifecycle.is_some() {
                return Err(LifecycleError::AlreadyAttached);
            }
            if alternate.is_some() != self.state.alternate.is_some() {
                return Err(LifecycleError::InconsistentFacts);
            }
            self.validate_projection_default(working, self.state.working_parameters.as_ref())?;
            self.lifecycle = Some(LifecycleState {
                working: BoundSnapshot {
                    stamp: self.context_stamp(PresetContext::Working),
                    snapshot: working,
                },
                alternate: alternate.map(|snapshot| BoundSnapshot {
                    stamp: self.context_stamp(PresetContext::Alternate),
                    snapshot,
                }),
                baseline_snapshot: baseline,
                current_target,
                in_flight: None,
                save_timer: None,
                save_generation: 0,
                timer_generation: 0,
            });
            return Ok(());
        }
        let mut lifecycle = self.lifecycle.take().ok_or(LifecycleError::NotAttached)?;
        match action {
            LifecycleAction::Attach { .. } => unreachable!("handled above"),
            LifecycleAction::BindSnapshot(bound) => {
                if bound.stamp != self.context_stamp(bound.stamp.context) {
                    return Err(LifecycleError::Stale);
                }
                let retained = match bound.stamp.context {
                    PresetContext::Working => Some(lifecycle.working),
                    PresetContext::Alternate => lifecycle.alternate,
                };
                if retained.is_some_and(|previous| {
                    previous.stamp == bound.stamp
                        && previous.snapshot.default_scene != bound.snapshot.default_scene
                }) {
                    return Err(LifecycleError::InconsistentFacts);
                }
                match bound.stamp.context {
                    PresetContext::Working => {
                        self.validate_projection_default(
                            bound.snapshot,
                            self.state.working_parameters.as_ref(),
                        )?;
                        lifecycle.working = bound;
                    }
                    PresetContext::Alternate => {
                        if self.state.alternate.is_none() {
                            return Err(LifecycleError::InconsistentFacts);
                        }
                        lifecycle.alternate = Some(bound);
                    }
                }
            }
            LifecycleAction::Load(inputs) => {
                self.lifecycle_load(&mut lifecycle, *inputs, now, effects)?
            }
            LifecycleAction::BeginSave { target, prepared } => {
                if lifecycle.in_flight.is_some() {
                    return Err(LifecycleError::ReentrantSave);
                }
                self.validate_bound(lifecycle.working)?;
                self.validate_bound(lifecycle.effective())?;
                let prepared =
                    prepared.ok_or(LifecycleError::Unresolved(RequiredFact::SavePreparation))?;
                require_callbacks(prepared.callbacks)?;
                if prepared.copied_from != lifecycle.effective()
                    || prepared.selected != self.state.scene
                    || prepared.candidate.default_scene != self.state.scene
                {
                    return Err(LifecycleError::Stale);
                }
                lifecycle.save_generation = lifecycle
                    .save_generation
                    .checked_add(1)
                    .ok_or(WorkflowError::Overflow)?;
                let token = SaveToken {
                    generation: lifecycle.save_generation,
                    captured: self.stamp_for(&lifecycle),
                };
                let flight = InFlightSave {
                    token,
                    target,
                    candidate: prepared.candidate,
                };
                lifecycle.in_flight = Some(flight);
                effects.push(LifecycleEffect::CandidatePrepared {
                    source: prepared.copied_from,
                    selected: prepared.selected,
                    candidate: prepared.candidate,
                    preparation_evidence: prepared.preparation_evidence,
                });
                effects.push(LifecycleEffect::StorageRequested(flight));
                effects.push(LifecycleEffect::Required {
                    fact: RequiredFact::StorageExecutionAndDurability,
                    snapshot: prepared.candidate,
                });
            }
            LifecycleAction::StorageResult { token, report } => {
                let flight = lifecycle
                    .in_flight
                    .filter(|flight| flight.token == token)
                    .ok_or(LifecycleError::Stale)?;
                match report {
                    StorageReport::ReportedFailure { code } => {
                        lifecycle.in_flight = None;
                        effects.push(LifecycleEffect::StorageFailureObserved { code });
                    }
                    StorageReport::ReportedSuccess(saved) => {
                        if token.captured != self.stamp_for(&lifecycle) {
                            return Err(LifecycleError::Stale);
                        }
                        self.lifecycle_saved(&mut lifecycle, flight, saved, now, effects)?;
                        lifecycle.in_flight = None;
                    }
                }
            }
            LifecycleAction::AbandonSave(token) => {
                if lifecycle
                    .in_flight
                    .is_none_or(|flight| flight.token != token)
                {
                    return Err(LifecycleError::Stale);
                }
                lifecycle.in_flight = None;
                effects.push(LifecycleEffect::SaveAbandoned(token));
            }
            LifecycleAction::ScheduleSave(policy) => match policy {
                SaveTimerPolicy::Unresolved => {
                    return Err(LifecycleError::Unresolved(
                        RequiredFact::StorageExecutionAndDurability,
                    ));
                }
                SaveTimerPolicy::CallerObservedNonZero => {}
                SaveTimerPolicy::CallerObservedZero(_) => {
                    if lifecycle
                        .baseline_snapshot
                        .storage
                        .is_some_and(|storage| storage.identity != 0)
                    {
                        lifecycle.timer_generation = lifecycle
                            .timer_generation
                            .checked_add(1)
                            .ok_or(WorkflowError::Overflow)?;
                        let timer = SaveTimer {
                            generation: lifecycle.timer_generation,
                            deadline: Millis(
                                now.0
                                    .checked_add(SAVE_INTERVAL_MS)
                                    .ok_or(WorkflowError::Overflow)?,
                            ),
                            baseline: self.state.baseline,
                            target: lifecycle.current_target,
                        };
                        lifecycle.save_timer = Some(timer);
                        effects.push(LifecycleEffect::SaveTimerRestarted(timer));
                    }
                }
            },
            LifecycleAction::SaveTimeout(timer) => {
                if lifecycle.save_timer == Some(timer) && now.0 >= timer.deadline.0 {
                    if timer.baseline != self.state.baseline
                        || timer.target != lifecycle.current_target
                    {
                        return Err(LifecycleError::Stale);
                    }
                    if lifecycle.in_flight.is_some() {
                        return Err(LifecycleError::ReentrantSave);
                    }
                    lifecycle.save_timer = None;
                    effects.push(LifecycleEffect::SaveRequested {
                        target: timer.target,
                        working: self.context_stamp(PresetContext::Working),
                        effective: self.state.effective_stamp(),
                        baseline: self.state.baseline,
                        selected: self.state.scene,
                    });
                }
            }
        }
        self.lifecycle = Some(lifecycle);
        Ok(())
    }

    fn stamp_for(&self, lifecycle: &LifecycleState) -> SaveStamp {
        SaveStamp {
            working: self.context_stamp(PresetContext::Working),
            effective: self.state.effective_stamp(),
            working_snapshot: lifecycle.working.snapshot,
            effective_snapshot: lifecycle.effective().snapshot,
            baseline: self.state.baseline,
            selected: self.state.scene,
            current_target: lifecycle.current_target,
        }
    }

    fn validate_bound(&self, bound: BoundSnapshot) -> LifecycleResult<()> {
        if bound.stamp != self.context_stamp(bound.stamp.context) {
            return Err(LifecycleError::Unresolved(
                RequiredFact::FullSnapshotCapture,
            ));
        }
        Ok(())
    }

    fn validate_projection_default(
        &self,
        snapshot: FullSnapshot,
        parameters: Option<&WorkingParameters>,
    ) -> LifecycleResult<()> {
        if parameters.is_some_and(|parameters| parameters.default_scene() != snapshot.default_scene)
        {
            return Err(LifecycleError::InconsistentFacts);
        }
        Ok(())
    }

    fn lifecycle_load(
        &mut self,
        lifecycle: &mut LifecycleState,
        inputs: LoadInputs,
        now: Millis,
        effects: &mut Vec<LifecycleEffect>,
    ) -> LifecycleResult<()> {
        if lifecycle.in_flight.is_some() {
            return Err(LifecycleError::LoadDuringSave);
        }
        let facts = inputs.facts.ok_or(LifecycleError::Unresolved(
            RequiredFact::CategoryNormalization,
        ))?;
        require_callbacks(facts.callbacks)?;
        if facts.normalized_from != inputs.working_input_r2 {
            return Err(LifecycleError::InconsistentFacts);
        }
        self.validate_projection_default(
            facts.normalized_working,
            facts.normalized_projection.parameters.as_ref(),
        )?;
        let reset_source = if let Some(alternate) = lifecycle.alternate {
            self.validate_bound(alternate)?;
            alternate.snapshot
        } else {
            facts.normalized_working
        };
        if facts.reset_from != reset_source
            || facts.reset_effective.default_scene != reset_source.default_scene
            || facts.reset_effective.storage != reset_source.storage
        {
            return Err(LifecycleError::InconsistentFacts);
        }
        let working_projection = match lifecycle.alternate {
            Some(_) => {
                if facts.reset_working_projection.is_some() {
                    return Err(LifecycleError::InconsistentFacts);
                }
                facts.normalized_projection
            }
            None => {
                let projection =
                    facts
                        .reset_working_projection
                        .ok_or(LifecycleError::Unresolved(
                            RequiredFact::EffectiveBankAndRuntimeCacheReset,
                        ))?;
                self.validate_projection_default(
                    facts.reset_effective,
                    projection.parameters.as_ref(),
                )?;
                validate_bank_reset(&facts.normalized_projection, &projection)?;
                projection
            }
        };
        // Real register ordering: r1 baseline, r2 default selection, forced install,
        // normalization, effective bank/cache reset. No guessed backend success.
        lifecycle.current_target = inputs.selected_target;
        self.copy_lifecycle_baseline(lifecycle, inputs.baseline_input_r1, effects)?;
        let mut core_effects = Vec::new();
        self.select_scene(
            inputs.working_input_r2.default_scene,
            false,
            &mut core_effects,
        );
        effects.extend(core_effects.into_iter().map(LifecycleEffect::Core));
        self.bump_revision(PresetContext::Working)?;
        self.state.working = working_projection.scenes;
        self.state.working_parameters = working_projection.parameters;
        lifecycle.working = BoundSnapshot {
            stamp: self.context_stamp(PresetContext::Working),
            snapshot: if lifecycle.alternate.is_some() {
                facts.normalized_working
            } else {
                facts.reset_effective
            },
        };
        effects.push(LifecycleEffect::ForcedWorkingInstalled {
            source_r2: inputs.working_input_r2.handle,
            normalized: facts.normalized_working,
        });
        effects.push(LifecycleEffect::CategoryNormalizationReported {
            evidence: facts.normalization_evidence,
        });
        if lifecycle.alternate.is_some() {
            self.bump_revision(PresetContext::Alternate)?;
            lifecycle.alternate = Some(BoundSnapshot {
                stamp: self.context_stamp(PresetContext::Alternate),
                snapshot: facts.reset_effective,
            });
        }
        effects.push(LifecycleEffect::EffectiveSecondToPrimaryResetReported {
            context: self.state.effective_stamp().context,
            evidence: facts.reset_evidence,
        });
        effects.push(LifecycleEffect::Required {
            fact: RequiredFact::LoaderFallbackAndMigration,
            snapshot: facts.reset_effective,
        });
        self.lifecycle_dirty(now, effects)
    }

    fn lifecycle_saved(
        &mut self,
        lifecycle: &mut LifecycleState,
        flight: InFlightSave,
        reported: ReportedSavedSnapshot,
        now: Millis,
        effects: &mut Vec<LifecycleEffect>,
    ) -> LifecycleResult<()> {
        require_callbacks(reported.callbacks)?;
        let storage = reported
            .saved
            .storage
            .ok_or(LifecycleError::InconsistentFacts)?;
        if reported.target != flight.target
            || reported.saved.default_scene != flight.token.captured.selected
            || reported.working_after_identity.storage != Some(storage)
            || reported.effective_after_default.default_scene != flight.token.captured.selected
        {
            return Err(LifecycleError::InconsistentFacts);
        }
        if lifecycle.alternate.is_some() {
            if reported.working_after_identity.default_scene
                != lifecycle.working.snapshot.default_scene
                || reported.effective_after_default.storage
                    != lifecycle.effective().snapshot.storage
            {
                return Err(LifecycleError::InconsistentFacts);
            }
        } else if reported.working_after_identity != reported.effective_after_default {
            return Err(LifecycleError::InconsistentFacts);
        }
        effects.push(LifecycleEffect::StorageSuccessObserved {
            saved: reported.saved,
        });
        self.bump_revision(PresetContext::Working)?;
        lifecycle.working = BoundSnapshot {
            stamp: self.context_stamp(PresetContext::Working),
            snapshot: reported.working_after_identity,
        };
        effects.push(LifecycleEffect::WorkingIdentityUpdated(storage));
        if lifecycle.alternate.is_some() {
            self.bump_revision(PresetContext::Alternate)?;
            lifecycle.alternate = Some(BoundSnapshot {
                stamp: self.context_stamp(PresetContext::Alternate),
                snapshot: reported.effective_after_default,
            });
        } else if let Some(parameters) = self.state.working_parameters.as_mut() {
            parameters.default_scene = self.state.scene;
        }
        effects.push(LifecycleEffect::EffectiveDefaultSceneUpdated {
            context: self.state.effective_stamp().context,
            scene: self.state.scene,
        });
        if flight.target == lifecycle.current_target {
            self.copy_lifecycle_baseline(lifecycle, reported.saved, effects)?;
            self.lifecycle_dirty(now, effects)?;
        } else {
            effects.push(LifecycleEffect::OrdinaryLoaderRequired {
                target: flight.target,
                saved: reported.saved,
            });
        }
        Ok(())
    }

    fn copy_lifecycle_baseline(
        &mut self,
        lifecycle: &mut LifecycleState,
        snapshot: FullSnapshot,
        effects: &mut Vec<LifecycleEffect>,
    ) -> LifecycleResult<()> {
        self.state.baseline = BaselineId(
            self.state
                .baseline
                .0
                .checked_add(1)
                .ok_or(WorkflowError::Overflow)?,
        );
        lifecycle.baseline_snapshot = snapshot;
        effects.push(LifecycleEffect::BaselineCopied {
            baseline: self.state.baseline,
            snapshot,
        });
        Ok(())
    }

    fn lifecycle_dirty(
        &mut self,
        now: Millis,
        effects: &mut Vec<LifecycleEffect>,
    ) -> LifecycleResult<()> {
        let mut core_effects = Vec::new();
        self.schedule_dirty(now, &mut core_effects)?;
        effects.extend(core_effects.into_iter().map(LifecycleEffect::Core));
        Ok(())
    }
}

/// At most twelve locally visible facts, including stale retained bindings and
/// captured save-token facts. Every output snapshot is supplied by these inputs
/// or retained facts. Check before mutation; no unbounded arena registry is kept.
fn validate_snapshot_aliases(
    lifecycle: Option<&LifecycleState>,
    action: &LifecycleAction,
) -> LifecycleResult<()> {
    let retained = lifecycle.map_or([None; 6], |state| {
        let flight = state.in_flight;
        [
            Some(state.working.snapshot),
            state.alternate.map(|bound| bound.snapshot),
            Some(state.baseline_snapshot),
            flight.map(|save| save.candidate),
            flight.map(|save| save.token.captured.working_snapshot),
            flight.map(|save| save.token.captured.effective_snapshot),
        ]
    });
    let incoming = match action {
        LifecycleAction::Attach {
            working,
            alternate,
            baseline,
            ..
        } => [
            Some(*working),
            *alternate,
            Some(*baseline),
            None,
            None,
            None,
        ],
        LifecycleAction::BindSnapshot(bound) => {
            [Some(bound.snapshot), None, None, None, None, None]
        }
        LifecycleAction::Load(inputs) => [
            Some(inputs.baseline_input_r1),
            Some(inputs.working_input_r2),
            inputs.facts.as_ref().map(|facts| facts.normalized_from),
            inputs.facts.as_ref().map(|facts| facts.normalized_working),
            inputs.facts.as_ref().map(|facts| facts.reset_from),
            inputs.facts.as_ref().map(|facts| facts.reset_effective),
        ],
        LifecycleAction::BeginSave { prepared, .. } => [
            prepared.map(|facts| facts.copied_from.snapshot),
            prepared.map(|facts| facts.candidate),
            None,
            None,
            None,
            None,
        ],
        LifecycleAction::StorageResult { token, report } => {
            let saved = match report {
                StorageReport::ReportedSuccess(saved) => Some(saved),
                StorageReport::ReportedFailure { .. } => None,
            };
            [
                Some(token.captured.working_snapshot),
                Some(token.captured.effective_snapshot),
                saved.map(|facts| facts.saved),
                saved.map(|facts| facts.working_after_identity),
                saved.map(|facts| facts.effective_after_default),
                None,
            ]
        }
        LifecycleAction::AbandonSave(token) => [
            Some(token.captured.working_snapshot),
            Some(token.captured.effective_snapshot),
            None,
            None,
            None,
            None,
        ],
        LifecycleAction::ScheduleSave(_) | LifecycleAction::SaveTimeout(_) => [None; 6],
    };
    let facts = [retained, incoming];
    let snapshots = facts.iter().flatten().flatten();
    // FullSnapshot PartialEq compares metadata ONLY, never semantic preset content.
    // The handle is identity in this single arena; all its metadata must agree.
    for (index, left) in snapshots.clone().enumerate() {
        for right in snapshots.clone().skip(index + 1) {
            if left.handle == right.handle && left != right {
                return Err(LifecycleError::InconsistentFacts);
            }
        }
    }
    Ok(())
}

fn require_callbacks(policy: CallbackPolicy) -> LifecycleResult<()> {
    if matches!(policy, CallbackPolicy::Unresolved) {
        return Err(LifecycleError::Unresolved(RequiredFact::CallbackReentrancy));
    }
    Ok(())
}

/// Verify only the supported ordinary bank projection. Full cache reset still
/// requires the caller's opaque output and evidence. No schema inference.
fn validate_bank_reset(
    before: &WorkingProjection,
    after: &WorkingProjection,
) -> LifecycleResult<()> {
    let mut expected = before.clone();
    if let Some(parameters) = expected.parameters.as_mut() {
        parameters.lifecycle_reset_ordinary_banks();
    }
    if expected != *after {
        return Err(LifecycleError::InconsistentFacts);
    }
    Ok(())
}

#[cfg(test)]
#[path = "lifecycle/tests.rs"]
mod tests;
