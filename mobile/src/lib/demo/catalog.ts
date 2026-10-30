/** The blocks of the demo unit: a few per category, with typical parameters. */

import type { BlockModel } from "@/lib/preset/edit";

type P = BlockModel["params"][number];
const knob = (id: string, name: string, value = 5): P => ({
  id, name, default: value, min: 0, max: 10, step: 0.1, unit: "",
});
const pct = (id: string, name: string, value = 50): P => ({
  id, name, default: value, min: 0, max: 100, step: 1, unit: "%",
});
const db = (id: string, name: string, value = 0, min = -24, max = 24): P => ({
  id, name, default: value, min, max, step: 0.5, unit: "dB",
});
const ms = (id: string, name: string, value: number, max = 2000): P => ({
  id, name, default: value, min: 1, max, step: 1, unit: "ms",
});

const amp = (model: string, name: string, description: string): BlockModel => ({
  model, name, category: "amp", description,
  params: [knob("gain", "Gain", 6.2), knob("bass", "Bass"), knob("mid", "Mid", 6.8),
    knob("treble", "Treble", 5.5), knob("presence", "Presence", 4), knob("master", "Master", 7.5)],
});

export const DEMO_CATALOG: readonly BlockModel[] = [
  amp("amp.brit-2203", "Brit 2203", "Marshall JCM800 2203"),
  amp("amp.brit-plexi-50", "Brit Plexi 50", "Marshall Super Lead 1959"),
  amp("amp.us-double-nrm", "US Double Nrm", "Fender Twin Reverb"),
  amp("amp.cali-rect", "Cali Rect", "Mesa Dual Rectifier"),
  {
    model: "nam-a2:deluxe-clean", name: "Deluxe Clean", category: "amp", description: "A2 · 48 kHz",
    extension: "Neural Amp Modeler A2", params: [db("input", "Input"), db("output", "Output")],
  },
  {
    model: "nam-a2:jcm800-edge", name: "JCM800 Edge", category: "amp", description: "A2 · 48 kHz",
    extension: "Neural Amp Modeler A2", params: [db("input", "Input"), db("output", "Output")],
  },
  {
    model: "drive.green-808", name: "Green 808", category: "drive", description: "Ibanez TS808",
    params: [knob("drive", "Drive", 3.5), knob("tone", "Tone"), knob("level", "Level", 6)],
  },
  {
    model: "drive.chief-od", name: "Chief OD", category: "drive", description: "Klon Centaur",
    params: [knob("gain", "Gain", 2), knob("treble", "Treble"), knob("output", "Output", 6.5)],
  },
  {
    model: "cab.412-brit-v30", name: "412 Brit V30", category: "cab", description: "Celestion Vintage 30",
    params: [ms("distance", "Distance", 2, 30), db("level", "Level"), pct("mix", "Mic Mix")],
  },
  {
    model: "ir.loader", name: "IR Loader", category: "irLoader", description: "Your IRs",
    params: [db("level", "Level"), pct("mix", "Mix", 100)],
  },
  {
    model: "dyn.opto-comp", name: "Opto Comp", category: "dynamics", description: "Optical compressor",
    params: [knob("peak", "Peak Red.", 4), knob("gain", "Gain", 5), pct("mix", "Mix", 100)],
  },
  {
    model: "eq.parametric-8", name: "Parametric-8", category: "eq", description: "8-band EQ",
    params: [db("low", "Low"), db("mid", "Mid", 2), db("high", "High", -1), db("level", "Level")],
  },
  {
    model: "delay.simple", name: "Simple Delay", category: "delay", description: "Digital delay",
    params: [ms("time", "Time", 420), pct("feedback", "Feedback", 30), pct("mix", "Mix", 25)],
  },
  {
    model: "reverb.plate", name: "Plate Verb", category: "reverb", description: "Studio plate",
    params: [ms("decay", "Decay", 2400, 10000), pct("predelay", "Pre-delay", 10), pct("mix", "Mix", 25)],
  },
  {
    model: "mod.chorus", name: "Chorus", category: "modulation", description: "Analogue chorus",
    params: [pct("rate", "Rate", 35), pct("depth", "Depth", 60), pct("mix", "Mix", 50)],
  },
  {
    model: "wah.crybaby", name: "Wah", category: "wah", description: "Dunlop Cry Baby",
    params: [pct("position", "Position", 40), pct("mix", "Mix", 100)],
  },
  {
    model: "pitch.whammy", name: "Pitch Shift", category: "pitch", description: "Octave and pitch",
    params: [{ id: "shift", name: "Shift", default: 12, min: -24, max: 24, step: 1, unit: "st" }, pct("mix", "Mix", 100)],
  },
  {
    model: "capture.plexi-crunch", name: "Plexi Crunch NC", category: "capture", description: "Neural Capture",
    params: [db("input", "Input"), db("level", "Level")],
  },
];

export function catalogModel(model: string) {
  const found = DEMO_CATALOG.find((m) => m.model === model);
  if (!found) throw new Error(`no demo block ${model}`);
  return found;
}
