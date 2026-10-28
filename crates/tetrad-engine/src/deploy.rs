//! Incremental deployment: the stock DSPController update rules in plan terms.
//!
//! | Change | Stock behaviour | Here |
//! | --- | --- | --- |
//! | Parameter (`0x51e4d8`) | ParameterChange to the slot, no recompile; all 8 scenes unless the parameter is in scene mode | [`Deployment::set_parameter`] |
//! | String parameter (`0x51e4d8` → driver `+0x34`) | the driver reloads the IR or capture it names | [`Deployment::set_text`] |
//! | Junction parameter (cases 0x2714, 0x2af8) | split/merge router and head writes (`0x582278`, `0x581f98`) | [`Deployment::set_parameter`]: [`PlanOp::Junction`] |
//! | Lane output control (case 0x59d8) | send-router gain and pan (`0x581de0`) | [`Deployment::set_parameter`]: [`PlanOp::Lane`] |
//! | Input gate (case 0x6d60) | gate message 0x31, or the input router's return gain | [`Deployment::set_input_gate`] |
//! | Bypass (`0x521514`) | ModelBypass, sidechain router active flag, latency recompute; same scene rule | [`Deployment::set_bypass`] |
//! | Scene (`0x527248`) | every value of the push, not diffed; latency | [`Deployment::select_scene`] |
//! | Add / replace / remove / move / split-mix | full recompile, unchanged pairs keep their cores (`0x5efa08`); a failed fit keeps the previous preset (`0x585464`) | [`Deployment::edit`] |
//! | Preset change | recompile; reuse only when the whole preset is equal (`0x2e2490`) | [`Deployment::load`] |
//! | Dormant model (O) | stock loads it | its blocks stay in the layout but leave the plan ([`Deployment::dsp_layout`]) |
//!
//! Every change works on a copy and commits only after it compiles. Ops name
//! plan placements; typed router writes, alignment delays and gate messages are
//! plan data ([`DeploymentPlan::delay_writes`], [`crate::Deployed::setup`],
//! [`DeploymentPlan::gate_messages`]). Junction and lane output values stay
//! normalized: the sender converts them with the stock fused laws.

use std::collections::BTreeSet;

use tetrad_catalog::{Specs, ids};

use crate::gate::{GateMessage, INPUT_GAIN, input_gain, input_gain_index, stored_input_gain};
use crate::plan::{ROUTERLESS_SIDECHAIN, SidechainScene, compile_keeping, compile_stored};
use crate::{
    COLUMNS, CompileError, CompileOptions, DeploymentPlan, DspCore, Endpoint, Layout, ModelId,
    Node, Pair, PairDelay, Placement, ROWS, Row, SceneIndex, Scened, Site, Slot, StoredMasks,
};

/// One incremental DSP change.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlanOp {
    /// ParameterChange (0x02) of a float value.
    SetParam {
        at: Placement,
        index: u32,
        value: f32,
    },
    /// ModelBypass (0x0c), batched as BulkModelBypass (0x3d) in pushes.
    SetBypass { at: Placement, bypassed: bool },
    /// Sidechain receive router `SidechainActive` = not bypassed (`0x517228`);
    /// skipped for 16008, 16013, 18012 and 30001. `router` is the sink's
    /// receive router in the plan.
    SidechainActive {
        sink: Placement,
        router: Option<Placement>,
        active: bool,
    },
    /// RouteToSidechainBuffer (0x48) or …SameCore (0x49) plus router ports.
    Sidechain {
        sink: Placement,
        scene: SidechainScene,
    },
    /// The alignment of one pair changed (`0x87c11c`): send the scene's
    /// [`DeploymentPlan::delay_writes`].
    PairDelay { pair: Pair, delay: PairDelay },
    /// A string parameter changed (cab `ir selector`, IR Loader `IR PATH`,
    /// capture `file_name`): the block's text hook reloads the scene text of
    /// [`crate::Deployed::texts`]. Stock never sends the string itself.
    Text { at: Placement, index: u32 },
    /// Push-all step of one row pair (`0x583078`): the splitter (`0x51e2b8`)
    /// then the mixer (`0x511548`) from their scene values, written to the
    /// pair's split and merge routers and crossover heads.
    PushJunctions { pair: Pair },
    /// One splitter or mixer value changed (`0x51e4d8` cases 0x2714,
    /// 0x2af8; the mixer's BYPASS index for its bypass, `0x514378`): the
    /// sender re-reads the scene values from the layout.
    Junction { site: Site, index: u32 },
    /// One LaneOutputControl (23000) value of a row changed (`0x51e4d8`
    /// case 0x59d8); the sender re-reads the lanes. The push's lane values
    /// come from the sender's lane I/O step, not from ops.
    Lane { row: Row, index: u32 },
}

/// What a change requires of the DSP.
#[derive(Debug, Clone, PartialEq)]
pub enum Update {
    /// Values only, in stock push order.
    Ops(Vec<PlanOp>),
    /// The preset is unchanged: stop the cores, re-push all values, restart
    /// (`0x5848b0` result 1). No model is reloaded and no router setup is sent.
    Reuse(Vec<PlanOp>),
    /// Full recompile: DeleteAllModels on `clear`, then load `plan`, send its
    /// router setup and push `ops` (`0x582570`, `0x879100`, `0x583078`).
    Redeploy {
        clear: Vec<DspCore>,
        plan: Box<DeploymentPlan>,
        ops: Vec<PlanOp>,
    },
}

/// An input gate edit (`0x51e4d8` case 0x6d60).
#[derive(Debug, Clone, PartialEq)]
pub struct GateUpdate {
    /// INPUT GAIN: the lane input router's return gain (`0x581a78`).
    pub ops: Vec<PlanOp>,
    /// Gate messages: the parameter (0x31) or, for INPUT GAIN, the trim (0x32).
    pub gate: Vec<GateMessage>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum DeployError {
    #[error(transparent)]
    Compile(#[from] CompileError),
    #[error(transparent)]
    Edit(#[from] crate::EditError),
    #[error("{0:?} holds no deployed model")]
    NotDeployed(Site),
    #[error("{site:?} has no parameter {index}")]
    Parameter { site: Site, index: u32 },
    #[error("{site:?} parameter {index}: {what} is not supported")]
    Unsupported {
        site: Site,
        index: u32,
        what: &'static str,
    },
}

/// LaneOutputControl parameters (model 23000).
const VOLUME: u32 = 0;

/// The deployed preset: layout, plan and selected scene.
#[derive(Debug, Clone, PartialEq)]
pub struct Deployment {
    layout: Layout,
    /// (O) Models whose grid blocks load no DSP code (the app's unlicensed plugin models;
    /// issue #17: the DSP never acknowledges the run-list clear of some of them). Their
    /// blocks stay in `layout`, and edits to them are stored, but they leave `dsp_layout`.
    dormant: BTreeSet<ModelId>,
    /// `layout` without the dormant blocks: the layout `plan` was compiled from.
    dsp_layout: Layout,
    plan: DeploymentPlan,
    scene: SceneIndex,
    options: CompileOptions,
}

impl Deployment {
    /// Compiles a preset for an empty DSP. The update lists no cores to clear.
    pub fn new(
        layout: Layout,
        specs: &Specs,
        options: CompileOptions,
        scene: SceneIndex,
    ) -> Result<(Self, Update), DeployError> {
        let stored = StoredMasks::default();
        Self::new_stored(layout, specs, options, scene, stored, BTreeSet::new())
    }

    /// [`Deployment::new`] with the preset's stored core masks (stock load) and the
    /// dormant models.
    pub fn new_stored(
        layout: Layout,
        specs: &Specs,
        options: CompileOptions,
        scene: SceneIndex,
        stored: StoredMasks,
        dormant: BTreeSet<ModelId>,
    ) -> Result<(Self, Update), DeployError> {
        let dsp_layout = without_dormant(&layout, &dormant, specs);
        let plan = compile_stored(&dsp_layout, specs, options, stored)?;
        let ops = push_ops(&plan, &dsp_layout, scene);
        let update = Update::Redeploy {
            clear: Vec::new(),
            plan: Box::new(plan.clone()),
            ops,
        };
        Ok((
            Self {
                layout,
                dormant,
                dsp_layout,
                plan,
                scene,
                options,
            },
            update,
        ))
    }

    /// The whole layout, dormant blocks included: what the grid shows and a save writes.
    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The layout the DSP runs: [`Self::layout`] without the dormant blocks.
    pub fn dsp_layout(&self) -> &Layout {
        &self.dsp_layout
    }

    pub fn plan(&self) -> &DeploymentPlan {
        &self.plan
    }

    pub fn scene(&self) -> SceneIndex {
        self.scene
    }

    /// Compiles an edited layout for a value edit: the DSP layout and its plan.
    fn recompile(&self, layout: &Layout, specs: &Specs) -> Result<Compiled, DeployError> {
        let dsp_layout = without_dormant(layout, &self.dormant, specs);
        let plan = compile_keeping(
            &dsp_layout,
            specs,
            self.options,
            Some(&self.plan.assignment),
        )?;
        Ok(Compiled { dsp_layout, plan })
    }

    /// The plan placement of a grid block. `None` for a dormant block, which has none.
    fn placement(
        &self,
        plan: &DeploymentPlan,
        layout: &Layout,
        site: Site,
    ) -> Result<Option<Placement>, DeployError> {
        let Site::Slot(slot) = site else {
            return Err(DeployError::NotDeployed(site));
        };
        match plan.block(slot) {
            Some(at) => Ok(Some(at)),
            None if layout
                .instance(site)
                .is_some_and(|instance| self.dormant.contains(&instance.model)) =>
            {
                Ok(None)
            }
            None => Err(DeployError::NotDeployed(site)),
        }
    }

    /// Sets one parameter with the stock scene rule (all scenes unless the
    /// parameter is in scene mode). A sidechain selector or input parameter
    /// re-routes its sink (`0x585b2c` → `0x5843a4`). Junction and lane output
    /// edits are normalized ops ([`PlanOp::Junction`], [`PlanOp::Lane`]).
    /// Input gates take [`Deployment::set_input_gate`]: their gate messages
    /// are not ops.
    pub fn set_parameter(
        &mut self,
        specs: &Specs,
        site: Site,
        index: u32,
        value: f32,
    ) -> Result<Update, DeployError> {
        if let Site::InputControl(_) = site {
            return Err(DeployError::Unsupported {
                site,
                index,
                what: "input gate edits through set_parameter (use set_input_gate)",
            });
        }
        let mut layout = self.layout.clone();
        let instance = layout
            .instance_mut(site)
            .ok_or(DeployError::NotDeployed(site))?;
        if !instance.set_param(index, self.scene, value) {
            return Err(DeployError::Parameter { site, index });
        }
        let compiled = self.recompile(&layout, specs)?;
        let ops = match site {
            Site::Splitter(_) | Site::Mixer(_) => vec![PlanOp::Junction { site, index }],
            Site::OutputControl(row) => vec![PlanOp::Lane { row, index }],
            _ => self.block_ops(&compiled.plan, &layout, specs, site, index)?,
        };
        self.commit(layout, compiled);
        Ok(Update::Ops(ops))
    }

    fn block_ops(
        &self,
        plan: &DeploymentPlan,
        layout: &Layout,
        specs: &Specs,
        site: Site,
        index: u32,
    ) -> Result<Vec<PlanOp>, DeployError> {
        let Some(at) = self.placement(plan, layout, site)? else {
            return Ok(Vec::new());
        };
        let model = layout
            .instance(site)
            .ok_or(DeployError::NotDeployed(site))?
            .model;
        let sidechain = specs.model(model.0).and_then(|spec| spec.sidechain);
        let selector = sidechain.is_some_and(|s| s.source_param == index);
        let input = sidechain.is_some_and(|s| s.input_param == Some(index));
        let mut ops = Vec::new();
        // Freeze MOMENTARY (26001 parameter 1) takes the footswitch path, not
        // a ParameterChange (`0x51e4d8` category 0x1a branch).
        if !(model.0 == 26_001 && index == 1)
            && let Some(value) = plan
                .slot(at)
                .and_then(|slot| slot.values.get(index as usize))
        {
            ops.push(PlanOp::SetParam {
                at,
                index,
                value: value.at(self.scene),
            });
        }
        // A selector re-route realigns between its two routes (`0x583028`);
        // the bridge sends that alignment inside the `Sidechain` op.
        if let Site::Slot(slot) = site
            && (selector || input)
        {
            ops.extend(sidechain_ops(plan, slot, self.scene));
        }
        Ok(ops)
    }

    /// Sets one string parameter (cab `ir selector`, IR Loader `IR PATH`,
    /// capture `file_name`) with the stock scene rule. The DSP gets no value:
    /// the op tells the bridge to reload the block's IR or capture.
    pub fn set_text(
        &mut self,
        specs: &Specs,
        site: Site,
        index: u32,
        text: &str,
    ) -> Result<Update, DeployError> {
        let Site::Slot(slot) = site else {
            return Err(DeployError::Unsupported {
                site,
                index,
                what: "string parameters outside the grid",
            });
        };
        let mut layout = self.layout.clone();
        let instance = layout
            .instance_mut(site)
            .ok_or(DeployError::NotDeployed(site))?;
        if !instance.set_text(index, self.scene, text) {
            return Err(DeployError::Parameter { site, index });
        }
        let compiled = self.recompile(&layout, specs)?;
        let at = self.placement(&compiled.plan, &layout, Site::Slot(slot))?;
        self.commit(layout, compiled);
        Ok(Update::Ops(
            at.map(|at| PlanOp::Text { at, index })
                .into_iter()
                .collect(),
        ))
    }

    /// An input gate edit: INPUT GAIN writes the lane input router's return
    /// gain (and the gate's trim when the gate runs); the other parameters go
    /// to the gate core as message 0x31, or nowhere without a gate (stock
    /// tests the gate core, `0x51e4d8`).
    pub fn set_input_gate(
        &mut self,
        specs: &Specs,
        row: Row,
        index: u32,
        value: f32,
    ) -> Result<GateUpdate, DeployError> {
        let site = Site::InputControl(row);
        let mut layout = self.layout.clone();
        let lane = layout.lane_mut(row);
        let control = lane.input_control.get_or_insert_with(|| {
            specs.model(ids::INPUT_GATE_CONTROL).map_or_else(
                || crate::Instance::new(crate::ModelId(0), Vec::new()),
                crate::new_instance,
            )
        });
        if !control.set_param(index, self.scene, value) {
            return Err(DeployError::Parameter { site, index });
        }
        let compiled = self.recompile(&layout, specs)?;
        let plan = &compiled.plan;
        let mut update = GateUpdate {
            ops: Vec::new(),
            gate: Vec::new(),
        };
        let gate = plan.gate.as_ref();
        if index == INPUT_GAIN {
            update
                .ops
                .extend(input_gain_op(plan, &layout, row, self.scene));
            if let Some(gate) = gate {
                update.gate.push(GateMessage::InputGain {
                    core: gate.core,
                    row,
                    value: gate.rows[row.index()].trim,
                });
            }
        } else if let Some(gate) = gate {
            update.gate.push(GateMessage::Parameter {
                core: gate.core,
                row,
                index,
                value,
            });
        }
        self.commit(layout, compiled);
        Ok(update)
    }

    /// Bypasses or enables a model with the stock scene rule. A mixer has no
    /// slot: its bypass mutes LEVEL B (`0x514378`) and the slave output of a
    /// split-only pair (`0x5147cc`); re-writing them at once is inferred.
    pub fn set_bypass(
        &mut self,
        specs: &Specs,
        site: Site,
        bypassed: bool,
    ) -> Result<Update, DeployError> {
        if let Site::Splitter(_) = site {
            return Err(DeployError::Unsupported {
                site,
                index: 0,
                what: "splitter bypass",
            });
        }
        let mut layout = self.layout.clone();
        let instance = layout
            .instance_mut(site)
            .ok_or(DeployError::NotDeployed(site))?;
        instance.set_bypass(self.scene, bypassed);
        let model = instance.model;
        let compiled = self.recompile(&layout, specs)?;
        let plan = &compiled.plan;
        let mut ops = Vec::new();
        if let Site::Mixer(_) = site {
            // The mixer's BYPASS mutes LEVEL B (`0x514378`) and the slave
            // output of a split-only pair (`0x5147cc`); re-writing every
            // lane VOLUME at once is inferred.
            let index = layout
                .instance(site)
                .ok_or(DeployError::NotDeployed(site))?
                .bypass_index();
            ops.push(PlanOp::Junction { site, index });
            ops.extend((0..ROWS).map(|row| PlanOp::Lane {
                row: Row::new(row as u8).expect("row in range"),
                index: VOLUME,
            }));
        } else {
            let Some(at) = self.placement(plan, &layout, site)? else {
                self.commit(layout, compiled);
                return Ok(Update::Ops(Vec::new()));
            };
            ops.push(PlanOp::SetBypass { at, bypassed });
            if let Some(op) = sidechain_active(plan, specs.model(model.0), site, at, bypassed) {
                ops.push(op);
            }
        }
        ops.extend(delay_ops(plan, self.scene));
        self.commit(layout, compiled);
        Ok(Update::Ops(ops))
    }

    /// A block MUTE edit (`0x51e4d8` at the MUTE index, from `0x521dbc`): in scene mode the
    /// current scene changes, else all eight. The DSP takes MUTE in the block's bypass
    /// record, so the op re-sends the bypass of the current scene, and only when MUTE at the
    /// current scene changed (`0x51e4d8` compares the old `0x2ec478` value before `0x517228`).
    pub fn set_mute(
        &mut self,
        specs: &Specs,
        slot: Slot,
        muted: bool,
        scene_mode: bool,
    ) -> Result<Update, DeployError> {
        let site = Site::Slot(slot);
        let mut layout = self.layout.clone();
        let instance = layout
            .instance_mut(site)
            .ok_or(DeployError::NotDeployed(site))?;
        let changed = instance.mute.at(self.scene) != muted;
        if scene_mode {
            instance.mute.set(self.scene, muted);
        } else {
            instance.mute = Scened::Fixed(muted);
        }
        let bypassed = instance.bypass.at(self.scene);
        let compiled = self.recompile(&layout, specs)?;
        let at = self.placement(&compiled.plan, &layout, site)?;
        self.commit(layout, compiled);
        Ok(Update::Ops(match at {
            Some(at) if changed => vec![PlanOp::SetBypass { at, bypassed }],
            _ => Vec::new(),
        }))
    }

    fn commit(&mut self, layout: Layout, compiled: Compiled) {
        self.layout = layout;
        self.dsp_layout = compiled.dsp_layout;
        self.plan = compiled.plan;
    }

    /// Selects a scene: stock pushes every value of the new scene, undiffed.
    pub fn select_scene(&mut self, scene: SceneIndex) -> Update {
        self.scene = scene;
        Update::Ops(push_ops(&self.plan, &self.dsp_layout, scene))
    }

    /// Applies a structural edit (add, remove, move, replace, split/mix, ports).
    /// Pairs whose items did not change keep their cores. When the result does
    /// not compile, the previous layout stays deployed and the error is
    /// returned, as stock restores the previous preset.
    pub fn edit(
        &mut self,
        specs: &Specs,
        edit: impl FnOnce(&mut Layout) -> Result<(), crate::EditError>,
    ) -> Result<Update, DeployError> {
        let mut layout = self.layout.clone();
        edit(&mut layout)?;
        let previous = self.plan.assignment.clone();
        let options = self.options;
        let dormant = self.dormant.clone();
        self.redeploy(specs, layout, dormant, |layout| {
            compile_keeping(layout, specs, options, Some(&previous))
        })
    }

    /// Loads another preset with the same dormant models. A layout the DSP already runs
    /// reuses the loaded models.
    pub fn load(&mut self, specs: &Specs, layout: Layout) -> Result<Update, DeployError> {
        let dormant = self.dormant.clone();
        self.load_stored(specs, layout, StoredMasks::default(), dormant)
    }

    /// [`Deployment::load`] with the preset's stored core masks and the dormant models now.
    pub fn load_stored(
        &mut self,
        specs: &Specs,
        layout: Layout,
        stored: StoredMasks,
        dormant: BTreeSet<ModelId>,
    ) -> Result<Update, DeployError> {
        let options = self.options;
        self.redeploy(specs, layout, dormant, |layout| {
            compile_stored(layout, specs, options, stored)
        })
    }

    /// Reuses the loaded models when the DSP layout is the one it runs (a layout that only
    /// differs in dormant blocks included), else compiles it and clears the cores.
    fn redeploy(
        &mut self,
        specs: &Specs,
        layout: Layout,
        dormant: BTreeSet<ModelId>,
        compile: impl FnOnce(&Layout) -> Result<DeploymentPlan, CompileError>,
    ) -> Result<Update, DeployError> {
        let dsp_layout = without_dormant(&layout, &dormant, specs);
        if dsp_layout == self.dsp_layout {
            self.layout = layout;
            self.dormant = dormant;
            return Ok(Update::Reuse(push_ops(
                &self.plan,
                &self.dsp_layout,
                self.scene,
            )));
        }
        let plan = compile(&dsp_layout)?;
        // Stock clears every core that holds models (`0x582570`).
        let clear = self
            .plan
            .cores
            .iter()
            .filter(|core| !core.slots.is_empty())
            .map(|core| core.core)
            .collect();
        let ops = push_ops(&plan, &dsp_layout, self.scene);
        self.layout = layout;
        self.dormant = dormant;
        self.dsp_layout = dsp_layout;
        self.plan = plan.clone();
        Ok(Update::Redeploy {
            clear,
            plan: Box::new(plan),
            ops,
        })
    }
}

/// A recompiled DSP layout and its plan.
struct Compiled {
    dsp_layout: Layout,
    plan: DeploymentPlan,
}

/// `layout` without the grid blocks of `dormant` models; sidechain selectors keep their
/// sources.
fn without_dormant(layout: &Layout, dormant: &BTreeSet<ModelId>, specs: &Specs) -> Layout {
    let mut stripped = layout.clone();
    for slot in stripped.lanes.iter_mut().flat_map(|lane| &mut lane.slots) {
        if slot
            .as_ref()
            .is_some_and(|instance| dormant.contains(&instance.model))
        {
            *slot = None;
        }
    }
    crate::routing::keep_sidechain_sources(layout, &mut stripped, specs);
    stripped
}

fn sidechain_ops(plan: &DeploymentPlan, slot: Slot, scene: SceneIndex) -> Vec<PlanOp> {
    plan.sidechains
        .iter()
        .filter(|sidechain| sidechain.site == slot)
        .map(|sidechain| PlanOp::Sidechain {
            sink: sidechain.sink,
            scene: sidechain.scenes[scene.index()],
        })
        .collect()
}

/// `0x517228`: a sidechain model with a receive router gets its active flag.
fn sidechain_active(
    plan: &DeploymentPlan,
    spec: Option<&tetrad_catalog::ModelSpec>,
    site: Site,
    at: Placement,
    bypassed: bool,
) -> Option<PlanOp> {
    let spec = spec.filter(|spec| spec.sidechain.is_some())?;
    if ROUTERLESS_SIDECHAIN.contains(&spec.id) {
        return None;
    }
    let Site::Slot(slot) = site else { return None };
    Some(PlanOp::SidechainActive {
        sink: at,
        router: plan.placement(Node::Receive(Endpoint::Sidechain(slot))),
        active: !bypassed,
    })
}

/// The stock value push (`0x583078`) of one scene: per pair with a junction
/// the splitter and mixer push ([`PlanOp::PushJunctions`]), every grid block
/// row by row (bypass, sidechain flag, then values except meters, strings and
/// the MUTE/BYPASS pair, `0x5174a8`), the sidechain routes (`0x585f04`) and the
/// alignment. The lane I/O step (`0x5205e8`) is the sender's. Router setup,
/// lane routes, delay writes and gate messages are plan data.
pub fn push_ops(plan: &DeploymentPlan, layout: &Layout, scene: SceneIndex) -> Vec<PlanOp> {
    let mut ops = Vec::new();
    for pair in Pair::ALL {
        let junctions = layout.pair(pair);
        if junctions.split.is_some() || junctions.merge.is_some() {
            ops.push(PlanOp::PushJunctions { pair });
        }
    }
    for row in 0..ROWS {
        for column in 0..COLUMNS {
            let slot = Slot {
                row: Row::new(row as u8).expect("row in range"),
                column: crate::Column::new(column as u8).expect("column in range"),
            };
            let (Some(at), Some(instance)) = (plan.block(slot), layout.instance(Site::Slot(slot)))
            else {
                continue;
            };
            let Some(deployed) = plan.slot(at) else {
                continue;
            };
            let bypassed = instance.bypass.at(scene);
            ops.push(PlanOp::SetBypass { at, bypassed });
            if let Some(router) = plan.placement(Node::Receive(Endpoint::Sidechain(slot))) {
                ops.push(PlanOp::SidechainActive {
                    sink: at,
                    router: Some(router),
                    active: !bypassed,
                });
            }
            for (index, value) in deployed.values.iter().enumerate() {
                if !deployed.skipped.contains(&(index as u32)) {
                    ops.push(PlanOp::SetParam {
                        at,
                        index: index as u32,
                        value: value.at(scene),
                    });
                }
            }
        }
    }
    for sidechain in &plan.sidechains {
        ops.push(PlanOp::Sidechain {
            sink: sidechain.sink,
            scene: sidechain.scenes[scene.index()],
        });
    }
    ops.extend(delay_ops(plan, scene));
    ops
}

/// `0x581a78`: INPUT GAIN as the lane input router's return gain.
fn input_gain_op(
    plan: &DeploymentPlan,
    layout: &Layout,
    row: Row,
    scene: SceneIndex,
) -> Option<PlanOp> {
    let at = plan.routers.lane_input[row.index()]?;
    let index = input_gain_index(plan.slot(at)?.model.0)?;
    let fed = crate::plan::fed_rows(layout)[row.index()];
    let value = input_gain(
        stored_input_gain(layout, row, scene),
        crate::gate::input_trim(fed),
        plan.gate.is_some(),
    );
    Some(PlanOp::SetParam { at, index, value })
}

fn delay_ops(plan: &DeploymentPlan, scene: SceneIndex) -> Vec<PlanOp> {
    Pair::ALL
        .into_iter()
        .map(|pair| PlanOp::PairDelay {
            pair,
            delay: plan.delays[pair as usize][scene.index()],
        })
        .collect()
}
