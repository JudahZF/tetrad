//! Preset → DSP deployment compiler and the owned 4×8 grid model.
//!
//! The deployment path ([`import_layout`] → [`route`] → [`fit`] → [`compile`] →
//! [`Deployment`]) reproduces the stock `zenDSPCompiler`: routing validation, the
//! FitChecker core assignment, per-core run lists with routers and links, latency
//! alignment and the stock incremental update rules.
//!
//! The older owned path ([`Grid`], [`Program`], [`import_summary`]) keeps a
//! deterministic schedule and an offline reference renderer. Its lane gains,
//! junction placement and renderer are owned policy, not stock SHARC behaviour.
#![forbid(unsafe_code)]

mod align;
mod deploy;
mod edit;
mod fit;
mod gate;
mod grid;
mod import;
mod junction;
mod layout;
mod plan;
mod ports;
mod preset;
mod program;
mod routers;
mod routing;

pub use deploy::{DeployError, Deployment, GateUpdate, PlanOp, Update, push_ops};
pub use edit::{DropSide, EditError, GridDrop, PairPoints, new_instance};
pub use fit::{
    Assignment, CORE_TEMPLATE, DspCore, Fit, FitError, Globals, Group, Inhibited, Item, ItemInput,
    ItemKind, ItemNode, PATH_HEAD, SOC_TEMPLATE, StoredMasks, budgets, costs, fit, fit_stored,
    group, utilisation,
};
pub use gate::{GATE_PARAMS, GateMessage, GatePlan, GateRow, INPUT_GAIN, INPUT_TRIM};
pub use grid::{
    Block, BlockKind, Detached, DetachedStage, Grid, Junction, LaneGains, LanePorts, PairRoute,
};
pub use import::{
    AbsentScalars, BypassPolarity, ImportError, ImportPolicy, Imported, PortSide, ShadowedPort,
    import_summary,
};
pub use junction::SplitterType;
pub use layout::{Instance, Lane, Layout, PairLayout, SidechainFlags, Site, TextParam};
pub use plan::{
    BlobLoad, CAB_LATENCY, Channel, CompileError, CompileOptions, CorePlan, Deployed,
    DeploymentPlan, Endpoint, HOP_LATENCY, Link, LinkKind, Node, PairDelay, Placement,
    ROUTERLESS_SIDECHAIN, RouterSet, RouterValue, RouterWrite, SLOTS_PER_CORE, SidechainPath,
    SidechainPlan, SidechainScene, SlotIndex, compile, compile_keeping, compile_stored, fed_rows,
};
pub use ports::{InputPort, OutputPort};
pub use preset::{ImportedLayout, import_layout};
pub use program::{Change, OpaquePolicy, Program, Step, StepValue};
pub use routing::{
    LaneSink, LaneSource, RouteError, RoutingGraph, SIDECHAIN_EXTERNAL, SidechainRoute,
    SidechainSource, route, sidechain_steps, source_order,
};

/// Recovered grid size: four rows of eight columns (`tetrad-core` 4×8 import layout,
/// split/mix point validation at VA `0x1cb228`).
pub const ROWS: usize = tetrad_core::GRID_ROWS;
pub const COLUMNS: usize = 8;
/// Recovered scene count: scene update rejects a selected scene above 7 (VA `0x2787d8`).
pub const SCENES: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Row(u8);

impl Row {
    pub fn new(index: u8) -> Result<Self, Error> {
        if usize::from(index) < ROWS {
            Ok(Self(index))
        } else {
            Err(Error::Row(index))
        }
    }

    pub fn index(self) -> usize {
        usize::from(self.0)
    }

    /// The pair this row belongs to; even rows are the master chain of their pair.
    pub fn pair(self) -> Pair {
        if self.0 < 2 { Pair::Upper } else { Pair::Lower }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Column(u8);

impl Column {
    pub fn new(index: u8) -> Result<Self, Error> {
        if usize::from(index) < COLUMNS {
            Ok(Self(index))
        } else {
            Err(Error::Column(index))
        }
    }

    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Slot {
    pub row: Row,
    pub column: Column,
}

/// A master/slave row pair. Stock split/mix configuration only accepts master rows 0
/// and 2 (VA `0x1cb228`); slave rows 1 and 3 only exist between the pair's junctions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Pair {
    Upper,
    Lower,
}

impl Pair {
    pub const ALL: [Self; 2] = [Self::Upper, Self::Lower];

    pub fn master(self) -> Row {
        Row(self as u8 * 2)
    }

    pub fn slave(self) -> Row {
        Row(self as u8 * 2 + 1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SceneIndex(u8);

impl SceneIndex {
    pub fn new(index: u8) -> Result<Self, Error> {
        if usize::from(index) < SCENES {
            Ok(Self(index))
        } else {
            Err(Error::Scene(index))
        }
    }

    pub fn index(self) -> usize {
        usize::from(self.0)
    }
}

/// Opaque stored I/O port identifier (`Chain.in_portid`/`out_portid`); port meaning is
/// not recovered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PortId(pub u32);

/// Opaque catalog model identifier. The engine does not process catalog models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModelId(pub u32);

/// Finite gain in decibels. The −96..=+24 dB range is owned policy.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub struct GainDb(f32);

impl GainDb {
    pub const UNITY: Self = Self(0.0);
    pub const MIN: f32 = -96.0;
    pub const MAX: f32 = 24.0;

    pub fn new(db: f32) -> Result<Self, Error> {
        if (Self::MIN..=Self::MAX).contains(&db) {
            Ok(Self(db))
        } else {
            Err(Error::Gain(db))
        }
    }

    pub fn db(self) -> f32 {
        self.0
    }

    pub fn linear(self) -> f32 {
        10f32.powf(self.0 / 20.0)
    }
}

/// A value that is either shared by all scenes or stored per scene. Mirrors the stock
/// `scene_mode` / per-scene value split (Param fields 4/5, `ColBypass.sceneMode`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Scened<T> {
    Fixed(T),
    PerScene([T; SCENES]),
}

impl<T: Copy> Scened<T> {
    pub fn at(&self, scene: SceneIndex) -> T {
        match self {
            Self::Fixed(value) => *value,
            Self::PerScene(values) => values[scene.index()],
        }
    }

    /// Replaces one scene's value, expanding a fixed value to per-scene storage.
    pub fn set(&mut self, scene: SceneIndex, value: T) {
        if let Self::Fixed(shared) = *self {
            *self = Self::PerScene([shared; SCENES]);
        }
        if let Self::PerScene(values) = self {
            values[scene.index()] = value;
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum Error {
    #[error("row {0} is outside the four grid rows")]
    Row(u8),
    #[error("column {0} is outside the eight grid columns")]
    Column(u8),
    #[error("scene {0} is outside the eight scenes")]
    Scene(u8),
    #[error("gain {0} dB is not finite or outside -96..=24 dB")]
    Gain(f32),
    #[error("{pair:?} pair splits at column {split} after it merges at column {merge}")]
    SplitAfterMerge {
        pair: Pair,
        split: usize,
        merge: usize,
    },
    #[error("{0:?} pair splits, so its slave row cannot also take an input port")]
    SlaveInputShadowed(Pair),
    #[error("{0:?} pair merges, so its slave row cannot also drive an output port")]
    SlaveOutputShadowed(Pair),
    #[error("input port {port:?} has {actual} frames, expected {expected}")]
    FrameMismatch {
        port: PortId,
        expected: usize,
        actual: usize,
    },
    #[error("render of {frames} frames exceeds the {max}-frame ceiling")]
    FrameLimit { frames: usize, max: usize },
    #[error("input port {port:?} has a non-finite sample at frame {frame}")]
    NonFiniteInput { port: PortId, frame: usize },
    #[error("output port {port:?} overflowed to a non-finite sample at frame {frame}")]
    NonFiniteOutput { port: PortId, frame: usize },
    #[error("model {model:?} at row {}, column {} has no owned processor", .slot.row.index(), .slot.column.index())]
    OpaqueModel { slot: Slot, model: ModelId },
    #[error("{stage:?} model {model:?} is detached from the schedule and has no owned processor")]
    DetachedModel {
        stage: DetachedStage,
        model: ModelId,
    },
}
