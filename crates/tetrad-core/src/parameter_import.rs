use crate::{Error, Result, Version};

/// Runtime alternatives only; authoritative wire storage remains in tetrad-preset.
/// Float values are copied without conversion, including NaN payloads.
#[derive(Debug, Clone, PartialEq)]
pub enum ParameterValue {
    Int(i32),
    Float(f32),
    String(String),
}

/// Eight positional scenes. None means no stored value for this scene.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredParameterScenes {
    pub scenes: [Option<ParameterValue>; 8],
}

/// Resolved runtime state from the earlier expression/scene-mode stage.
/// Assignment codes are opaque; this does not infer expression enum meanings.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParameterControls {
    pub expression_assignment: i32,
    pub expression_min: f32,
    pub expression_max: f32,
    pub scene_mode: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeParameter {
    pub scenes: [ParameterValue; 8],
    pub controls: ParameterControls,
}

/// The caller resolves the XML-designated sidechain position. This flag does
/// not select generic normalization, a power mapping, or either migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedParameterMetadata {
    pub sidechain: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultReason {
    AddedParameter,
    LegacyInputGain,
    MissingPosition,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneValueSource {
    Defaults(DefaultReason),
    Stored {
        position: usize,
    },
    /// Accepts floats only, writing Float(0.0) for other present alternatives.
    LastStoredFloats {
        position: usize,
    },
}

/// Pure scene-value plan for the recovered positional file-import path.
/// T is XML parameter count plus MUTE and BYPASS. M is the stored count.
/// Versions must already be resolved with ImportVersions/Version; no signed
/// version conversion or implicit version default is performed here.
#[derive(Debug, Clone, Copy)]
pub struct ParameterImportPlan {
    model_id: u32,
    total: usize,
    stored: usize,
    modern: bool,
    legacy_input: bool,
    unsigned_gap: u32,
}

impl ParameterImportPlan {
    pub fn new(
        model_id: u32,
        xml_parameter_count: usize,
        stored_parameter_count: usize,
        modified_version: Version,
    ) -> Result<Self> {
        let total = xml_parameter_count
            .checked_add(2)
            .ok_or(Error::ParameterCount(xml_parameter_count))?;
        let target_count = checked_count(total)?;
        let stored_count = checked_count(stored_parameter_count)?;
        let modern = modified_version
            >= Version {
                major: 2,
                minor: 0,
                patch: 0,
            };
        let legacy_input = model_id == 28000
            && modified_version
                < Version {
                    major: 3,
                    minor: 0,
                    patch: 0,
                };
        Ok(Self {
            model_id,
            total,
            stored: stored_parameter_count,
            modern,
            legacy_input,
            // ARM VA 0x680900 subtracts u32 counts; 0x6809b0 uses BLS.
            // Only this verified predicate wraps. Index arithmetic never does.
            unsigned_gap: target_count.wrapping_sub(stored_count),
        })
    }

    pub fn scene_source(self, destination: usize) -> Result<SceneValueSource> {
        if destination >= self.total {
            return Err(Error::ParameterPosition {
                position: destination,
                count: self.total,
            });
        }
        let mute = self.total - 2;
        // VA 0x681118..0x681164 runs before the candidate/default checks.
        let candidate = if matches!(self.model_id, 5011 | 5016)
            && self.stored == self.total
            && destination == mute
        {
            destination + 1
        } else {
            destination
        };
        let added =
            self.modern && self.unsigned_gap > 1 && self.stored.checked_sub(1) == Some(candidate);
        let input_gain = self.legacy_input && candidate == 3;
        if candidate < self.stored && !added && !input_gain {
            return Ok(SceneValueSource::Stored {
                position: candidate,
            });
        }
        // VA 0x6811b8..0x6811e4: modern AND NOT legacy input model.
        // This is also reached when the candidate was deliberately suppressed.
        if destination == mute && self.modern && !self.legacy_input {
            let position = self
                .stored
                .checked_sub(1)
                .ok_or(Error::MissingLastParameter { destination })?;
            return Ok(SceneValueSource::LastStoredFloats { position });
        }
        Ok(SceneValueSource::Defaults(if added {
            DefaultReason::AddedParameter
        } else if input_gain {
            DefaultReason::LegacyInputGain
        } else {
            DefaultReason::MissingPosition
        }))
    }

    /// `initialized` contains caller-resolved defaults and the controls already
    /// imported at this destination in the earlier positional stage. Only scenes
    /// change here. In particular controls do not follow the candidate position.
    /// Missing individual scenes retain their destination defaults.
    pub fn import_parameter(
        self,
        destination: usize,
        initialized: &RuntimeParameter,
        stored: &[StoredParameterScenes],
        metadata: ResolvedParameterMetadata,
    ) -> Result<RuntimeParameter> {
        if stored.len() != self.stored {
            return Err(Error::StoredParameterCount {
                expected: self.stored,
                actual: stored.len(),
            });
        }
        let source = self.scene_source(destination)?;
        let (position, floats_only) = match source {
            SceneValueSource::Defaults(_) => return Ok(initialized.clone()),
            SceneValueSource::Stored { position } => (position, false),
            SceneValueSource::LastStoredFloats { position } => (position, true),
        };
        let scenes = &stored
            .get(position)
            .ok_or(Error::SourceParameterPosition {
                position,
                count: stored.len(),
            })?
            .scenes;
        let mut imported = initialized.clone();
        for (destination_value, stored_value) in imported.scenes.iter_mut().zip(scenes) {
            let Some(value) = stored_value else { continue };
            *destination_value = match value {
                ParameterValue::Float(value) => {
                    // Fallback VA 0x6811f8 bypasses the sidechain float branch.
                    let value = if !floats_only && metadata.sidechain && *value > 1.0 {
                        1.0
                    } else {
                        *value
                    };
                    ParameterValue::Float(value)
                }
                _ if floats_only => ParameterValue::Float(0.0),
                _ => value.clone(),
            };
        }
        Ok(imported)
    }
}

fn checked_count(count: usize) -> Result<u32> {
    let count_u32 = u32::try_from(count).map_err(|_| Error::ParameterCount(count))?;
    if count_u32 > i32::MAX.unsigned_abs() {
        return Err(Error::ParameterCount(count));
    }
    Ok(count_u32)
}
