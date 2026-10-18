//! Resync DefaultParameters (19, `0x162f4c` → `0x15d890`) and ModelPreset (71, `0x11d324`
//! → `0x1d037c`) entries with blocks from `block::serialize`. Synthetic blocks only.

use std::time::Duration;

use tetrad_control::block::{CaptureDefault, ModelPresetRecord, TypedBlock, ValueCase};
use tetrad_control::codecs::{
    CodecLimits, ConnectionMessage, ControlMessage, DefaultParametersMessage, MessageAction,
    ModelPresetID, ModelPresetMessage, ModelRepoMessage, ParamValue,
};
use tetrad_control::device::{DeviceModel, DeviceResult};
use tetrad_control::link::{ControlLink, LinkLimits, Step};
use tetrad_control::pipeline::Stages;
use tetrad_control::session::Session;
use tetrad_control::{Envelope, Limits, Reassembler};
use tetrad_preset::ParamValue as Stored;

const NOW: Duration = Duration::from_millis(1);

#[derive(Default)]
struct Library {
    keys: Vec<String>,
    looked_up: Vec<String>,
    records: Vec<ModelPresetRecord>,
}

fn block(model: u32, value: Stored, case: ValueCase) -> TypedBlock {
    TypedBlock {
        model: tetrad_preset::Model::block(model, [value]),
        cases: vec![case],
    }
}

impl DeviceModel for Library {
    fn model_repo(&mut self) -> DeviceResult<Vec<u8>> {
        Ok(vec![1])
    }

    fn default_param_ids(&mut self) -> DeviceResult<Vec<String>> {
        Ok(self.keys.clone())
    }

    fn capture_default(&mut self, key: &str) -> DeviceResult<Option<CaptureDefault>> {
        self.looked_up.push(key.to_owned());
        Ok(key.starts_with("capture").then(|| CaptureDefault {
            id: key.to_owned(),
            block: block(
                14000,
                Stored::String(format!("{key}name")),
                ValueCase::String,
            ),
        }))
    }

    fn model_preset_records(&mut self) -> DeviceResult<Vec<ModelPresetRecord>> {
        Ok(self.records.clone())
    }
}

fn resync(model: &mut Library) -> Vec<ControlMessage> {
    let session = Session::new(Duration::ZERO, Duration::from_secs(5)).unwrap();
    let mut link = ControlLink::new(session, LinkLimits::default());
    let read = Some(MessageAction::Read.into());
    let connect: [ControlMessage; 2] = [
        ModelRepoMessage {
            action: read,
            ..Default::default()
        }
        .into(),
        ConnectionMessage {
            connected: Some(true),
            ..Default::default()
        }
        .into(),
    ];
    for message in connect {
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
    }
    let step: Step = link.resync(false, NOW, model, Stages::default()).unwrap();
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

/// 19: DELETE, then one message (action 0, absent) with an entry per `default_param` key
/// in QString order whose library capture exists. Numbers other than 14000/14001 are never
/// looked up; duplicates collapse. The block carries its scene 0 value.
#[test]
fn default_parameters_resync_sends_library_captures_in_key_order() {
    let mut library = Library {
        keys: [
            "capture-b",
            "1",
            "14001",
            "capture-a",
            "capture-b",
            "Zed",
            "missing",
        ]
        .map(String::from)
        .to_vec(),
        ..Default::default()
    };
    let sent: Vec<_> = resync(&mut library)
        .into_iter()
        .filter_map(|message| match message {
            ControlMessage::DefaultParameters(message) => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(
        library.looked_up,
        ["14001", "Zed", "capture-a", "capture-b", "missing"]
    );
    let [delete, list] = &sent[..] else {
        panic!("expected two DefaultParameters messages, got {sent:?}");
    };
    assert_eq!(
        delete,
        &DefaultParametersMessage {
            action: Some(MessageAction::Delete.into()),
            ..delete.clone()
        }
    );
    assert_eq!(list.action, None);
    let entries: Vec<_> = list
        .all_defaults
        .iter()
        .map(|entry| {
            let model = entry.model.as_ref().unwrap();
            let value = model.params[0].param_values.clone();
            (entry.id.clone().unwrap(), model.hash, value)
        })
        .collect();
    let entry = |id: &str| {
        let value = vec![ParamValue::string(format!("{id}name"))];
        (id.to_owned(), Some(14000), value)
    };
    assert_eq!(entries, [entry("capture-a"), entry("capture-b")]);
}

/// 71: DELETE, then UPDATE with every record in order. Id and name pass `toAscii`
/// (characters above U+00FF become `?`); only default entries carry a block, the special
/// id included (resync flag 1). Int-typed values are `qRound64` of the stored float.
#[test]
fn model_preset_resync_sends_every_record_with_default_blocks() {
    let record = |id: &str, name: &str, default_block: Option<TypedBlock>| ModelPresetRecord {
        id: id.into(),
        is_factory: default_block.is_some(),
        hash: 31000,
        name: name.into(),
        default_block,
    };
    let mut library = Library {
        records: vec![
            record("12", "Crunch ✓é", None),
            record(
                "3",
                "Clean",
                Some(block(31000, Stored::Float(2.5), ValueCase::Int)),
            ),
            record(
                "SpecialFactoryModelPresetID",
                "",
                Some(block(31000, Stored::Float(0.25), ValueCase::Float)),
            ),
        ],
        ..Default::default()
    };
    let sent: Vec<_> = resync(&mut library)
        .into_iter()
        .filter_map(|message| match message {
            ControlMessage::ModelPreset(message) => Some(message),
            _ => None,
        })
        .collect();
    let [delete, update] = &sent[..] else {
        panic!("expected two ModelPreset messages, got {sent:?}");
    };
    assert_eq!(
        delete,
        &ModelPresetMessage {
            action: Some(MessageAction::Delete.into()),
            ..delete.clone()
        }
    );
    assert_eq!(update.action, Some(MessageAction::Update.into()));
    let entries: Vec<_> = update
        .presets
        .iter()
        .map(|preset| {
            let value = preset.model.as_ref().map(|model| {
                assert_eq!(model.hash, Some(31000));
                model.params[0].param_values[0].clone()
            });
            (
                preset.id.clone().unwrap(),
                preset.name.clone(),
                preset.is_default,
                value,
            )
        })
        .collect();
    let id = |value: &str, is_factory| ModelPresetID {
        value: Some(value.into()),
        is_factory: Some(is_factory),
        hash: Some(31000),
    };
    assert_eq!(
        entries,
        [
            (id("12", false), Some("Crunch ?é".into()), Some(false), None),
            (
                id("3", true),
                Some("Clean".into()),
                Some(true),
                Some(ParamValue::int(3))
            ),
            (
                id("SpecialFactoryModelPresetID", true),
                Some(String::new()),
                Some(true),
                Some(ParamValue::float(0.25))
            ),
        ]
    );
}
