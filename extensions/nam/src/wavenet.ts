// WaveNet `.nam` models, A1 (the trainer's standard to nano sizes) and A2 (one size of an A2
// file). NeuralAmpModelerCore (MIT, © Steven Atkinson) defines the weight order and the
// per-sample computation: `NAM/wavenet.cpp` and `NAM/dsp.cpp` (Conv1D, Conv1x1) in v0.3.0,
// `NAM/wavenet/model.cpp` and `NAM/conv1d.cpp` in v0.6.0. For each sample, with `x` the input
// sample (NAM's condition):
//
//   input = [x]
//   each layer array (C channels, head size H, head kernel Kh and dilation dh):
//     v = rechannel · input                                   1x1, no bias
//     each layer (kernel K, dilation d, activation act):
//       z = bias + Σ_k conv[k] · v(t - (K-1-k)·d) + mixin · x
//       a = act(z)                     gated: act(z[0..C]) · sigmoid(z[C..2C])
//       head += a                      the first array's head sum starts at zero
//       v = v + layer1x1 · a + bias    residual
//     head = bias + Σ_k head_rechannel[k] · head(t - (Kh-1-k)·dh)   the next array's sum starts here
//     input = v
//   output = head_scale · head[0]
//
// A1 files have one kernel size and activation per array and a 1x1 head; A2 files list a kernel
// size and activation (LeakyReLU) per layer and have a convolutional head (kernel 16).
//
// Weights, in file order: for each array, rechannel [C][in]; for each layer, conv [Z][C][K]
// and its bias [Z] (Z = 2C when gated, else C), mixin [Z][1], layer1x1 [C][C] and its bias
// [C]; then head_rechannel [H][C][Kh] and its bias [H] when the head has one. Last,
// head_scale: NAM reads it from the weights, not from the config.

import type { BlockBuilder, BlockIr, Delay, NetBuffer, NetworkBuilder, Signal, State } from "@tetrad/extensions";

import { at0, count, type NamFile, record } from "./nam";

/** What Tetrad compiles at most, after macro expansion (`MAX_COMPILED_NODES` in tetrad-app). */
const MAX_COMPILED_NODES = 24_576;
/** The largest block state the DSP template allocates (qcdsp `device/template.rs`). */
const MAX_STATE_BYTES = 0x7fff;
/** Volume's own state words, before the parameters (qcdsp `device/layout.rs`). */
const STOCK_STATE_WORDS = 17;
/** Nodes of each macro after expansion (qcdsp `Expander` in `graph.rs`). */
const TANH_NODES = 36;
const SIGMOID_NODES = 40;
const RECIP_NODES = 12;
/** The NAM trainer's standard WaveNet sizes, by channels of the two layer arrays. */
const SIZES: { readonly [channels: string]: string } = {
  "16/8": "standard",
  "12/6": "lite",
  "8/4": "feather",
  "4/2": "nano",
};
const MAX_DILATION = 1 << 16;
/** NAM's LeakyReLU slope when the file names none. */
const LEAKY_RELU_SLOPE = 0.01;
/** The network builder's limits (`NetworkBuilder` in the SDK). */
const NETWORK_MAX_KERNEL = 16;
const NETWORK_MAX_CHANNELS = 64;
const NETWORK_MAX_BUFFERS = 1024;
const NETWORK_MAX_OPS = 4096;
const FILM_KEYS = [
  "conv_pre_film",
  "conv_post_film",
  "input_mixin_pre_film",
  "input_mixin_post_film",
  "activation_pre_film",
  "activation_post_film",
  "layer1x1_post_film",
  "head1x1_post_film",
];
/** The layer array keys this version reads; any other key changes the model, so it refuses. */
const KNOWN_KEYS = new Set([
  "input_size",
  "condition_size",
  "head_size",
  "head_bias",
  "head",
  "channels",
  "bottleneck",
  "kernel_size",
  "kernel_sizes",
  "dilations",
  "activation",
  "gated",
  "gating_mode",
  "secondary_activation",
  "groups_input",
  "groups_input_mixin",
  "layer1x1",
  "head1x1",
  "slimmable",
  ...FILM_KEYS,
]);

/**
 * Fasttanh is NAM's fast approximation of tanh; the graph computes tanh for both. A leaky ReLU
 * has the SDK's network form, `max(x, slope · x)`.
 */
type Activation = "Tanh" | "Hardtanh" | "ReLU" | { readonly leakyRelu: number };

export type Layer = {
  readonly kernelSize: number;
  readonly dilation: number;
  readonly activation: Activation;
  readonly gated: boolean;
};

export type LayerArray = {
  readonly inputSize: number;
  readonly channels: number;
  readonly headSize: number;
  readonly headBias: boolean;
  readonly headKernelSize: number;
  readonly headDilation: number;
  readonly layers: readonly Layer[];
};

export type WavenetModel = {
  readonly name: string;
  readonly arrays: readonly LayerArray[];
  readonly weights: readonly number[];
};

/** Checks a WaveNet file. Throws with a reason the user can act on. */
export function parseWavenet({ name, config, weights }: NamFile): WavenetModel {
  const refuse = (what: string) => new Error(`${name} uses ${what}, which this version does not play`);
  if (config["head"] != null) throw refuse("a post-stack head");
  if (config["condition_dsp"] != null) throw refuse("a condition DSP");
  if ((config["in_channels"] ?? 1) !== 1 || (config["out_channels"] ?? 1) !== 1) {
    throw new Error(`${name} has more than one input or output channel`);
  }
  const layers = config["layers"];
  if (!Array.isArray(layers) || layers.length === 0) throw new Error(`${name} has no layer arrays`);
  const arrays = layers.map((value: unknown, i) => {
    const where = `${name} layer array ${i + 1}`;
    return layerArray(record(value, where), where, refuse);
  });
  arrays.forEach((array, i) => {
    const where = `${name} layer array ${i + 1}`;
    const previous = arrays[i - 1];
    const input = previous?.channels ?? 1;
    if (array.inputSize !== input) {
      throw new Error(`${where} has input_size ${array.inputSize}; it needs ${input}`);
    }
    if (previous !== undefined && array.channels !== previous.headSize) {
      throw new Error(`${where} has ${array.channels} channels; the head before it has ${previous.headSize}`);
    }
  });
  const last = at0(arrays, arrays.length - 1);
  if (last.headSize !== 1) throw new Error(`${name} has head_size ${last.headSize}; a mono model needs 1`);
  const expected = weightCount(arrays);
  if (weights.length !== expected) {
    throw new Error(`${name} has ${weights.length} weights; its config needs ${expected}`);
  }
  return { name, arrays, weights };
}

function layerArray(
  config: { readonly [key: string]: unknown },
  where: string,
  refuse: (what: string) => Error,
): LayerArray {
  const unknown = Object.keys(config).find((key) => !KNOWN_KEYS.has(key));
  if (unknown !== undefined) throw refuse(`"${unknown}"`);
  for (const key of FILM_KEYS) {
    const film = config[key];
    if (film !== undefined && film !== false && !(isObject(film) && film["active"] === false)) {
      throw refuse(`FiLM (${key})`);
    }
  }
  if ((config["groups_input"] ?? 1) !== 1 || (config["groups_input_mixin"] ?? 1) !== 1) {
    throw refuse("grouped convolutions");
  }
  if (config["slimmable"] != null) throw refuse("slimmable layers");
  const channels = count(config["channels"], `${where} channels`);
  if ((config["bottleneck"] ?? channels) !== channels) throw refuse("a bottleneck");
  const layer1x1 = config["layer1x1"];
  const plainLayer1x1 = isObject(layer1x1) && layer1x1["active"] === true && (layer1x1["groups"] ?? 1) === 1;
  if (layer1x1 !== undefined && !plainLayer1x1) throw refuse("layer1x1 settings");
  const head1x1 = config["head1x1"];
  if (head1x1 !== undefined && !(isObject(head1x1) && head1x1["active"] === false)) throw refuse("a head 1x1");
  if (count(config["condition_size"], `${where} condition_size`) !== 1) {
    throw refuse("a condition of more than one channel");
  }

  // Newer trainer exports nest the head (A2 heads are convolutions); older files have
  // head_size and head_bias.
  let headSize: number;
  let headBias: unknown;
  let headKernelSize = 1;
  let headDilation = 1;
  const head = config["head"];
  if (head != null) {
    const nested = record(head, `${where} head`);
    headSize = count(nested["out_channels"], `${where} head out_channels`);
    headBias = nested["bias"];
    headKernelSize = count(nested["kernel_size"], `${where} head kernel_size`);
    headDilation = dilation(nested["head_dilation"] ?? 1, `${where} head_dilation`);
  } else {
    headSize = count(config["head_size"], `${where} head_size`);
    headBias = config["head_bias"];
  }
  if (typeof headBias !== "boolean") throw new Error(`${where} head bias is not true or false`);

  const dilations = config["dilations"];
  if (!Array.isArray(dilations) || dilations.length === 0)
    throw new Error(`${where} dilations is not a list of layers`);
  const n = dilations.length;
  const kernelSizes = kernels(config, n, where);
  const activations = perLayer(config["activation"], n, `${where} activation`).map((value) =>
    activation(value, where, refuse),
  );
  const gates = gating(config, n, where, refuse);
  return {
    inputSize: count(config["input_size"], `${where} input_size`),
    channels,
    headSize,
    headBias,
    headKernelSize,
    headDilation,
    layers: dilations.map((value: unknown, i) => ({
      kernelSize: at0(kernelSizes, i),
      dilation: dilation(value, `${where} dilations`),
      activation: at0(activations, i),
      gated: at0(gates, i),
    })),
  };
}

/** A setting that is one value for every layer, or a list with one value per layer. */
function perLayer(value: unknown, n: number, what: string): readonly unknown[] {
  if (!Array.isArray(value)) return Array.from({ length: n }, () => value);
  if (value.length !== n) throw new Error(`${what} has ${value.length} entries for ${n} layers`);
  return value;
}

function kernels(config: { readonly [key: string]: unknown }, n: number, where: string): number[] {
  // `kernel_size` for the whole array, or A2's `kernel_sizes` per layer; not both.
  const single = config["kernel_size"];
  const list = config["kernel_sizes"];
  if ((single === undefined) === (list === undefined)) {
    throw new Error(`${where} needs one of kernel_size and kernel_sizes`);
  }
  if (list !== undefined && !Array.isArray(list)) throw new Error(`${where} kernel_sizes is not a list`);
  return perLayer(list ?? single, n, `${where} kernel_sizes`).map((value) => count(value, `${where} kernel size`));
}

function activation(value: unknown, where: string, refuse: (what: string) => Error): Activation {
  // A string, or an object with a `type`; only LeakyReLU takes a parameter.
  const type = isObject(value) ? value["type"] : value;
  switch (type) {
    case "Tanh":
    case "Fasttanh":
      return "Tanh";
    case "Hardtanh":
      return "Hardtanh";
    case "ReLU":
      return "ReLU";
    case "LeakyReLU": {
      const slope = (isObject(value) ? value["negative_slope"] : undefined) ?? LEAKY_RELU_SLOPE;
      if (typeof slope !== "number" || !(slope >= 0 && slope <= 1)) {
        throw refuse(`a LeakyReLU slope of ${String(slope)} (it plays slopes from 0 to 1)`);
      }
      return { leakyRelu: slope };
    }
    default:
      if (typeof type !== "string") throw new Error(`${where} activation is not one activation name`);
      throw refuse(`the ${type} activation`);
  }
}

function gating(
  config: { readonly [key: string]: unknown },
  n: number,
  where: string,
  refuse: (what: string) => Error,
): boolean[] {
  // Newer files have `gating_mode`, one or one per layer; older ones the `gated` flag.
  const mode = config["gating_mode"];
  let gates: boolean[];
  if (mode === undefined) {
    const flag = config["gated"] ?? false;
    if (typeof flag !== "boolean") throw new Error(`${where} gated is not true or false`);
    gates = Array.from({ length: n }, () => flag);
  } else {
    gates = perLayer(mode, n, `${where} gating_mode`).map((value) => {
      if (value === "gated" || value === "none") return value === "gated";
      throw refuse(typeof value === "string" ? `the ${value} gating mode` : "a gating mode that is not a name");
    });
  }
  // NAM reads the secondary activation of gated layers only (a Sigmoid when there is none).
  const secondary = config["secondary_activation"];
  const secondaries = Array.isArray(secondary)
    ? perLayer(secondary, n, `${where} secondary_activation`)
    : gates.map(() => secondary);
  gates.forEach((gated, i) => {
    const value = secondaries[i];
    const type = isObject(value) ? value["type"] : value;
    if (gated && value !== undefined && type !== "Sigmoid") {
      throw refuse("a secondary activation other than a Sigmoid gate");
    }
  });
  return gates;
}

function dilation(value: unknown, what: string): number {
  if (typeof value !== "number" || !Number.isInteger(value) || value < 1 || value > MAX_DILATION) {
    throw new Error(`${what} must be whole numbers from 1 to ${MAX_DILATION}`);
  }
  return value;
}

function isObject(value: unknown): value is { readonly [key: string]: unknown } {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** The weights the arrays read, in NAM's order (see the top of this file), and head_scale. */
export function weightCount(arrays: readonly LayerArray[]): number {
  let total = 1;
  for (const { inputSize, channels: c, headSize: h, headBias, headKernelSize, layers } of arrays) {
    total += c * inputSize + h * c * headKernelSize + (headBias ? h : 0);
    for (const { kernelSize: k, gated } of layers) {
      const z = gated ? 2 * c : c;
      total += z * c * k + z + z + c * c + c;
    }
  }
  return total;
}

/**
 * The block: input gain, the layer arrays, head_scale and output gain. Mono: the model runs once
 * and both channels get its output. Ungated models within the network limits (all the NAM
 * trainer makes) become a network (IR 3), which Tetrad compiles to frame kernels; Tetrad refuses
 * a model whose CPU or memory does not fit one DSP core. Other models use the scalar graph and
 * throw, naming the model's size, when they need more nodes or state than a scalar block holds.
 */
export function wavenet(dsp: BlockBuilder, model: WavenetModel): BlockIr {
  const fits = (array: LayerArray) =>
    array.layers.every((layer) => !layer.gated && layer.kernelSize <= NETWORK_MAX_KERNEL) &&
    array.headKernelSize <= NETWORK_MAX_KERNEL &&
    [array.inputSize, array.channels, array.headSize].every((n) => n <= NETWORK_MAX_CHANNELS);
  // What `network` allocates: the input, the head sum, and per array its input mix and head;
  // per layer a mixed buffer, the next residual and six operations.
  const layers = model.arrays.reduce((n, array) => n + array.layers.length, 0);
  const arrays = model.arrays.length;
  const buffers = 2 + 2 * arrays + 2 * layers;
  const ops = 1 + 2 * arrays + 6 * layers;
  if (model.arrays.every(fits) && buffers <= NETWORK_MAX_BUFFERS && ops <= NETWORK_MAX_OPS) {
    return network(dsp, model);
  }
  const size = new Size();
  graph(size, model);
  if (size.nodes > MAX_COMPILED_NODES || size.stateBytes > MAX_STATE_BYTES) {
    const channels = model.arrays.map((array) => array.channels).join("/");
    const kind = SIZES[channels] ?? `with ${channels} channels`;
    throw new Error(
      `${model.name} (WaveNet ${kind}) needs ${size.nodes} DSP nodes and ${size.stateBytes} bytes of state; ` +
        `this version plays models up to ${MAX_COMPILED_NODES} nodes and ${MAX_STATE_BYTES} bytes. ` +
        `Use an LSTM model or a WaveNet with fewer channels or shorter dilations.`,
    );
  }
  return graph(dsp, model);
}

/** The model as network operations, in the order of the formula at the top of this file. */
function network(dsp: BlockBuilder, model: WavenetModel): BlockIr {
  let at = 0;
  const take = (n: number) => {
    const values = model.weights.slice(at, at + n);
    at += n;
    return values;
  };
  const inputGain = dsp.parameter("Input", 0, 4, 1);
  const outputGain = dsp.parameter("Output", 0, 4, 1);
  const net = dsp.network();
  const x = net.input();
  net.gain(x, inputGain);
  let input = x;
  let head: NetBuffer | undefined;
  for (const array of model.arrays) {
    const { channels: c } = array;
    let v = net.buffer(c);
    net.conv(input, v, { weights: take(c * array.inputSize) });
    let sum = head;
    for (const { kernelSize: k, dilation, activation } of array.layers) {
      const z = net.buffer(c);
      net.conv(v, z, { kernel: k, dilation, weights: take(c * c * k), bias: take(c) });
      net.conv(x, z, { weights: take(c), accumulate: true });
      activate(net, z, activation);
      if (sum === undefined) {
        sum = net.buffer(c);
        net.copy(z, sum);
      } else {
        net.add(z, sum);
      }
      const next = net.buffer(c);
      net.conv(z, next, { weights: take(c * c), bias: take(c) });
      net.add(v, next);
      v = next;
    }
    if (sum === undefined) throw new Error("a layer array has no layers");
    const out = net.buffer(array.headSize);
    const { headKernelSize: kernel, headDilation: dilation } = array;
    const weights = take(array.headSize * c * kernel);
    net.conv(sum, out, { kernel, dilation, weights, ...(array.headBias ? { bias: take(array.headSize) } : {}) });
    head = out;
    input = v;
  }
  if (head === undefined) throw new Error("the model has no layer arrays");
  const [scale = 0] = take(1);
  return dsp.finish(dsp.mul(dsp.mul(net.output(head), dsp.constant(scale)), outputGain));
}

const ACTIVATIONS = { Tanh: "tanh", Hardtanh: "hardtanh", ReLU: "relu" } as const;

function activate(net: NetworkBuilder, buffer: NetBuffer, activation: Activation): void {
  if (typeof activation === "string") {
    net.activation(buffer, ACTIVATIONS[activation]);
    return;
  }
  try {
    net.activation(buffer, activation);
  } catch (e) {
    // Tetrad before leaky ReLU networks knows the three named activations only.
    if (e instanceof TypeError) throw new Error("this model uses LeakyReLU; update Tetrad to play it");
    throw e;
  }
}

function graph(dsp: BlockBuilder, model: WavenetModel): BlockIr {
  let at = 0;
  const take = (n: number) => {
    const values = model.weights.slice(at, at + n);
    at += n;
    return values;
  };
  dsp.mono();
  const inputGain = dsp.parameter("Input", 0, 4, 1);
  const outputGain = dsp.parameter("Output", 0, 4, 1);
  const x = dsp.mul(dsp.input(), inputGain);
  let input: readonly Signal[] = [x];
  let head: readonly Signal[] | undefined;
  for (const array of model.arrays) {
    const { channels: c } = array;
    let v = matrix(dsp, take(c * input.length), input);
    let sum = head;
    for (const layer of array.layers) {
      const z = layer.gated ? 2 * c : c;
      const conv = take(z * c * layer.kernelSize);
      const convBias = take(z);
      const mixin = take(z);
      const layer1x1 = take(c * c);
      const layerBias = take(c);
      const past = taps(dsp, v, layer.kernelSize, layer.dilation);
      const pre = convolve(dsp, conv, past, convBias, (o) => [[at0(mixin, o), x]]);
      const a = range(c).map((o) => {
        const value = activateSignal(dsp, layer.activation, at0(pre, o));
        return layer.gated ? dsp.mul(value, dsp.sigmoid(at0(pre, c + o))) : value;
      });
      sum = sum === undefined ? a : sum.map((s, j) => dsp.add(s, at0(a, j)));
      const mixed = matrix(dsp, layer1x1, a, layerBias);
      v = v.map((value, o) => dsp.add(value, at0(mixed, o)));
    }
    if (sum === undefined) throw new Error("a layer array has no layers");
    const { headSize: h, headKernelSize: k } = array;
    const weights = take(h * c * k);
    head = convolve(
      dsp,
      weights,
      taps(dsp, sum, k, array.headDilation),
      array.headBias ? take(h) : range(h).map(() => 0),
    );
    input = v;
  }
  const [scale = 0] = take(1);
  const out = dsp.mul(dsp.mul(at0(head ?? [], 0), dsp.constant(scale)), outputGain);
  return dsp.finish(out);
}

/**
 * A causal dilated convolution of `past` (from [`taps`]): one output per bias, `bias[o] +
 * Σ_j Σ_k weights[o][j][k] · past[K-1-k][j]` (NAM orders its weights [out][in][k]), plus the
 * `extra` terms of each output.
 */
function convolve(
  dsp: BlockBuilder,
  weights: readonly number[],
  past: readonly (readonly Signal[])[],
  bias: readonly number[],
  extra: (output: number) => [number, Signal][] = () => [],
): Signal[] {
  const k = past.length;
  const inputs = at0(past, 0).length;
  return bias.map((b, o) => {
    const terms = extra(o);
    for (let j = 0; j < inputs; j++) {
      for (let tap = 0; tap < k; tap++) {
        terms.push([at0(weights, (o * inputs + j) * k + tap), at0(at0(past, k - 1 - tap), j)]);
      }
    }
    return affine(dsp, terms, b);
  });
}

/**
 * `past[m][j]` is `values[j]` from `m · dilation` samples ago, m = 0..kernel-1. Each step is a
 * delay line of `dilation` samples fed by the step before it, so a channel stores
 * (kernel - 1) · dilation samples: the least a dilated convolution needs.
 */
function taps(dsp: BlockBuilder, values: readonly Signal[], kernel: number, dilation: number): Signal[][] {
  const past = [[...values]];
  for (let m = 1; m < kernel; m++) {
    past.push(
      at0(past, m - 1).map((value) => {
        const line = dsp.delay(dilation);
        dsp.updateDelay(line, value);
        return line.signal;
      }),
    );
  }
  return past;
}

/** `weights · values (+ bias)`: one output per row of the row-major `weights`. */
function matrix(
  dsp: BlockBuilder,
  weights: readonly number[],
  values: readonly Signal[],
  bias?: readonly number[],
): Signal[] {
  const n = values.length;
  return range(weights.length / n).map((row) =>
    affine(
      dsp,
      values.map((value, j): [number, Signal] => [at0(weights, row * n + j), value]),
      bias === undefined ? 0 : at0(bias, row),
    ),
  );
}

/** `bias + Σ weight · value`, skipping zero weights and a zero bias. */
function affine(dsp: BlockBuilder, terms: readonly (readonly [number, Signal])[], bias: number): Signal {
  let sum = bias === 0 ? undefined : dsp.constant(bias);
  for (const [weight, value] of terms) {
    if (weight === 0) continue;
    const product = dsp.mul(dsp.constant(weight), value);
    sum = sum === undefined ? product : dsp.add(sum, product);
  }
  return sum ?? dsp.constant(0);
}

function activateSignal(dsp: BlockBuilder, activation: Activation, value: Signal): Signal {
  if (typeof activation === "object") return dsp.max(value, dsp.mul(dsp.constant(activation.leakyRelu), value));
  switch (activation) {
    case "Tanh":
      return dsp.tanh(value);
    case "Hardtanh":
      return dsp.min(dsp.max(value, dsp.constant(-1)), dsp.constant(1));
    case "ReLU":
      return dsp.max(value, dsp.constant(0));
  }
}

function range(n: number): number[] {
  return Array.from({ length: n }, (_, i) => i);
}

/**
 * A builder that only measures what `graph` makes: nodes after macro expansion, and the state
 * the DSP needs for them. The state leaves out compiler spill slots and hoisted parameter math,
 * so a model near the limit can still fail to compile.
 */
class Size implements BlockBuilder {
  /** Node 0 is the input. */
  nodes = 1;
  private parameters = 0;
  private memories = 0;
  private delaySamples = 0;

  get stateBytes(): number {
    // A bank holds each state, each delay's cursor and each delay's samples.
    return 4 * (STOCK_STATE_WORDS + this.parameters + this.memories + this.delaySamples);
  }

  private push(nodes = 1): Signal {
    this.nodes += nodes;
    return this.nodes as Signal;
  }

  input(): Signal {
    return 0 as Signal;
  }
  constant(): Signal {
    return this.push();
  }
  parameter(): Signal {
    this.parameters += 1;
    return this.push();
  }
  state(): State {
    this.memories += 1;
    return { signal: this.push() };
  }
  delay(samples: number): Delay {
    this.memories += 1;
    this.delaySamples += samples;
    return { signal: this.push() };
  }
  add(): Signal {
    return this.push();
  }
  sub(): Signal {
    return this.push();
  }
  mul(): Signal {
    return this.push();
  }
  min(): Signal {
    return this.push();
  }
  max(): Signal {
    return this.push();
  }
  div(): Signal {
    return this.push(RECIP_NODES + 1);
  }
  neg(): Signal {
    return this.push();
  }
  abs(): Signal {
    return this.push();
  }
  recip(): Signal {
    return this.push(RECIP_NODES);
  }
  tanh(): Signal {
    return this.push(TANH_NODES);
  }
  sigmoid(): Signal {
    return this.push(SIGMOID_NODES);
  }
  updateState(): void {}
  updateDelay(): void {}
  mono(): void {}
  network(): NetworkBuilder {
    throw new Error("gated models use the scalar graph");
  }
  finish(): BlockIr {
    return {};
  }
}
