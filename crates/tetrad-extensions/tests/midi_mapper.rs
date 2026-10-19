//! The published MIDI Mapper package in the real runner, driven by a small fake host:
//! activation, the learn flow and applying a mapping. Skips when the package is not built.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde_json::{Map, Value, json};
use tetrad_extensions::package::Package;
use tetrad_extensions::protocol::{
    self, Binding, CallResult, ControlChange, ControlKind, ErrorCode, Event, ExtensionInfo,
    HostMessage, MidiEvent, PROTOCOL_VERSION, RunnerMessage, Site,
};
use tetrad_extensions::runner::{self, Limits};

fn package() -> Option<Package> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/repository/packages");
    let path = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            name.contains("midi-mapper") && name.ends_with(".tetradext")
        })?;
    Some(Package::decode(&std::fs::read(path).unwrap()).unwrap())
}

struct Host {
    to_runner: std::io::PipeWriter,
    from_runner: BufReader<std::io::PipeReader>,
    kv: Map<String, Value>,
    status: Vec<String>,
    edits: Vec<(String, Value)>,
    warnings: Vec<String>,
    /// The answer to the next control edit; `None` succeeds.
    refuse_edit: Option<ErrorCode>,
}

impl Host {
    fn send(&mut self, message: &HostMessage) {
        self.to_runner
            .write_all(&protocol::encode(message).unwrap())
            .unwrap();
    }

    fn recv(&mut self) -> RunnerMessage {
        let mut line = Vec::new();
        self.from_runner.read_until(b'\n', &mut line).unwrap();
        assert!(!line.is_empty(), "runner closed its output");
        protocol::decode(line.trim_ascii_end()).unwrap()
    }

    fn answer(&mut self, method: &str, params: Value) -> CallResult {
        let key = || params["key"].as_str().unwrap().to_owned();
        let value = match method {
            "storage.get" => self.kv.get(&key()).cloned().unwrap_or(Value::Null),
            "storage.set" => {
                self.kv.insert(key(), params["value"].clone());
                Value::Null
            }
            "storage.delete" => {
                self.kv.remove(&key());
                Value::Null
            }
            "controls.list" => json!([{
                "site": "slot", "row": 0, "column": 1, "model": 7, "name": "Amp",
                "bypassed": false, "parameters": [{"index": 3, "name": "Gain", "value": 0.5}]
            }]),
            "ui.setStatus" => {
                self.status = serde_json::from_value(params["lines"].clone()).unwrap();
                Value::Null
            }
            "controls.set" | "controls.setBypass" | "controls.toggleBypass" => {
                self.edits.push((method.to_owned(), params));
                if let Some(code) = self.refuse_edit.take() {
                    return CallResult::error(code, "preset changed");
                }
                Value::Null
            }
            other => panic!("unexpected call {other}"),
        };
        CallResult::Ok(value)
    }

    /// Answers calls until the runner sends a message that is not a request or a log.
    fn serve(&mut self) -> RunnerMessage {
        loop {
            match self.recv() {
                RunnerMessage::Request {
                    id, method, params, ..
                } => {
                    let result = self.answer(&method, params);
                    self.send(&HostMessage::Response { id, result });
                }
                RunnerMessage::Log { text, .. } => self.warnings.push(text),
                other => return other,
            }
        }
    }

    fn event(&mut self, scope: u64, event: Event) {
        self.send(&HostMessage::Event { scope, event });
        assert_eq!(self.serve(), RunnerMessage::Done { scope, error: None });
    }
}

fn cc(controller: u8, value: u8) -> Event {
    Event::Midi {
        message: MidiEvent::ControlChange {
            channel: 2,
            controller,
            value,
        },
    }
}

#[test]
fn learns_and_applies_a_cc_mapping() {
    let Some(package) = package() else {
        eprintln!("skipped: build extensions/repository/packages/*midi-mapper*.tetradext first");
        return;
    };
    let (input, to_runner) = std::io::pipe().unwrap();
    let (from_runner, mut output) = std::io::pipe().unwrap();
    let module = package.module.clone();
    let thread = std::thread::spawn(move || {
        runner::run(
            &module,
            BufReader::new(input),
            &mut output,
            Limits::default(),
        )
    });
    let mut host = Host {
        to_runner,
        from_runner: BufReader::new(from_runner),
        kv: Map::new(),
        status: Vec::new(),
        edits: Vec::new(),
        warnings: Vec::new(),
        refuse_edit: None,
    };

    host.send(&HostMessage::Activate {
        protocol: PROTOCOL_VERSION,
        extension: ExtensionInfo {
            id: package.manifest.id.clone(),
            version: package.manifest.version.to_string(),
        },
        config: package.manifest.default_config(),
    });
    let RunnerMessage::Activated { registrations } = host.serve() else {
        panic!("not activated")
    };
    assert_eq!(registrations.midi_channels, [2]);
    assert_eq!(registrations.commands, ["learn", "cancel", "undo", "clear"]);
    assert!(registrations.control_changes);
    let labelled: Vec<&str> = package
        .manifest
        .commands
        .iter()
        .map(|c| c.id.as_str())
        .collect();
    assert_eq!(
        labelled, registrations.commands,
        "every command has a manifest label"
    );

    host.event(1, Event::Running);
    assert_eq!(host.status, ["Channel 2, 0 mappings"]);

    host.event(2, Event::Command { id: "learn".into() });
    host.event(3, cc(20, 5));
    assert!(
        host.status[0].contains("Now touch a control"),
        "{:?}",
        host.status
    );
    assert!(host.edits.is_empty(), "learning does not apply mappings");

    let binding = Binding {
        site: Site::Slot { row: 0, column: 1 },
        model: 7,
        control: ControlKind::Parameter { index: 3 },
    };
    host.event(
        4,
        Event::ControlChanged {
            change: ControlChange {
                binding,
                value: 0.2,
            },
        },
    );
    assert_eq!(host.status, ["Channel 2, 1 mapping", "CC 20 → Amp: Gain"]);
    assert_eq!(host.kv["mappings"][0]["mode"], "continuous");

    host.event(5, cc(20, 127));
    let (method, params) = host.edits.pop().unwrap();
    assert_eq!(method, "controls.set");
    assert_eq!(params["value"].as_f64(), Some(1.0));
    assert_eq!(
        serde_json::from_value::<Binding>(params["binding"].clone()).unwrap(),
        binding
    );

    // An expected refusal after a preset change is not an error and not a log line.
    host.refuse_edit = Some(ErrorCode::StaleContext);
    host.event(6, cc(20, 0));
    assert!(host.warnings.is_empty(), "{:?}", host.warnings);

    host.event(7, Event::Command { id: "undo".into() });
    assert_eq!(host.kv["mappings"], json!([]));

    host.send(&HostMessage::Stop);
    assert!(thread.join().unwrap().is_ok());
}
