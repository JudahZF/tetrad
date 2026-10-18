//! Lossless local preset editing driven by owned UI commands. No DSP is loaded.

use tetrad_preset::{Limits, PresetDocument};
use tetrad_ui::{SceneIndex, UiCommand, UiSnapshot, UiState};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error(transparent)]
    Preset(#[from] tetrad_preset::Error),
    #[error(transparent)]
    Ui(#[from] tetrad_ui::UiError),
    #[error(transparent)]
    GridImport(#[from] tetrad_engine::ImportError),
    #[error(transparent)]
    Engine(#[from] tetrad_engine::Error),
    #[error("preset has {0} scene entries; this view supports at most eight")]
    SceneCount(usize),
    #[error("offline preset editor cannot execute {0:?}")]
    Unsupported(UiCommand),
}

#[derive(Clone)]
pub struct OfflineSession {
    original: Vec<u8>,
    working: PresetDocument,
    selected_scene: SceneIndex,
}

impl OfflineSession {
    pub fn from_bytes(bytes: &[u8], selected_scene: SceneIndex) -> Result<Self, SessionError> {
        let working = PresetDocument::parse(bytes, Limits::default())?;
        let session = Self {
            original: bytes.to_vec(),
            working,
            selected_scene,
        };
        session.snapshot()?;
        Ok(session)
    }

    pub fn snapshot(&self) -> Result<UiSnapshot, SessionError> {
        snapshot(&self.working, &self.original, self.selected_scene)
    }

    /// Opt-in admitted grid metadata only. Detached controls and model parameters
    /// are not executed; caller-selected bypass polarity is not stock audio proof.
    pub fn grid_snapshot(
        &self,
        policy: tetrad_engine::ImportPolicy,
    ) -> Result<UiSnapshot, SessionError> {
        let imported = tetrad_engine::import_summary(&self.working.summary()?, policy)?;
        let scene = tetrad_engine::SceneIndex::new(self.selected_scene.get())?;
        let mut view = self.snapshot()?;
        for row in 0..tetrad_engine::ROWS {
            for column in 0..tetrad_engine::COLUMNS {
                let slot = tetrad_engine::Slot {
                    row: tetrad_engine::Row::new(row as u8)?,
                    column: tetrad_engine::Column::new(column as u8)?,
                };
                if let Some(block) = imported.grid.block(slot) {
                    let label = match block.kind {
                        tetrad_engine::BlockKind::Model(id) => format!("MODEL {}", id.0),
                        tetrad_engine::BlockKind::Gain(gain) => {
                            format!("{:.1} DB", gain.at(scene).db())
                        }
                    };
                    view.grid[row * tetrad_engine::COLUMNS + column] = tetrad_ui::GridCell {
                        label,
                        bypassed: block.bypass.at(scene),
                    };
                }
            }
        }
        view.status = format!(
            "OWNED GRID / {} DEFAULTS / {} DETACHED / NO DSP",
            imported.defaulted.len(),
            imported.grid.detached().len()
        );
        let mut admission = UiState::new();
        admission.set_snapshot(view.clone())?;
        Ok(view)
    }

    /// Commit all admitted commands together. Refused edits preserve bytes and
    /// view selection. Selection changes the offline view, not a DSP scene.
    pub fn apply(&mut self, commands: &[UiCommand]) -> Result<UiSnapshot, SessionError> {
        let mut working = self.working.clone();
        let mut selected = self.selected_scene;
        for command in commands {
            match command {
                UiCommand::SelectScene { scene } => selected = *scene,
                UiCommand::SetSceneLabel { scene, label } => {
                    let index = usize::from(scene.get());
                    let summary = working.summary()?;
                    let current = summary
                        .scenes
                        .get(index)
                        .and_then(|scene| scene.label.as_deref())
                        .unwrap_or("");
                    // Confirmation emits all eight view entries, including absent
                    // tails. An unchanged display entry must not create presence.
                    if current != label {
                        working.set_scene_label(index, label)?;
                    }
                }
                UiCommand::SetSceneColor { scene, raw_color } => {
                    let index = usize::from(scene.get());
                    let summary = working.summary()?;
                    let current = summary
                        .scenes
                        .get(index)
                        .and_then(|scene| scene.color)
                        .unwrap_or(0);
                    if current != *raw_color {
                        working.set_scene_color(index, *raw_color)?;
                    }
                }
                UiCommand::Rejected(error) => return Err((*error).into()),
                other => return Err(SessionError::Unsupported(other.clone())),
            }
        }
        let projection = snapshot(&working, &self.original, selected)?;
        self.working = working;
        self.selected_scene = selected;
        Ok(projection)
    }

    pub fn export(&self) -> Vec<u8> {
        self.working.to_bytes()
    }
}

fn snapshot(
    document: &PresetDocument,
    original: &[u8],
    selected_scene: SceneIndex,
) -> Result<UiSnapshot, SessionError> {
    let summary = document.summary()?;
    if summary.scene_count > tetrad_ui::SCENE_COUNT {
        return Err(SessionError::SceneCount(summary.scene_count));
    }
    let mut snapshot = UiSnapshot {
        // No stored hash or caller-selected path becomes a verified loaded identity.
        preset_title: summary.name.unwrap_or_else(|| "UNTITLED SOURCE".into()),
        selected_scene,
        dirty: document.to_bytes() != original,
        status: "LOCAL PRESET METADATA - NO DSP OR GRID LOAD".into(),
        ..UiSnapshot::default()
    };
    for scene in summary.scenes {
        if let Some(label) = scene.label {
            snapshot.scenes[scene.index].label = label;
        }
        if let Some(color) = scene.color {
            snapshot.scenes[scene.index].raw_color = color;
        }
    }
    let mut admission = UiState::new();
    admission.set_snapshot(snapshot.clone())?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tetrad_ui::{Control, InputEvent};

    fn fixture() -> Vec<u8> {
        // Name, scene labels A/B, unpacked scene colors 1/2 and unknown field 100.
        vec![
            0x12, 4, b'T', b'e', b's', b't', 0x7a, 1, b'A', 0x7a, 1, b'B', 0xf8, 1, 1, 0xf8, 1, 2,
            0xa0, 6, 7,
        ]
    }

    #[test]
    fn ui_confirmation_exports_losslessly_without_touching_original() {
        let bytes = fixture();
        let scene = SceneIndex::new(1).unwrap();
        let mut session = OfflineSession::from_bytes(&bytes, scene).unwrap();
        let mut ui = UiState::new();
        ui.set_snapshot(session.snapshot().unwrap()).unwrap();
        ui.show(tetrad_ui::Screen::Gig);
        ui.handle_input(InputEvent::Activate(Control::EditScenes));
        ui.handle_input(InputEvent::TextChanged("Bridge".into()));
        ui.handle_input(InputEvent::DraftColor(3));
        let commands = ui.handle_input(InputEvent::Activate(Control::ConfirmScenes));
        let snapshot = session.apply(&commands).unwrap();
        assert_eq!(snapshot.scenes[1].label, "Bridge");
        assert_eq!(snapshot.scenes[1].raw_color, 3);
        assert!(snapshot.dirty);
        assert_eq!(snapshot.preset_id, None);
        let exported = session.export();
        assert_eq!(&exported[exported.len() - 3..], &[0xa0, 6, 7]);
        assert_eq!(session.original, bytes);
        assert_eq!(
            PresetDocument::parse(&exported, Limits::default())
                .unwrap()
                .summary()
                .unwrap()
                .name
                .as_deref(),
            Some("Test")
        );
    }

    #[test]
    fn late_refusal_rolls_back_edits_and_view_selection() {
        let bytes = fixture();
        let first = SceneIndex::new(0).unwrap();
        let mut session = OfflineSession::from_bytes(&bytes, first).unwrap();
        let refused = session.apply(&[
            UiCommand::SelectScene {
                scene: SceneIndex::new(1).unwrap(),
            },
            UiCommand::SetSceneLabel {
                scene: first,
                label: "changed".into(),
            },
            UiCommand::SetSceneColor {
                scene: SceneIndex::new(7).unwrap(),
                raw_color: 9,
            },
        ]);
        assert!(refused.is_err());
        assert_eq!(session.export(), bytes);
        assert_eq!(session.snapshot().unwrap().selected_scene, first);
        assert!(!session.snapshot().unwrap().dirty);
    }
}
