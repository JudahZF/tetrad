//! The full typed preset the deployment compiler consumes.
//!
//! A [`Layout`] holds every audio model a stock preset deploys: four lanes of eight
//! slots, the per-lane input gate (stock default 28000) and output control (stock
//! default 23000), and per pair a splitter and mixer. Each [`Instance`] carries
//! per-scene parameter values in DSP parameter order. Stock appends two hidden
//! parameters to the XML list, MUTE then BYPASS, and copies the scene bypass into
//! the last one as 0.0/1.0 (`0x2ecca4`, `0x2ecc70`; see `tetrad-core::ParameterImportPlan`).

use crate::{
    COLUMNS, Column, InputPort, ModelId, OutputPort, Pair, ROWS, Row, SCENES, SceneIndex, Scened,
    Slot,
};

/// Sidechain flags stored on a model (`Model` fields 7 and 8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SidechainFlags {
    pub sink: bool,
    pub source: bool,
}

/// A string-typed parameter, such as a cab's IR selector. Stock never sends it
/// as a DSP value; the IR or capture it names is loaded separately.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextParam {
    pub index: u32,
    /// Stored scene values in order; `None` where a scene stores nothing.
    pub scenes: Vec<Option<String>>,
}

/// One deployed model with its scene-resolved state.
#[derive(Debug, Clone, PartialEq)]
pub struct Instance {
    pub model: ModelId,
    /// Stored (normalized 0..=1) values of the XML parameters in index order.
    /// String parameters hold 0.0 here and their text in `texts`. MUTE and
    /// BYPASS are not stored here; the compiler appends them.
    pub params: Vec<Scened<f32>>,
    pub texts: Vec<TextParam>,
    pub bypass: Scened<bool>,
    pub mute: Scened<bool>,
    pub sidechain: SidechainFlags,
    /// Stored `Param.scene_mode` per XML parameter
    /// (`ProgramData::parameterSceneMode`, `0x2ea908`). Storage shape does not
    /// imply it: a scene-mode value may hold eight equal values.
    pub scene_mode: Vec<bool>,
    /// Bypass scene mode (`ColBypass.sceneMode` or differing scene values).
    pub bypass_scene_mode: bool,
}

impl Instance {
    pub fn new(model: ModelId, params: Vec<Scened<f32>>) -> Self {
        Self {
            model,
            scene_mode: vec![false; params.len()],
            params,
            texts: Vec::new(),
            bypass: Scened::Fixed(false),
            mute: Scened::Fixed(false),
            sidechain: SidechainFlags::default(),
            bypass_scene_mode: false,
        }
    }

    /// The stock edit rule (`0x51e4d8`): a parameter in scene mode changes in
    /// `scene` only; otherwise all eight scenes take the value. `false` when
    /// the parameter does not exist.
    pub fn set_param(&mut self, index: u32, scene: SceneIndex, value: f32) -> bool {
        let scene_mode = self
            .scene_mode
            .get(index as usize)
            .copied()
            .unwrap_or(false);
        let Some(param) = self.params.get_mut(index as usize) else {
            return false;
        };
        if scene_mode {
            param.set(scene, value);
        } else {
            *param = Scened::Fixed(value);
        }
        true
    }

    /// The same rule for a string parameter. `false` when `index` is not one.
    pub fn set_text(&mut self, index: u32, scene: SceneIndex, text: &str) -> bool {
        let scene_mode = self
            .scene_mode
            .get(index as usize)
            .copied()
            .unwrap_or(false);
        let Some(param) = self.texts.iter_mut().find(|param| param.index == index) else {
            return false;
        };
        if scene_mode {
            // A shorter list (a new block's single default) holds for every scene.
            let fill = param.scenes.first().cloned().flatten();
            param.scenes.resize(SCENES, fill);
            param.scenes[scene.index()] = Some(text.to_owned());
        } else {
            param.scenes = vec![Some(text.to_owned()); SCENES];
        }
        true
    }

    /// The same rule for the block bypass (`0x521514` → `0x521170`).
    pub fn set_bypass(&mut self, scene: SceneIndex, bypassed: bool) {
        if self.bypass_scene_mode {
            self.bypass.set(scene, bypassed);
        } else {
            self.bypass = Scened::Fixed(bypassed);
        }
    }

    /// DSP parameter values for one scene: XML parameters, then MUTE, then BYPASS.
    pub fn dsp_values(&self, scene: SceneIndex) -> Vec<f32> {
        self.params
            .iter()
            .map(|value| value.at(scene))
            .chain([f32::from(u8::from(self.mute.at(scene)))])
            .chain([f32::from(u8::from(self.bypass.at(scene)))])
            .collect()
    }

    /// Index of the hidden BYPASS parameter.
    pub fn bypass_index(&self) -> u32 {
        self.params.len() as u32 + 1
    }
}

/// One grid row: ports, lane controls and eight slots.
#[derive(Debug, Clone, PartialEq)]
pub struct Lane {
    pub input: InputPort,
    pub output: OutputPort,
    /// `Chain.input_control` (stock default model 28000, Input Gate Control).
    pub input_control: Option<Instance>,
    /// `Chain.output_control` (stock default model 23000, LaneOutputControl).
    pub output_control: Option<Instance>,
    pub slots: [Option<Instance>; COLUMNS],
}

impl Default for Lane {
    fn default() -> Self {
        Self {
            input: InputPort::Empty,
            output: OutputPort::Empty,
            input_control: None,
            output_control: None,
            slots: Default::default(),
        }
    }
}

/// Split/mix state of a master/slave row pair. Points are preset topology; the
/// splitter and mixer models and their bypass vary by scene.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PairLayout {
    pub split: Option<Column>,
    pub merge: Option<Column>,
    pub splitter: Option<Instance>,
    pub mixer: Option<Instance>,
}

/// Where a model sits in a layout. This is the stable identity a plan diff uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Site {
    InputControl(Row),
    Slot(Slot),
    Splitter(Pair),
    Mixer(Pair),
    OutputControl(Row),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Layout {
    pub lanes: [Lane; ROWS],
    pub pairs: [PairLayout; 2],
}

impl Layout {
    pub fn lane(&self, row: Row) -> &Lane {
        &self.lanes[row.index()]
    }

    pub fn lane_mut(&mut self, row: Row) -> &mut Lane {
        &mut self.lanes[row.index()]
    }

    pub fn pair(&self, pair: Pair) -> &PairLayout {
        &self.pairs[pair as usize]
    }

    pub fn pair_mut(&mut self, pair: Pair) -> &mut PairLayout {
        &mut self.pairs[pair as usize]
    }

    pub fn instance(&self, site: Site) -> Option<&Instance> {
        match site {
            Site::InputControl(row) => self.lane(row).input_control.as_ref(),
            Site::OutputControl(row) => self.lane(row).output_control.as_ref(),
            Site::Slot(slot) => self.lane(slot.row).slots[slot.column.index()].as_ref(),
            Site::Splitter(pair) => self.pair(pair).splitter.as_ref(),
            Site::Mixer(pair) => self.pair(pair).mixer.as_ref(),
        }
    }

    pub fn instance_mut(&mut self, site: Site) -> Option<&mut Instance> {
        match site {
            Site::InputControl(row) => self.lane_mut(row).input_control.as_mut(),
            Site::OutputControl(row) => self.lane_mut(row).output_control.as_mut(),
            Site::Slot(slot) => self.lane_mut(slot.row).slots[slot.column.index()].as_mut(),
            Site::Splitter(pair) => self.pair_mut(pair).splitter.as_mut(),
            Site::Mixer(pair) => self.pair_mut(pair).mixer.as_mut(),
        }
    }

    /// Replaces the instance at a site and returns the previous one.
    pub fn set_instance(&mut self, site: Site, instance: Option<Instance>) -> Option<Instance> {
        let target = match site {
            Site::InputControl(row) => &mut self.lane_mut(row).input_control,
            Site::OutputControl(row) => &mut self.lane_mut(row).output_control,
            Site::Slot(slot) => &mut self.lane_mut(slot.row).slots[slot.column.index()],
            Site::Splitter(pair) => &mut self.pair_mut(pair).splitter,
            Site::Mixer(pair) => &mut self.pair_mut(pair).mixer,
        };
        std::mem::replace(target, instance)
    }

    /// Every occupied site in layout order: rows top to bottom, each row as input
    /// control, slots, output control; then splitters and mixers.
    pub fn sites(&self) -> impl Iterator<Item = (Site, &Instance)> {
        let rows = (0..ROWS as u8).flat_map(move |row| {
            let row = Row(row);
            std::iter::once(Site::InputControl(row))
                .chain((0..COLUMNS as u8).map(move |column| {
                    Site::Slot(Slot {
                        row,
                        column: Column(column),
                    })
                }))
                .chain(std::iter::once(Site::OutputControl(row)))
        });
        let pairs = Pair::ALL
            .into_iter()
            .flat_map(|pair| [Site::Splitter(pair), Site::Mixer(pair)]);
        rows.chain(pairs)
            .filter_map(|site| self.instance(site).map(|instance| (site, instance)))
    }

    /// The owned 4×8 routing grid (`Grid`) of this layout, for the reference
    /// renderer and older callers. Slot models become opaque blocks.
    pub fn grid(&self) -> crate::Grid {
        let mut grid = crate::Grid::new();
        for (index, lane) in self.lanes.iter().enumerate() {
            let row = Row(index as u8);
            let port = |raw: u32| (raw != 0).then_some(crate::PortId(raw));
            grid.set_ports(
                row,
                crate::LanePorts {
                    input: port(lane.input as u32),
                    output: port(lane.output as u32),
                },
            );
            for (column, instance) in lane.slots.iter().enumerate() {
                let block = instance.as_ref().map(|instance| crate::Block {
                    kind: crate::BlockKind::Model(instance.model),
                    bypass: instance.bypass,
                });
                let slot = Slot {
                    row,
                    column: Column(column as u8),
                };
                grid.set_block(slot, block);
            }
        }
        for pair in Pair::ALL {
            let layout = self.pair(pair);
            grid.set_route(
                pair,
                crate::PairRoute {
                    split: layout.split.map(crate::Junction::at),
                    merge: layout.merge.map(crate::Junction::at),
                },
            );
        }
        grid
    }
}

/// Every scene value equal: collapse to `Fixed`.
pub(crate) fn collapse<T: Copy + PartialEq>(values: [T; SCENES]) -> Scened<T> {
    if values.iter().all(|value| *value == values[0]) {
        Scened::Fixed(values[0])
    } else {
        Scened::PerScene(values)
    }
}
