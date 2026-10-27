// The controls of a NAM block: Input, Output and, with `tone`, the controls of the Neural Amp
// Modeler plugin (NeuralAmpModelerPlugin, MIT, © Steven Atkinson): Bass, Mid and Treble
// (`ToneStack.cpp`: a 150 Hz low shelf, a 425 Hz peak and an 1800 Hz high shelf) and the Output
// in dB. Shared by the LSTM and WaveNet blocks.

import type { BlockBuilder, Signal } from "@tetrad/extensions";

/** The sample rate of every NAM model (`checkRate`). */
const RATE = 48000;
export type Controls = {
  /** The Input gain, a parameter (a network takes only a parameter as gain). */
  readonly input: Signal;
  /** What follows the model: the tone stack and the Output. */
  readonly output: (y: Signal) => Signal;
};

/** The knobs: Input and Output only, or those of the plugin. */
export function controls(dsp: BlockBuilder, tone: boolean): Controls {
  if (!tone) {
    const input = dsp.parameter("Input", 0, 4, 1);
    const gain = dsp.parameter("Output", 0, 4, 1);
    return { input, output: (y) => dsp.mul(y, gain) };
  }
  // Input is a linear gain: a network cannot take a computed one. 0 to 10 is the plugin's -20 to +20 dB.
  const input = dsp.parameter("Input", 0, 10, 1);
  const bass = dsp.parameter("Bass", 0, 10, 5);
  const mid = dsp.parameter("Mid", 0, 10, 5);
  const treble = dsp.parameter("Treble", 0, 10, 5);
  const outputDb = dsp.parameter("Output", -40, 40, 0);
  return {
    input,
    output: (y) => {
      const stack = toneStack(dsp, y, bass, mid, treble);
      return dsp.mul(stack, exp(dsp, dsp.mul(outputDb, dsp.constant(Math.LN10 / 20))));
    },
  };
}

const half = (dsp: BlockBuilder): Signal => dsp.constant(0.5);

/** 0 below `x` = 0, 1 above it, with a ramp over 0.001 (comparisons are not in the graph). */
function step(dsp: BlockBuilder, x: Signal): Signal {
  return dsp.min(dsp.max(dsp.mul(x, dsp.constant(1000)), dsp.constant(0)), dsp.constant(1));
}

/**
 * `e^x` for |x| up to about 5: `e^(x/8)` from the tanh (`e^y = (1 + tanh(y/2)) / (1 - tanh(y/2))`,
 * within about 3e-4), then three squarings. A function of parameters only, so the DSP works it out
 * once per control change and not per sample.
 */
function exp(dsp: BlockBuilder, x: Signal): Signal {
  const t = dsp.tanh(dsp.mul(x, dsp.constant(1 / 16)));
  let e = dsp.div(dsp.add(dsp.constant(1), t), dsp.sub(dsp.constant(1), t));
  for (let i = 0; i < 3; i++) e = dsp.mul(e, e);
  return e;
}

/** Bass, then Mid, then Treble, with the plugin's frequencies, Q and dB per knob step. */
function toneStack(dsp: BlockBuilder, y: Signal, bass: Signal, mid: Signal, treble: Signal): Signal {
  const five = dsp.constant(5);
  const knob = (value: Signal, dbPerStep: number) => dsp.mul(dsp.sub(value, five), dsp.constant((dbPerStep * Math.LN10) / 40));
  const k = (hz: number) => {
    const w = (2 * Math.PI * hz) / RATE;
    return { cos: Math.cos(w), sin: Math.sin(w) };
  };
  const c = (value: number) => dsp.constant(value);
  const one = c(1);

  const lowFilter = k(150);
  const lowLnA = knob(bass, 4);
  const lowA = exp(dsp, lowLnA);
  const lowRoot = exp(dsp, dsp.mul(lowLnA, half(dsp)));
  const lowAlpha = lowFilter.sin / (2 * 0.707);
  const lowAp = dsp.add(lowA, one);
  const lowAm = dsp.sub(lowA, one);
  const lowR2a = dsp.mul(c(2 * lowAlpha), lowRoot);
  const lowShelf = biquad(dsp, y, {
    b0: dsp.mul(lowA, dsp.add(dsp.sub(lowAp, dsp.mul(lowAm, c(lowFilter.cos))), lowR2a)),
    b1: dsp.mul(dsp.mul(c(2), lowA), dsp.sub(lowAm, dsp.mul(lowAp, c(lowFilter.cos)))),
    b2: dsp.mul(lowA, dsp.sub(dsp.sub(lowAp, dsp.mul(lowAm, c(lowFilter.cos))), lowR2a)),
    a0: dsp.add(dsp.add(lowAp, dsp.mul(lowAm, c(lowFilter.cos))), lowR2a),
    a1: dsp.mul(c(-2), dsp.add(lowAm, dsp.mul(lowAp, c(lowFilter.cos)))),
    a2: dsp.sub(dsp.add(lowAp, dsp.mul(lowAm, c(lowFilter.cos))), lowR2a),
  });

  const midFilter = k(425);
  const midA = exp(dsp, knob(mid, 3));
  // The plugin widens a cut (Q 1.5) against a boost (Q 0.7).
  const boost = step(dsp, dsp.sub(mid, five));
  const cutAlpha = midFilter.sin / (2 * 1.5);
  const midAlpha = dsp.add(c(cutAlpha), dsp.mul(boost, c(midFilter.sin / (2 * 0.7) - cutAlpha)));
  const peaking = biquad(dsp, lowShelf, {
    b0: dsp.add(one, dsp.mul(midAlpha, midA)),
    b1: c(-2 * midFilter.cos),
    b2: dsp.sub(one, dsp.mul(midAlpha, midA)),
    a0: dsp.add(one, dsp.div(midAlpha, midA)),
    a1: c(-2 * midFilter.cos),
    a2: dsp.sub(one, dsp.div(midAlpha, midA)),
  });

  const highFilter = k(1800);
  const highLnA = knob(treble, 2);
  const highA = exp(dsp, highLnA);
  const highRoot = exp(dsp, dsp.mul(highLnA, half(dsp)));
  const highAlpha = highFilter.sin / (2 * 0.707);
  const highAp = dsp.add(highA, one);
  const highAm = dsp.sub(highA, one);
  const highR2a = dsp.mul(c(2 * highAlpha), highRoot);
  return biquad(dsp, peaking, {
    b0: dsp.mul(highA, dsp.add(dsp.add(highAp, dsp.mul(highAm, c(highFilter.cos))), highR2a)),
    b1: dsp.mul(dsp.mul(c(-2), highA), dsp.add(highAm, dsp.mul(highAp, c(highFilter.cos)))),
    b2: dsp.mul(highA, dsp.sub(dsp.add(highAp, dsp.mul(highAm, c(highFilter.cos))), highR2a)),
    a0: dsp.add(dsp.sub(highAp, dsp.mul(highAm, c(highFilter.cos))), highR2a),
    a1: dsp.mul(c(2), dsp.sub(highAm, dsp.mul(highAp, c(highFilter.cos)))),
    a2: dsp.sub(dsp.sub(highAp, dsp.mul(highAm, c(highFilter.cos))), highR2a),
  });
}

type Coefficients = { b0: Signal; b1: Signal; b2: Signal; a0: Signal; a1: Signal; a2: Signal };

/**
 * One biquad, `x + g(x)` with `g` the filter minus the input: a flat filter has all-zero numerator
 * terms and passes `x` exactly, which the 32-bit state of a plain biquad at 150 Hz does not.
 * Transposed direct form II, the coefficients divided by `a0` as the plugin's filter does.
 */
function biquad(dsp: BlockBuilder, x: Signal, { b0, b1, b2, a0, a1, a2 }: Coefficients): Signal {
  const inverse = dsp.recip(a0);
  const scaled = (value: Signal) => dsp.mul(value, inverse);
  const d0 = scaled(dsp.sub(b0, a0));
  const d1 = scaled(dsp.sub(b1, a1));
  const d2 = scaled(dsp.sub(b2, a2));
  const na1 = scaled(a1);
  const na2 = scaled(a2);
  const s1 = dsp.state(0);
  const s2 = dsp.state(0);
  const w = dsp.add(dsp.mul(d0, x), s1.signal);
  dsp.updateState(s1, dsp.add(dsp.sub(dsp.mul(d1, x), dsp.mul(na1, w)), s2.signal));
  dsp.updateState(s2, dsp.sub(dsp.mul(d2, x), dsp.mul(na2, w)));
  return dsp.add(x, w);
}
