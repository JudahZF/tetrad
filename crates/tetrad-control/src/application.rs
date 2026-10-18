//! Local SceneMessage routing into the recovered application state. Effects
//! remain intents; this module has no renderer, backend or transport executor.

use tetrad_core::workflow::{Action, ApplicationCore, Effect, Millis, SceneIndex};

use crate::scene::{SCENE_MESSAGE_TYPE, SceneLimits, SceneRequest};
use crate::{Envelope, Error, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplicationSceneOutcome {
    pub request_id: Option<u64>,
    pub selected_scene: u32,
    pub effects: Vec<Effect>,
}

/// The wire UPDATE calls the low-level, non-forced scene setter, not the front
/// action guarded by bank preview. READ observes state without advancing the
/// injected action clock. The result is local, not a stock wire response.
pub fn dispatch_application_scene(
    core: &mut ApplicationCore,
    envelope: &Envelope,
    limits: SceneLimits,
    now: Millis,
) -> Result<ApplicationSceneOutcome> {
    if envelope.message_type != SCENE_MESSAGE_TYPE {
        return Err(Error::UnsupportedSceneType(envelope.message_type));
    }
    let request = SceneRequest::decode(envelope.plain_payload()?, limits)?;
    let effects = match request.action.unwrap_or(0) {
        1 => {
            let selected = request.selected_scene.unwrap_or(0);
            if selected > 7 {
                return Err(Error::Scene(selected));
            }
            core.dispatch(
                Action::SelectScene {
                    scene: SceneIndex::new(selected as i32)?,
                    force: false,
                },
                now,
            )?
        }
        3 => Vec::new(),
        action => return Err(Error::UnsupportedSceneAction(action)),
    };
    Ok(ApplicationSceneOutcome {
        request_id: request.request_id,
        selected_scene: u32::from(core.state().scene.get()),
        effects,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tetrad_core::workflow::{
        BankPosition, BankSlot, BaselineId, CallerValidatedBank, ModeId, ObserverEvent,
        PresetIndex, SceneStorage, UiText, WorkflowError, WorkflowInit,
    };

    fn core() -> ApplicationCore {
        ApplicationCore::new(WorkflowInit {
            working: SceneStorage::new(Vec::new()).unwrap(),
            alternate: None,
            baseline: BaselineId(11),
            mode: ModeId::new(0).unwrap(),
            previous_mode: ModeId::new(0).unwrap(),
            page: 0,
            ui_dirty: false,
            last_notified_dirty: false,
            position: BankPosition {
                bank: CallerValidatedBank(3),
                slot: BankSlot::new(2).unwrap(),
            },
            title: UiText::from_literal("Synthetic preview").unwrap(),
            requested: Some(PresetIndex(26)),
            loaded: Some(PresetIndex(26)),
        })
    }

    fn envelope(payload: &[u8]) -> Envelope {
        Envelope {
            message_type: SCENE_MESSAGE_TYPE,
            transform: 0,
            decompress: 0,
            opaque: [0xa5, 0x5a],
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn wire_and_front_actions_share_scene_state_but_keep_distinct_bank_guards() {
        let mut app = core();
        app.dispatch(
            Action::FrontSelectScene(SceneIndex::new(2).unwrap()),
            Millis(10),
        )
        .unwrap();
        app.dispatch(
            Action::BeginBank {
                special_folder: false,
            },
            Millis(11),
        )
        .unwrap();
        let front = app
            .dispatch(
                Action::FrontSelectScene(SceneIndex::new(7).unwrap()),
                Millis(12),
            )
            .unwrap();
        assert!(front.is_empty());
        assert_eq!(app.state().scene.get(), 2);
        let source = envelope(&[8, 1, 16, 42, 24, 7]);
        let outcome =
            dispatch_application_scene(&mut app, &source, SceneLimits::default(), Millis(13))
                .unwrap();
        assert_eq!(outcome.request_id, Some(42));
        assert_eq!(outcome.selected_scene, 7);
        assert!(outcome.effects.iter().any(|effect| matches!(effect,
            Effect::Observer(ObserverEvent::SceneSelected { current, previous })
                if current.get() == 7 && previous.get() == 2)));
        assert!(app.state().bank.active);
        assert_eq!(app.state().bank.loaded, Some(PresetIndex(26)));
        assert_eq!(source, envelope(&[8, 1, 16, 42, 24, 7]));
        let repeated =
            dispatch_application_scene(&mut app, &source, SceneLimits::default(), Millis(14))
                .unwrap();
        assert!(repeated.effects.is_empty());
        let before_read = app.clone();
        let read = dispatch_application_scene(
            &mut app,
            &envelope(&[8, 3, 16, 0]),
            SceneLimits::default(),
            Millis(0),
        )
        .unwrap();
        assert_eq!(read.request_id, Some(0));
        assert_eq!(read.selected_scene, 7);
        assert!(read.effects.is_empty());
        assert_eq!(app, before_read);
        let reset = dispatch_application_scene(
            &mut app,
            &envelope(&[8, 1]),
            SceneLimits::default(),
            Millis(15),
        )
        .unwrap();
        assert_eq!(reset.selected_scene, 0);
        assert_eq!(app.state().previous_scene.get(), 7);
    }

    #[test]
    fn wire_and_workflow_errors_roll_back_the_same_application_state() {
        let mut app = core();
        app.dispatch(
            Action::SelectScene {
                scene: SceneIndex::new(5).unwrap(),
                force: false,
            },
            Millis(100),
        )
        .unwrap();
        let before = app.clone();
        for payload in [
            &[8, 1, 24, 8][..],
            &[8, 2],
            &[8, 1, 24, 2, 0x80],
            &[8, 1, 24, 2, 0],
        ] {
            assert!(
                dispatch_application_scene(
                    &mut app,
                    &envelope(payload),
                    SceneLimits::default(),
                    Millis(101)
                )
                .is_err()
            );
            assert_eq!(app, before);
        }
        assert_eq!(
            dispatch_application_scene(
                &mut app,
                &envelope(&[8, 1, 24, 2]),
                SceneLimits::default(),
                Millis(99)
            ),
            Err(Error::Workflow(WorkflowError::ClockRegression))
        );
        assert_eq!(app, before);
        let accepted = dispatch_application_scene(
            &mut app,
            &envelope(&[8, 1, 24, 2]),
            SceneLimits::default(),
            Millis(100),
        )
        .unwrap();
        assert_eq!(accepted.selected_scene, 2);
    }
}
