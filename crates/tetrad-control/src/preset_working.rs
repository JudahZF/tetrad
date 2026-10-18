//! Explicit preset-document bytes to a working scene-label/color prefix only.
//! This is not a full preset import, lifecycle load, or effect executor. Original
//! bytes and their summary remain immutable observations, not working-state export.

use tetrad_core::workflow::{
    ApplicationCore, BankPosition, BaselineId, MAX_SCENE_STORAGE_SLOTS, ModeId, PresetIndex,
    SceneEntry, SceneStorage, UiText, WorkflowError, WorkflowInit,
};
use tetrad_preset::{Limits, PresetDocument, PresetSummary};

/// The caller explicitly asserts the input is a preset document. Product type
/// and metadata are not selectors and are not accepted by this API.
#[derive(Debug, Clone, Copy)]
pub struct PresetDocumentAssertion;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Omission {
    LabelWithoutColor { index: usize },
    ColorWithoutLabel { index: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OmissionAck {
    RequireNoOmissions,
    ExplicitTail { count: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureAdmission {
    SceneLabelColorPrefixOnly,
}

/// Caller facts, never derived from the document name, hash or default scene.
#[derive(Debug, Clone)]
pub struct WorkingShell {
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

#[derive(Debug, thiserror::Error)]
pub enum WorkingPresetError {
    #[error(transparent)]
    Preset(#[from] tetrad_preset::Error),
    #[error("preset document did not round trip exactly")]
    RoundTripMismatch,
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    #[error("scene prefix has {count} unacknowledged tail omissions")]
    UnacknowledgedOmissions { count: usize },
    #[error("scene tail acknowledgement count {actual} must equal {expected}")]
    OmissionCountMismatch { expected: usize, actual: usize },
    #[error("scene-field export does not support alternate storage")]
    AlternateStorage,
    #[error("working scene count {actual} must equal the admitted pair count {expected}")]
    WorkingSceneCountMismatch { expected: usize, actual: usize },
    #[error("working scene label at index {index} is not valid UTF-16")]
    InvalidSceneLabel { index: usize },
}

/// Original lossless bytes and one typed observation. Neither can be edited
/// through this API; changing the installed core does not change these bytes.
#[derive(Debug)]
pub struct RetainedDocument {
    bytes: Vec<u8>,
    summary: PresetSummary,
}

impl RetainedDocument {
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn summary(&self) -> &PresetSummary {
        &self.summary
    }
}

/// Only projection can construct a capture. Read-only access prevents a caller
/// from changing pairs or omissions before the consuming installation check.
#[derive(Debug)]
pub struct ScenePrefixCapture {
    document: RetainedDocument,
    pairs: Vec<SceneEntry>,
    omissions: Vec<Omission>,
}

impl ScenePrefixCapture {
    pub fn document(&self) -> &RetainedDocument {
        &self.document
    }

    pub fn pairs(&self) -> &[SceneEntry] {
        &self.pairs
    }

    pub fn omissions(&self) -> &[Omission] {
        &self.omissions
    }

    pub fn admission(&self) -> CaptureAdmission {
        CaptureAdmission::SceneLabelColorPrefixOnly
    }

    /// Checks the exact tail acknowledgement before any core exists. Installs
    /// through initialization, not scene-field actions or lifecycle operations.
    pub fn install(
        self,
        ack: OmissionAck,
        shell: WorkingShell,
    ) -> Result<InstalledScenePrefix, WorkingPresetError> {
        let expected = self.omissions.len();
        match ack {
            OmissionAck::RequireNoOmissions if expected != 0 => {
                return Err(WorkingPresetError::UnacknowledgedOmissions { count: expected });
            }
            OmissionAck::ExplicitTail { count } if count != expected => {
                return Err(WorkingPresetError::OmissionCountMismatch {
                    expected,
                    actual: count,
                });
            }
            _ => {}
        }
        let core = ApplicationCore::new(WorkflowInit {
            working: SceneStorage::new(self.pairs)?,
            alternate: None,
            baseline: shell.baseline,
            mode: shell.mode,
            previous_mode: shell.previous_mode,
            page: shell.page,
            ui_dirty: shell.ui_dirty,
            last_notified_dirty: shell.last_notified_dirty,
            position: shell.position,
            title: shell.title,
            requested: shell.requested,
            loaded: shell.loaded,
        });
        Ok(InstalledScenePrefix {
            core,
            document: self.document,
            omissions: self.omissions,
        })
    }
}

/// Use `application::dispatch_application_scene` directly on `core`. Its existing
/// outcome contains unsupported intents, not executed effects. The core is
/// ordinary mutable working state, separate from the immutable original document.
#[derive(Debug)]
pub struct InstalledScenePrefix {
    pub core: ApplicationCore,
    document: RetainedDocument,
    omissions: Vec<Omission>,
}

impl InstalledScenePrefix {
    pub fn document(&self) -> &RetainedDocument {
        &self.document
    }

    pub fn omissions(&self) -> &[Omission] {
        &self.omissions
    }

    pub fn admission(&self) -> CaptureAdmission {
        CaptureAdmission::SceneLabelColorPrefixOnly
    }

    /// Returns a scene-field edited export of the original document, not a full
    /// core serialization or preset save. Caller limits bound parsing and edits.
    /// Only differing admitted working labels/colors are rewritten; unpaired
    /// tails, summary padding and all other fields keep their original bytes.
    /// Labels require exact UTF-16 conversion, without whitespace normalization.
    /// Alternate storage and a changed working count refuse export. Packed colors
    /// may survive unchanged, but any color edit invokes the restrictive editor.
    /// Success and failure leave the core and all retained observations unchanged.
    pub fn export_scene_fields(&self, limits: Limits) -> Result<Vec<u8>, WorkingPresetError> {
        let state = self.core.state();
        if state.alternate.is_some() {
            return Err(WorkingPresetError::AlternateStorage);
        }
        let summary = self.document.summary();
        let labels = summary.scenes.iter().filter(|s| s.label.is_some()).count();
        let colors = summary.scenes.iter().filter(|s| s.color.is_some()).count();
        let paired = labels.min(colors);
        let working = state.working.entries();
        if working.len() != paired {
            return Err(WorkingPresetError::WorkingSceneCountMismatch {
                expected: paired,
                actual: working.len(),
            });
        }
        let labels = working
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                String::from_utf16(entry.label.units())
                    .map_err(|_| WorkingPresetError::InvalidSceneLabel { index })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut candidate = PresetDocument::parse(self.document.bytes(), limits)?;
        if candidate.to_bytes() != self.document.bytes() {
            return Err(WorkingPresetError::RoundTripMismatch);
        }
        for (index, ((entry, label), original)) in working
            .iter()
            .zip(&labels)
            .zip(&summary.scenes[..paired])
            .enumerate()
        {
            if original.label.as_deref() != Some(label.as_str()) {
                candidate.set_scene_label(index, label)?;
            }
            if original.color != Some(entry.raw_color) {
                candidate.set_scene_color(index, entry.raw_color)?;
            }
        }
        Ok(candidate.to_bytes())
    }
}

/// Parses with caller limits, requires exact wire round trip, then admits only
/// paired labels/colors. Summary padding from tempos or parameters is not a hole.
/// Unpaired tails stay retained and are not subject to working UiText limits.
/// Even an empty valid protobuf remains a prefix-only observation, not a preset
/// completeness assertion. An absent storage BLOB must not be passed as empty.
pub fn project_scene_prefix(
    bytes: &[u8],
    _assertion: PresetDocumentAssertion,
    limits: Limits,
) -> Result<ScenePrefixCapture, WorkingPresetError> {
    let document = PresetDocument::parse(bytes, limits)?;
    let retained_bytes = document.to_bytes();
    if retained_bytes != bytes {
        return Err(WorkingPresetError::RoundTripMismatch);
    }
    let summary = document.summary()?;
    let labels = summary.scenes.iter().filter(|s| s.label.is_some()).count();
    let colors = summary.scenes.iter().filter(|s| s.color.is_some()).count();
    let paired = labels.min(colors);
    // Check before allocating the working projection, not after building it.
    if paired > MAX_SCENE_STORAGE_SLOTS {
        return Err(WorkflowError::ResourceLimit.into());
    }
    let mut pairs = Vec::with_capacity(paired);
    for scene in &summary.scenes[..paired] {
        pairs.push(SceneEntry {
            label: UiText::from_literal(scene.label.as_deref().expect("present label prefix"))?,
            raw_color: scene.color.expect("present color prefix"),
        });
    }
    // The existing parser limits bound these observed repeated-field counts.
    let omissions = (paired..labels.max(colors))
        .map(|index| {
            if labels > colors {
                Omission::LabelWithoutColor { index }
            } else {
                Omission::ColorWithoutLabel { index }
            }
        })
        .collect();
    Ok(ScenePrefixCapture {
        document: RetainedDocument {
            bytes: retained_bytes,
            summary,
        },
        pairs,
        omissions,
    })
}
