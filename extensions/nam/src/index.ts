// Neural Amp Modeler: builds one DSP block from each LSTM or WaveNet `.nam` file.
//
// Tetrad sends each asset file of the `amp` block to the provider. The provider reads the
// model and builds a qcdsp graph that does what NeuralAmpModelerCore's `nam::lstm::LSTM` or
// `nam::wavenet::WaveNet` does for one sample. Tetrad validates the graph, compiles it for the
// DSP and adds it to the Guitar Amplifier list. The graph uses qcdsp's tanh and sigmoid
// approximations (within 1e-4). A WaveNet too large for one DSP block is refused with its size.

import type { ExtensionDefinition } from "@tetrad/extensions";

import { lstm, parseLstm, read } from "./nam";
import { parseWavenet, wavenet } from "./wavenet";

const extension: ExtensionDefinition = {
  activate(ctx) {
    ctx.blocks.provide("amp", (asset) => {
      const file = read(asset.name, asset.text);
      switch (file.architecture) {
        case "LSTM":
          return lstm(ctx.blocks.builder(), parseLstm(file));
        case "WaveNet":
          return wavenet(ctx.blocks.builder(), parseWavenet(file));
        default:
          throw new Error(
            `${asset.name} is a ${String(file.architecture)} model; this version plays LSTM and WaveNet models only`,
          );
      }
    });
  },
};

export default extension;
