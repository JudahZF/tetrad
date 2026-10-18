//! Gig stomp rename/momentary workflow (`GigView` slot 6, `0x65cb3c`).
//!
//! Label setter `0x514474` writes the working preset's grouped (`+5424`) or
//! single (`+5448`) map; empty text removes the key. Momentary setter `0x5227c4`
//! returns on Atma, mirrors the effective flag into parameter 1 of each
//! assigned model 26001 through the general setter, then writes the bool map.
//! Pure decisions live in `crate::performance::stomp`.

use std::collections::BTreeMap;

use super::{
    ApplicationCore, ControlRequest, Effect, Millis, PresetContext, RendererRequest, Result,
    UiText, WorkflowError,
};
use crate::performance::stomp::{
    LabelEdit, LabelMap, StompIndex, effective_momentary, label_edit, rename_starts_dirty,
};

/// Preset fields 32 (`stomp_labels`), 34 (`single_stomp_labels`) and 35
/// (`stomp_is_momentary`), keyed by stomp index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StompMaps {
    pub grouped_labels: BTreeMap<StompIndex, UiText>,
    pub single_labels: BTreeMap<StompIndex, UiText>,
    pub momentary: BTreeMap<StompIndex, bool>,
}

/// Grid address of an assigned block.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlockAddress {
    pub row: u8,
    pub column: u8,
}

/// One confirmed rename dialog plus the facts stock reads during the slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StompRename {
    pub stomp: StompIndex,
    pub label: UiText,
    pub momentary: bool,
    /// Assigned records of the effective context (`0x2e2c44`).
    pub assigned_count: usize,
    /// `0x2c3f48(index, 1)`; generation needs grid and model names.
    pub generated_default: UiText,
    /// Getter `0x2e220c`, which has a platform branch (`0x49a754`).
    pub current_momentary: bool,
    /// Atma predicate `0x499a60`.
    pub atma: bool,
    /// An assigned block is model 27000 (`0x514598`).
    pub non_momentary_model_assigned: bool,
    /// Assigned model-26001 blocks, in assignment order.
    pub mirror_blocks: Vec<BlockAddress>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StompAction {
    /// Replaces the working maps (an external load result) and advances the
    /// working revision.
    Install(StompMaps),
    Rename(StompRename),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StompEffect {
    /// Parameter 1 of a model-26001 block, through the general setter
    /// (`0x51e4d8`): Float `1.0` when momentary, else `0.0` (`0x522970`).
    MirrorMomentary {
        block: BlockAddress,
        momentary: bool,
    },
}

impl ApplicationCore {
    pub(super) fn apply_stomp(
        &mut self,
        action: StompAction,
        now: Millis,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        if self.state.alternate.is_some() {
            return Err(WorkflowError::AlternateStompContext);
        }
        match action {
            StompAction::Install(maps) => {
                self.bump_revision(PresetContext::Working)?;
                self.state.stomps = maps;
            }
            StompAction::Rename(rename) => self.rename_stomp(rename, now, effects)?,
        }
        Ok(())
    }

    fn rename_stomp(
        &mut self,
        rename: StompRename,
        now: Millis,
        effects: &mut Vec<Effect>,
    ) -> Result<()> {
        let key = rename.stomp;
        let map = LabelMap::for_assignment_count(rename.assigned_count);
        let labels = match map {
            LabelMap::Grouped => &mut self.state.stomps.grouped_labels,
            LabelMap::Single => &mut self.state.stomps.single_labels,
        };
        let old = labels.get(&key).cloned().unwrap_or_default();
        let edit = label_edit(&rename.label, &rename.generated_default, &old);
        if edit != LabelEdit::Unchanged {
            match edit {
                LabelEdit::Set(text) if !text.units().is_empty() => {
                    labels.insert(key, text);
                }
                _ => {
                    labels.remove(&key);
                }
            }
            self.bump_revision(PresetContext::Working)?;
            effects.push(Effect::Control(ControlRequest::StompLabel {
                stomp: rename.stomp.get(),
                grouped: map == LabelMap::Grouped,
            }));
        }
        if rename.momentary != rename.current_momentary && !rename.atma {
            let effective =
                effective_momentary(rename.momentary, rename.non_momentary_model_assigned);
            for &block in &rename.mirror_blocks {
                effects.push(Effect::Stomp(StompEffect::MirrorMomentary {
                    block,
                    momentary: effective,
                }));
            }
            self.state.stomps.momentary.insert(key, effective);
            self.bump_revision(PresetContext::Working)?;
            effects.push(Effect::Control(ControlRequest::StompMomentary(
                rename.stomp.get(),
            )));
        }
        if rename_starts_dirty(
            &rename.label,
            &old,
            rename.momentary,
            rename.current_momentary,
        ) {
            self.schedule_dirty(now, effects)?;
        }
        effects.push(Effect::Renderer(RendererRequest::RefreshStomps));
        effects.push(Effect::Renderer(RendererRequest::RefreshTitle));
        Ok(())
    }
}
