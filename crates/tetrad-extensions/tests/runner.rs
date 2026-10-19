//! The runner over real pipes, as the host drives it: activation, scoped calls and every
//! fault the supervisor relies on.

use std::io::{BufRead, BufReader, Write};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::json;
use tetrad_extensions::protocol::{
    self, CallResult, ErrorCode, Event, ExtensionInfo, HostMessage, MidiEvent,
    PROTOCOL_VERSION, RunnerMessage,
};
use tetrad_extensions::runner::{self, Limits, RunnerError};

struct Harness {
    to_runner: std::io::PipeWriter,
    from_runner: BufReader<std::io::PipeReader>,
    thread: JoinHandle<Result<(), RunnerError>>,
}

impl Harness {
    fn start(source: &str, limits: Limits) -> Self {
        let (input, to_runner) = std::io::pipe().unwrap();
        let (from_runner, mut output) = std::io::pipe().unwrap();
        let source = source.to_owned();
        let thread = std::thread::spawn(move || {
            runner::run(&source, BufReader::new(input), &mut output, limits)
        });
        let mut harness = Self {
            to_runner,
            from_runner: BufReader::new(from_runner),
            thread,
        };
        harness.send(&HostMessage::Activate {
            protocol: PROTOCOL_VERSION,
            extension: ExtensionInfo {
                id: "test.ext".into(),
                version: "1.0.0".into(),
            },
            config: json!({"channel": 2}),
        });
        harness
    }

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

    /// The next message that is not a log line.
    fn next(&mut self) -> RunnerMessage {
        loop {
            match self.recv() {
                RunnerMessage::Log { .. } => {}
                message => return message,
            }
        }
    }

    fn fault(mut self) -> String {
        let message = match self.next() {
            RunnerMessage::Fault { message } => message,
            other => panic!("expected a fault, got {other:?}"),
        };
        assert!(self.thread.join().unwrap().is_err());
        message
    }
}

fn quick() -> Limits {
    Limits {
        handler: Duration::from_millis(300),
        activation: Duration::from_millis(300),
        ..Limits::default()
    }
}

const MAPPER: &str = r#"
export default {
  activate(ctx) {
    ctx.midi.reserveChannel(ctx.config.channel);
    ctx.commands.register("reset", async (scope) => { await scope.storage.delete("x"); });
    ctx.midi.onMessage(async (message, scope) => {
      if (message.kind !== "cc") return;
      const target = { site: "slot", row: 0, column: 1, model: 7, control: { kind: "parameter", index: 3 } };
      const result = await scope.controls.set(target, message.value / 127);
      ctx.log("applied", result);
    });
  },
};
"#;

#[test]
fn midi_event_becomes_a_scoped_host_call() {
    let mut h = Harness::start(MAPPER, Limits::default());
    let RunnerMessage::Activated { registrations } = h.next() else {
        panic!("not activated")
    };
    assert_eq!(registrations.midi_channels, [2]);
    assert_eq!(registrations.commands, ["reset"]);

    h.send(&HostMessage::Event {
        scope: 9,
        event: Event::Midi {
            message: MidiEvent::ControlChange {
                channel: 2,
                controller: 20,
                value: 127,
            },
        },
    });
    let RunnerMessage::Request { id, scope, method, params } = h.next() else {
        panic!("no request")
    };
    assert_eq!((scope, method.as_str()), (Some(9), "controls.set"));
    assert_eq!(params["value"].as_f64(), Some(1.0));
    assert_eq!(params["binding"]["control"]["index"], json!(3));

    h.send(&HostMessage::Response {
        id,
        result: CallResult::Ok(json!({"applied": true})),
    });
    let RunnerMessage::Log { text, .. } = h.recv() else {
        panic!("no log")
    };
    assert_eq!(text, r#"applied {"applied":true}"#);
    assert_eq!(h.next(), RunnerMessage::Done { scope: 9, error: None });

    h.send(&HostMessage::Stop);
    assert!(h.thread.join().unwrap().is_ok());
}

#[test]
fn a_rejected_call_rejects_with_its_code_and_the_handler_reports_it() {
    let mut h = Harness::start(
        r#"export default { activate(ctx) {
            ctx.commands.register("go", async (scope) => {
              try { await scope.controls.toggleBypass({}); }
              catch (e) { ctx.log(e.code); throw e; }
            });
        } };"#,
        Limits::default(),
    );
    assert!(matches!(h.next(), RunnerMessage::Activated { .. }));
    h.send(&HostMessage::Event {
        scope: 1,
        event: Event::Command { id: "go".into() },
    });
    let RunnerMessage::Request { id, .. } = h.next() else {
        panic!("no request")
    };
    h.send(&HostMessage::Response {
        id,
        result: CallResult::error(ErrorCode::StaleContext, "preset changed"),
    });
    let RunnerMessage::Log { text, .. } = h.recv() else {
        panic!("no log")
    };
    assert_eq!(text, "staleContext");
    let RunnerMessage::Done { scope: 1, error: Some(error) } = h.next() else {
        panic!("handler error not reported")
    };
    assert!(error.contains("preset changed"), "{error}");
}

#[test]
fn events_run_one_at_a_time() {
    let mut h = Harness::start(
        r#"export default { activate(ctx) {
            ctx.commands.register("a", async (scope) => { await scope.storage.get("k"); });
            ctx.commands.register("b", async (scope) => { await scope.storage.get("k"); });
        } };"#,
        Limits::default(),
    );
    assert!(matches!(h.next(), RunnerMessage::Activated { .. }));
    for (scope, id) in [(1, "a"), (2, "b")] {
        h.send(&HostMessage::Event {
            scope,
            event: Event::Command { id: id.into() },
        });
    }
    let RunnerMessage::Request { id, scope: Some(1), .. } = h.next() else {
        panic!("first handler did not start")
    };
    h.send(&HostMessage::Response {
        id,
        result: CallResult::Ok(json!(null)),
    });
    assert_eq!(h.next(), RunnerMessage::Done { scope: 1, error: None });
    let RunnerMessage::Request { scope: Some(2), .. } = h.next() else {
        panic!("second handler did not start after the first")
    };
}

#[test]
fn a_busy_loop_is_interrupted() {
    let h = Harness::start(
        r#"export default { activate(ctx) {
            ctx.commands.register("spin", () => { for (;;) {} });
        } };"#,
        quick(),
    );
    let mut h = h;
    assert!(matches!(h.next(), RunnerMessage::Activated { .. }));
    h.send(&HostMessage::Event {
        scope: 1,
        event: Event::Command { id: "spin".into() },
    });
    assert!(h.fault().contains("slice"));
}

#[test]
fn a_promise_that_never_settles_faults_the_handler() {
    let mut h = Harness::start(
        r#"export default { activate(ctx) {
            ctx.commands.register("hang", () => new Promise(() => {}));
        } };"#,
        quick(),
    );
    assert!(matches!(h.next(), RunnerMessage::Activated { .. }));
    h.send(&HostMessage::Event {
        scope: 1,
        event: Event::Command { id: "hang".into() },
    });
    assert!(h.fault().contains("did not finish in time"));
}

#[test]
fn the_heap_limit_holds() {
    let h = Harness::start(
        r#"export default { activate(ctx) {
            const a = []; for (;;) a.push(new Array(1024).fill(1));
        } };"#,
        Limits {
            slice: Duration::from_secs(10),
            ..quick()
        },
    );
    let fault = h.fault();
    assert!(fault.contains("memory"), "{fault}");
}

#[test]
fn the_activation_context_cannot_mutate() {
    let h = Harness::start(
        r#"export default { activate(ctx) { ctx.controls.set({}, 1); } };"#,
        quick(),
    );
    assert!(h.fault().contains("activation failed"));
}

#[test]
fn registration_after_activation_is_refused() {
    let mut h = Harness::start(
        r#"let saved; export default { activate(ctx) {
            saved = ctx;
            ctx.commands.register("late", () => saved.commands.register("x", () => {}));
        } };"#,
        quick(),
    );
    assert!(matches!(h.next(), RunnerMessage::Activated { .. }));
    h.send(&HostMessage::Event {
        scope: 1,
        event: Event::Command { id: "late".into() },
    });
    let RunnerMessage::Done { error: Some(error), .. } = h.next() else {
        panic!("late registration was accepted")
    };
    assert!(error.contains("only allowed during activate"), "{error}");
}

#[test]
fn bundles_cannot_import_at_run_time() {
    let h = Harness::start(r#"import fs from "fs"; export default { activate() {} };"#, quick());
    let fault = h.fault();
    assert!(fault.contains("fs"), "{fault}");
}

#[test]
fn a_waiting_activation_times_out() {
    let h = Harness::start(
        r#"export default { activate: () => new Promise(() => {}) };"#,
        quick(),
    );
    assert!(h.fault().contains("did not finish in time"));
}
