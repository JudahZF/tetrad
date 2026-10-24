// Neural Amp Modeler A2: builds one DSP block from each A2 `.nam` file.
//
// An A2 file is a `SlimmableContainer`: the same amp trained at several sizes, each a WaveNet
// submodel with the largest share of a host's CPU it is for (`max_value`, ascending; the last
// is 1). NeuralAmpModelerCore (MIT, © Steven Atkinson) defines it in `NAM/container.cpp`. A
// host picks one size. The QC plays the smallest, A2-Lite (3 channels, about a quarter of one
// DSP core): A2-Full (8 channels) needs more DSP memory and CPU than one core has.
//
// The WaveNet code is the Neural Amp Modeler A1 extension's (`extensions/nam`), which reads
// A2's per-layer kernel sizes, LeakyReLU and convolutional head.

import type { ExtensionDefinition } from "@tetrad/extensions";

import { namFile, parse, record } from "../../nam/src/nam";
import { parseWavenet, wavenet } from "../../nam/src/wavenet";

const extension: ExtensionDefinition = {
  activate(ctx) {
    ctx.blocks.provide("amp", (asset) => {
      const file = parse(asset.name, asset.text);
      const architecture = file["architecture"];
      if (architecture !== "SlimmableContainer") {
        throw new Error(
          `${asset.name} is a ${String(architecture)} model, not an A2 model; the Neural Amp Modeler A1 extension plays it`,
        );
      }
      const model = namFile(asset.name, smallest(asset.name, record(file["config"], `${asset.name} config`)));
      if (model.architecture !== "WaveNet") {
        throw new Error(`${asset.name} has a ${String(model.architecture)} size; A2 sizes are WaveNets`);
      }
      return wavenet(ctx.blocks.builder(), parseWavenet(model));
    });
  },
};

/** The smallest submodel of a container config. Throws with a reason the user can act on. */
function smallest(name: string, config: { readonly [key: string]: unknown }): { readonly [key: string]: unknown } {
  const submodels = config["submodels"];
  if (!Array.isArray(submodels) || submodels.length === 0) throw new Error(`${name} has no sizes`);
  const sizes = submodels.map((value: unknown, i) => record(value, `${name} size ${i + 1}`));
  const disordered = new Error(`${name} has sizes that are not in order up to a max_value of 1`);
  let previous = -Infinity;
  for (const size of sizes) {
    const share = size["max_value"];
    if (typeof share !== "number" || !(share > previous)) throw disordered;
    previous = share;
  }
  if (previous < 1) throw disordered;
  const [first] = sizes;
  return record(first?.["model"], `${name} size 1 model`);
}

export default extension;
