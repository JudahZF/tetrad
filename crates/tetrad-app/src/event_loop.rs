//! Synchronous, injected UI input → command batch → admitted state → RGB frame.
//! No source, backend or presenter here accesses a device or executes stock code.

use tetrad_ui::{Frame, InputEvent, UiCommand, UiError, UiSnapshot, UiState};

use crate::session::{OfflineSession, SessionError};

/// A projection that has passed the same admission checks as `UiState`.
/// Backends must construct this before committing any application state.
#[derive(Debug, Clone)]
pub struct AdmittedSnapshot(UiSnapshot);

impl AdmittedSnapshot {
    pub fn new(snapshot: UiSnapshot) -> Result<Self, UiError> {
        let mut admission = UiState::new();
        admission.set_snapshot(snapshot.clone())?;
        Ok(Self(snapshot))
    }

    pub fn snapshot(&self) -> &UiSnapshot {
        &self.0
    }
}

/// Each call is one synchronous transaction. `Err` MUST mean no application
/// effects occurred. `Ok` MUST describe the committed state, admitted BEFORE
/// commit. A transport with uncertain/partial effects cannot implement this
/// contract by returning a normal refusal; it needs a separate recovery policy.
///
/// The loop does not accept unsolicited snapshots or keep an async reply queue.
pub trait CommandBackend {
    type Error;

    fn snapshot(&self) -> Result<AdmittedSnapshot, Self::Error>;
    fn apply(&mut self, commands: &[UiCommand]) -> Result<AdmittedSnapshot, Self::Error>;
}

/// Lossless local scene metadata transactions, optionally projecting admitted
/// grid metadata. Neither path loads DSP or invents a verified preset identity.
pub struct OfflineBackend {
    session: OfflineSession,
    grid_policy: Option<tetrad_engine::ImportPolicy>,
}

impl OfflineBackend {
    pub fn new(
        session: OfflineSession,
        grid_policy: Option<tetrad_engine::ImportPolicy>,
    ) -> Result<Self, SessionError> {
        let backend = Self {
            session,
            grid_policy,
        };
        backend.snapshot()?;
        Ok(backend)
    }

    pub fn session(&self) -> &OfflineSession {
        &self.session
    }

    pub fn into_session(self) -> OfflineSession {
        self.session
    }

    fn project(&self, session: &OfflineSession) -> Result<AdmittedSnapshot, SessionError> {
        let snapshot = match self.grid_policy {
            Some(policy) => session.grid_snapshot(policy)?,
            None => session.snapshot()?,
        };
        Ok(AdmittedSnapshot::new(snapshot)?)
    }
}

impl CommandBackend for OfflineBackend {
    type Error = SessionError;

    fn snapshot(&self) -> Result<AdmittedSnapshot, Self::Error> {
        self.project(&self.session)
    }

    fn apply(&mut self, commands: &[UiCommand]) -> Result<AdmittedSnapshot, Self::Error> {
        // Clone retains the exact wire source and original dirty baseline. Even
        // a failure after candidate.apply (grid projection/admission) is inert.
        let mut candidate = self.session.clone();
        candidate.apply(commands)?;
        let snapshot = self.project(&candidate)?;
        self.session = candidate;
        Ok(snapshot)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopEvent {
    Input(InputEvent),
    Quit,
}

/// `None` is exhaustion, not a request to spin. The caller owns blocking/time
/// bounds and conversion from physical input or serialized replay events.
pub trait InputSource {
    type Error;

    fn next_event(&mut self) -> Result<Option<LoopEvent>, Self::Error>;
}

/// Presentation is separate from application commit. An error may mean a frame
/// was partially displayed/written; it MUST NOT be interpreted as backend undo.
pub trait FrameSink {
    type Error;

    fn present(&mut self, frame: &Frame) -> Result<(), Self::Error>;
}

#[derive(Debug)]
pub enum BatchDisposition<E> {
    NoCommands,
    Applied { commands: usize },
    Rejected(UiError),
    Refused(E),
}

#[derive(Debug)]
pub enum StepResult<E> {
    Frame {
        batch: BatchDisposition<E>,
        frame: Frame,
    },
    Quit,
}

#[derive(Debug)]
pub enum RunStop<I, B, P> {
    Quit,
    InputExhausted,
    InputFailed(I),
    LimitReached,
    UiRejected(UiError),
    BackendRefused(B),
    PresentationFailed {
        error: P,
        batch: BatchDisposition<B>,
    },
}

#[derive(Debug)]
pub struct RunReport<I, B, P> {
    /// Consumed events, including quit, but not exhaustion/source errors.
    pub events: usize,
    pub stop: RunStop<I, B, P>,
}

/// Owns one backend and one UI. No external replacement or mutable backend/UI
/// accessor is exposed: each acknowledgement belongs to the current call.
pub struct EventLoop<B> {
    ui: UiState,
    backend: B,
    quit: bool,
}

impl<B: CommandBackend> EventLoop<B> {
    /// Admit the initial backend view, preserving navigation but discarding any
    /// draft: equal scene fields or absent preset IDs do not prove source ownership.
    pub fn new(mut ui: UiState, backend: B) -> Result<Self, B::Error> {
        let snapshot = backend.snapshot()?;
        if ui.scene_dialog_open() {
            // Back closes an open draft without navigation or backend commands.
            ui.handle_input(InputEvent::Back);
        }
        ui.set_snapshot(snapshot.0)
            .expect("AdmittedSnapshot has already passed UiState admission");
        Ok(Self {
            ui,
            backend,
            quit: false,
        })
    }

    pub fn ui(&self) -> &UiState {
        &self.ui
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }

    pub fn into_parts(self) -> (UiState, B) {
        (self.ui, self.backend)
    }

    /// Generate requests on a UI clone. A refusal/rejection keeps the entire
    /// pre-event UI (including a taken confirmation draft), but cancels pointer
    /// state so a released confirmation cannot fire again. Only a committed
    /// snapshot changes loaded state. This does not undo backend/presenter I/O.
    pub fn step(&mut self, event: LoopEvent) -> StepResult<B::Error> {
        if self.quit || matches!(event, LoopEvent::Quit) {
            self.quit = true;
            self.cancel_pointer();
            return StepResult::Quit;
        }
        let LoopEvent::Input(input) = event else {
            unreachable!();
        };
        let mut candidate = self.ui.clone();
        let commands = candidate.handle_input(input);
        let batch = if let Some(error) = commands.iter().find_map(|command| match command {
            UiCommand::Rejected(error) => Some(*error),
            _ => None,
        }) {
            self.cancel_pointer();
            BatchDisposition::Rejected(error)
        } else if commands.is_empty() {
            self.ui = candidate;
            BatchDisposition::NoCommands
        } else {
            match self.backend.apply(&commands) {
                Ok(snapshot) => {
                    candidate
                        .set_snapshot(snapshot.0)
                        .expect("AdmittedSnapshot has already passed UiState admission");
                    self.ui = candidate;
                    BatchDisposition::Applied {
                        commands: commands.len(),
                    }
                }
                Err(error) => {
                    self.cancel_pointer();
                    BatchDisposition::Refused(error)
                }
            }
        };
        StepResult::Frame {
            batch,
            frame: self.ui.render(),
        }
    }

    /// Consume at most `max_events`; do not read ahead. Present once per input,
    /// including a retained view on refusal, then stop on any refusal/failure.
    /// No initial frame is implicit: callers can present `ui().render()` first.
    /// Resuming after failure never replays the already consumed event.
    pub fn run<S: InputSource, P: FrameSink>(
        &mut self,
        source: &mut S,
        sink: &mut P,
        max_events: usize,
    ) -> RunReport<S::Error, B::Error, P::Error> {
        let mut events = 0;
        let stop = loop {
            if self.quit {
                break RunStop::Quit;
            }
            if events == max_events {
                break RunStop::LimitReached;
            }
            let event = match source.next_event() {
                Ok(Some(event)) => event,
                Ok(None) => break RunStop::InputExhausted,
                Err(error) => break RunStop::InputFailed(error),
            };
            events += 1;
            let StepResult::Frame { batch, frame } = self.step(event) else {
                break RunStop::Quit;
            };
            if let Err(error) = sink.present(&frame) {
                break RunStop::PresentationFailed { error, batch };
            }
            match batch {
                BatchDisposition::Rejected(error) => break RunStop::UiRejected(error),
                BatchDisposition::Refused(error) => break RunStop::BackendRefused(error),
                BatchDisposition::NoCommands | BatchDisposition::Applied { .. } => {}
            }
        };
        self.cancel_pointer();
        RunReport { events, stop }
    }

    fn cancel_pointer(&mut self) {
        self.ui.handle_input(InputEvent::CancelPointer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tetrad_ui::{Control, SceneIndex, Screen};

    fn fixture() -> Vec<u8> {
        // Two scene labels/colors and an unknown field, all synthetic.
        vec![
            0x12, 4, b'T', b'e', b's', b't', 0x7a, 1, b'A', 0x7a, 1, b'B', 0xf8, 1, 1, 0xf8, 1, 2,
            0xa0, 6, 7,
        ]
    }

    fn scene(index: u8) -> SceneIndex {
        SceneIndex::new(index).unwrap()
    }

    fn offline_loop() -> EventLoop<OfflineBackend> {
        let session = OfflineSession::from_bytes(&fixture(), scene(0)).unwrap();
        let backend = OfflineBackend::new(session, None).unwrap();
        EventLoop::new(UiState::new(), backend).unwrap()
    }

    fn input<B: CommandBackend>(
        event_loop: &mut EventLoop<B>,
        event: InputEvent,
    ) -> StepResult<B::Error> {
        event_loop.step(LoopEvent::Input(event))
    }

    fn pointer_events(ui: &UiState, control: Control) -> [LoopEvent; 2] {
        let bounds = ui
            .hit_regions()
            .into_iter()
            .find(|region| region.control == control && region.enabled)
            .unwrap()
            .bounds;
        let x = bounds.x + bounds.width as i32 / 2;
        let y = bounds.y + bounds.height as i32 / 2;
        [
            LoopEvent::Input(InputEvent::PointerDown { x, y }),
            LoopEvent::Input(InputEvent::PointerUp { x, y }),
        ]
    }

    fn click<B: CommandBackend>(
        event_loop: &mut EventLoop<B>,
        control: Control,
    ) -> StepResult<B::Error> {
        let [down, up] = pointer_events(event_loop.ui(), control);
        assert!(matches!(
            event_loop.step(down),
            StepResult::Frame {
                batch: BatchDisposition::NoCommands,
                ..
            }
        ));
        event_loop.step(up)
    }

    struct Script {
        events: std::vec::IntoIter<Result<LoopEvent, &'static str>>,
        reads: usize,
    }

    impl Script {
        fn new(events: impl IntoIterator<Item = Result<LoopEvent, &'static str>>) -> Self {
            Self {
                events: events.into_iter().collect::<Vec<_>>().into_iter(),
                reads: 0,
            }
        }
    }

    impl InputSource for Script {
        type Error = &'static str;

        fn next_event(&mut self) -> Result<Option<LoopEvent>, Self::Error> {
            self.reads += 1;
            self.events.next().transpose()
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        frames: Vec<Frame>,
        fail_at: Option<usize>,
    }

    impl FrameSink for RecordingSink {
        type Error = &'static str;

        fn present(&mut self, frame: &Frame) -> Result<(), Self::Error> {
            self.frames.push(frame.clone());
            if self.fail_at == Some(self.frames.len()) {
                Err("partial presentation")
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn pointer_scene_request_commits_backend_before_rendering_to_sink() {
        let mut event_loop = offline_loop();
        click(&mut event_loop, Control::SceneMenu);
        let original = event_loop.ui().render();
        let events = pointer_events(event_loop.ui(), Control::SelectScene(scene(1)));
        let mut source = Script::new(events.into_iter().map(Ok));
        let mut sink = RecordingSink::default();
        let report = event_loop.run(&mut source, &mut sink, 10);
        assert!(matches!(report.stop, RunStop::InputExhausted));
        assert_eq!(report.events, 2);
        assert_eq!(sink.frames.len(), 2);
        assert_eq!(sink.frames[1], event_loop.ui().render());
        assert_ne!(sink.frames[1], original);
        assert_eq!(event_loop.ui().snapshot().selected_scene, scene(1));
        assert_eq!(
            event_loop
                .backend()
                .session()
                .snapshot()
                .unwrap()
                .selected_scene,
            scene(1)
        );
        assert_eq!(event_loop.backend().session().export(), fixture());
        assert_eq!(event_loop.ui().snapshot().preset_id, None);
        assert!(!event_loop.ui().snapshot().dirty);
    }

    #[test]
    fn late_confirmation_refusal_restores_draft_and_cancels_released_pointer() {
        let mut event_loop = offline_loop();
        // Swipe up from the Grid bottom into Gig View (the Grid has no Gig button).
        for event in [
            InputEvent::PointerDown { x: 400, y: 470 },
            InputEvent::PointerMove { x: 400, y: 300 },
            InputEvent::PointerUp { x: 400, y: 300 },
        ] {
            input(&mut event_loop, event);
        }
        click(&mut event_loop, Control::EditScenes);
        input(&mut event_loop, InputEvent::TextChanged("Bridge".into()));
        click(&mut event_loop, Control::SelectScene(scene(7)));
        input(&mut event_loop, InputEvent::TextChanged("Missing".into()));
        let [down, up] = pointer_events(event_loop.ui(), Control::ConfirmScenes);
        event_loop.step(down);
        let StepResult::Frame { batch, frame } = event_loop.step(up.clone()) else {
            panic!("confirmation must produce a frame");
        };
        assert!(matches!(
            batch,
            BatchDisposition::Refused(SessionError::Preset(_))
        ));
        assert_eq!(frame, event_loop.ui().render());
        assert_eq!(event_loop.backend().session().export(), fixture());
        assert!(event_loop.ui().scene_dialog_open());
        assert_eq!(event_loop.ui().screen(), Screen::Gig);
        assert!(
            event_loop
                .ui()
                .hit_regions()
                .iter()
                .any(|r| r.label == "A: Bridge")
        );
        assert!(
            event_loop
                .ui()
                .hit_regions()
                .iter()
                .any(|r| r.label == "H: Missing")
        );
        assert!(matches!(
            event_loop.step(up),
            StepResult::Frame {
                batch: BatchDisposition::NoCommands,
                ..
            }
        ));
        // Correct the refused absent tail, then explicitly confirm the retained draft.
        input(&mut event_loop, InputEvent::TextChanged(String::new()));
        assert!(matches!(
            click(&mut event_loop, Control::ConfirmScenes),
            StepResult::Frame {
                batch: BatchDisposition::Applied { .. },
                ..
            }
        ));
        assert!(!event_loop.ui().scene_dialog_open());
        assert_eq!(event_loop.ui().snapshot().scenes[0].label, "Bridge");
        assert_eq!(event_loop.ui().screen(), Screen::Gig);
        assert!(event_loop.ui().snapshot().dirty);
        let bytes = event_loop.backend().session().export();
        assert_eq!(&bytes[bytes.len() - 3..], &[0xa0, 6, 7]);
        // A later no-op transaction must not reset the original dirty baseline.
        click(&mut event_loop, Control::SelectScene(scene(1)));
        assert!(event_loop.ui().snapshot().dirty);
    }

    #[test]
    fn presentation_failure_reports_commit_and_never_replays_consumed_input() {
        let mut event_loop = offline_loop();
        click(&mut event_loop, Control::SceneMenu);
        let events = pointer_events(event_loop.ui(), Control::SelectScene(scene(1)));
        let mut source = Script::new(events.into_iter().map(Ok));
        let mut sink = RecordingSink {
            fail_at: Some(2),
            ..RecordingSink::default()
        };
        let report = event_loop.run(&mut source, &mut sink, 10);
        assert!(matches!(
            report.stop,
            RunStop::PresentationFailed {
                error: "partial presentation",
                batch: BatchDisposition::Applied { commands: 1 },
            }
        ));
        assert_eq!(report.events, 2);
        assert_eq!(event_loop.ui().snapshot().selected_scene, scene(1));
        assert_eq!(
            event_loop
                .backend()
                .session()
                .snapshot()
                .unwrap()
                .selected_scene,
            scene(1)
        );
        sink.fail_at = None;
        let report = event_loop.run(&mut source, &mut sink, 10);
        assert_eq!(report.events, 0);
        assert!(matches!(report.stop, RunStop::InputExhausted));
        assert_eq!(sink.frames.len(), 2);
    }

    #[test]
    fn unsupported_batch_and_invalid_projection_refuse_without_mutation() {
        let mut event_loop = offline_loop();
        let mut source = Script::new([Ok(LoopEvent::Input(InputEvent::Activate(
            Control::CycleMode,
        )))]);
        let mut sink = RecordingSink::default();
        let report = event_loop.run(&mut source, &mut sink, 10);
        assert!(matches!(
            report.stop,
            RunStop::BackendRefused(SessionError::Unsupported(UiCommand::CycleMode))
        ));
        assert_eq!(sink.frames[0], event_loop.ui().render());
        let mut backend = event_loop.into_parts().1;
        assert!(matches!(
            backend.apply(&[
                UiCommand::SetSceneLabel {
                    scene: scene(0),
                    label: "Changed".into(),
                },
                UiCommand::CycleMode,
            ]),
            Err(SessionError::Unsupported(UiCommand::CycleMode))
        ));
        assert_eq!(backend.session().export(), fixture());
        assert!(matches!(
            backend.apply(&[UiCommand::SetSceneLabel {
                scene: scene(0),
                label: "X".repeat(tetrad_ui::MAX_TEXT_BYTES + 1),
            }]),
            Err(SessionError::Ui(UiError::TextTooLong))
        ));
        assert_eq!(backend.session().export(), fixture());
        assert!(!backend.snapshot().unwrap().snapshot().dirty);
        // Force projection failure after candidate.apply, never on live state.
        backend.grid_policy = Some(tetrad_engine::ImportPolicy::STRICT);
        assert!(matches!(
            backend.apply(&[UiCommand::SelectScene { scene: scene(1) }]),
            Err(SessionError::GridImport(_))
        ));
        assert_eq!(
            backend.session().snapshot().unwrap().selected_scene,
            scene(0)
        );
        assert_eq!(backend.session().export(), fixture());
        assert!(
            AdmittedSnapshot::new(UiSnapshot {
                status: "X".repeat(tetrad_ui::MAX_TEXT_BYTES + 1),
                ..UiSnapshot::default()
            })
            .is_err()
        );
    }

    #[test]
    fn rejection_keeps_draft_and_is_distinct_from_backend_refusal() {
        let mut event_loop = offline_loop();
        // Swipe up from the Grid bottom into Gig View: stock edits scene names there.
        for event in [
            InputEvent::PointerDown { x: 400, y: 470 },
            InputEvent::PointerMove { x: 400, y: 300 },
            InputEvent::PointerUp { x: 400, y: 300 },
        ] {
            input(&mut event_loop, event);
        }
        click(&mut event_loop, Control::EditScenes);
        input(&mut event_loop, InputEvent::TextChanged("Kept".into()));
        let mut source = Script::new([Ok(LoopEvent::Input(InputEvent::TextChanged(
            "X".repeat(tetrad_ui::MAX_TEXT_BYTES + 1),
        )))]);
        let mut sink = RecordingSink::default();
        let report = event_loop.run(&mut source, &mut sink, 10);
        assert!(matches!(
            report.stop,
            RunStop::UiRejected(UiError::TextTooLong)
        ));
        assert!(event_loop.ui().scene_dialog_open());
        assert!(
            event_loop
                .ui()
                .hit_regions()
                .iter()
                .any(|r| r.label == "A: Kept")
        );
        assert_eq!(event_loop.backend().session().export(), fixture());
        assert_eq!(sink.frames[0], event_loop.ui().render());
    }

    #[test]
    fn termination_is_bounded_explicit_and_cancels_incomplete_gestures() {
        let mut event_loop = offline_loop();
        click(&mut event_loop, Control::SceneMenu);
        let [down, up] = pointer_events(event_loop.ui(), Control::SelectScene(scene(1)));
        let mut source = Script::new([Ok(down), Ok(up), Ok(LoopEvent::Quit)]);
        let mut sink = RecordingSink::default();
        let report = event_loop.run(&mut source, &mut sink, 0);
        assert!(matches!(report.stop, RunStop::LimitReached));
        assert_eq!(source.reads, 0);
        assert!(sink.frames.is_empty());
        let report = event_loop.run(&mut source, &mut sink, 1);
        assert!(matches!(report.stop, RunStop::LimitReached));
        assert_eq!(report.events, 1);
        assert_eq!(source.reads, 1);
        let report = event_loop.run(&mut source, &mut sink, 10);
        assert!(matches!(report.stop, RunStop::Quit));
        assert_eq!(report.events, 2);
        assert_eq!(event_loop.ui().snapshot().selected_scene, scene(0));
        let report = event_loop.run(&mut source, &mut sink, 10);
        assert!(matches!(report.stop, RunStop::Quit));
        assert_eq!(report.events, 0);
        assert_eq!(source.reads, 3);
        assert!(matches!(
            input(
                &mut event_loop,
                InputEvent::Activate(Control::SelectScene(scene(1)))
            ),
            StepResult::Quit
        ));

        for terminal in [None, Some(Err("input disconnected"))] {
            let failed = terminal.is_some();
            let mut event_loop = offline_loop();
            click(&mut event_loop, Control::SceneMenu);
            let [down, up] = pointer_events(event_loop.ui(), Control::SelectScene(scene(1)));
            let mut source = Script::new(std::iter::once(Ok(down)).chain(terminal));
            let mut sink = RecordingSink::default();
            let report = event_loop.run(&mut source, &mut sink, 10);
            if failed {
                assert!(matches!(
                    report.stop,
                    RunStop::InputFailed("input disconnected")
                ));
            } else {
                assert!(matches!(report.stop, RunStop::InputExhausted));
            }
            assert_eq!(report.events, 1);
            assert!(matches!(
                event_loop.step(up),
                StepResult::Frame {
                    batch: BatchDisposition::NoCommands,
                    ..
                }
            ));
            assert_eq!(event_loop.ui().snapshot().selected_scene, scene(0));
        }
    }

    #[test]
    fn presentation_failure_on_refusal_keeps_both_dispositions() {
        let mut event_loop = offline_loop();
        let mut source = Script::new([Ok(LoopEvent::Input(InputEvent::Activate(
            Control::CycleMode,
        )))]);
        let mut sink = RecordingSink {
            fail_at: Some(1),
            ..RecordingSink::default()
        };
        let report = event_loop.run(&mut source, &mut sink, 10);
        assert!(matches!(
            report.stop,
            RunStop::PresentationFailed {
                error: "partial presentation",
                batch: BatchDisposition::Refused(SessionError::Unsupported(UiCommand::CycleMode)),
            }
        ));
        assert_eq!(event_loop.backend().session().export(), fixture());
    }

    #[test]
    fn initial_admission_discards_unbound_drafts_even_when_scene_fields_match() {
        for same_source in [false, true] {
            let source = OfflineSession::from_bytes(&fixture(), scene(0)).unwrap();
            let mut ui = UiState::new();
            ui.set_snapshot(source.snapshot().unwrap()).unwrap();
            ui.show(Screen::Gig);
            ui.handle_input(InputEvent::Activate(Control::EditScenes));
            ui.handle_input(InputEvent::TextChanged("Intended for original".into()));
            let [down, up] = pointer_events(&ui, Control::ConfirmScenes);
            let LoopEvent::Input(down) = down else {
                unreachable!();
            };
            ui.handle_input(down);

            let mut bytes = fixture();
            if !same_source {
                bytes[2] = b'Y';
                *bytes.last_mut().unwrap() = 8;
            }
            let backend =
                OfflineBackend::new(OfflineSession::from_bytes(&bytes, scene(0)).unwrap(), None)
                    .unwrap();
            assert_eq!(
                source.snapshot().unwrap().scenes,
                backend.snapshot().unwrap().snapshot().scenes
            );
            let mut event_loop = EventLoop::new(ui, backend).unwrap();
            assert_eq!(event_loop.ui().screen(), Screen::Gig);
            assert!(!event_loop.ui().scene_dialog_open());
            for event in [
                up,
                LoopEvent::Input(InputEvent::Activate(Control::ConfirmScenes)),
            ] {
                assert!(matches!(
                    event_loop.step(event),
                    StepResult::Frame {
                        batch: BatchDisposition::NoCommands,
                        ..
                    }
                ));
                assert_eq!(event_loop.backend().session().export(), bytes);
                assert!(!event_loop.ui().snapshot().dirty);
            }

            // A new draft belongs to the installed backend and remains editable.
            click(&mut event_loop, Control::EditScenes);
            input(
                &mut event_loop,
                InputEvent::TextChanged("Current source".into()),
            );
            assert!(matches!(
                click(&mut event_loop, Control::ConfirmScenes),
                StepResult::Frame {
                    batch: BatchDisposition::Applied { .. },
                    ..
                }
            ));
            assert_eq!(event_loop.ui().snapshot().scenes[0].label, "Current source");
            assert!(event_loop.ui().snapshot().dirty);
            assert_eq!(source.export(), fixture());
            let changed = event_loop.backend().session().export();
            assert_eq!(&changed[changed.len() - 3..], &bytes[bytes.len() - 3..]);
        }
    }
}
