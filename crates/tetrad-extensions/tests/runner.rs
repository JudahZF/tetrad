//! The runner over real pipes, as the host drives it: activation, scoped calls, triggers and
//! every fault the supervisor relies on.

use std::io::{BufRead, BufReader, Write};
use std::thread::JoinHandle;
use std::time::Duration;

use serde_json::json;
use tetrad_extensions::protocol::{
    self, AssetText, BlockPublish, BuildOutcome, CallResult, ErrorCode, Event, ExtensionInfo,
    HostMessage, MidiEvent, PROTOCOL_VERSION, PUBLISH_BLOCK, RunnerMessage, TriggerSpec,
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
    let RunnerMessage::Request {
        id,
        scope,
        method,
        params,
    } = h.next()
    else {
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
    assert_eq!(
        h.next(),
        RunnerMessage::Done {
            scope: 9,
            error: None,
            result: None
        }
    );

    h.send(&HostMessage::Stop);
    assert!(h.thread.join().unwrap().is_ok());
}

/// A USB handler's return value is the reply in `Done`; nothing returned (a JSON `null`)
/// leaves `result` out, and a throw reports its error.
#[test]
fn a_usb_message_is_answered_by_the_handler_result() {
    let mut h = Harness::start(
        r#"export default { activate(ctx) {
            ctx.usb.onMessage(async (message, scope) => {
                if (message === "fail") throw new Error("refused");
                if (message === "quiet") return;
                return { echo: message, scope: scope.id };
            });
        } };"#,
        Limits::default(),
    );
    let RunnerMessage::Activated { registrations } = h.next() else {
        panic!("not activated")
    };
    assert!(registrations.usb);
    let mut ask = |scope, message| {
        h.send(&HostMessage::Event {
            scope,
            event: Event::Usb { message },
        });
        h.next()
    };
    assert_eq!(
        ask(1, json!({"cmd": "ping"})),
        RunnerMessage::Done {
            scope: 1,
            error: None,
            result: Some(json!({"echo": {"cmd": "ping"}, "scope": 1})),
        }
    );
    assert_eq!(
        ask(2, json!("quiet")),
        RunnerMessage::Done {
            scope: 2,
            error: None,
            result: None,
        }
    );
    let RunnerMessage::Done { error, result, .. } = ask(3, json!("fail")) else {
        panic!("no done")
    };
    assert!(error.unwrap().contains("refused"));
    assert_eq!(result, None);
    h.send(&HostMessage::Stop);
    assert!(h.thread.join().unwrap().is_ok());
}

#[test]
fn declared_triggers_register_and_fire_as_scoped_calls() {
    let mut h = Harness::start(
        r#"export default { activate(ctx) {
            ctx.triggers.declare({ id: "pad", label: "Pad 1" });
            ctx.commands.register("hit", (scope) => scope.triggers.fire(
              { trigger: "pad", source: "hit:1", label: "Hit 1", value: 0.5, continuous: false }));
        } };"#,
        Limits::default(),
    );
    let RunnerMessage::Activated { registrations } = h.next() else {
        panic!("not activated")
    };
    assert_eq!(
        registrations.triggers,
        [TriggerSpec {
            id: "pad".into(),
            label: "Pad 1".into()
        }]
    );

    h.send(&HostMessage::Event {
        scope: 4,
        event: Event::Command { id: "hit".into() },
    });
    let RunnerMessage::Request {
        id,
        scope,
        method,
        params,
    } = h.next()
    else {
        panic!("no request")
    };
    assert_eq!((scope, method.as_str()), (Some(4), "triggers.fire"));
    assert_eq!(
        params,
        json!({"trigger": "pad", "source": "hit:1", "label": "Hit 1", "value": 0.5, "continuous": false})
    );
    h.send(&HostMessage::Response {
        id,
        result: CallResult::Ok(json!(null)),
    });
    assert_eq!(
        h.next(),
        RunnerMessage::Done {
            scope: 4,
            error: None,
            result: None
        }
    );
}

#[test]
fn bad_trigger_declarations_fail_activation() {
    for declare in [
        r#"ctx.triggers.declare({ id: "Pad", label: "Pad" });"#,
        r#"ctx.triggers.declare({ id: "pad", label: "" });"#,
        r#"ctx.triggers.declare({ id: "pad", label: "A" }); ctx.triggers.declare({ id: "pad", label: "B" });"#,
        r#"for (let i = 0; i < 9; i++) ctx.triggers.declare({ id: `t${i}`, label: "T" });"#,
    ] {
        let h = Harness::start(
            &format!("export default {{ activate(ctx) {{ {declare} }} }};"),
            quick(),
        );
        let fault = h.fault();
        assert!(fault.contains("activation failed"), "{declare}: {fault}");
    }
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
    let RunnerMessage::Done {
        scope: 1,
        error: Some(error),
        ..
    } = h.next()
    else {
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
    let RunnerMessage::Request {
        id, scope: Some(1), ..
    } = h.next()
    else {
        panic!("first handler did not start")
    };
    h.send(&HostMessage::Response {
        id,
        result: CallResult::Ok(json!(null)),
    });
    assert_eq!(
        h.next(),
        RunnerMessage::Done {
            scope: 1,
            error: None,
            result: None
        }
    );
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
    let RunnerMessage::Done {
        error: Some(error), ..
    } = h.next()
    else {
        panic!("late registration was accepted")
    };
    assert!(error.contains("only allowed during activate"), "{error}");
}

#[test]
fn bundles_cannot_import_at_run_time() {
    let h = Harness::start(
        r#"import fs from "fs"; export default { activate() {} };"#,
        quick(),
    );
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

fn build_block(scope: u64, text: &str) -> HostMessage {
    HostMessage::Event {
        scope,
        event: Event::BuildBlock {
            block: "amp".into(),
            variant: "Hot".into(),
            asset: AssetText {
                name: "hot.json".into(),
                text: text.into(),
            },
        },
    }
}

/// Answers the `blocks.publish` call of `scope` and returns its params.
fn published(h: &mut Harness, scope: u64) -> BlockPublish {
    let RunnerMessage::Request {
        id,
        scope: call_scope,
        method,
        params,
    } = h.next()
    else {
        panic!("no publish call")
    };
    assert_eq!((call_scope, method.as_str()), (Some(scope), PUBLISH_BLOCK));
    h.send(&HostMessage::Response {
        id,
        result: CallResult::Ok(json!(null)),
    });
    assert_eq!(
        h.next(),
        RunnerMessage::Done {
            scope,
            error: None,
            result: None
        }
    );
    serde_json::from_value(params).unwrap()
}

#[test]
fn a_block_provider_builds_the_ir_qcdsp_builds() {
    let mut h = Harness::start(
        r#"export default { activate(ctx) {
            ctx.blocks.provide("amp", (asset) => {
              const b = ctx.blocks.builder();
              const x = b.input();
              const drive = b.parameter("Drive", 0, 4, JSON.parse(asset.text).drive);
              const last = b.state(0);
              const late = b.delay(2);
              const y = b.tanh(b.mul(drive, x));
              const echo = b.add(y, b.mul(b.constant(0.5), last.signal));
              const out = b.add(echo, b.mul(b.constant(0.25), late.signal));
              b.updateState(last, y);
              b.updateDelay(late, x);
              b.mono();
              return b.finish(out);
            });
        } };"#,
        Limits::default(),
    );
    let RunnerMessage::Activated { registrations } = h.next() else {
        panic!("not activated")
    };
    assert_eq!(registrations.blocks, ["amp"]);

    h.send(&build_block(5, r#"{"drive": 2}"#));
    let publish = published(&mut h, 5);
    assert_eq!(
        (publish.block.as_str(), publish.variant.as_str()),
        ("amp", "Hot")
    );
    let BuildOutcome::Ir(ir) = publish.outcome else {
        panic!("provider failed: {:?}", publish.outcome)
    };
    let block = qcdsp::Block::from_json(&ir.to_string()).unwrap();
    assert_eq!(block.version(), 2, "tanh and mono need IR 2");

    // The same graph from qcdsp's own builder, in the same order.
    let expected = (|| -> qcdsp::Result<qcdsp::Block> {
        let mut b = qcdsp::BlockBuilder::new("block", "Block", qcdsp::Category::Utility);
        let x = b.input();
        let drive = b.parameter(qcdsp::Parameter::new("Drive", 0.0, 4.0, 2.0)?)?;
        let last = b.state(0.0)?;
        let late = b.delay(2)?;
        let product = b.mul(drive, x)?;
        let y = b.tanh(product)?;
        let half = b.constant(0.5)?;
        let echo = b.mul(half, last.signal())?;
        let echo = b.add(y, echo)?;
        let quarter = b.constant(0.25)?;
        let tail = b.mul(quarter, late.signal())?;
        let out = b.add(echo, tail)?;
        b.update_state(last, y)?;
        b.update_delay(late, x)?;
        b.mono();
        b.finish(out)
    })()
    .unwrap();
    assert_eq!(
        serde_json::to_value(&block).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );

    // y = tanh(2x), then y + 0.5 y[n-1] + 0.25 x[n-2].
    let mut processor = qcdsp::Processor::new(block).unwrap();
    let input = [0.25, -0.5, 0.0];
    let y = input.map(|x: f32| (2.0 * x).tanh());
    let want = [y[0], y[1] + 0.5 * y[0], y[2] + 0.5 * y[1] + 0.25 * input[0]];
    for (x, want) in input.into_iter().zip(want) {
        let got = processor.process_sample(x);
        assert!((got - want).abs() < 2e-4, "{x}: {got} != {want}");
    }
}

#[test]
fn a_block_provider_builds_the_network_qcdsp_builds() {
    let mut h = Harness::start(
        r#"export default { activate(ctx) {
            ctx.blocks.provide("amp", () => {
              const b = ctx.blocks.builder();
              const level = b.parameter("Input", 0, 4, 1);
              const net = b.network();
              const x = net.input();
              net.gain(x, level);
              const z = net.buffer(2);
              net.conv(x, z, { kernel: 2, dilation: 3, weights: [0.5, -0.25, 1, 0.125], bias: [0.1, 0] });
              net.activation(z, "tanh");
              const y = net.buffer(1);
              net.conv(z, y, { weights: [0.75, -1.5] });
              net.add(x, y);
              return b.finish(b.mul(net.output(y), b.constant(0.5)));
            });
        } };"#,
        Limits::default(),
    );
    assert!(matches!(h.next(), RunnerMessage::Activated { .. }));
    h.send(&build_block(3, ""));
    let BuildOutcome::Ir(ir) = published(&mut h, 3).outcome else {
        panic!("provider failed")
    };
    let block = qcdsp::Block::from_json(&ir.to_string()).unwrap();
    assert_eq!(block.version(), 3, "a network needs IR 3");

    let expected = (|| -> qcdsp::Result<qcdsp::Block> {
        let mut b = qcdsp::BlockBuilder::new("block", "Block", qcdsp::Category::Utility);
        b.parameter(qcdsp::Parameter::new("Input", 0.0, 4.0, 1.0)?)?;
        let mut net = qcdsp::NetworkBuilder::new();
        let x = net.input();
        net.gain(x, 0);
        let z = net.buffer(2);
        net.conv(
            x,
            z,
            2,
            3,
            vec![0.5, -0.25, 1.0, 0.125],
            Some(vec![0.1, 0.0]),
            false,
        );
        net.activation(z, qcdsp::Activation::Tanh);
        let y = net.buffer(1);
        net.conv(z, y, 1, 1, vec![0.75, -1.5], None, false);
        net.add(x, y);
        let output = b.network(net.finish(y))?;
        let half = b.constant(0.5)?;
        let out = b.mul(output, half)?;
        b.finish(out)
    })()
    .unwrap();
    assert_eq!(
        serde_json::to_value(&block).unwrap(),
        serde_json::to_value(&expected).unwrap()
    );
}

#[test]
fn a_failing_provider_publishes_its_error() {
    let mut h = Harness::start(
        r#"export default { activate(ctx) {
            ctx.blocks.provide("amp", () => {
              const a = ctx.blocks.builder();
              return ctx.blocks.builder().finish(a.input());
            });
        } };"#,
        Limits::default(),
    );
    assert!(matches!(h.next(), RunnerMessage::Activated { .. }));
    h.send(&build_block(2, ""));
    let publish = published(&mut h, 2);
    let BuildOutcome::Error(error) = publish.outcome else {
        panic!("a foreign signal was accepted")
    };
    assert!(error.contains("another block builder"), "{error}");
}

/// A parameter whose range is empty as f32 (1 and 1 + 1e-9) would normalise to 0/0 on the
/// unit: the builder refuses it.
#[test]
fn an_empty_parameter_range_is_refused() {
    let mut h = Harness::start(
        r#"export default { activate(ctx) {
            ctx.blocks.provide("amp", () => {
              const b = ctx.blocks.builder();
              return b.finish(b.mul(b.input(), b.parameter("Gain", 1, 1 + 1e-9, 1)));
            });
        } };"#,
        Limits::default(),
    );
    assert!(matches!(h.next(), RunnerMessage::Activated { .. }));
    h.send(&build_block(2, ""));
    let BuildOutcome::Error(error) = published(&mut h, 2).outcome else {
        panic!("an empty range was accepted")
    };
    assert!(error.contains("min < max"), "{error}");
}

#[test]
fn block_builds_get_the_longer_budget() {
    let spin = "const end = Date.now() + 200; while (Date.now() < end) {}";
    let source = format!(
        r#"export default {{ activate(ctx) {{
            ctx.blocks.provide("amp", () => {{
              {spin}
              const b = ctx.blocks.builder();
              return b.finish(b.input());
            }});
            ctx.commands.register("spin", () => {{ {spin} }});
        }} }};"#
    );
    let mut h = Harness::start(&source, quick());
    assert!(matches!(h.next(), RunnerMessage::Activated { .. }));
    h.send(&build_block(1, ""));
    assert!(matches!(published(&mut h, 1).outcome, BuildOutcome::Ir(_)));
    h.send(&HostMessage::Event {
        scope: 2,
        event: Event::Command { id: "spin".into() },
    });
    assert!(h.fault().contains("slice"));
}
