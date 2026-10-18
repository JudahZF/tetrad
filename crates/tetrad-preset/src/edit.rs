//! Grid, scene, stomp and expression edits on the typed [`Preset`].
//!
//! Addressing is positional, as in the stock loader `0x00681d44`: row = position
//! in `chains` and `bypass`, column = position in `Chain.models` and `col_bypass`.
//! Stored coordinate fields move with a block only when they are present.
//! Slot shapes follow the 262-file factory corpus: an empty slot is model 0 with
//! explicit false flags and a bypass record of eight false scenes without scene
//! mode; an occupied slot's record also stores `sceneMode`.

use crate::{
    Error, ParamValue, Result,
    migrate::{MIDI_GENERAL_GROUPS, MIDI_GENERAL_SLOTS, MIDI_MESSAGES, ModelCatalog},
    model::{ColBypass, Expression, MidiMessage, Model, Param, Preset, SceneBypass, SceneValue},
    model::{StompAssignment, StompRole},
};

pub const ROWS: usize = tetrad_core::GRID_ROWS;
pub const COLUMNS: usize = 8;
pub const SCENES: usize = 8;
/// Stock default scene colors, 0xAARRGGBB (`DAT_0155f710` through QColor::rgb).
pub(crate) const DEFAULT_SCENE_COLORS: [u32; SCENES] = [
    0xffff_2727,
    0xff0a_74e0,
    0xffff_d236,
    0xffff_02c2,
    0xff45_f862,
    0xffff_7000,
    0xff69_54ff,
    0xff00_ffdd,
];
/// Stock load drops assignments above this index ("corrupt preset?", VA 0x683798).
pub const MAX_STOMP_INDEX: u32 = 10;
/// Tempo-model parameter whose scene mode also enables the followers (`0x00517f54`).
const TEMPO_SCENE_LEADER: usize = 6;
const TEMPO_SCENE_FOLLOWERS: std::ops::Range<usize> = 10..23;

/// A grid position. Operations check it against [`ROWS`] and [`COLUMNS`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Slot {
    pub row: usize,
    pub column: usize,
}

impl Slot {
    fn check(self) -> Result<Self> {
        if self.row >= ROWS {
            return Err(Error::Index {
                kind: "grid row",
                index: self.row,
                len: ROWS,
            });
        }
        if self.column >= COLUMNS {
            return Err(Error::Index {
                kind: "grid column",
                index: self.column,
                len: COLUMNS,
            });
        }
        Ok(self)
    }

    fn matches(self, row: u32, column: u32) -> bool {
        u32::try_from(self.row) == Ok(row) && u32::try_from(self.column) == Ok(column)
    }
}

fn check_scene(scene: usize) -> Result<()> {
    if scene >= SCENES {
        return Err(Error::Index {
            kind: "scene",
            index: scene,
            len: SCENES,
        });
    }
    Ok(())
}

/// Scene-mode write rule: with scene mode on, only `scene` changes; otherwise
/// every stored scene takes the value (stock files keep them equal).
fn write_scene<T: Clone>(items: &mut [T], scene_mode: bool, scene: usize, value: T) -> Result<()> {
    check_scene(scene)?;
    if items.len() != SCENES {
        return Err(Error::Layout("a scene list needs eight entries"));
    }
    if scene_mode {
        items[scene] = value;
    } else {
        items.fill(value);
    }
    Ok(())
}

impl Model {
    /// Stock empty slot: model 0 with explicit false flags.
    pub fn empty_slot() -> Self {
        Self {
            hash: Some(0),
            notify_flag: Some(false),
            sidechain_sink_flag: Some(false),
            sidechain_source_flag: Some(false),
            ..Self::default()
        }
    }

    /// A new grid block in the stock factory shape. `values` are the
    /// caller-resolved stored defaults (catalog defaults plus MUTE).
    pub fn block(model_id: u32, values: impl IntoIterator<Item = ParamValue>) -> Self {
        Self {
            hash: Some(model_id),
            params: values.into_iter().map(Param::new).collect(),
            bypass_expression: vec![Expression {
                expression_max: 1.0,
                ..Expression::default()
            }],
            notify_flag: Some(false),
            sidechain_sink_flag: Some(false),
            sidechain_source_flag: Some(false),
            ..Self::default()
        }
    }

    pub fn is_empty(&self) -> bool {
        self.hash.unwrap_or(0) == 0
    }

    pub fn param_mut(&mut self, index: usize) -> Result<&mut Param> {
        let len = self.params.len();
        self.params.get_mut(index).ok_or(Error::Index {
            kind: "parameter",
            index,
            len,
        })
    }
}

impl Param {
    /// Stock factory shape: no expression (0, range 0..1), scene mode off and
    /// the same value in all eight scenes.
    pub fn new(value: ParamValue) -> Self {
        Self {
            expression: Some(0),
            expression_min: Some(0.0),
            expression_max: Some(1.0),
            scene_mode: Some(false),
            values: vec![
                SceneValue {
                    value: Some(value),
                    ..SceneValue::default()
                };
                SCENES
            ],
            ..Self::default()
        }
    }

    pub fn value(&self, scene: usize) -> Option<&ParamValue> {
        self.values.get(scene)?.value.as_ref()
    }

    /// Writes one scene in scene mode, otherwise all scenes. New non-finite
    /// floats are refused (owned policy); stored ones are kept as they are.
    /// Unknown fields inside a replaced value message are not kept.
    pub fn set_value(&mut self, scene: usize, value: ParamValue) -> Result<()> {
        if matches!(value, ParamValue::Float(v) if !v.is_finite()) {
            return Err(Error::NonFinite);
        }
        let value = SceneValue {
            value: Some(value),
            ..SceneValue::default()
        };
        write_scene(
            &mut self.values,
            self.scene_mode.unwrap_or(false),
            scene,
            value,
        )
    }

    /// Turning scene mode off copies `current_scene` into every scene (stock
    /// `0x00522674`); the copies replace whole value messages, so unknown
    /// fields of the other scenes are not kept. For the tempo model use
    /// [`Preset::enable_tempo_scene_mode`]: stock couples its parameters.
    pub fn set_scene_mode(&mut self, enabled: bool, current_scene: usize) -> Result<()> {
        check_scene(current_scene)?;
        if !enabled {
            let current = self
                .values
                .get(current_scene)
                .cloned()
                .ok_or(Error::Layout("a scene list needs eight entries"))?;
            write_scene(&mut self.values, false, current_scene, current)?;
        }
        self.scene_mode = Some(enabled);
        Ok(())
    }

    /// Assigns an opaque expression code (0 = none; factory files use 1 and 2)
    /// and its range. The range must be finite.
    pub fn set_expression(&mut self, expression: i32, min: f32, max: f32) -> Result<()> {
        if !min.is_finite() || !max.is_finite() {
            return Err(Error::NonFinite);
        }
        self.expression = Some(expression);
        self.expression_min = Some(min);
        self.expression_max = Some(max);
        Ok(())
    }
}

impl ColBypass {
    pub(crate) fn empty_slot() -> Self {
        Self {
            scene_bypass: vec![SceneBypass::default(); SCENES],
            ..Self::default()
        }
    }

    fn occupied_slot() -> Self {
        Self {
            scene_mode: Some(false),
            ..Self::empty_slot()
        }
    }

    pub fn bypassed(&self, scene: usize) -> Option<bool> {
        Some(self.scene_bypass.get(scene)?.bypass)
    }

    /// Same scene-mode rule as [`Param::set_value`]. Stored `true` is the
    /// bypass bit stock copies to the model's last runtime parameter.
    pub fn set_bypassed(&mut self, scene: usize, bypassed: bool) -> Result<()> {
        let value = SceneBypass {
            bypass: bypassed,
            ..SceneBypass::default()
        };
        write_scene(
            &mut self.scene_bypass,
            self.scene_mode.unwrap_or(false),
            scene,
            value,
        )
    }

    pub fn set_scene_mode(&mut self, enabled: bool, current_scene: usize) -> Result<()> {
        check_scene(current_scene)?;
        if !enabled {
            let current = self
                .scene_bypass
                .get(current_scene)
                .cloned()
                .ok_or(Error::Layout("a scene list needs eight entries"))?;
            write_scene(&mut self.scene_bypass, false, current_scene, current)?;
        }
        self.scene_mode = Some(enabled);
        Ok(())
    }
}

impl Preset {
    fn slot_ref(&self, slot: Slot) -> Result<&Model> {
        let slot = slot.check()?;
        self.chains
            .get(slot.row)
            .ok_or(Error::Layout("missing chain row"))?
            .models
            .get(slot.column)
            .ok_or(Error::Layout("missing model column"))
    }

    fn slot_and_bypass(&mut self, slot: Slot) -> Result<(&mut Model, &mut ColBypass)> {
        let slot = slot.check()?;
        let model = self
            .chains
            .get_mut(slot.row)
            .ok_or(Error::Layout("missing chain row"))?
            .models
            .get_mut(slot.column)
            .ok_or(Error::Layout("missing model column"))?;
        let bypass = self
            .bypass
            .get_mut(slot.row)
            .ok_or(Error::Layout("missing bypass row"))?
            .col_bypass
            .get_mut(slot.column)
            .ok_or(Error::Layout("missing bypass column"))?;
        Ok((model, bypass))
    }

    /// The model in an occupied slot, or `None` for an empty slot.
    pub fn block(&self, slot: Slot) -> Result<Option<&Model>> {
        Ok(Some(self.slot_ref(slot)?).filter(|model| !model.is_empty()))
    }

    pub fn block_mut(&mut self, slot: Slot) -> Result<&mut Model> {
        let (model, _) = self.slot_and_bypass(slot)?;
        if model.is_empty() {
            return Err(Error::SlotEmpty {
                row: slot.row,
                column: slot.column,
            });
        }
        Ok(model)
    }

    /// Bypass record of an occupied slot.
    pub fn block_bypass_mut(&mut self, slot: Slot) -> Result<&mut ColBypass> {
        self.block_mut(slot)?;
        Ok(self.slot_and_bypass(slot)?.1)
    }

    /// Writes a slot. Stored `Model.column` / `ColBypass.column` fields are
    /// rewritten to the slot position when present and stay absent otherwise.
    fn put(&mut self, slot: Slot, model: Model, bypass: ColBypass) -> Result<()> {
        let column = Some(slot.column as u32);
        let (target, target_bypass) = self.slot_and_bypass(slot)?;
        *target = Model {
            column: model.column.and(column),
            ..model
        };
        *target_bypass = ColBypass {
            column: bypass.column.and(column),
            ..bypass
        };
        Ok(())
    }

    /// Places a model in an empty slot with an active (not bypassed) record.
    /// The empty slot's messages are replaced (their unknown fields go).
    /// Sidechain sources of other blocks are kept (stock follow, see
    /// `sidechain.rs`); the preset must be in loaded form.
    pub fn insert_block(
        &mut self,
        slot: Slot,
        model: Model,
        catalog: &impl ModelCatalog,
    ) -> Result<()> {
        if model.is_empty() {
            return Err(Error::Unsupported(
                "an inserted block needs a nonzero model ID",
            ));
        }
        if self.block(slot)?.is_some() {
            return Err(Error::SlotOccupied {
                row: slot.row,
                column: slot.column,
            });
        }
        self.follow_sidechains(catalog, &[], |preset| {
            preset.put(slot, model, ColBypass::occupied_slot())
        })
    }

    /// Empties a slot. Its bypass record and stomp assignment go with it
    /// (stock `0x00522c60` clears both stomp roles); its update notification
    /// goes too (inferred). Sidechain sources of other blocks are kept.
    pub fn remove_block(&mut self, slot: Slot, catalog: &impl ModelCatalog) -> Result<Model> {
        let removed = self.block_mut(slot)?.clone();
        self.follow_sidechains(catalog, &[], |preset| {
            preset.put(slot, Model::empty_slot(), ColBypass::empty_slot())?;
            preset
                .stomp_mode_assignments
                .retain(|s| !slot.matches(s.row, s.column));
            preset
                .model_update_notifications
                .retain(|n| !slot.matches(n.row, n.column));
            Ok(())
        })?;
        Ok(removed)
    }

    /// Changes the model of an occupied slot. The slot keeps its bypass record
    /// and stomp assignment. Returns the previous model.
    pub fn replace_model(&mut self, slot: Slot, model: Model) -> Result<Model> {
        if model.is_empty() {
            return Err(Error::Unsupported("use remove_block to empty a slot"));
        }
        let previous = self.block_mut(slot)?.clone();
        let bypass = self.slot_and_bypass(slot)?.1.clone();
        self.put(slot, model, bypass)?;
        Ok(previous)
    }

    /// Moves a block to an empty slot with its bypass record, stomp assignment
    /// and update notification. Both slots get new empty-slot or block
    /// messages (unknown fields of the destination empty slot go). Stomp
    /// assignments stay in (row, column) order. Sidechain sources of other
    /// blocks are kept, including sinks that select the moved block.
    pub fn move_block(&mut self, from: Slot, to: Slot, catalog: &impl ModelCatalog) -> Result<()> {
        if self.block(to)?.is_some() {
            return Err(Error::SlotOccupied {
                row: to.row,
                column: to.column,
            });
        }
        self.move_blocks(&[(from, to)], catalog)
    }

    /// Moves blocks at once, each as [`Preset::move_block`] does: a grid drop moves the
    /// blocks that make room and the dropped block together (stock `0x59e194`). Each source
    /// must hold a block; each target must be empty or a source, and no two moves share a
    /// target. Nothing changes on an error.
    pub fn move_blocks(
        &mut self,
        moves: &[(Slot, Slot)],
        catalog: &impl ModelCatalog,
    ) -> Result<()> {
        let mut records = Vec::with_capacity(moves.len());
        for &(from, to) in moves {
            let vacated = moves.iter().any(|&(source, _)| source == to);
            let shared = moves.iter().filter(|&&(_, target)| target == to).count() > 1;
            if shared || (self.block(to)?.is_some() && !vacated) {
                return Err(Error::SlotOccupied {
                    row: to.row,
                    column: to.column,
                });
            }
            let model = self.block_mut(from)?.clone();
            records.push((to, model, self.slot_and_bypass(from)?.1.clone()));
        }
        self.follow_sidechains(catalog, moves, |preset| {
            for &(from, _) in moves {
                preset.put(from, Model::empty_slot(), ColBypass::empty_slot())?;
            }
            for (to, model, bypass) in records {
                preset.put(to, model, bypass)?;
            }
            let moved = |row: &mut u32, column: &mut u32| {
                let target = moves.iter().find(|(from, _)| from.matches(*row, *column));
                if let Some(&(_, to)) = target {
                    (*row, *column) = (to.row as u32, to.column as u32);
                }
            };
            for assignment in &mut preset.stomp_mode_assignments {
                moved(&mut assignment.row, &mut assignment.column);
            }
            preset
                .stomp_mode_assignments
                .sort_by_key(|s| (s.row, s.column));
            for notification in &mut preset.model_update_notifications {
                moved(&mut notification.row, &mut notification.column);
            }
            Ok(())
        })
    }

    /// Scene mode on for a tempo-model (25000) parameter. Stock `0x00517f54`
    /// also turns it on for parameters 10..=22 when the parameter is 6. Only
    /// the "on" coupling is recovered; turn it off per [`Param`].
    pub fn enable_tempo_scene_mode(&mut self, param: usize) -> Result<()> {
        let model = self
            .tempo_program_data
            .first_mut()
            .ok_or(Error::Layout("missing tempo model"))?;
        let coupled = if param == TEMPO_SCENE_LEADER {
            TEMPO_SCENE_FOLLOWERS
        } else {
            0..0
        };
        // Bounds check first, so that an error changes nothing.
        model.param_mut(coupled.clone().last().unwrap_or(param).max(param))?;
        for index in std::iter::once(param).chain(coupled) {
            model.params[index].scene_mode = Some(true);
        }
        Ok(())
    }

    /// Assigns an occupied slot to a footswitch. Factory files hold at most
    /// one assignment per slot, so an existing one for the slot is replaced.
    /// Stock saves assignments from a map in (row, column) order with the role
    /// always set; new entries keep that order.
    pub fn assign_stomp(&mut self, slot: Slot, stomp_index: u32, role: StompRole) -> Result<()> {
        self.block_mut(slot)?;
        if stomp_index > MAX_STOMP_INDEX {
            return Err(Error::Index {
                kind: "stomp",
                index: stomp_index as usize,
                len: MAX_STOMP_INDEX as usize + 1,
            });
        }
        self.unassign_stomp(slot);
        let (row, column) = (slot.row as u32, slot.column as u32);
        let position = self
            .stomp_mode_assignments
            .partition_point(|s| (s.row, s.column) < (row, column));
        self.stomp_mode_assignments.insert(
            position,
            StompAssignment {
                row,
                column,
                stomp_index,
                role: Some(role),
                ..StompAssignment::default()
            },
        );
        Ok(())
    }

    pub fn unassign_stomp(&mut self, slot: Slot) -> Option<StompAssignment> {
        let position = self
            .stomp_mode_assignments
            .iter()
            .position(|s| slot.matches(s.row, s.column))?;
        Some(self.stomp_mode_assignments.remove(position))
    }

    pub fn set_scene_label(&mut self, scene: usize, label: &str) -> Result<()> {
        write_scene(&mut self.scene_labels, true, scene, label.to_owned())
    }

    pub fn set_scene_color(&mut self, scene: usize, raw_color: u32) -> Result<()> {
        write_scene(&mut self.scene_colors, true, scene, raw_color)
    }

    /// Stock scene copy (`0x0050e274`): every stored per-scene value of scene `from` goes to
    /// scene `to` (block, junction and lane-control parameters with bypass, the preset-level
    /// tempo models, scene tempo, scene label and scene color). Scene-mode flags, expression
    /// assignments and the default scene stay.
    pub fn copy_scene(&mut self, from: usize, to: usize) -> Result<()> {
        self.each_scene_list(from, to, |items, from, to| items.copy(from, to))
    }

    /// Stock scene swap (`0x0051a2a8`): the copy in both directions.
    pub fn swap_scenes(&mut self, first: usize, second: usize) -> Result<()> {
        self.each_scene_list(first, second, |items, a, b| items.swap(a, b))
    }

    /// Runs `op` on every per-scene list long enough to hold both scenes. Scene labels and
    /// colors grow to eight entries first, as stock writes eight.
    fn each_scene_list(
        &mut self,
        a: usize,
        b: usize,
        op: impl Fn(&mut dyn SceneList, usize, usize),
    ) -> Result<()> {
        check_scene(a)?;
        check_scene(b)?;
        let apply = |items: &mut dyn SceneList| {
            if items.scene_len() > a.max(b) {
                op(items, a, b);
            }
        };
        for list in [
            &mut self.scene_labels as &mut dyn SceneList,
            &mut self.scene_colors,
        ] {
            list.pad(SCENES);
            apply(list);
        }
        apply(&mut self.scene_tempo);
        let models = self.chains.iter_mut().flat_map(|chain| {
            chain
                .models
                .iter_mut()
                .chain(&mut chain.splitter)
                .chain(&mut chain.combined_splitter)
                .chain(&mut chain.mixer)
                .chain(&mut chain.input_control)
                .chain(&mut chain.output_control)
        });
        for model in models.chain(&mut self.tempo_program_data) {
            for param in &mut model.params {
                apply(&mut param.values);
            }
        }
        for chain in &mut self.chains {
            apply(&mut chain.split_bypass);
            apply(&mut chain.mix_bypass);
        }
        for column in self.bypass.iter_mut().flat_map(|row| &mut row.col_bypass) {
            apply(&mut column.scene_bypass);
        }
        Ok(())
    }

    pub fn set_default_scene(&mut self, scene: usize) -> Result<()> {
        check_scene(scene)?;
        self.default_scene = Some(scene as u32);
        Ok(())
    }

    /// Preset-load MIDI message `index` of the fixed 12-entry list. Stock keeps
    /// an 8-bit type (VA 0x683d78); wider types are refused, not truncated.
    pub fn set_midi_message(&mut self, index: usize, message: MidiMessage) -> Result<()> {
        check_midi_type(&message)?;
        if self.midi_messages.len() != MIDI_MESSAGES {
            return Err(Error::Layout("the MIDI message list needs twelve entries"));
        }
        let slot = self.midi_messages.get_mut(index).ok_or(Error::Index {
            kind: "MIDI message",
            index,
            len: MIDI_MESSAGES,
        })?;
        *slot = message;
        Ok(())
    }

    /// Entry of the 10×12 general MIDI table (`midi_messages_general_v2`).
    /// Slot 0 of each group is mirrored into the v1 list as the saver writes it.
    pub fn set_midi_general(
        &mut self,
        group: usize,
        slot: usize,
        message: MidiMessage,
    ) -> Result<()> {
        check_midi_type(&message)?;
        if self.midi_messages_general_v2.len() != MIDI_GENERAL_GROUPS * MIDI_GENERAL_SLOTS
            || self.midi_messages_general.len() != MIDI_GENERAL_GROUPS
        {
            return Err(Error::Layout(
                "the general MIDI tables need 10 and 120 entries",
            ));
        }
        for (index, len, kind) in [
            (group, MIDI_GENERAL_GROUPS, "MIDI group"),
            (slot, MIDI_GENERAL_SLOTS, "MIDI group slot"),
        ] {
            if index >= len {
                return Err(Error::Index { kind, index, len });
            }
        }
        if slot == 0 {
            self.midi_messages_general[group] = message.clone();
        }
        self.midi_messages_general_v2[group * MIDI_GENERAL_SLOTS + slot] = message;
        Ok(())
    }

    /// Replaces one whole MIDI list, as the stock editor stores it (`0x3c1a7c` →
    /// `0x5146d4` on-load, `0x514784` per source): `messages` fill the first slots and the
    /// rest become empty. Short lists of older files are first padded to the stock
    /// writer's 12 / 10 / 120 entries.
    pub fn set_midi_list(&mut self, list: MidiList, messages: &[MidiMessage]) -> Result<()> {
        let len = match list {
            MidiList::OnLoad => MIDI_MESSAGES,
            MidiList::Group(group) if group < MIDI_GENERAL_GROUPS => MIDI_GENERAL_SLOTS,
            MidiList::Group(group) => {
                return Err(Error::Index {
                    kind: "MIDI group",
                    index: group,
                    len: MIDI_GENERAL_GROUPS,
                });
            }
        };
        if messages.len() > len {
            return Err(Error::Index {
                kind: "MIDI message",
                index: messages.len() - 1,
                len,
            });
        }
        messages.iter().try_for_each(check_midi_type)?;
        let padded = |list: &mut Vec<MidiMessage>, len| {
            if list.len() < len {
                list.resize(len, MidiMessage::default());
            }
        };
        padded(&mut self.midi_messages, MIDI_MESSAGES);
        padded(&mut self.midi_messages_general, MIDI_GENERAL_GROUPS);
        padded(
            &mut self.midi_messages_general_v2,
            MIDI_GENERAL_GROUPS * MIDI_GENERAL_SLOTS,
        );
        let filled = messages
            .iter()
            .cloned()
            .chain(std::iter::repeat_with(MidiMessage::default));
        match list {
            MidiList::OnLoad => {
                for (slot, message) in self.midi_messages.iter_mut().zip(filled) {
                    *slot = message;
                }
            }
            MidiList::Group(group) => {
                for (slot, message) in filled.take(MIDI_GENERAL_SLOTS).enumerate() {
                    self.set_midi_general(group, slot, message)?;
                }
            }
        }
        Ok(())
    }
}

/// One Preset MIDI Out list of [`Preset::set_midi_list`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MidiList {
    /// Field 16, sent on every preset load.
    OnLoad,
    /// Source 0..=9 of field 33 (stomps A..H, then EXP 1 and EXP 2).
    Group(usize),
}

/// A stored per-scene list of any element type, for scene copy and swap.
trait SceneList {
    fn scene_len(&self) -> usize;
    fn pad(&mut self, len: usize);
    fn swap(&mut self, a: usize, b: usize);
    fn copy(&mut self, from: usize, to: usize);
}

impl<T: Clone + Default> SceneList for Vec<T> {
    fn scene_len(&self) -> usize {
        self.len()
    }

    fn pad(&mut self, len: usize) {
        if self.len() < len {
            self.resize(len, T::default());
        }
    }

    fn swap(&mut self, a: usize, b: usize) {
        self.as_mut_slice().swap(a, b);
    }

    fn copy(&mut self, from: usize, to: usize) {
        self[to] = self[from].clone();
    }
}

fn check_midi_type(message: &MidiMessage) -> Result<()> {
    if message.kind > 0xff {
        return Err(Error::Unsupported("stock MIDI message types are 8-bit"));
    }
    Ok(())
}
