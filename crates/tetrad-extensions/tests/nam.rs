//! The newest Neural Amp Modeler package in the real runner, driven by a small fake host.
//! Synthetic LSTM and WaveNet `.nam` files go in as assets; the graph the extension publishes
//! must do what NeuralAmpModelerCore's `nam::lstm::LSTM` and `nam::wavenet::WaveNet` do (ported
//! below with exact activations), within the error of qcdsp's approximations. Skips when the
//! package is not built.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::thread::JoinHandle;

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
    thread: JoinHandle<Result<(), runner::RunnerError>>,
}

impl Host {
    /// The newest package, activated in the runner; `None` when it is not built.
    fn start() -> Option<Self> {
        let Some(package) = package() else {
            eprintln!("skipped: build extensions/repository/packages/tetrad.nam-*.tetradext first");
            return None;
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
            thread,
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
        Some(host)
    }

    fn stop(mut self) {
        self.send(&HostMessage::Stop);
        assert!(self.thread.join().unwrap().is_ok());
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

    /// Builds one asset that must build, and checks the block's shape.
    fn block(&mut self, scope: u64, name: &str, text: String) -> qcdsp::Block {
        let BuildOutcome::Ir(ir) = self.build(scope, name, text) else {
            panic!("{name} did not build")
        };
        let block: qcdsp::Block = serde_json::from_value(ir).unwrap();
        block.validate().unwrap();
        assert_eq!(block.channels(), qcdsp::Channels::Mono);
        let names: Vec<_> = block.parameters().iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["Input", "Output"]);
        block
    }

    /// Builds one asset that must be refused, and returns the reason.
    fn refused(&mut self, scope: u64, name: &str, text: String) -> String {
        match self.build(scope, name, text) {
            BuildOutcome::Error(error) => error,
            BuildOutcome::Ir(_) => panic!("built {name}, which it cannot play"),
        }
    }
}

/// Runs `block` on a guitar-like test signal next to `reference` and returns the largest
/// difference and the largest reference output. The device kernel must compute the same steps
/// as the graph.
fn compare(
    block: &qcdsp::Block,
    samples: usize,
    mut reference: impl FnMut(f32) -> f32,
) -> (f32, f32) {
    let mut graph = qcdsp::Processor::new(block.clone()).unwrap();
    let mut lowered = qcdsp::Processor::compiled(block.clone()).unwrap();
    let (mut worst, mut peak) = (0.0_f32, 0.0_f32);
    for n in 0..samples {
        // A decaying 110 Hz tone with an overtone.
        let t = n as f32 / 48_000.0;
        let x = 0.8
            * (-3.0 * t).exp()
            * ((110.0 * std::f32::consts::TAU * t).sin()
                + 0.3 * (330.0 * std::f32::consts::TAU * t).sin());
        let expected = reference(x);
        let actual = graph.process_sample(x);
        worst = worst.max((actual - expected).abs());
        peak = peak.max(expected.abs());
        let device = lowered.process_sample(x);
        assert!(
            (device - actual).abs() <= 2e-6 * actual.abs().max(1.0),
            "sample {n}"
        );
    }
    (worst, peak)
}

/// Prints what the block costs on the DSP: it must be mono, within the 32 KB state.
fn report(what: &str, block: &qcdsp::Block) {
    let (layout, generated) = qcdsp::device::generate(block).unwrap();
    assert_eq!(layout.banks.len(), 1);
    assert!(layout.bytes() <= 0x7fff);
    let cpu = generated.cycles_per_call as f64 / 300_000.0 * 100.0 * 2.0;
    eprintln!(
        "{what}: {} nodes after expansion, {} state bytes, {} code parcels, {cpu:.1}% DSP (x2)",
        block.expanded().node_count(),
        layout.bytes(),
        generated.parcels.len()
    );
}

fn sigmoid(v: f32) -> f32 {
    1.0 / (1.0 + (-v).exp())
}

/// `nam::lstm::LSTM` for one input and one output channel, f32, exact activations.
struct Lstm {
    hidden: usize,
    layers: Vec<LstmLayer>,
    head: Vec<f32>,
    bias: f32,
}

struct LstmLayer {
    w: Vec<f32>,
    b: Vec<f32>,
    h: Vec<f32>,
    c: Vec<f32>,
}

impl Lstm {
    fn new(hidden: usize, layers: usize, weights: &[f32]) -> Self {
        let mut at = weights.iter().copied();
        let mut take = |n| (&mut at).take(n).collect::<Vec<f32>>();
        let layers = (0..layers)
            .map(|layer| {
                let input = if layer == 0 { 1 } else { hidden };
                LstmLayer {
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

/// One layer array of a WaveNet config, as the NAM trainer writes it.
#[derive(Clone)]
struct ArrayConfig {
    input_size: usize,
    channels: usize,
    head_size: usize,
    kernel: usize,
    dilations: Vec<usize>,
    activation: &'static str,
    gated: bool,
    head_bias: bool,
}

impl ArrayConfig {
    /// A trainer-shaped array: kernel 3, Tanh, no gating, dilations 1..512 doubling.
    fn trainer(input_size: usize, channels: usize, head_size: usize, head_bias: bool) -> Self {
        Self {
            input_size,
            channels,
            head_size,
            kernel: 3,
            dilations: (0..10).map(|i| 1 << i).collect(),
            activation: "Tanh",
            gated: false,
            head_bias,
        }
    }

    fn json(&self) -> Value {
        json!({
            "input_size": self.input_size,
            "condition_size": 1,
            "head_size": self.head_size,
            "channels": self.channels,
            "kernel_size": self.kernel,
            "dilations": self.dilations,
            "activation": self.activation,
            "gated": self.gated,
            "head_bias": self.head_bias,
        })
    }

    /// Rows of the dilated conv and the input mixin: twice the channels when gated.
    fn z(&self) -> usize {
        if self.gated {
            2 * self.channels
        } else {
            self.channels
        }
    }

    fn weight_count(&self) -> usize {
        let (c, z) = (self.channels, self.z());
        c * self.input_size
            + self.dilations.len() * (z * c * self.kernel + z + z + c * c + c)
            + self.head_size * c
            + if self.head_bias { self.head_size } else { 0 }
    }
}

fn weight_count(arrays: &[ArrayConfig]) -> usize {
    arrays.iter().map(ArrayConfig::weight_count).sum::<usize>() + 1
}

/// `nam::wavenet::WaveNet` for one input and one output channel, f32, exact activations, from
/// zero buffers. Weight order and steps follow NeuralAmpModelerCore v0.3.0: `NAM/wavenet.cpp`
/// (`_Layer::set_weights_` 25-30, `_Layer::process_` 32-71, `_LayerArray::set_weights_`
/// 173-179, `WaveNet::set_weights_` 310-329, `WaveNet::process` 368-400), `NAM/wavenet.h`
/// (`_Layer` 26-33: conv with bias, mixin without, 1x1 with) and `NAM/dsp.cpp`
/// (`Conv1D::set_weights_` 207-221, `Conv1D::process_` 244-260, `Conv1x1::set_weights_`
/// 288-296). The current `NAM/wavenet/model.cpp` keeps these semantics for these configs.
struct WaveNet {
    arrays: Vec<Array>,
    head_scale: f32,
}

struct Array {
    config: ArrayConfig,
    rechannel: Vec<f32>,
    layers: Vec<WaveNetLayer>,
    head: Vec<f32>,
    head_bias: Vec<f32>,
}

struct WaveNetLayer {
    dilation: usize,
    /// `conv[(out * channels + in) * kernel + k]` is NAM's `_weight[k](out, in)`.
    conv: Vec<f32>,
    conv_bias: Vec<f32>,
    mixin: Vec<f32>,
    layer1x1: Vec<f32>,
    layer1x1_bias: Vec<f32>,
    /// The layer inputs, newest first; `history[n]` is the input from `n` samples ago.
    history: VecDeque<Vec<f32>>,
}

/// `weights · values + bias`, with `weights` row-major.
fn matvec(weights: &[f32], values: &[f32], bias: &[f32]) -> Vec<f32> {
    weights
        .chunks(values.len())
        .enumerate()
        .map(|(row, w)| {
            bias.get(row).copied().unwrap_or(0.0)
                + w.iter().zip(values).map(|(w, v)| w * v).sum::<f32>()
        })
        .collect()
}

impl WaveNet {
    fn new(arrays: &[ArrayConfig], weights: &[f32]) -> Self {
        assert_eq!(weights.len(), weight_count(arrays));
        let mut at = weights.iter().copied();
        let mut take = |n| (&mut at).take(n).collect::<Vec<f32>>();
        let arrays = arrays
            .iter()
            .map(|config| {
                let (c, z, k) = (config.channels, config.z(), config.kernel);
                let rechannel = take(c * config.input_size);
                let layers = config
                    .dilations
                    .iter()
                    .map(|&dilation| WaveNetLayer {
                        dilation,
                        conv: take(z * c * k),
                        conv_bias: take(z),
                        mixin: take(z),
                        layer1x1: take(c * c),
                        layer1x1_bias: take(c),
                        history: vec![vec![0.0; c]; (k - 1) * dilation + 1].into(),
                    })
                    .collect();
                let head = take(config.head_size * c);
                let head_bias = take(if config.head_bias {
                    config.head_size
                } else {
                    0
                });
                Array {
                    config: config.clone(),
                    rechannel,
                    layers,
                    head,
                    head_bias,
                }
            })
            .collect();
        // NAM reads head_scale from the last weight, not from the config.
        let head_scale = take(1)[0];
        Self { arrays, head_scale }
    }

    fn process(&mut self, x: f32) -> f32 {
        // The condition is the input sample; it is also the first array's input.
        let mut input = vec![x];
        let mut head: Option<Vec<f32>> = None;
        for array in &mut self.arrays {
            let config = &array.config;
            let (c, k) = (config.channels, config.kernel);
            let mut v = matvec(&array.rechannel, &input, &[]);
            let mut sum = head.take().unwrap_or_else(|| vec![0.0; c]);
            for layer in &mut array.layers {
                layer.history.pop_back();
                layer.history.push_front(v.clone());
                // Conv1D::process_: `_weight[k]` reads the input `dilation * (K - 1 - k)` ago.
                let z: Vec<f32> = (0..config.z())
                    .map(|o| {
                        let mut value = layer.conv_bias[o] + layer.mixin[o] * x;
                        for j in 0..c {
                            for tap in 0..k {
                                let past = &layer.history[(k - 1 - tap) * layer.dilation];
                                value += layer.conv[(o * c + j) * k + tap] * past[j];
                            }
                        }
                        value
                    })
                    .collect();
                let activate = |v: f32| match config.activation {
                    "Tanh" | "Fasttanh" => v.tanh(),
                    "Hardtanh" => v.clamp(-1.0, 1.0),
                    "ReLU" => v.max(0.0),
                    other => panic!("no {other} in the port"),
                };
                let a: Vec<f32> = (0..c)
                    .map(|o| {
                        let value = activate(z[o]);
                        if config.gated {
                            value * sigmoid(z[c + o])
                        } else {
                            value
                        }
                    })
                    .collect();
                for (s, a) in sum.iter_mut().zip(&a) {
                    *s += a;
                }
                let mixed = matvec(&layer.layer1x1, &a, &layer.layer1x1_bias);
                for (v, m) in v.iter_mut().zip(mixed) {
                    *v += m;
                }
            }
            head = Some(matvec(&array.head, &sum, &array.head_bias));
            input = v;
        }
        self.head_scale * head.unwrap()[0]
    }
}

/// Deterministic weights in about -0.5..0.5, like a trained small model.
fn weights(count: usize) -> Vec<f32> {
    let mut seed = 0x1234_5678_u32;
    (0..count)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5
        })
        .collect()
}

fn nam(architecture: &str, config: Value, weights: &[f32]) -> String {
    json!({
        "version": "0.5.4",
        "architecture": architecture,
        "config": config,
        "weights": weights,
        "sample_rate": 48000,
        "metadata": {"name": "test"}
    })
    .to_string()
}

fn lstm_config(hidden: usize, layers: usize) -> Value {
    json!({"input_size": 1, "hidden_size": hidden, "num_layers": layers})
}

fn wavenet_config(arrays: &[ArrayConfig]) -> Value {
    let layers: Vec<Value> = arrays.iter().map(ArrayConfig::json).collect();
    json!({"layers": layers, "head": null, "head_scale": 0.02})
}

/// WaveNet weights with NAM's head_scale of 0.02 as the last weight.
fn wavenet_weights(arrays: &[ArrayConfig]) -> Vec<f32> {
    let mut w = weights(weight_count(arrays));
    *w.last_mut().unwrap() = 0.02;
    w
}

/// `weights` times `factor`, except head_scale (the last weight).
fn scaled(weights: &[f32], factor: f32) -> Vec<f32> {
    let (head_scale, rest) = weights.split_last().unwrap();
    rest.iter()
        .map(|w| factor * w)
        .chain([*head_scale])
        .collect()
}

#[test]
fn builds_lstm_models_that_match_nam_core() {
    let Some(mut host) = Host::start() else {
        return;
    };
    // Two layers of 6: both the layer input and the hidden recurrence are exercised.
    let (hidden, layers) = (6, 2);
    let count = (4 * hidden * (1 + hidden) + 6 * hidden)
        + (4 * hidden * (2 * hidden) + 6 * hidden)
        + hidden
        + 1;
    let w = weights(count);
    let block = host.block(
        1,
        "Test Amp.nam",
        nam("LSTM", lstm_config(hidden, layers), &w),
    );
    let mut reference = Lstm::new(hidden, layers, &w);
    let (worst, _) = compare(&block, 4800, |x| reference.process(x));
    assert!(worst <= 2e-3, "largest error against NAM core: {worst}");
    report(&format!("LSTM {layers}x{hidden}"), &block);

    // Models it cannot play give a reason the user can act on.
    let convnet = host.refused(2, "Big.nam", nam("ConvNet", lstm_config(1, 1), &[0.0]));
    assert!(
        convnet.contains("LSTM and WaveNet models only"),
        "{convnet}"
    );
    let short = host.refused(
        3,
        "Short.nam",
        nam("LSTM", lstm_config(hidden, layers), &w[1..]),
    );
    assert!(short.contains("weights"), "{short}");
    host.stop();
}

#[test]
fn builds_wavenet_models_that_match_nam_core() {
    let Some(mut host) = Host::start() else {
        return;
    };
    // Two arrays, as the trainer makes them: the second reads the first's output and
    // continues its head sum. Kernel 3 with dilations 1, 2 and 4 exercises every tap.
    let small = |input_size, channels, head_size, head_bias| ArrayConfig {
        kernel: 3,
        dilations: vec![1, 2, 4],
        ..ArrayConfig::trainer(input_size, channels, head_size, head_bias)
    };
    let arrays = [small(1, 3, 2, false), small(3, 2, 1, true)];
    let w = scaled(&wavenet_weights(&arrays), 3.0);
    let config = wavenet_config(&arrays);
    let block = host.block(1, "Small.nam", nam("WaveNet", config, &w));
    let mut reference = WaveNet::new(&arrays, &w);
    // Weights in -1.5..1.5 drive the activations into saturation. qcdsp's tanh is within 1e-4
    // of tanh, an error of 1e-4 of its range; the 15 activations add their errors through the
    // residual and head sums, so the output error stays near 1e-4 of the output peak (head
    // scale 0.02 makes the output small). The bound is 2e-3 of the peak: twenty times that,
    // and far below what a wrong weight or tap gives (a reversed kernel gives 0.4 of the peak).
    let (worst, peak) = compare(&block, 4800, |x| reference.process(x));
    eprintln!("WaveNet 3/2 x [1, 2, 4]: largest error against NAM core {worst:e}, peak {peak}");
    assert!(
        worst <= 2e-3 * peak,
        "largest error against NAM core: {worst} (peak {peak})"
    );
    report("WaveNet 3/2 x [1, 2, 4]", &block);

    // Gated layers, Hardtanh and ReLU, kernel 2 and the newer file keys: a nested head, a
    // gating mode, a layer1x1 object and an activation object. Only the sigmoid gate is
    // approximated here.
    let arrays = [
        ArrayConfig {
            input_size: 1,
            channels: 2,
            head_size: 2,
            kernel: 2,
            dilations: vec![1, 3],
            activation: "Hardtanh",
            gated: true,
            head_bias: false,
        },
        ArrayConfig {
            input_size: 2,
            channels: 2,
            head_size: 1,
            kernel: 3,
            dilations: vec![2, 1],
            activation: "ReLU",
            gated: false,
            head_bias: true,
        },
    ];
    let w = scaled(&wavenet_weights(&arrays), 3.0);
    let mut config = wavenet_config(&arrays);
    let first = &mut config["layers"][0];
    let first = first.as_object_mut().unwrap();
    first.remove("head_size");
    first.remove("head_bias");
    first.remove("gated");
    first.insert(
        "head".into(),
        json!({"out_channels": 2, "kernel_size": 1, "bias": false}),
    );
    first.insert("gating_mode".into(), json!("gated"));
    first.insert("layer1x1".into(), json!({"active": true, "groups": 1}));
    config["layers"][1]["activation"] = json!({"type": "ReLU"});
    let block = host.block(2, "Gated.nam", nam("WaveNet", config, &w));
    let mut reference = WaveNet::new(&arrays, &w);
    let (worst, peak) = compare(&block, 4800, |x| reference.process(x));
    eprintln!("WaveNet gated 2/2: largest error against NAM core {worst:e}, peak {peak}");
    assert!(
        worst <= 2e-3 * peak,
        "largest error against NAM core: {worst} (peak {peak})"
    );
    host.stop();
}

#[test]
fn refuses_wavenet_models_it_cannot_play() {
    let Some(mut host) = Host::start() else {
        return;
    };
    // The trainer's sizes, with nonzero weights so that no product is skipped. A size that
    // fits is built and measured; one that does not is refused with the estimate.
    let lite_first = ArrayConfig {
        dilations: (0..7).map(|i| 1 << i).collect(),
        ..ArrayConfig::trainer(1, 12, 6, false)
    };
    let lite_second = ArrayConfig {
        dilations: [128, 256, 512]
            .into_iter()
            .chain((0..10).map(|i| 1 << i))
            .collect(),
        ..ArrayConfig::trainer(12, 6, 1, true)
    };
    let sizes = [
        (
            "nano",
            [
                ArrayConfig::trainer(1, 4, 2, false),
                ArrayConfig::trainer(4, 2, 1, true),
            ],
        ),
        (
            "feather",
            [
                ArrayConfig::trainer(1, 8, 4, false),
                ArrayConfig::trainer(8, 4, 1, true),
            ],
        ),
        ("lite", [lite_first, lite_second]),
        (
            "standard",
            [
                ArrayConfig::trainer(1, 16, 8, false),
                ArrayConfig::trainer(16, 8, 1, true),
            ],
        ),
    ];
    for (scope, (size, arrays)) in (1..).zip(&sizes) {
        let text = nam("WaveNet", wavenet_config(arrays), &wavenet_weights(arrays));
        match host.build(scope, &format!("{size}.nam"), text) {
            BuildOutcome::Ir(ir) => {
                report(
                    &format!("WaveNet {size}"),
                    &serde_json::from_value(ir).unwrap(),
                );
            }
            BuildOutcome::Error(error) => {
                eprintln!("WaveNet {size}: {error}");
                assert!(
                    error.contains(&format!("(WaveNet {size}) needs")),
                    "{error}"
                );
            }
        }
    }
    // The standard size is far over both limits.
    let standard = &sizes[3].1;
    let error = host.refused(
        9,
        "Standard.nam",
        nam(
            "WaveNet",
            wavenet_config(standard),
            &wavenet_weights(standard),
        ),
    );
    assert!(
        error.contains("this version plays models up to 24576 nodes"),
        "{error}"
    );

    // Configs that change the computation in ways this version does not model.
    let arrays = [ArrayConfig {
        dilations: vec![1, 2],
        ..ArrayConfig::trainer(1, 2, 1, true)
    }];
    let w = wavenet_weights(&arrays);
    let refusals = [
        ("conv_pre_film", json!({"active": true}), "FiLM"),
        ("groups_input", json!(2), "grouped convolutions"),
        ("gating_mode", json!("blended"), "blended gating mode"),
        ("activation", json!("SiLU"), "SiLU activation"),
        ("kernel_sizes", json!([3, 3]), "\"kernel_sizes\""),
    ];
    for (scope, (key, value, reason)) in (10..).zip(refusals) {
        let mut config = wavenet_config(&arrays);
        config["layers"][0][key] = value;
        let error = host.refused(scope, "Odd.nam", nam("WaveNet", config, &w));
        assert!(error.contains(reason), "{key}: {error}");
        assert!(
            error.contains("this version does not play"),
            "{key}: {error}"
        );
    }
    let mut config = wavenet_config(&arrays);
    config["head"] = json!({"channels": 2, "out_channels": 1, "kernel_sizes": [1]});
    let error = host.refused(20, "Head.nam", nam("WaveNet", config, &w));
    assert!(error.contains("post-stack head"), "{error}");
    let error = host.refused(
        21,
        "Short.nam",
        nam("WaveNet", wavenet_config(&arrays), &w[1..]),
    );
    assert!(error.contains("weights; its config needs"), "{error}");
    host.stop();
}
