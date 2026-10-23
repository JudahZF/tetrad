//! The newest published MIDI Mapper package in the real runner, driven by a small fake host:
//! activation and the trigger each MIDI message fires. Skips when the package is not built.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde_json::{Value, json};
use tetrad_extensions::package::Package;
use tetrad_extensions::protocol::{
    self, CallResult, Event, ExtensionInfo, HostMessage, MidiEvent, PROTOCOL_VERSION,
    RunnerMessage, TriggerSpec,
};
use tetrad_extensions::runner::{self, Limits};

fn package() -> Option<Package> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../extensions/repository/packages");
    std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy();
            name.starts_with("tetrad.midi-mapper-") && name.ends_with(".tetradext")
        })
        .map(|p| Package::decode(&std::fs::read(p).unwrap()).unwrap())
        .max_by(|a, b| a.manifest.version.cmp(&b.manifest.version))
}

struct Host {
    to_runner: std::io::PipeWriter,
    from_runner: BufReader<std::io::PipeReader>,
    status: Vec<String>,
    fired: Vec<Value>,
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

    /// Answers calls until the runner sends a message that is not a request or a log.
    fn serve(&mut self) -> RunnerMessage {
        loop {
            match self.recv() {
                RunnerMessage::Request {
                    id, method, params, ..
                } => {
                    match method.as_str() {
                        "ui.setStatus" => {
                            self.status = serde_json::from_value(params["lines"].clone()).unwrap()
                        }
                        "triggers.fire" => self.fired.push(params),
                        other => panic!("unexpected call {other}"),
                    }
                    let result = CallResult::Ok(Value::Null);
                    self.send(&HostMessage::Response { id, result });
                }
                RunnerMessage::Log { text, .. } => panic!("unexpected log: {text}"),
                other => return other,
            }
        }
    }

    /// Runs one event and returns the firings it made.
    fn event(&mut self, scope: u64, event: Event) -> Vec<Value> {
        self.send(&HostMessage::Event { scope, event });
        assert_eq!(
            self.serve(),
            RunnerMessage::Done {
                scope,
                error: None,
                result: None
            }
        );
        std::mem::take(&mut self.fired)
    }
}

fn midi(message: MidiEvent) -> Event {
    Event::Midi { message }
}

#[test]
fn fires_a_trigger_for_each_cc_note_and_program_change() {
    let Some(package) = package() else {
        eprintln!("skipped: build extensions/repository/packages/*midi-mapper*.tetradext first");
        return;
    };
    let (input, to_runner) = std::io::pipe().unwrap();
    let (from_runner, mut output) = std::io::pipe().unwrap();
    let module = package.module.expect("MIDI Mapper has a module");
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
        status: Vec::new(),
        fired: Vec::new(),
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
    assert_eq!(
        registrations.triggers,
        [TriggerSpec {
            id: "midi".into(),
            label: "Ch 2".into()
        }]
    );
    assert!(registrations.commands.is_empty() && !registrations.control_changes);

    assert!(host.event(1, Event::Running).is_empty());
    assert_eq!(host.status, ["Channel 2 · sends triggers to Mappings"]);

    let cc = MidiEvent::ControlChange {
        channel: 2,
        controller: 20,
        value: 127,
    };
    assert_eq!(
        host.event(2, midi(cc)),
        [
            json!({"trigger": "midi", "source": "cc:20", "label": "CC 20", "value": 1, "continuous": true})
        ]
    );
    let note = |velocity| MidiEvent::NoteOn {
        channel: 2,
        key: 60,
        velocity,
    };
    let fired = host.event(3, midi(note(127)));
    assert_eq!(
        fired,
        [
            json!({"trigger": "midi", "source": "note:60", "label": "Note 60", "value": 1, "continuous": false})
        ]
    );
    // Velocity 0 and note-off are releases: they fire nothing.
    assert!(host.event(4, midi(note(0))).is_empty());
    let off = MidiEvent::NoteOff {
        channel: 2,
        key: 60,
        velocity: 64,
    };
    assert!(host.event(5, midi(off)).is_empty());
    let pc = MidiEvent::ProgramChange {
        channel: 2,
        program: 4,
    };
    assert_eq!(
        host.event(6, midi(pc)),
        [
            json!({"trigger": "midi", "source": "pc:4", "label": "PC 5", "value": 1, "continuous": false})
        ]
    );

    host.send(&HostMessage::Stop);
    assert!(thread.join().unwrap().is_ok());
}
