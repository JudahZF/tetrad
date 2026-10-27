// Neural Amp Modeler A1: builds one DSP block from each LSTM or WaveNet `.nam` file. A2 files
// (several sizes in one `SlimmableContainer`) are for the Neural Amp Modeler A2 extension,
// `extensions/nam-a2`, which shares this folder's WaveNet code.
//
// Tetrad sends each asset file of the `amp` block to the provider. The provider reads the
// model and builds a qcdsp graph that does what NeuralAmpModelerCore's `nam::lstm::LSTM` or
// `nam::wavenet::WaveNet` does for one sample. Tetrad validates the graph, compiles it for the
// DSP and adds it to the Guitar Amplifier list. The graph uses qcdsp's tanh and sigmoid
// approximations (within 1e-4). A WaveNet too large for one DSP block is refused with its size.

import type { ExtensionDefinition } from "@tetrad/extensions";

import { lstm, namFile, parse, parseLstm } from "./nam";
import { parseWavenet, wavenet } from "./wavenet";

const extension: ExtensionDefinition = {
  activate(ctx) {
    ctx.blocks.provide("amp", (asset) => {
      const file = parse(asset.name, asset.text);
      const architecture = file["architecture"];
      switch (architecture) {
        case "LSTM":
          return lstm(ctx.blocks.builder(), parseLstm(namFile(asset.name, file)));
        case "WaveNet":
          return wavenet(ctx.blocks.builder(), parseWavenet(namFile(asset.name, file)));
        case "SlimmableContainer":
          throw new Error(`${asset.name} is an A2 model; the Neural Amp Modeler A2 extension plays it`);
        default:
          throw new Error(
            `${asset.name} is a ${String(architecture)} model; this version plays LSTM and WaveNet models only`,
          );
      }
    });
  },
};

export default extension;
