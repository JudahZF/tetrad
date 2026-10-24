// Reads `.nam` files and builds LSTM graphs. NeuralAmpModelerCore (MIT, © Steven Atkinson),
// `NAM/lstm.cpp`, defines the LSTM weight order and the per-sample computation. WaveNet models
// are in `wavenet.ts`.

import type { BlockBuilder, BlockIr, Signal } from "@tetrad/extensions";

/** The QC's sample rate. A model trained for another rate sounds wrong. */
const SAMPLE_RATE = 48_000;

/** A `.nam` file: its architecture, its config and its flat weight list. */
export type NamFile = {
  readonly name: string;
  readonly architecture: unknown;
  readonly config: { readonly [key: string]: unknown };
  readonly weights: readonly number[];
};

/** The parts of an LSTM model that the graph needs. */
export type LstmModel = {
  readonly inputSize: number;
  readonly hiddenSize: number;
  readonly numLayers: number;
  readonly weights: readonly number[];
};

/** Parses a `.nam` file's JSON. Throws with a reason the user can act on. */
export function parse(name: string, text: string): { readonly [key: string]: unknown } {
  let json: unknown;
  try {
    json = JSON.parse(text);
  } catch {
    throw new Error(`${name} is not a .nam file (it is not JSON)`);
  }
  return record(json, name);
}

/**
 * Reads the parts that single models share, from a parsed file or a submodel of an A2 file.
 * Throws with a reason the user can act on.
 */
export function namFile(name: string, file: { readonly [key: string]: unknown }): NamFile {
  const rate = file["sample_rate"];
  if (rate !== undefined && rate !== null && rate !== SAMPLE_RATE) {
    throw new Error(`${name} is for ${String(rate)} Hz; the QC runs at ${SAMPLE_RATE} Hz`);
  }
  return {
    name,
    architecture: file["architecture"],
    config: record(file["config"], `${name} config`),
    weights: numbers(file["weights"], `${name} weights`),
  };
}

/** Checks an LSTM file. Throws with a reason the user can act on. */
export function parseLstm({ name, config, weights }: NamFile): LstmModel {
  const channels = (key: string) => {
    const value = config[key];
    return value === undefined ? 1 : count(value, `${name} ${key}`);
  };
  if (channels("in_channels") !== 1 || channels("out_channels") !== 1) {
    throw new Error(`${name} has more than one input or output channel`);
  }
  const inputSize = config["input_size"] === undefined ? 1 : count(config["input_size"], `${name} input_size`);
  const model: LstmModel = {
    inputSize,
    hiddenSize: count(config["hidden_size"], `${name} hidden_size`),
    numLayers: count(config["num_layers"], `${name} num_layers`),
    weights,
  };
  if (model.inputSize !== 1) throw new Error(`${name} has input_size ${model.inputSize}; only 1 is supported`);
  const expected = weightCount(model);
  if (model.weights.length !== expected) {
    throw new Error(`${name} has ${model.weights.length} weights; its config needs ${expected}`);
  }
  return model;
}

/** Each layer: W (4H x (I+H)), b (4H), h0 (H), c0 (H); then the head (H) and its bias. */
export function weightCount({ inputSize, hiddenSize: h, numLayers }: LstmModel): number {
  let total = h + 1;
  for (let layer = 0; layer < numLayers; layer++) {
    const input = layer === 0 ? inputSize : h;
    total += 4 * h * (input + h) + 4 * h + 2 * h;
  }
  return total;
}

/**
 * The block graph: input gain, the LSTM layers, the linear head and output gain. Mono: the
 * model runs once and both channels get its output.
 */
export function lstm(dsp: BlockBuilder, model: LstmModel): BlockIr {
  const { hiddenSize: h, weights } = model;
  let at = 0;
  const take = (n: number) => {
    const values = weights.slice(at, at + n);
    at += n;
    return values;
  };
  dsp.mono();
  const inputGain = dsp.parameter("Input", 0, 4, 1);
  const outputGain = dsp.parameter("Output", 0, 4, 1);
  let x: Signal[] = [dsp.mul(dsp.input(), inputGain)];
  for (let layer = 0; layer < model.numLayers; layer++) {
    const columns = x.length + h;
    const w = take(4 * h * columns);
    const b = take(4 * h);
    const h0 = take(h);
    const c0 = take(h);
    const hidden = h0.map((value) => dsp.state(value));
    const cell = c0.map((value) => dsp.state(value));
    // xh = [x; h(t - 1)]: the states read the previous sample's hidden values.
    const xh = [...x, ...hidden.map((state) => state.signal)];
    const gate = (row: number) => dot(dsp, w.slice(row * columns, (row + 1) * columns), xh, b[row] ?? 0);
    const next: Signal[] = [];
    for (let k = 0; k < h; k++) {
      const input = dsp.sigmoid(gate(k));
      const forget = dsp.sigmoid(gate(h + k));
      const candidate = dsp.tanh(gate(2 * h + k));
      const output = dsp.sigmoid(gate(3 * h + k));
      const kept = dsp.mul(forget, at0(cell, k).signal);
      const added = dsp.mul(input, candidate);
      const c = dsp.add(kept, added);
      const hNew = dsp.mul(output, dsp.tanh(c));
      dsp.updateState(at0(cell, k), c);
      dsp.updateState(at0(hidden, k), hNew);
      next.push(hNew);
    }
    x = next;
  }
  const head = take(h);
  const [bias = 0] = take(1);
  const out = dsp.mul(dot(dsp, head, x, bias), outputGain);
  return dsp.finish(out);
}

/** `bias + sum(weights[j] * values[j])`, skipping zero weights. */
function dot(dsp: BlockBuilder, weights: readonly number[], values: readonly Signal[], bias: number): Signal {
  let sum = dsp.constant(bias);
  weights.forEach((weight, j) => {
    if (weight !== 0) sum = dsp.add(sum, dsp.mul(dsp.constant(weight), at0(values, j)));
  });
  return sum;
}

export function at0<T>(values: readonly T[], index: number): T {
  const value = values[index];
  if (value === undefined) throw new Error(`index ${index} is out of range`);
  return value;
}

export function record(value: unknown, what: string): { readonly [key: string]: unknown } {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error(`${what} is not an object`);
  }
  return value as { readonly [key: string]: unknown };
}

export function count(value: unknown, what: string): number {
  if (typeof value !== "number" || !Number.isInteger(value) || value < 1 || value > 64) {
    throw new Error(`${what} must be a whole number from 1 to 64`);
  }
  return value;
}

export function numbers(value: unknown, what: string): number[] {
  if (!Array.isArray(value)) throw new Error(`${what} is not a list`);
  return value.map((item, index) => {
    if (typeof item !== "number" || !Number.isFinite(item)) {
      throw new Error(`${what}[${index}] is not a finite number`);
    }
    return item;
  });
}
