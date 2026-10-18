//! Stock dispatcher and session contracts through `ControlLink`, with a
//! synthetic device model. Each test names the recovered receiver it pins.

use std::time::Duration;

use tetrad_control::codecs::{
    CPULoadMessage, CodecLimits, ConnectionMessage, ControlMessage, EnumValue, GlobalEQMessage,
    GlobalEQParameter, GridMessage, GridModelMeterMessage, GridUpdateType, KeepAliveMessage,
    MessageAction, ModeMessage, ModelPresetID, ModelRepoMessage, RecallPresetMessage, SceneMessage,
};
use tetrad_control::device::{
    DeviceError, DeviceModel, DeviceResult, GridEcho, MeterTarget, ModeCommand, Reply,
};
use tetrad_control::link::{ControlLink, LinkLimits, Step};
use tetrad_control::pipeline::Stages;
use tetrad_control::session::{Session, SessionEvent};
use tetrad_control::{Envelope, Limits, Reassembler};

#[derive(Default)]
struct Model {
    calls: Vec<String>,
    scene: u32,
}

impl DeviceModel for Model {
    fn save_default_parameters(&mut self, row: i32, column: i32) -> DeviceResult<()> {
        self.calls.push(format!("save({row}, {column})"));
        Ok(())
    }

    fn model_repo(&mut self) -> DeviceResult<Vec<u8>> {
        Ok(b"<repo/>".to_vec())
    }

    fn scene(&mut self) -> DeviceResult<u32> {
        Ok(self.scene)
    }

    fn select_scene(&mut self, scene: u32) -> DeviceResult<()> {
        self.scene = scene;
        Ok(())
    }

    fn global_eq(&mut self) -> DeviceResult<GlobalEQMessage> {
        Ok(GlobalEQMessage {
            bypassed: Some(true),
            ..Default::default()
        })
    }

    fn set_global_eq_parameter(&mut self, parameter: &GlobalEQParameter) -> DeviceResult<()> {
        self.calls
            .push(format!("eq {:?}", parameter.parameter_index));
        Ok(())
    }

    fn set_global_eq_bypass(&mut self, bypassed: bool) -> DeviceResult<()> {
        self.calls.push(format!("bypass {bypassed}"));
        Ok(())
    }

    fn load_global_eq_preset(&mut self, _: &ModelPresetID) -> DeviceResult<()> {
        self.calls.push("eq preset".into());
        Ok(())
    }

    fn update_grid(&mut self, _: &GridMessage) -> DeviceResult<GridEcho> {
        self.calls.push("grid update".into());
        Ok(GridEcho::default())
    }

    fn delete_grid_preset(&mut self, _: &GridMessage) -> DeviceResult<()> {
        self.calls.push("grid delete".into());
        Ok(())
    }

    fn cpu_load(&mut self) -> DeviceResult<CPULoadMessage> {
        Ok(CPULoadMessage::default())
    }

    fn current_preset(&mut self) -> DeviceResult<RecallPresetMessage> {
        Ok(RecallPresetMessage::default())
    }

    fn recall_preset(&mut self, _: &RecallPresetMessage) -> DeviceResult<Reply> {
        self.calls.push("recall".into());
        Ok(Reply::Now)
    }

    fn set_mode(&mut self, command: ModeCommand) -> DeviceResult<()> {
        self.calls.push(format!("{command:?}"));
        Ok(())
    }
}

fn link() -> ControlLink {
    ControlLink::new(
        Session::new(Duration::ZERO, Duration::from_secs(5))
            .unwrap()
            .with_coros_version("4.1.0-beta"),
        LinkLimits::default(),
    )
}

fn feed(link: &mut ControlLink, model: &mut Model, message: ControlMessage, at: u64) -> Step {
    let envelope = Envelope {
        message_type: message.message_type() as u32,
        transform: 0,
        decompress: 0,
        opaque: [0; 2],
        payload: message.encode(CodecLimits::default()).unwrap(),
    };
    feed_envelope(link, model, envelope, at)
}

fn feed_envelope(link: &mut ControlLink, model: &mut Model, envelope: Envelope, at: u64) -> Step {
    let mut step = Step::default();
    for report in envelope.reports(Limits::default()).unwrap() {
        let part = link
            .receive_report(&report, Duration::from_millis(at), model, Stages::default())
            .unwrap();
        step.reports.extend(part.reports);
        step.sent.extend(part.sent);
        step.session.extend(part.session);
        step.notices.extend(part.notices);
    }
    step
}

fn replies(step: &Step) -> Vec<Envelope> {
    let mut reassembler = Reassembler::new(Limits::default());
    step.reports
        .iter()
        .filter_map(|report| reassembler.push(report).unwrap())
        .collect()
}

/// KeepAlive UPDATE, a sent ModelRepo and a Connection message are the three
/// flags of `0x6e24a8`; any message except KeepAlive also renews keep-alive.
fn connect(link: &mut ControlLink, model: &mut Model) {
    let read = ModelRepoMessage {
        action: Some(MessageAction::Read.into()),
        request_id: Some(5),
        ..Default::default()
    };
    let step = feed(link, model, read.into(), 1);
    let repo = replies(&step);
    assert_eq!(repo.len(), 1);
    // `0x11e9d8` sends without the request ID.
    let repo = ModelRepoMessage::decode(&repo[0].payload, CodecLimits::default())
        .unwrap()
        .message;
    assert_eq!(repo.request_id, None);
    assert_eq!(repo.model_repo_payload.as_deref(), Some(&b"<repo/>"[..]));
    assert!(!link.session().connected());
    let step = feed(
        link,
        model,
        ConnectionMessage {
            connected: Some(true),
            ..Default::default()
        }
        .into(),
        2,
    );
    assert_eq!(step.session, [SessionEvent::Connected]);
}

#[test]
fn handshake_needs_keep_alive_model_repo_and_connection_and_expiry_tears_down() {
    let mut link = link();
    let mut model = Model::default();
    connect(&mut link, &mut model);
    assert!(link.session().connected());

    // KeepAlive UPDATE copies the online flag and its timeout in seconds.
    let step = feed(
        &mut link,
        &mut model,
        KeepAliveMessage {
            action: Some(MessageAction::Update.into()),
            is_online: Some(true),
            timeout: Some(2.5),
            ..Default::default()
        }
        .into(),
        3,
    );
    assert_eq!(step.session, [SessionEvent::Online(true)]);
    assert_eq!(link.session().timeout(), Duration::from_millis(2500));

    let quiet = link
        .tick(Duration::from_millis(2502), &mut model, Stages::default())
        .unwrap();
    assert!(quiet.session.is_empty());
    let expired = link
        .tick(Duration::from_millis(2503), &mut model, Stages::default())
        .unwrap();
    assert_eq!(
        expired.session,
        [
            SessionEvent::KeepAliveExpired,
            SessionEvent::Disconnected,
            SessionEvent::Online(false),
            SessionEvent::FlushTransmit,
        ]
    );
    // Teardown clears every flag: a later message alone does not reconnect.
    assert_eq!(link.session().handshake(), Default::default());
}

#[test]
fn state_reply_echoes_request_id_with_update_action_only_while_connected() {
    let mut link = link();
    let mut model = Model::default();
    let read = |id| -> ControlMessage {
        GlobalEQMessage {
            action: Some(MessageAction::Read.into()),
            request_id: Some(id),
            ..Default::default()
        }
        .into()
    };
    // Stock state senders return early while disconnected.
    assert!(feed(&mut link, &mut model, read(7), 0).reports.is_empty());
    connect(&mut link, &mut model);
    let step = feed(&mut link, &mut model, read(u64::MAX), 3);
    let reply = &replies(&step)[0];
    assert_eq!(reply.message_type, 38);
    let reply = GlobalEQMessage::decode(&reply.payload, CodecLimits::default())
        .unwrap()
        .message;
    assert_eq!(reply.action, Some(MessageAction::Update.into()));
    assert_eq!(reply.request_id, Some(u64::MAX));
    assert_eq!(reply.bypassed, Some(true));
}

#[test]
fn global_eq_update_prefers_preset_then_bounds_parameter_index() {
    let mut link = link();
    let mut model = Model::default();
    let parameter = |index| GlobalEQParameter {
        parameter_index: Some(index),
        value: Some(0.5),
    };
    let update = GlobalEQMessage {
        action: Some(MessageAction::Update.into()),
        parameters: vec![parameter(27), parameter(28), parameter(0)],
        bypassed: Some(false),
        ..Default::default()
    };
    feed(&mut link, &mut model, update.clone().into(), 0);
    assert_eq!(model.calls, ["eq Some(27)", "eq Some(0)", "bypass false"]);
    model.calls.clear();
    let preset = GlobalEQMessage {
        model_preset_to_load: Some(ModelPresetID::default()),
        ..update
    };
    let step = feed(&mut link, &mut model, preset.into(), 1);
    assert_eq!(model.calls, ["eq preset"]);
    assert!(step.reports.is_empty());
}

#[test]
fn grid_model_meter_subscriptions_and_the_stock_timer_rule() {
    let mut link = link();
    let mut model = Model::default();
    let meter = |action: MessageAction, row, column| -> ControlMessage {
        GridModelMeterMessage {
            action: Some(action.into()),
            row,
            column,
            ..Default::default()
        }
        .into()
    };
    for (row, column) in [
        (Some(3), Some(7)),
        (Some(4), Some(0)),
        (Some(2), Some(-1)),
        (None, None),
    ] {
        feed(
            &mut link,
            &mut model,
            meter(MessageAction::Create, row, column),
            0,
        );
    }
    let subscribed: Vec<_> = link.state().grid_meters.iter().copied().collect();
    assert_eq!(
        subscribed,
        [
            MeterTarget::Row(2),
            MeterTarget::Model { row: 0, column: 0 },
            MeterTarget::Model { row: 3, column: 7 },
        ]
    );
    feed(
        &mut link,
        &mut model,
        meter(MessageAction::Delete, Some(2), Some(-1)),
        0,
    );
    assert_eq!(link.state().grid_meters.len(), 2);
    assert!(link.state().timers.grid_meters.enabled);
    feed(
        &mut link,
        &mut model,
        meter(MessageAction::Delete, Some(-1), Some(-1)),
        0,
    );
    assert!(link.state().grid_meters.is_empty());
    assert!(!link.state().timers.grid_meters.enabled);
    // `0x110a70`: a CREATE out of range still starts the timer; only DELETE All stops it.
    let mut link = self::link();
    feed(
        &mut link,
        &mut model,
        meter(MessageAction::Create, Some(4), Some(0)),
        0,
    );
    assert!(link.state().grid_meters.is_empty() && link.state().timers.grid_meters.enabled);
    feed(
        &mut link,
        &mut model,
        meter(MessageAction::Delete, Some(4), Some(0)),
        0,
    );
    assert!(link.state().timers.grid_meters.enabled);
}

#[test]
fn scene_update_bounds_and_read_reply_through_the_dispatcher() {
    let mut link = link();
    let mut model = Model::default();
    connect(&mut link, &mut model);
    let scene = |payload: &[u8]| Envelope {
        message_type: 13,
        transform: 0,
        decompress: 0,
        opaque: [0; 2],
        payload: payload.to_vec(),
    };
    feed_envelope(&mut link, &mut model, scene(&[8, 1, 24, 6]), 3);
    assert_eq!(model.scene, 6);
    feed_envelope(&mut link, &mut model, scene(&[8, 1, 24, 8]), 3);
    assert_eq!(model.scene, 6);
    let step = feed_envelope(&mut link, &mut model, scene(&[8, 3, 16, 9]), 3);
    let reply = &replies(&step)[0];
    assert_eq!(reply.message_type, 13);
    assert_eq!(
        SceneMessage::decode(&reply.payload, CodecLimits::default())
            .unwrap()
            .message,
        SceneMessage {
            action: Some(MessageAction::Update.into()),
            request_id: Some(9),
            selected_scene: Some(6),
        }
    );
}

#[test]
fn unsupported_and_malformed_requests_are_notices_not_link_failures() {
    let mut link = link();
    let mut model = Model::default();
    let step = feed(
        &mut link,
        &mut model,
        tetrad_control::codecs::LicenseMessage {
            action: Some(EnumValue::Known(MessageAction::Read)),
            ..Default::default()
        }
        .into(),
        0,
    );
    assert!(matches!(
        step.notices[..],
        [tetrad_control::link::Notice::Device(
            DeviceError::Unsupported(_)
        )]
    ));
    let corrupt = Envelope {
        message_type: 38,
        transform: 0,
        decompress: 0,
        opaque: [0; 2],
        payload: vec![0x0f],
    };
    let step = feed_envelope(&mut link, &mut model, corrupt, 0);
    assert!(step.reports.is_empty());
    assert!(matches!(
        step.notices[..],
        [tetrad_control::link::Notice::Parse {
            message_type: 38,
            ..
        }]
    ));
}

#[test]
fn pushes_need_connection_and_subscriptions_and_meter_timer_follows_subscriptions() {
    use tetrad_control::codecs::TunerMessage;
    use tetrad_control::link::Notification;

    struct Meters(Model, Vec<Vec<MeterTarget>>);
    impl DeviceModel for Meters {
        fn model_repo(&mut self) -> DeviceResult<Vec<u8>> {
            self.0.model_repo()
        }
        fn grid_model_meters(
            &mut self,
            targets: &[MeterTarget],
        ) -> DeviceResult<Vec<ControlMessage>> {
            self.1.push(targets.to_vec());
            Ok(vec![GridMessage::default().into()])
        }
    }

    let mut link = link();
    let mut model = Model::default();
    let at = Duration::from_millis;
    let meter = Notification::TunerMeter(110.0);
    // Disconnected: nothing is sent.
    assert!(
        link.notify(meter.clone(), at(0), &mut model, Stages::default())
            .unwrap()
            .reports
            .is_empty()
    );
    connect(&mut link, &mut model);
    // Connected but not subscribed: still nothing.
    assert!(
        link.notify(meter.clone(), at(3), &mut model, Stages::default())
            .unwrap()
            .reports
            .is_empty()
    );
    let subscribe = tetrad_control::codecs::TunerMessage {
        action: Some(MessageAction::Update.into()),
        enable_meter: Some(true),
        ..Default::default()
    };
    struct Tuner;
    impl DeviceModel for Tuner {
        fn update_tuner(&mut self, _: tetrad_control::device::TunerCommand) -> DeviceResult<()> {
            Ok(())
        }
    }
    let mut tuner = Tuner;
    let envelope = Envelope {
        message_type: 6,
        transform: 0,
        decompress: 0,
        opaque: [0; 2],
        payload: subscribe.encode(CodecLimits::default()).unwrap(),
    };
    for report in envelope.reports(Limits::default()).unwrap() {
        link.receive_report(&report, at(3), &mut tuner, Stages::default())
            .unwrap();
    }
    let step = link
        .notify(meter, at(4), &mut model, Stages::default())
        .unwrap();
    let sent = TunerMessage::decode(&replies(&step)[0].payload, CodecLimits::default())
        .unwrap()
        .message;
    assert_eq!((sent.meter, sent.request_id), (Some(110.0), None));
    assert!(
        link.notify(
            Notification::TunerMeter(f32::NAN),
            at(4),
            &mut model,
            Stages::default()
        )
        .unwrap()
        .reports
        .is_empty()
    );

    // The grid meter timer runs only while a subscription exists.
    let mut meters = Meters(Model::default(), Vec::new());
    assert!(
        link.tick(at(5), &mut meters, Stages::default())
            .unwrap()
            .reports
            .is_empty()
    );
    let create = GridModelMeterMessage {
        action: Some(MessageAction::Create.into()),
        row: Some(1),
        column: Some(-1),
        ..Default::default()
    };
    let envelope = Envelope {
        message_type: 37,
        transform: 0,
        decompress: 0,
        opaque: [0; 2],
        payload: create.encode(CodecLimits::default()).unwrap(),
    };
    for report in envelope.reports(Limits::default()).unwrap() {
        link.receive_report(&report, at(5), &mut meters, Stages::default())
            .unwrap();
    }
    // The first meter follows one 100 ms interval after the subscription.
    assert!(
        link.tick(at(6), &mut meters, Stages::default())
            .unwrap()
            .sent
            .is_empty()
    );
    assert!(
        link.tick(at(50), &mut meters, Stages::default())
            .unwrap()
            .sent
            .is_empty()
    );
    let step = link.tick(at(105), &mut meters, Stages::default()).unwrap();
    assert_eq!(step.sent.len(), 1);
    let grid = GridMessage::decode(&replies(&step)[0].payload, CodecLimits::default())
        .unwrap()
        .message;
    assert_eq!(
        (grid.action, grid.update_type),
        (
            Some(MessageAction::Update.into()),
            Some(GridUpdateType::MeterUpdate.into())
        )
    );
    assert_eq!(
        link.tick(at(205), &mut meters, Stages::default())
            .unwrap()
            .sent
            .len(),
        1
    );
    assert_eq!(
        meters.1,
        [vec![MeterTarget::Row(1)], vec![MeterTarget::Row(1)]]
    );
}

/// Grid (1) `0x257408`: UPDATE with a preset applies it and DELETE removes
/// it. Other actions, and UPDATE without a preset, are ignored.
#[test]
fn grid_preset_actions_select_update_or_delete() {
    let mut link = link();
    let mut model = Model::default();
    connect(&mut link, &mut model);
    let grid = |action: MessageAction, has_preset: bool| -> ControlMessage {
        GridMessage {
            action: Some(action.into()),
            preset: has_preset
                .then(|| tetrad_control::codecs::RawMessageBytes { bytes: Vec::new() }),
            ..Default::default()
        }
        .into()
    };
    feed(&mut link, &mut model, grid(MessageAction::Update, true), 3);
    feed(&mut link, &mut model, grid(MessageAction::Delete, true), 4);
    feed(&mut link, &mut model, grid(MessageAction::Update, false), 5);
    feed(&mut link, &mut model, grid(MessageAction::Create, true), 6);
    assert_eq!(model.calls, ["grid update", "grid delete"]);
}

/// RecallPreset (`0x274238`): CREATE loads like UPDATE and replies; READ
/// only replies.
#[test]
fn recall_preset_create_loads_like_update() {
    let mut link = link();
    let mut model = Model::default();
    connect(&mut link, &mut model);
    let recall = |action: EnumValue<MessageAction>| -> ControlMessage {
        RecallPresetMessage {
            action: Some(action),
            ..Default::default()
        }
        .into()
    };
    let created = feed(
        &mut link,
        &mut model,
        recall(MessageAction::Create.into()),
        3,
    );
    assert_eq!(model.calls, ["recall"]);
    assert_eq!(replies(&created).len(), 1);
    let read = feed(&mut link, &mut model, recall(MessageAction::Read.into()), 4);
    assert_eq!(model.calls, ["recall"]);
    assert_eq!(replies(&read).len(), 1);
}

/// Mode UPDATE `0x271aa0`: `mode` (has-bit 4) wins and above 8 does nothing, then a
/// non-empty list, then the ATMA page; a message with none of them does nothing.
#[test]
fn mode_update_takes_mode_then_a_non_empty_list_then_the_page() {
    let mut link = link();
    let mut model = Model::default();
    connect(&mut link, &mut model);
    let update = |mode: Option<u32>, modes: Option<Vec<u32>>, page: Option<u32>| {
        ControlMessage::from(ModeMessage {
            action: Some(MessageAction::Update.into()),
            mode,
            available_modes: modes.map(|modes| tetrad_control::codecs::AvailableModes { modes }),
            atma_page: page,
            ..Default::default()
        })
    };
    for (at, message) in [
        update(Some(2), Some(vec![0, 1]), Some(1)),
        update(Some(9), Some(vec![0, 1]), None),
        update(None, Some(vec![]), Some(1)),
        update(None, Some(vec![4]), Some(1)),
        update(None, None, None),
    ]
    .into_iter()
    .enumerate()
    {
        feed(&mut link, &mut model, message, 3 + at as u64);
    }
    assert_eq!(
        model.calls,
        ["Mode(2)", "AtmaPage(1)", "AvailableModes([4])"]
    );
}

/// CPULoad (`0x6e0128`): CREATE sends the first report at once, echoing the
/// request ID, then repeats every 1000 ms.
#[test]
fn cpu_load_create_sends_at_once_then_every_second() {
    let mut link = link();
    let mut model = Model::default();
    connect(&mut link, &mut model);
    let create = CPULoadMessage {
        action: Some(MessageAction::Create.into()),
        request_id: Some(7),
        ..Default::default()
    };
    let step = feed(&mut link, &mut model, create.into(), 3);
    let first = CPULoadMessage::decode(&replies(&step)[0].payload, CodecLimits::default())
        .unwrap()
        .message;
    assert_eq!(first.request_id, Some(7));
    let tick = |link: &mut ControlLink, model: &mut Model, millis| {
        link.tick(Duration::from_millis(millis), model, Stages::default())
            .unwrap()
            .sent
            .len()
    };
    assert_eq!(tick(&mut link, &mut model, 1002), 0);
    assert_eq!(tick(&mut link, &mut model, 1003), 1);
}

/// Owned: IOMeter CREATE starts `sendIOMeters` (100 ms, `0x6e0ae0`); a model that does not
/// support the meters answers once with a notice and the timer stops, instead of a notice
/// every 100 ms. A new CREATE starts it again.
#[test]
fn an_unsupported_periodic_sender_stops_after_one_notice() {
    use tetrad_control::codecs::IOMeterMessage;
    let mut link = link();
    let mut model = Model::default();
    connect(&mut link, &mut model);
    let create = || {
        IOMeterMessage {
            action: Some(MessageAction::Create.into()),
            ..Default::default()
        }
        .into()
    };
    feed(&mut link, &mut model, create(), 10);
    let mut tick = |millis| {
        link.tick(Duration::from_millis(millis), &mut model, Stages::default())
            .unwrap()
            .notices
            .len()
    };
    assert_eq!(tick(110), 1);
    assert_eq!(tick(210), 0);
    assert!(!link.state().timers.io_meters.enabled);
    feed(&mut link, &mut model, create(), 300);
    assert!(link.state().timers.io_meters.enabled);
}

/// DefaultParameters `0x15eb0c` switches on the action: CREATE stores the block at row and
/// column and replies UPDATE with both; READ sends the list (its inputs are unsupported
/// here, so one notice); DELETE has no arm.
#[test]
fn default_parameters_create_saves_and_replies_row_and_column() {
    use tetrad_control::codecs::DefaultParametersMessage;
    let mut link = link();
    let mut model = Model::default();
    connect(&mut link, &mut model);
    let message = |action: MessageAction| {
        DefaultParametersMessage {
            action: Some(action.into()),
            row: Some(2),
            column: Some(5),
            ..Default::default()
        }
        .into()
    };
    let step = feed(&mut link, &mut model, message(MessageAction::Create), 10);
    assert_eq!(model.calls, ["save(2, 5)"]);
    let [reply] = &replies(&step)[..] else {
        panic!("one reply");
    };
    let reply = DefaultParametersMessage::decode(&reply.payload, CodecLimits::default())
        .unwrap()
        .message;
    assert_eq!(
        (
            reply.action,
            reply.row,
            reply.column,
            reply.all_defaults.len()
        ),
        (Some(MessageAction::Update.into()), Some(2), Some(5), 0)
    );
    let read = feed(&mut link, &mut model, message(MessageAction::Read), 20);
    assert!(read.reports.is_empty());
    assert_eq!(read.notices.len(), 1);
    let delete = feed(&mut link, &mut model, message(MessageAction::Delete), 30);
    assert!(delete.reports.is_empty());
    assert_eq!(model.calls.len(), 1);
}
