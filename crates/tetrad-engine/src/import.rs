//! Conservative metadata import from `tetrad_preset::PresetSummary` into an owned [`Grid`].
//!
//! Addressing follows the stock full-file loader (VA `0x681d44`): grid rows are `chains` list
//! positions, columns are `Chain.models` positions, and `Bypass`/`ColBypass` apply by list
//! position. Stored coordinates must agree with positions when present. Everything the loader reads
//! but this crate cannot place or execute is refused or kept as a [`Detached`] model. Every stored
//! bypass record is validated the same way whether or not its slot is occupied. Model parameters
//! are neither imported nor executed.

use crate::{
    Block, COLUMNS, Column, Detached, DetachedStage, Error, Grid, Junction, LanePorts, ModelId,
    Pair, PairRoute, PortId, ROWS, Row, SCENES, Scened, Slot,
};
use tetrad_preset::{
    ChainSummary, ColBypassSummary, ModelSummary, PresetSummary, SceneBypassSummary, WirePresence,
};

/// What to do when a scalar the import needs has no wire occurrence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbsentScalars {
    Refuse,
    /// Use the protobuf getter value (0, false), as generated stock C++ reads it.
    /// Every use is listed in [`Imported::defaulted`].
    Proto3Default,
}

/// How a stored `SceneBypass.bypass` bool maps to [`Block::bypass`]. Stock audio
/// polarity is not recovered; `0x2ecca4` only copies the bool to the model's last
/// parameter as 0.0/1.0 without inversion. Records of empty slots need no polarity:
/// they are validated and then ignored, as stock skips hash-0 slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BypassPolarity {
    /// Refuse any preset with an occupied slot or stored split/mix bypass values.
    Refuse,
    /// Owned: stored `true` marks the block bypassed (identity in the renderer).
    StoredTrueBypasses,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportPolicy {
    pub absent: AbsentScalars,
    pub bypass: BypassPolarity,
}

impl ImportPolicy {
    pub const STRICT: Self = Self {
        absent: AbsentScalars::Refuse,
        bypass: BypassPolarity::Refuse,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortSide {
    Input,
    Output,
}

/// A stored slave-row port that the pair's split (input) or merge (output) replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShadowedPort {
    pub row: Row,
    pub side: PortSide,
    pub raw: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Imported {
    /// Compiles; carries the detached models.
    pub grid: Grid,
    /// Paths of absent scalars filled under [`AbsentScalars::Proto3Default`], once
    /// each, including validated records that the grid then ignores.
    pub defaulted: Vec<String>,
    pub shadowed: Vec<ShadowedPort>,
    /// Live routing stored on slave chains (points, junction models, junction
    /// bypass). Stock never reads it (`0x681d44`), so it is ignored.
    pub ignored: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ImportError {
    #[error("preset stores {0} chains; the grid needs exactly {ROWS}")]
    ChainCount(usize),
    #[error("preset stores {0} bypass rows; the grid has {ROWS}")]
    BypassRows(usize),
    #[error("{path} stores {count} entries; import needs {expected}")]
    ListLength {
        path: String,
        count: usize,
        expected: &'static str,
    },
    #[error("{path} stores {value} but its list position is {position}")]
    CoordinateMismatch {
        path: String,
        value: u32,
        position: usize,
    },
    #[error("{path} has no stored value and absent scalars are refused")]
    Absent { path: String },
    #[error("{path} field {field} occurs {count} times")]
    Duplicate {
        path: String,
        field: u32,
        count: usize,
    },
    #[error("{path} contains {count} unknown fields")]
    Unknown { path: String, count: usize },
    #[error("{path} = {value} is outside -1..=7")]
    Point { path: String, value: i32 },
    #[error("{path}: {feature} is not supported")]
    Unsupported { path: String, feature: &'static str },
    #[error("occupied slot {path} has no stored bypass scene values")]
    BypassMissing { path: String },
    #[error("{path} stores bypass state and the bypass policy refuses it")]
    BypassRefused { path: String },
    #[error("{path} stores model {model:?}, which the catalog does not define")]
    UnknownModel { path: String, model: ModelId },
    #[error("{path} stores {actual:?} model {model:?} where a {expected:?} model belongs")]
    ModelRole {
        path: String,
        model: ModelId,
        expected: tetrad_catalog::ModelRole,
        actual: tetrad_catalog::ModelRole,
    },
    #[error("{path} = {raw} is not a stock port")]
    Port { path: String, raw: u32 },
    #[error(transparent)]
    Grid(#[from] Error),
}

type Result<T> = std::result::Result<T, ImportError>;

/// Builds a compiled-valid grid from preset metadata. Only placement, ports,
/// split/mix points and block bypass are imported; see the module docs.
pub fn import_summary(summary: &PresetSummary, policy: ImportPolicy) -> Result<Imported> {
    validate_summary(summary, policy, false)
}

/// Shared validation pass. `deploy` admits what only the deployment compiler can
/// route: sidechain flags, the root follow flag and bypassed splitters/mixers.
/// [`import_summary`] refuses them because its grid cannot represent them.
pub(crate) fn validate_summary(
    summary: &PresetSummary,
    policy: ImportPolicy,
    deploy: bool,
) -> Result<Imported> {
    let mut import = Importer {
        policy,
        deploy,
        grid: Grid::new(),
        defaulted: Vec::new(),
        shadowed: Vec::new(),
        ignored: Vec::new(),
        bypass: [[None; COLUMNS]; ROWS],
    };
    single("preset", &summary.presence, 36)?;
    if !deploy && summary.side_chain_follow_exists == Some(true) {
        return Err(unsupported("preset", "side_chain_follow_exists"));
    }
    if summary.chains.len() != ROWS {
        return Err(ImportError::ChainCount(summary.chains.len()));
    }
    if summary.bypass.len() > ROWS {
        return Err(ImportError::BypassRows(summary.bypass.len()));
    }
    for (index, bypass) in summary.bypass.iter().enumerate() {
        let path = format!("bypass[{index}]");
        unknown(&path, bypass.unknown_field_count)?;
        coordinate(&path, &bypass.presence, 2, bypass.row, index)?;
        if bypass.col_bypass.len() > COLUMNS {
            return Err(list(&path, bypass.col_bypass.len(), "at most 8"));
        }
        for col in &bypass.col_bypass {
            import.bypass[index][col.position] = import.col_bypass(&path, col)?;
        }
    }
    // Routes before chains: slave ports depend on them.
    for pair in Pair::ALL {
        import.route(pair, &summary.chains[pair.master().index()])?;
        import.ignore_slave_routing(&summary.chains[pair.slave().index()]);
    }
    for (index, chain) in summary.chains.iter().enumerate() {
        let row = Row(index as u8);
        import.chain(row, chain)?;
    }
    import.grid.detached.sort();
    import.grid.compile()?;
    Ok(Imported {
        grid: import.grid,
        defaulted: import.defaulted,
        shadowed: import.shadowed,
        ignored: import.ignored,
    })
}

struct Importer {
    policy: ImportPolicy,
    deploy: bool,
    grid: Grid,
    defaulted: Vec<String>,
    shadowed: Vec<ShadowedPort>,
    ignored: Vec<String>,
    /// Validated `bypass[row].col_bypass[column]` state; `None` when no scene value is stored.
    bypass: [[Option<Scened<bool>>; COLUMNS]; ROWS],
}

impl Importer {
    /// Reads a single-occurrence scalar, applying the absent-scalar policy.
    fn scalar<T>(
        &mut self,
        path: &str,
        presence: &[WirePresence],
        field: u32,
        value: Option<T>,
        default: T,
    ) -> Result<T> {
        single(path, presence, field)?;
        match (value, self.policy.absent) {
            (Some(value), _) => Ok(value),
            (None, AbsentScalars::Refuse) => Err(ImportError::Absent {
                path: format!("{path}.{field}"),
            }),
            (None, AbsentScalars::Proto3Default) => {
                self.defaulted.push(format!("{path}.{field}"));
                Ok(default)
            }
        }
    }

    /// Reads the hash of any model kind (slot, lane control, splitter, mixer) and refuses
    /// its sidechain flags (fields 7/8).
    fn hash(&mut self, path: &str, model: &ModelSummary) -> Result<Option<ModelId>> {
        single(path, &model.presence, 7)?;
        single(path, &model.presence, 8)?;
        if !self.deploy
            && (model.sidechain_sink_flag == Some(true)
                || model.sidechain_source_flag == Some(true))
        {
            return Err(unsupported(path, "sidechain routing"));
        }
        let hash = self.scalar(path, &model.presence, 1, model.hash, 0)?;
        Ok((!is_empty_hash(hash)).then_some(ModelId(hash)))
    }

    /// Stock reads the first control point, splitter (or combined splitter) and mixer
    /// of even chain positions (VA `0x681d44`). Extra or missing entries are refused.
    fn route(&mut self, pair: Pair, chain: &ChainSummary) -> Result<()> {
        let path = format!("chains[{}]", chain.position);
        let [points] = chain.split_control_points.as_slice() else {
            let path = format!("{path}.split_control_points");
            return Err(list(&path, chain.split_control_points.len(), "exactly 1"));
        };
        let points_path = format!("{path}.split_control_points[0]");
        unknown(&points_path, points.unknown_field_count)?;
        let split = self.scalar(&points_path, &points.presence, 1, points.split, 0)?;
        let mix = self.scalar(&points_path, &points.presence, 2, points.mix, 0)?;
        let junction = |field, value: i32| match value {
            -1 => Ok(None),
            0..=7 => Ok(Some(Junction::at(Column(value as u8)))),
            _ => Err(ImportError::Point {
                path: format!("{points_path}.{field}"),
                value,
            }),
        };
        self.grid.set_route(
            pair,
            PairRoute {
                split: junction("split", split)?,
                merge: junction("mix", mix)?,
            },
        );

        // Stock reads `combined_splitter[0]` when that list is non-empty, else
        // `splitter[0]` (`0x681d44`); stock presets store both.
        let splitter = match (
            chain.splitter.as_slice(),
            chain.combined_splitter.as_slice(),
        ) {
            (_, [combined]) if chain.splitter.len() <= 1 => ("combined_splitter", combined),
            ([splitter], []) => ("splitter", splitter),
            _ => {
                let count = chain.splitter.len() + chain.combined_splitter.len();
                let path = format!("{path}.splitter+combined_splitter");
                return Err(list(&path, count, "one splitter and at most one combined"));
            }
        };
        let [mixer] = chain.mixer.as_slice() else {
            return Err(list(
                &format!("{path}.mixer"),
                chain.mixer.len(),
                "exactly 1",
            ));
        };
        for (stage, (name, model)) in [
            (DetachedStage::Splitter(pair), splitter),
            (DetachedStage::Mixer(pair), ("mixer", mixer)),
        ] {
            if let Some(model) = self.hash(&format!("{path}.{name}[0]"), model)? {
                self.grid.detached.push(Detached { stage, model });
            }
        }
        for (name, scenes) in [
            ("split_bypass", &chain.split_bypass),
            ("mix_bypass", &chain.mix_bypass),
        ] {
            let path = format!("{path}.{name}");
            if scenes.is_empty() {
                continue;
            }
            let values = self.scene_values(&path, scenes)?;
            if self.policy.bypass == BypassPolarity::Refuse {
                return Err(ImportError::BypassRefused { path });
            }
            if !self.deploy && values.contains(&true) {
                return Err(unsupported(&path, "bypassed splitter/mixer"));
            }
        }
        Ok(())
    }

    /// Stock reads routing only at even chain positions (`0x681d44`), so slave
    /// chains may store anything there. Stock presets store inert placeholders
    /// (points `(-1, -1)`, empty-hash junctions, all-`false` bypass); any other
    /// value is listed in `ignored` and never refuses the preset.
    fn ignore_slave_routing(&mut self, chain: &ChainSummary) {
        let path = |name: &str| format!("chains[{}].{name}", chain.position);
        if chain
            .split_control_points
            .iter()
            .any(|points| points.split.unwrap_or(-1) != -1 || points.mix.unwrap_or(-1) != -1)
        {
            self.ignored.push(path("split_control_points"));
        }
        for (name, models) in [
            ("splitter", &chain.splitter),
            ("mixer", &chain.mixer),
            ("combined_splitter", &chain.combined_splitter),
        ] {
            if models
                .iter()
                .any(|model| model.hash.is_some_and(|hash| !is_empty_hash(hash)))
            {
                self.ignored.push(path(name));
            }
        }
        for (name, scenes) in [
            ("split_bypass", &chain.split_bypass),
            ("mix_bypass", &chain.mix_bypass),
        ] {
            if scenes.iter().any(|scene| scene.bypass == Some(true)) {
                self.ignored.push(path(name));
            }
        }
    }

    fn chain(&mut self, row: Row, chain: &ChainSummary) -> Result<()> {
        let path = format!("chains[{}]", chain.position);
        unknown(&path, chain.unknown_field_count)?;
        coordinate(&path, &chain.presence, 14, chain.row, row.index())?;
        if chain.models.len() != COLUMNS {
            let path = format!("{path}.models");
            return Err(list(&path, chain.models.len(), "exactly 8"));
        }

        let input = self.scalar(&path, &chain.presence, 1, chain.in_portid, 0)?;
        let output = self.scalar(&path, &chain.presence, 2, chain.out_portid, 0)?;
        let route = self.grid.route(row.pair());
        let is_slave = row == row.pair().slave();
        let mut port = |side, raw, shadowed: bool| {
            if is_slave && shadowed {
                self.shadowed.push(ShadowedPort { row, side, raw });
                None
            } else {
                Some(PortId(raw))
            }
        };
        let ports = LanePorts {
            input: port(PortSide::Input, input, route.split.is_some()),
            output: port(PortSide::Output, output, route.merge.is_some()),
        };
        self.grid.set_ports(row, ports);

        // Stock substitutes default controls unless exactly one is stored (0x681d44).
        for (name, controls, stage) in [
            (
                "input_control",
                &chain.input_control,
                DetachedStage::InputControl(row),
            ),
            (
                "output_control",
                &chain.output_control,
                DetachedStage::OutputControl(row),
            ),
        ] {
            let [control] = controls.as_slice() else {
                return Err(list(&format!("{path}.{name}"), controls.len(), "exactly 1"));
            };
            if let Some(model) = self.hash(&format!("{path}.{name}[0]"), control)? {
                self.grid.detached.push(Detached { stage, model });
            }
        }

        for (index, model) in chain.models.iter().enumerate() {
            let path = format!("{path}.models[{index}]");
            coordinate(&path, &model.presence, 5, model.column, index)?;
            let Some(id) = self.hash(&path, model)? else {
                continue;
            };
            let bypass = match self.bypass[row.index()][index] {
                None => return Err(ImportError::BypassMissing { path }),
                Some(_) if self.policy.bypass == BypassPolarity::Refuse => {
                    let path = format!("bypass[{}].col_bypass[{index}]", row.index());
                    return Err(ImportError::BypassRefused { path });
                }
                Some(bypass) => bypass,
            };
            let block = Block {
                kind: crate::BlockKind::Model(id),
                bypass,
            };
            let slot = Slot {
                row,
                column: Column(index as u8),
            };
            self.grid.set_block(slot, Some(block));
        }
        Ok(())
    }

    /// Validates one stored `ColBypass` and mirrors `0x67c5d4`: scene mode is the stored
    /// flag OR any scene differing from the first. Shared values need one entry; scene
    /// mode needs all eight. Applies to every record; slot occupancy is not consulted.
    fn col_bypass(
        &mut self,
        row_path: &str,
        col: &ColBypassSummary,
    ) -> Result<Option<Scened<bool>>> {
        let path = format!("{row_path}.col_bypass[{}]", col.position);
        unknown(&path, col.unknown_field_count)?;
        coordinate(&path, &col.presence, 2, col.column, col.position)?;
        single(&path, &col.presence, 3)?;
        let values = self.scene_values(&format!("{path}.scene_bypass"), &col.scene_bypass)?;
        let scene_mode = col.scene_mode == Some(true);
        match values.first() {
            None if !scene_mode => return Ok(None),
            Some(&first) if !scene_mode && values.iter().all(|&value| value == first) => {
                return Ok(Some(Scened::Fixed(first)));
            }
            _ => {}
        }
        let scenes: [bool; SCENES] = values.try_into().map_err(|values: Vec<bool>| {
            list(
                &format!("{path}.scene_bypass"),
                values.len(),
                "8 in scene mode",
            )
        })?;
        Ok(Some(Scened::PerScene(scenes)))
    }

    fn scene_values(&mut self, path: &str, scenes: &[SceneBypassSummary]) -> Result<Vec<bool>> {
        if scenes.len() > SCENES {
            return Err(list(path, scenes.len(), "at most 8"));
        }
        scenes
            .iter()
            .map(|scene| {
                let path = format!("{path}[{}]", scene.position);
                unknown(&path, scene.unknown_field_count)?;
                self.scalar(&path, &scene.presence, 1, scene.bypass, false)
            })
            .collect()
    }
}

/// Stock presets mark empty slots with hash `0xFFFFFFFF`; 0 is also empty. Stock
/// treats any hash without a catalog model as empty (`0x2ecca4` tests the resolved
/// model pointer); this importer only admits the two sentinels as empty.
pub(crate) fn is_empty_hash(hash: u32) -> bool {
    hash == 0 || hash == u32::MAX
}

fn single(path: &str, presence: &[WirePresence], field: u32) -> Result<()> {
    match presence.iter().find(|entry| entry.number == field) {
        Some(entry) if entry.occurrences > 1 => Err(ImportError::Duplicate {
            path: path.to_owned(),
            field,
            count: entry.occurrences,
        }),
        _ => Ok(()),
    }
}

/// A stored coordinate must match its list position; absence is accepted because the
/// stock loader addresses by position.
fn coordinate(
    path: &str,
    presence: &[WirePresence],
    field: u32,
    value: Option<u32>,
    position: usize,
) -> Result<()> {
    single(path, presence, field)?;
    match value {
        Some(value) if value as usize != position => Err(ImportError::CoordinateMismatch {
            path: format!("{path}.{field}"),
            value,
            position,
        }),
        _ => Ok(()),
    }
}

fn unknown(path: &str, count: usize) -> Result<()> {
    if count == 0 {
        Ok(())
    } else {
        Err(ImportError::Unknown {
            path: path.to_owned(),
            count,
        })
    }
}

pub(crate) fn list(path: &str, count: usize, expected: &'static str) -> ImportError {
    ImportError::ListLength {
        path: path.to_owned(),
        count,
        expected,
    }
}

pub(crate) fn unsupported(path: &str, feature: &'static str) -> ImportError {
    ImportError::Unsupported {
        path: path.to_owned(),
        feature,
    }
}
