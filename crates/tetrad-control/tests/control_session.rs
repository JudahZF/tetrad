//! Stock session flows recovered from `USBHIDController` and the Version,
//! ResetCommsBuffers, Confirmation and forward receivers. Synthetic model.

use std::time::Duration;

use tetrad_control::codecs::{
    BackupsForwardMessage, BackupsForwardResponse, CodecLimits, ConfirmationAction,
    ConfirmationMessage, ConnectionMessage, ControlMessage, CortexMessageType, GenericErrorCode,
    GenericErrorMessage, GlobalEQMessage, KeepAliveMessage, MessageAction, ModelRepoMessage,
    ProductionAutomationModeMessage, ResetCommsBuffersMessage, VersionMessage,
};
use tetrad_control::device::{DeviceModel, DeviceResult, VersionExchange};
use tetrad_control::link::{
    ConfirmationRequest, ControlLink, LinkLimits, NotSent, Notice, Notification, Step,
};
use tetrad_control::pipeline::Stages;
use tetrad_control::session::{Session, SessionEvent};
use tetrad_control::{Envelope, Limits, Reassembler};

#[derive(Default)]
struct Model {
    exchange: Option<VersionExchange>,
    answers: Vec<(u64, ConfirmationAction)>,
    chunks: Vec<(Vec<u8>, bool)>,
    production: Option<bool>,
}

impl DeviceModel for Model {
    fn model_repo(&mut self) -> DeviceResult<Vec<u8>> {
        Ok(vec![1])
    }

    fn version_info(&mut self) -> DeviceResult<VersionMessage> {
        Ok(VersionMessage {
            linux_kernel_version: Some("6.1".into()),
            device_serial_number: Some("QC0".into()),
            cortex_control_version: Some("leak".into()),
            ..Default::default()
        })
    }

    fn begin_version_exchange(&mut self, _: &VersionMessage) -> DeviceResult<VersionExchange> {
        Ok(self.exchange.expect("test sets the exchange"))
    }

    fn confirmation_answered(&mut self, id: u64, response: ConfirmationAction) -> DeviceResult<()> {
        self.answers.push((id, response));
        Ok(())
    }

    fn forward_response(&mut self, response: ControlMessage, last: bool) -> DeviceResult<()> {
        let ControlMessage::BackupsForward(message) = response else {
            panic!("only backups forwards in this test");
        };
        let payload = message.response.and_then(|r| r.payload);
        self.chunks.push((payload.unwrap_or_default(), last));
        Ok(())
    }

    fn set_production_automation(&mut self, enable: bool) -> DeviceResult<()> {
        self.production = Some(enable);
        Ok(())
    }

    fn global_eq(&mut self) -> DeviceResult<GlobalEQMessage> {
        Ok(GlobalEQMessage::default())
    }
}

const NOW: Duration = Duration::from_millis(1);
const XXTEA_KEY: [u8; 16] = *b"0123456789abcdef";
const KEY_MATERIAL: &[u8] = b"vecB key material";

fn link(coros_version: &str) -> ControlLink {
    let session = Session::new(Duration::ZERO, Duration::from_secs(5))
        .unwrap()
        .with_coros_version(coros_version);
    ControlLink::new(session, LinkLimits::default())
}

fn feed(link: &mut ControlLink, model: &mut Model, message: impl Into<ControlMessage>) -> Step {
    let message = message.into();
    let envelope = Envelope {
        message_type: message.message_type() as u32,
        transform: 0,
        decompress: 0,
        opaque: [0; 2],
        payload: message.encode(CodecLimits::default()).unwrap(),
    };
    let mut step = Step::default();
    for report in envelope.reports(Limits::default()).unwrap() {
        let part = link
            .receive_report(&report, NOW, model, Stages::default())
            .unwrap();
        step.reports.extend(part.reports);
        step.sent.extend(part.sent);
        step.session.extend(part.session);
        step.notices.extend(part.notices);
    }
    step
}

fn decoded(step: &Step) -> Vec<ControlMessage> {
    let mut reassembler = Reassembler::new(Limits::default());
    step.reports
        .iter()
        .filter_map(|report| reassembler.push(report).unwrap())
        .map(|envelope| {
            ControlMessage::decode(&envelope, LinkLimits::default().payload)
                .unwrap()
                .message
        })
        .collect()
}

fn connect(link: &mut ControlLink, model: &mut Model) {
    feed(
        link,
        model,
        ModelRepoMessage {
            action: Some(MessageAction::Read.into()),
            ..Default::default()
        },
    );
    let step = feed(
        link,
        model,
        ConnectionMessage {
            connected: Some(true),
            ..Default::default()
        },
    );
    assert_eq!(step.session, [SessionEvent::Connected]);
}

/// `0x2a59d0`: the version gate answers on the priority path before the
/// session is connected; READ masks the builder output (`0x2a36d0`).
#[test]
fn version_gate_and_read_reply_use_the_priority_path_before_connection() {
    let mut link = link("4.1.0");
    let mut model = Model::default();
    let update = |version: &str| VersionMessage {
        action: Some(MessageAction::Update.into()),
        request_id: Some(3),
        cortex_control_version: Some(version.into()),
        ..Default::default()
    };
    assert_eq!(
        decoded(&feed(&mut link, &mut model, update("4.0.12"))),
        [ControlMessage::GenericError(GenericErrorMessage {
            request_id: Some(3),
            error_code: Some(GenericErrorCode::VersionIsOlder.into()),
        })]
    );
    // Equal common parts with fewer components is older: "4.1" < "4.1.0".
    assert_eq!(
        decoded(&feed(&mut link, &mut model, update("4.1"))),
        [ControlMessage::GenericError(GenericErrorMessage {
            request_id: Some(3),
            error_code: Some(GenericErrorCode::VersionIsOlder.into()),
        })]
    );
    assert_eq!(
        decoded(&feed(&mut link, &mut model, update("4.1.0"))),
        [ControlMessage::Version(VersionMessage {
            action: Some(MessageAction::Update.into()),
            request_id: Some(3),
            cortex_control_version_valid: Some(true),
            ..Default::default()
        })]
    );

    // READ naming one info field gets only that field, never the Control
    // version fields.
    let read = VersionMessage {
        action: Some(MessageAction::Read.into()),
        request_id: Some(4),
        linux_kernel_version: Some(String::new()),
        ..Default::default()
    };
    let reply = VersionMessage {
        action: Some(MessageAction::Update.into()),
        request_id: Some(4),
        linux_kernel_version: Some("6.1".into()),
        ..Default::default()
    };
    model.exchange = Some(VersionExchange::Ready);
    assert_eq!(
        decoded(&feed(&mut link, &mut model, read.clone())),
        [ControlMessage::Version(reply.clone())]
    );
    // A pending key exchange replies with no request ID and the XXTEA
    // `comms_version`, then sends a Version READ.
    model.exchange = Some(VersionExchange::Pending);
    assert!(feed(&mut link, &mut model, read).reports.is_empty());
    let later = decoded(
        &link
            .complete_version(&XXTEA_KEY, KEY_MATERIAL, NOW, &mut model, Stages::default())
            .unwrap(),
    );
    let [key_reply, read_back]: [ControlMessage; 2] = later.try_into().unwrap();
    let ControlMessage::Version(key_reply) = key_reply else {
        panic!("key exchange reply is a Version");
    };
    assert_eq!(key_reply.action, Some(MessageAction::Update.into()));
    assert_eq!(key_reply.request_id, None);
    assert_eq!(key_reply.linux_kernel_version.as_deref(), Some("6.1"));
    // 17 bytes pad to five words; the length word makes six, 24 bytes.
    assert_eq!(key_reply.comms_version.as_ref().map(Vec::len), Some(24));
    assert_eq!(
        read_back,
        ControlMessage::Version(VersionMessage {
            action: Some(MessageAction::Read.into()),
            ..Default::default()
        })
    );
    assert!(
        link.complete_version(&XXTEA_KEY, KEY_MATERIAL, NOW, &mut model, Stages::default())
            .unwrap()
            .reports
            .is_empty()
    );
}

/// `0x1354d8`: a live session tears down first (Connection(false) on the
/// priority path, open dialogs cancelled), then the session ID is echoed.
#[test]
fn reset_comms_buffers_tears_down_then_echoes_and_cancels_dialogs() {
    let mut link = link("4.1.0");
    let mut model = Model::default();
    connect(&mut link, &mut model);
    let request = || ConfirmationRequest {
        title: "Preset Conflict".into(),
        message: "Overwrite?".into(),
        actions: vec![ConfirmationAction::Ok, ConfirmationAction::Cancel],
        ok_title: "OVERWRITE".into(),
        cancel_title: String::new(),
    };
    let (first, step) = link
        .request_confirmation(request(), NOW, &mut model, Stages::default())
        .unwrap();
    assert_eq!(first, Some(0));
    assert_eq!(
        decoded(&step),
        [ControlMessage::Confirmation(ConfirmationMessage {
            confirmation_id: Some(0),
            title: Some("Preset Conflict".into()),
            msg: Some("Overwrite?".into()),
            actions: vec![
                ConfirmationAction::Ok.into(),
                ConfirmationAction::Cancel.into()
            ],
            ok_title: Some("OVERWRITE".into()),
            ..Default::default()
        })]
    );
    // The answer is matched by ID once; a repeat is ignored.
    let answer = ConfirmationMessage {
        request_id: Some(9),
        confirmation_id: Some(0),
        response: Some(ConfirmationAction::Ok.into()),
        ..Default::default()
    };
    feed(&mut link, &mut model, answer.clone());
    feed(&mut link, &mut model, answer);
    assert_eq!(model.answers, [(0, ConfirmationAction::Ok)]);
    let (second, _) = link
        .request_confirmation(request(), NOW, &mut model, Stages::default())
        .unwrap();
    assert_eq!(second, Some(1));

    let step = feed(
        &mut link,
        &mut model,
        ResetCommsBuffersMessage {
            request_id: Some(5),
            session_id: Some("s1".into()),
        },
    );
    assert_eq!(
        step.session,
        [SessionEvent::Disconnected, SessionEvent::FlushTransmit]
    );
    assert_eq!(
        decoded(&step),
        [
            ConnectionMessage {
                connected: Some(false),
                ..Default::default()
            }
            .into(),
            ResetCommsBuffersMessage {
                request_id: None,
                session_id: Some("s1".into()),
            }
            .into(),
        ]
    );
    assert_eq!(model.answers[1..], [(1, ConfirmationAction::Cancel)]);
    // Without a session the receiver only flushes and echoes.
    let idle = feed(&mut link, &mut model, ResetCommsBuffersMessage::default());
    assert_eq!(idle.session, [SessionEvent::FlushTransmit]);
    assert_eq!(idle.sent, [CortexMessageType::ResetCommsBuffers]);
}

/// Forward receivers match Control's UPDATE responses by forward ID and keep
/// a Backups forward open until `is_last_chunk`.
#[test]
fn backups_forward_chunks_until_last_and_ignore_unknown_ids() {
    let mut link = link("4.1.0");
    let mut model = Model::default();
    connect(&mut link, &mut model);
    let request = BackupsForwardMessage {
        action: Some(MessageAction::Create.into()),
        backups_request_id: Some(7),
        ..Default::default()
    };
    let step = link
        .notify(
            Notification::Forward(request.into()),
            NOW,
            &mut model,
            Stages::default(),
        )
        .unwrap();
    // Type 30 is sent with the stock transform flag, which needs a stage.
    assert!(matches!(
        step.notices[..],
        [Notice::Send(
            tetrad_control::pipeline::PipelineError::TransformUnavailable(30)
        )]
    ));
    struct Clear;
    impl tetrad_control::pipeline::Transform for Clear {
        fn decode(&self, input: &[u8]) -> Result<Vec<u8>, tetrad_control::pipeline::StageFailure> {
            Ok(input.to_vec())
        }
        fn encode(&self, input: &[u8]) -> Result<Vec<u8>, tetrad_control::pipeline::StageFailure> {
            Ok(input.to_vec())
        }
    }
    let stages = Stages {
        compression: None,
        transform: Some(&Clear),
    };
    let request = BackupsForwardMessage {
        action: Some(MessageAction::Create.into()),
        backups_request_id: Some(7),
        ..Default::default()
    };
    let sent = link
        .notify(
            Notification::Forward(request.into()),
            NOW,
            &mut model,
            stages,
        )
        .unwrap();
    assert_eq!(sent.sent, [CortexMessageType::BackupsForward]);

    let response = |id, chunk: &[u8], last| BackupsForwardMessage {
        action: Some(MessageAction::Update.into()),
        backups_request_id: Some(id),
        response: Some(BackupsForwardResponse {
            payload: Some(chunk.to_vec()),
            is_last_chunk: Some(last),
            ..Default::default()
        }),
        ..Default::default()
    };
    feed(&mut link, &mut model, response(8, b"x", true));
    feed(&mut link, &mut model, response(7, b"ab", false));
    feed(&mut link, &mut model, response(7, b"c", true));
    feed(&mut link, &mut model, response(7, b"late", true));
    assert_eq!(
        model.chunks,
        [(b"ab".to_vec(), false), (b"c".to_vec(), true)]
    );
}

/// `0x6e192c`: production automation disables the ordinary senders, and the
/// production Version receiver `0x2a52c8` answers at once. KeepAlive applies
/// Control's timeout only on a suffixed CorOS version.
#[test]
fn production_mode_gates_senders_and_release_builds_keep_their_timeout() {
    let mut link = link("4.1.0");
    let mut model = Model::default();
    connect(&mut link, &mut model);
    feed(
        &mut link,
        &mut model,
        KeepAliveMessage {
            action: Some(MessageAction::Update.into()),
            timeout: Some(1.0),
            ..Default::default()
        },
    );
    assert_eq!(link.session().timeout(), Duration::from_secs(5));

    feed(
        &mut link,
        &mut model,
        ProductionAutomationModeMessage {
            enable: Some(true),
            ..Default::default()
        },
    );
    assert_eq!(model.production, Some(true));
    let read = GlobalEQMessage {
        action: Some(MessageAction::Read.into()),
        ..Default::default()
    };
    let step = feed(&mut link, &mut model, read);
    assert!(step.reports.is_empty());
    assert_eq!(
        step.notices,
        [Notice::NotSent {
            message_type: CortexMessageType::GlobalEQ,
            reason: NotSent::SenderDisabled
        }]
    );
    let version = feed(
        &mut link,
        &mut model,
        VersionMessage {
            action: Some(MessageAction::Read.into()),
            request_id: Some(2),
            ..Default::default()
        },
    );
    let [ControlMessage::Version(reply)] = &decoded(&version)[..] else {
        panic!("one Version reply");
    };
    assert_eq!(reply.request_id, Some(2));
    assert_eq!(reply.device_serial_number.as_deref(), Some("QC0"));
    assert_eq!(reply.cortex_control_version, None);
}

/// `sendIOMeters` (`0x6e0040`) sends only a changed block while subscribed;
/// the resync push (`0x29a4f8`) keeps the stock order: CloudProduct DELETE
/// (`0xf5470`), recents then favorites for presets, captures and IRs
/// (`0x1abc68`/`0x1abcb4`), Mode as the mode and then the list, unchanged
/// UndoRedo lists skipped (`0x1be1a8`), unsupported families skipped, and
/// CloudLogin DELETE last after a factory restore only (`0x23d688`).
#[test]
fn io_meters_send_on_change_and_resync_keeps_stock_order() {
    use tetrad_control::codecs::{
        EnumValue, IOMeterMessage, ModeMessage, RecentFavoriteType, RecentsFavoritesMessage,
        UndoRedoMessage,
    };

    #[derive(Default)]
    struct Meters {
        level: f32,
        lists: Vec<(Option<bool>, Option<EnumValue<RecentFavoriteType>>)>,
    }
    impl DeviceModel for Meters {
        fn model_repo(&mut self) -> DeviceResult<Vec<u8>> {
            Ok(vec![1])
        }
        fn io_meters(&mut self) -> DeviceResult<IOMeterMessage> {
            Ok(IOMeterMessage {
                input_1: Some(self.level),
                ..Default::default()
            })
        }
        fn mode(&mut self) -> DeviceResult<ModeMessage> {
            Ok(ModeMessage::default())
        }
        fn undo_redo(&mut self) -> DeviceResult<UndoRedoMessage> {
            Ok(UndoRedoMessage {
                undo_frames: vec![0],
                ..Default::default()
            })
        }
        fn scene(&mut self) -> DeviceResult<u32> {
            Ok(2)
        }
        fn recents_favorites(
            &mut self,
            request: &RecentsFavoritesMessage,
        ) -> DeviceResult<Option<RecentsFavoritesMessage>> {
            self.lists.push((request.is_favorites, request.item_type));
            Ok(Some(RecentsFavoritesMessage::default()))
        }
    }

    let mut link = link("4.1.0");
    let mut model = Meters::default();
    let feed = |link: &mut ControlLink, model: &mut Meters, message: ControlMessage| {
        let envelope = Envelope {
            message_type: message.message_type() as u32,
            transform: 0,
            decompress: 0,
            opaque: [0; 2],
            payload: message.encode(CodecLimits::default()).unwrap(),
        };
        for report in envelope.reports(Limits::default()).unwrap() {
            link.receive_report(&report, NOW, model, Stages::default())
                .unwrap();
        }
    };
    let read = MessageAction::Read.into();
    feed(
        &mut link,
        &mut model,
        ModelRepoMessage {
            action: Some(read),
            ..Default::default()
        }
        .into(),
    );
    feed(
        &mut link,
        &mut model,
        ConnectionMessage {
            connected: Some(true),
            ..Default::default()
        }
        .into(),
    );
    let subscribe = tetrad_control::codecs::IOMeterMessage {
        action: Some(MessageAction::Create.into()),
        ..Default::default()
    };
    feed(&mut link, &mut model, subscribe.into());
    let tick = |link: &mut ControlLink, model: &mut Meters, millis| {
        link.tick(Duration::from_millis(millis), model, Stages::default())
            .unwrap()
            .sent
    };
    // The first send comes one 100 ms interval after the subscription at NOW.
    assert_eq!(
        tick(&mut link, &mut model, 101),
        [CortexMessageType::IOMeter]
    );
    assert!(tick(&mut link, &mut model, 201).is_empty());
    model.level = -6.0;
    assert_eq!(
        tick(&mut link, &mut model, 301),
        [CortexMessageType::IOMeter]
    );

    let first = link
        .resync(
            false,
            Duration::from_millis(310),
            &mut model,
            Stages::default(),
        )
        .unwrap();
    use CortexMessageType::{CloudLogin, CloudProduct, Mode, RecentsFavorites, Scene, UndoRedo};
    let lists = [RecentsFavorites; 6];
    assert_eq!(
        first.sent,
        [&[CloudProduct][..], &lists, &[UndoRedo, Mode, Mode, Scene]].concat()
    );
    // Includes DefaultParameters (19) and ModelPreset (71): this model has no inputs for them.
    assert_eq!(first.notices.len(), 12);
    use RecentFavoriteType::{ImpulseResponse, NeuralCapture, Preset};
    let expected: Vec<_> = [Preset, NeuralCapture, ImpulseResponse]
        .into_iter()
        .flat_map(|kind| [false, true].map(|favorites| (Some(favorites), Some(kind.into()))))
        .collect();
    assert_eq!(model.lists, expected);
    // CloudLogin is a transform type (`stock_send_options`); this link has no cipher.
    struct Plain;
    impl tetrad_control::pipeline::Transform for Plain {
        fn decode(&self, input: &[u8]) -> Result<Vec<u8>, tetrad_control::pipeline::StageFailure> {
            Ok(input.to_vec())
        }
        fn encode(&self, input: &[u8]) -> Result<Vec<u8>, tetrad_control::pipeline::StageFailure> {
            Ok(input.to_vec())
        }
    }
    let stages = Stages {
        compression: None,
        transform: Some(&Plain),
    };
    let factory = link
        .factory_resync(Duration::from_millis(320), &mut model, stages)
        .unwrap();
    assert_eq!(
        factory.sent,
        [
            &[CloudProduct][..],
            &lists,
            &[Mode, Mode, Scene, CloudLogin]
        ]
        .concat()
    );
}

/// `0x3fbb90` Download suspension: SuspendConnection{true} first, then only
/// backup/forward types pass; resume clears the sets, resyncs and sends
/// SuspendConnection{false}. LocalBackup chunks travel both ways (`0x17d104`,
/// `0x17ddfc`).
#[test]
fn download_suspension_blocks_traffic_and_local_backup_chunks_both_ways() {
    use tetrad_control::codecs::{LocalBackupMessage, SuspendConnectionMessage, SuspendReason};
    use tetrad_control::link::{BACKUP_CHUNK_BYTES, Suspension};

    #[derive(Default)]
    struct Backup {
        applied: Vec<String>,
    }
    impl DeviceModel for Backup {
        fn model_repo(&mut self) -> DeviceResult<Vec<u8>> {
            Ok(vec![1])
        }
        fn global_eq(&mut self) -> DeviceResult<GlobalEQMessage> {
            Ok(GlobalEQMessage::default())
        }
        fn apply_local_backup(
            &mut self,
            backup_json: String,
        ) -> DeviceResult<Vec<LocalBackupMessage>> {
            self.applied.push(backup_json);
            Ok(Vec::new())
        }
    }

    let mut link = link("4.1.0");
    let mut model = Backup::default();
    let feed = |link: &mut ControlLink, model: &mut Backup, message: ControlMessage| {
        let envelope = Envelope {
            message_type: message.message_type() as u32,
            transform: 0,
            decompress: 0,
            opaque: [0; 2],
            payload: message.encode(CodecLimits::default()).unwrap(),
        };
        let mut step = Step::default();
        for report in envelope.reports(Limits::default()).unwrap() {
            let part = link
                .receive_report(&report, NOW, model, Stages::default())
                .unwrap();
            step.reports.extend(part.reports);
            step.notices.extend(part.notices);
        }
        step
    };
    let read = Some(MessageAction::Read.into());
    feed(
        &mut link,
        &mut model,
        ModelRepoMessage {
            action: read,
            ..Default::default()
        }
        .into(),
    );
    feed(
        &mut link,
        &mut model,
        ConnectionMessage {
            connected: Some(true),
            ..Default::default()
        }
        .into(),
    );

    let step = link
        .suspend(
            Suspension::Download,
            SuspendReason::Backup,
            NOW,
            &mut model,
            Stages::default(),
        )
        .unwrap();
    assert_eq!(
        decoded(&step),
        [ControlMessage::SuspendConnection(
            SuspendConnectionMessage {
                suspend: Some(true),
                reason: Some(SuspendReason::Backup.into()),
                ..Default::default()
            }
        )]
    );
    let eq = GlobalEQMessage {
        action: read,
        ..Default::default()
    };
    let blocked = feed(&mut link, &mut model, eq.clone().into());
    assert!(blocked.reports.is_empty());
    assert!(matches!(blocked.notices[..], [Notice::Dropped(_)]));

    // Incoming chunks join until is_last_chunk.
    let chunk = |text: &str, last| -> ControlMessage {
        LocalBackupMessage {
            action: Some(MessageAction::Update.into()),
            backup_json: Some(text.into()),
            is_last_chunk: Some(last),
            ..Default::default()
        }
        .into()
    };
    feed(&mut link, &mut model, chunk("{\"a\":", false));
    feed(&mut link, &mut model, chunk("1}", true));
    assert_eq!(model.applied, ["{\"a\":1}"]);

    // Outgoing: 150 000-byte chunks, one per 1000 ms backup timer period. The
    // first chunk is due at once; the notice follows the last chunk only.
    link.send_local_backup(&"x".repeat(BACKUP_CHUNK_BYTES + 1));
    let mut sent = Vec::new();
    let mut notices = Vec::new();
    for millis in [2, 1002] {
        let step = link
            .tick(Duration::from_millis(millis), &mut model, Stages::default())
            .unwrap();
        for message in decoded(&step) {
            let ControlMessage::LocalBackup(message) = message else {
                panic!("only backup chunks");
            };
            sent.push((message.backup_json.unwrap().len(), message.is_last_chunk));
        }
        notices.push(step.notices);
    }
    assert_eq!(sent, [(BACKUP_CHUNK_BYTES, Some(false)), (1, Some(true))]);
    assert_eq!(notices, [vec![], vec![Notice::LocalBackupSent]]);

    let resumed = link
        .resume(
            SuspendReason::Backup,
            Duration::from_millis(1010),
            &mut model,
            Stages::default(),
        )
        .unwrap();
    assert_eq!(
        resumed.sent.last(),
        Some(&CortexMessageType::SuspendConnection)
    );
    let message = ControlMessage::from(eq);
    let envelope = Envelope {
        message_type: 38,
        transform: 0,
        decompress: 0,
        opaque: [0; 2],
        payload: message.encode(CodecLimits::default()).unwrap(),
    };
    let report = envelope.reports(Limits::default()).unwrap()[0];
    let unblocked = link
        .receive_report(
            &report,
            Duration::from_millis(1010),
            &mut model,
            Stages::default(),
        )
        .unwrap();
    assert_eq!(unblocked.sent, [CortexMessageType::GlobalEQ]);
}

/// `0x6e0474` stops keep-alive during a Download suspension, so no expiry
/// is due. `0x6e048c` re-arms it for one full timeout after resume.
#[test]
fn download_suspension_pauses_keep_alive_until_resume_rearms_it() {
    use tetrad_control::codecs::SuspendReason;
    use tetrad_control::link::Suspension;

    let mut link = link("4.1.0");
    let mut model = Model::default();
    connect(&mut link, &mut model);
    link.suspend(
        Suspension::Download,
        SuspendReason::Backup,
        Duration::from_millis(2),
        &mut model,
        Stages::default(),
    )
    .unwrap();
    let paused = link
        .tick(Duration::from_secs(60), &mut model, Stages::default())
        .unwrap();
    assert!(paused.session.is_empty());
    assert!(link.session().connected());

    link.resume(
        SuspendReason::Backup,
        Duration::from_secs(60),
        &mut model,
        Stages::default(),
    )
    .unwrap();
    let early = link
        .tick(Duration::from_millis(64_999), &mut model, Stages::default())
        .unwrap();
    assert!(early.session.is_empty());
    let expired = link
        .tick(Duration::from_secs(65), &mut model, Stages::default())
        .unwrap();
    assert_eq!(
        expired.session,
        [
            SessionEvent::KeepAliveExpired,
            SessionEvent::Disconnected,
            SessionEvent::FlushTransmit,
        ]
    );
}

/// A falling edge restores the stock keep-alive interval, so a wire timeout
/// does not outlive its session.
#[test]
fn falling_edge_restores_the_stock_keep_alive_interval() {
    use tetrad_control::session::KEEP_ALIVE_INTERVAL;

    let mut link = link("4.1.0-rc1");
    let mut model = Model::default();
    connect(&mut link, &mut model);
    feed(
        &mut link,
        &mut model,
        KeepAliveMessage {
            action: Some(MessageAction::Update.into()),
            timeout: Some(2.0),
            ..Default::default()
        },
    );
    assert_eq!(link.session().timeout(), Duration::from_secs(2));

    let expired = link
        .tick(Duration::from_secs(3), &mut model, Stages::default())
        .unwrap();
    assert_eq!(
        expired.session,
        [
            SessionEvent::KeepAliveExpired,
            SessionEvent::Disconnected,
            SessionEvent::FlushTransmit,
        ]
    );
    assert_eq!(link.session().timeout(), KEEP_ALIVE_INTERVAL);
}
