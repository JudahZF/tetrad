// Neural Amp Modeler: builds one DSP block from each LSTM `.nam` file.
//
// Tetrad sends each asset file of the `amp` block to the provider. The provider reads the
// model and builds a qcdsp graph that does what NeuralAmpModelerCore's `nam::lstm::LSTM` does
// for one sample. Tetrad validates the graph, compiles it for the DSP and adds it to the
// Guitar Amplifier list. The graph uses qcdsp's tanh and sigmoid approximations (within 1e-4).

import type { ExtensionDefinition } from "@tetrad/extensions";

import { lstm, parse } from "./nam";

const extension: ExtensionDefinition = {
  activate(ctx) {
    ctx.blocks.provide("amp", (asset) => lstm(ctx.blocks.builder(), parse(asset.name, asset.text)));
  },
};

export default extension;
