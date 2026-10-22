//! The newest Neural Amp Modeler package in the real runner, driven by a small fake host. A
//! synthetic LSTM `.nam` file goes in as an asset; the graph the extension publishes must do
//! what NeuralAmpModelerCore's `nam::lstm::LSTM` does (ported below with exact tanh and
//! sigmoid), within the error of qcdsp's approximations. Skips when the package is not built.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;

use serde_json::{Value, json};
use tetrad_extensions::package::Package;
use tetrad_extensions::protocol::{
    self, AssetText, BlockPublish, BuildOutcome, CallResult, Event, ExtensionInfo, HostMessage,
    PROTOCOL_VERSION, PUBLISH_BLOCK, RunnerMessage,
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
            name.starts_with("tetrad.nam-") && name.ends_with(".tetradext")
        })
        .map(|p| Package::decode(&std::fs::read(p).unwrap()).unwrap())
        .max_by(|a, b| a.manifest.version.cmp(&b.manifest.version))
}

struct Host {
    to_runner: std::io::PipeWriter,
    from_runner: BufReader<std::io::PipeReader>,
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

    /// Builds one asset and returns what the provider published.
    fn build(&mut self, scope: u64, name: &str, text: String) -> BuildOutcome {
        let event = Event::BuildBlock {
            block: "amp".into(),
            variant: name.trim_end_matches(".nam").into(),
            asset: AssetText {
                name: name.into(),
                text,
            },
        };
        self.send(&HostMessage::Event { scope, event });
        let mut published = None;
        loop {
            match self.recv() {
                RunnerMessage::Request {
                    id, method, params, ..
                } => {
                    assert_eq!(method, PUBLISH_BLOCK);
                    let publish: BlockPublish = serde_json::from_value(params).unwrap();
                    assert_eq!(publish.block, "amp");
                    published = Some(publish.outcome);
                    let result = CallResult::Ok(Value::Null);
                    self.send(&HostMessage::Response { id, result });
                }
                RunnerMessage::Done { scope: done, error } => {
                    assert_eq!((done, error), (scope, None));
                    return published.expect("the provider published");
                }
                other => panic!("unexpected {other:?}"),
            }
        }
    }
}

/// `nam::lstm::LSTM` for one input and one output channel, f32, exact activations.
struct Reference {
    hidden: usize,
    layers: Vec<Layer>,
    head: Vec<f32>,
    bias: f32,
}

struct Layer {
    w: Vec<f32>,
    b: Vec<f32>,
    h: Vec<f32>,
    c: Vec<f32>,
}

impl Reference {
    fn new(hidden: usize, layers: usize, weights: &[f32]) -> Self {
        let mut at = weights.iter().copied();
        let mut take = |n| (&mut at).take(n).collect::<Vec<f32>>();
        let layers = (0..layers)
            .map(|layer| {
                let input = if layer == 0 { 1 } else { hidden };
                Layer {
                    w: take(4 * hidden * (input + hidden)),
                    b: take(4 * hidden),
                    h: take(hidden),
                    c: take(hidden),
                }
            })
            .collect();
        let head = take(hidden);
        let bias = take(1)[0];
        Self {
            hidden,
            layers,
            head,
            bias,
        }
    }

    fn process(&mut self, x: f32) -> f32 {
        let sigmoid = |v: f32| 1.0 / (1.0 + (-v).exp());
        let h = self.hidden;
        let mut input = vec![x];
        for layer in &mut self.layers {
            let xh: Vec<f32> = input.iter().chain(&layer.h).copied().collect();
            let ifgo: Vec<f32> = (0..4 * h)
                .map(|row| {
                    let w = &layer.w[row * xh.len()..(row + 1) * xh.len()];
                    layer.b[row] + w.iter().zip(&xh).map(|(w, x)| w * x).sum::<f32>()
                })
                .collect();
            for k in 0..h {
                layer.c[k] =
                    sigmoid(ifgo[h + k]) * layer.c[k] + sigmoid(ifgo[k]) * ifgo[2 * h + k].tanh();
            }
            for k in 0..h {
                layer.h[k] = sigmoid(ifgo[3 * h + k]) * layer.c[k].tanh();
            }
            input = layer.h.clone();
        }
        self.bias
            + self
                .head
                .iter()
                .zip(&input)
                .map(|(w, x)| w * x)
                .sum::<f32>()
    }
}

/// Deterministic weights in about -0.5..0.5, like a trained small LSTM.
fn weights(count: usize) -> Vec<f32> {
    let mut seed = 0x1234_5678_u32;
    (0..count)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        })
        .collect()
}

fn nam(architecture: &str, hidden: usize, layers: usize, weights: &[f32]) -> String {
    json!({
        "version": "0.5.4",
        "architecture": architecture,
        "config": {"input_size": 1, "hidden_size": hidden, "num_layers": layers},
        "weights": weights,
        "sample_rate": 48000,
        "metadata": {"name": "test"}
    })
    .to_string()
}

#[test]
fn builds_lstm_models_that_match_nam_core() {
    let Some(package) = package() else {
        eprintln!("skipped: build extensions/repository/packages/tetrad.nam-*.tetradext first");
        return;
    };
    let (input, to_runner) = std::io::pipe().unwrap();
    let (from_runner, mut output) = std::io::pipe().unwrap();
    let module = package.module.clone().expect("NAM has a module");
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
    };
    host.send(&HostMessage::Activate {
        protocol: PROTOCOL_VERSION,
        extension: ExtensionInfo {
            id: package.manifest.id.clone(),
            version: package.manifest.version.to_string(),
        },
        config: package.manifest.default_config(),
    });
    let RunnerMessage::Activated { registrations } = host.recv() else {
        panic!("not activated")
    };
    assert_eq!(registrations.blocks, ["amp"]);

    // Two layers of 6: both the layer input and the hidden recurrence are exercised.
    let (hidden, layers) = (6, 2);
    let count = (4 * hidden * (1 + hidden) + 6 * hidden)
        + (4 * hidden * (2 * hidden) + 6 * hidden)
        + hidden
        + 1;
    let w = weights(count);
    let BuildOutcome::Ir(ir) = host.build(1, "Test Amp.nam", nam("LSTM", hidden, layers, &w))
    else {
        panic!("the model did not build")
    };
    let block: qcdsp::Block = serde_json::from_value(ir).unwrap();
    block.validate().unwrap();
    assert_eq!(block.channels(), qcdsp::Channels::Mono);
    let names: Vec<_> = block.parameters().iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, ["Input", "Output"]);

    let mut reference = Reference::new(hidden, layers, &w);
    let mut graph = qcdsp::Processor::new(block.clone()).unwrap();
    let mut lowered = qcdsp::Processor::compiled(block.clone()).unwrap();
    let mut worst = 0.0_f32;
    for n in 0..4800 {
        // A guitar-like test signal: a decaying 110 Hz tone with an overtone.
        let t = n as f32 / 48_000.0;
        let x = 0.8
            * (-3.0 * t).exp()
            * ((110.0 * std::f32::consts::TAU * t).sin()
                + 0.3 * (330.0 * std::f32::consts::TAU * t).sin());
        let expected = reference.process(x);
        let actual = graph.process_sample(x);
        worst = worst.max((actual - expected).abs());
        // The device kernel computes the same steps as the graph.
        let device = lowered.process_sample(x);
        assert!(
            (device - actual).abs() <= 2e-6 * actual.abs().max(1.0),
            "sample {n}"
        );
    }
    assert!(worst <= 2e-3, "largest error against NAM core: {worst}");

    // It also fits a DSP block: mono, within the 32 KB state, with a cycle estimate.
    let (layout, generated) = qcdsp::device::generate(&block).unwrap();
    assert_eq!(layout.banks.len(), 1);
    let cpu = generated.cycles_per_call as f64 / 300_000.0 * 100.0 * 2.0;
    eprintln!(
        "LSTM {layers}x{hidden}: {} nodes, {} state bytes, {} code parcels, {cpu:.1}% DSP (x2)",
        block.expanded().node_count(),
        layout.bytes(),
        generated.parcels.len()
    );

    // Models it cannot play give a reason the user can act on.
    let refused = |outcome| match outcome {
        BuildOutcome::Error(error) => error,
        BuildOutcome::Ir(_) => panic!("built a model it cannot play"),
    };
    let wavenet = refused(host.build(2, "Big.nam", nam("WaveNet", 1, 1, &[0.0])));
    assert!(wavenet.contains("LSTM models only"), "{wavenet}");
    let short = refused(host.build(3, "Short.nam", nam("LSTM", hidden, layers, &w[1..])));
    assert!(short.contains("weights"), "{short}");

    host.send(&HostMessage::Stop);
    assert!(thread.join().unwrap().is_ok());
}
