// WaveNet `.nam` models. NeuralAmpModelerCore (MIT, © Steven Atkinson) defines the weight order
// and the per-sample computation: `NAM/wavenet.cpp` and `NAM/dsp.cpp` (Conv1D, Conv1x1) in
// v0.3.0, `NAM/wavenet/model.cpp` and `NAM/conv1d.cpp` on main. For each sample, with `x` the
// input sample (NAM's condition):
//
//   input = [x]
//   each layer array (C channels, kernel K, head size H):
//     v = rechannel · input                                   1x1, no bias
//     each layer (dilation d):
//       z = bias + Σ_k conv[k] · v(t - (K-1-k)·d) + mixin · x
//       a = act(z)                     gated: act(z[0..C]) · sigmoid(z[C..2C])
//       head += a                      the first array's head sum starts at zero
//       v = v + layer1x1 · a + bias    residual
//     head = head_rechannel · head (+ bias if head_bias)       the next array's sum starts here
//     input = v
//   output = head_scale · head[0]
//
// Weights, in file order: for each array, rechannel [C][in]; for each layer, conv [Z][C][K]
// and its bias [Z] (Z = 2C when gated, else C), mixin [Z][1], layer1x1 [C][C] and its bias
// [C]; then head_rechannel [H][C] and its bias [H] when head_bias. Last, head_scale: NAM reads
// it from the weights, not from the config.

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
  "dilations",
  "activation",
  "gated",
  "gating_mode",
  "secondary_activation",
  "groups_input",
  "groups_input_mixin",
  "layer1x1",
  "head1x1",
  ...FILM_KEYS,
]);

/** Fasttanh is NAM's fast approximation of tanh; the graph computes tanh for both. */
type Activation = "Tanh" | "Hardtanh" | "ReLU";

export type LayerArray = {
  readonly inputSize: number;
  readonly channels: number;
  readonly headSize: number;
  readonly headBias: boolean;
  readonly kernelSize: number;
  readonly dilations: readonly number[];
  readonly activation: Activation;
  readonly gated: boolean;
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

  // Newer trainer exports nest the head; older files have head_size and head_bias.
  let headSize: number;
  let headBias: unknown;
  const head = config["head"];
  if (head != null) {
    const nested = record(head, `${where} head`);
    if (nested["kernel_size"] !== 1 || (nested["head_dilation"] ?? 1) !== 1) {
      throw refuse("a head kernel wider than 1");
    }
    headSize = count(nested["out_channels"], `${where} head out_channels`);
    headBias = nested["bias"];
  } else {
    headSize = count(config["head_size"], `${where} head_size`);
    headBias = config["head_bias"];
  }
  if (typeof headBias !== "boolean") throw new Error(`${where} head bias is not true or false`);

  return {
    inputSize: count(config["input_size"], `${where} input_size`),
    channels,
    headSize,
    headBias,
    kernelSize: count(config["kernel_size"], `${where} kernel_size`),
    dilations: dilations(config["dilations"], `${where} dilations`),
    activation: activation(config["activation"], where, refuse),
    gated: gated(config, where, refuse),
  };
}

function activation(value: unknown, where: string, refuse: (what: string) => Error): Activation {
  // A string, or an object with a `type`; Tanh, Hardtanh and ReLU take no parameters.
  const type = isObject(value) ? value["type"] : value;
  switch (type) {
    case "Tanh":
    case "Fasttanh":
      return "Tanh";
    case "Hardtanh":
      return "Hardtanh";
    case "ReLU":
      return "ReLU";
    default:
      if (typeof type !== "string") throw new Error(`${where} activation is not one activation name`);
      throw refuse(`the ${type} activation`);
  }
}

function gated(config: { readonly [key: string]: unknown }, where: string, refuse: (what: string) => Error): boolean {
  // Newer files have `gating_mode`; older ones the `gated` flag.
  const mode = config["gating_mode"];
  let result: boolean;
  if (mode === undefined) {
    const flag = config["gated"] ?? false;
    if (typeof flag !== "boolean") throw new Error(`${where} gated is not true or false`);
    result = flag;
  } else if (mode === "gated" || mode === "none") {
    result = mode === "gated";
  } else {
    throw refuse(typeof mode === "string" ? `the ${mode} gating mode` : "a gating mode for each layer");
  }
  const secondary = config["secondary_activation"];
  const type = isObject(secondary) ? secondary["type"] : secondary;
  if (secondary !== undefined && !(result && type === "Sigmoid")) {
    throw refuse("a secondary activation other than a Sigmoid gate");
  }
  return result;
}

function dilations(value: unknown, what: string): number[] {
  if (!Array.isArray(value) || value.length === 0) throw new Error(`${what} is not a list of layers`);
  return value.map((item: unknown) => {
    if (typeof item !== "number" || !Number.isInteger(item) || item < 1 || item > MAX_DILATION) {
      throw new Error(`${what} must be whole numbers from 1 to ${MAX_DILATION}`);
    }
    return item;
  });
}

function isObject(value: unknown): value is { readonly [key: string]: unknown } {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** The weights the arrays read, in NAM's order (see the top of this file), and head_scale. */
export function weightCount(arrays: readonly LayerArray[]): number {
  let total = 1;
  for (const { inputSize, channels: c, headSize: h, headBias, kernelSize: k, dilations, gated } of arrays) {
    const z = gated ? 2 * c : c;
    total += c * inputSize + dilations.length * (z * c * k + z + z + c * c + c) + h * c + (headBias ? h : 0);
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
    !array.gated &&
    array.kernelSize <= NETWORK_MAX_KERNEL &&
    [array.inputSize, array.channels, array.headSize].every((n) => n <= NETWORK_MAX_CHANNELS);
  // What `network` allocates: the input, the head sum, and per array its input mix and head;
  // per layer a mixed buffer, the next residual and six operations.
  const layers = model.arrays.reduce((n, array) => n + array.dilations.length, 0);
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
    const { channels: c, kernelSize: k, activation } = array;
    let v = net.buffer(c);
    net.conv(input, v, { weights: take(c * array.inputSize) });
    let sum = head;
    for (const dilation of array.dilations) {
      const z = net.buffer(c);
      net.conv(v, z, { kernel: k, dilation, weights: take(c * c * k), bias: take(c) });
      net.conv(x, z, { weights: take(c), accumulate: true });
      net.activation(z, ACTIVATIONS[activation]);
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
    const weights = take(array.headSize * c);
    net.conv(sum, out, { weights, ...(array.headBias ? { bias: take(array.headSize) } : {}) });
    head = out;
    input = v;
  }
  if (head === undefined) throw new Error("the model has no layer arrays");
  const [scale = 0] = take(1);
  return dsp.finish(dsp.mul(dsp.mul(net.output(head), dsp.constant(scale)), outputGain));
}

const ACTIVATIONS = { Tanh: "tanh", Hardtanh: "hardtanh", ReLU: "relu" } as const;

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
    const { channels: c, kernelSize: k } = array;
    const z = array.gated ? 2 * c : c;
    let v = matrix(dsp, take(c * input.length), input);
    let sum = head;
    for (const dilation of array.dilations) {
      const conv = take(z * c * k);
      const convBias = take(z);
      const mixin = take(z);
      const layer1x1 = take(c * c);
      const layerBias = take(c);
      const past = taps(dsp, v, k, dilation);
      // conv[k] reads the input (K-1-k)·d samples ago; NAM orders its weights [out][in][k].
      const pre = range(z).map((o) => {
        const terms: [number, Signal][] = [[at0(mixin, o), x]];
        for (let j = 0; j < c; j++) {
          for (let tap = 0; tap < k; tap++) {
            terms.push([at0(conv, (o * c + j) * k + tap), at0(at0(past, k - 1 - tap), j)]);
          }
        }
        return affine(dsp, terms, at0(convBias, o));
      });
      const a = range(c).map((o) => {
        const value = activate(dsp, array.activation, at0(pre, o));
        return array.gated ? dsp.mul(value, dsp.sigmoid(at0(pre, c + o))) : value;
      });
      sum = sum === undefined ? a : sum.map((s, j) => dsp.add(s, at0(a, j)));
      const mixed = matrix(dsp, layer1x1, a, layerBias);
      v = v.map((value, o) => dsp.add(value, at0(mixed, o)));
    }
    if (sum === undefined) throw new Error("a layer array has no layers");
    head = matrix(dsp, take(array.headSize * c), sum, array.headBias ? take(array.headSize) : undefined);
    input = v;
  }
  const [scale = 0] = take(1);
  const out = dsp.mul(dsp.mul(at0(head ?? [], 0), dsp.constant(scale)), outputGain);
  return dsp.finish(out);
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

function activate(dsp: BlockBuilder, activation: Activation, value: Signal): Signal {
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
