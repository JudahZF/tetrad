// Fixture: a control macro with no MIDI. It proves the platform has no MIDI coupling.

import type { Binding, Block, ExtensionDefinition } from "@tetrad/extensions";

const extension: ExtensionDefinition = {
  activate(ctx) {
    ctx.commands.register("set-first", async (scope) => {
      const block = (await scope.controls.list()).find((b) => b.parameters.length > 0);
      const parameter = block?.parameters[0];
      if (block === undefined || parameter === undefined) {
        await scope.ui.setStatus(["No block with parameters in this preset"]);
        return;
      }
      await scope.controls.set(bindingFor(block, parameter.index), 0.5);
      await scope.ui.setStatus([`${block.name}: ${parameter.name} set to 50%`]);
    });
  },
};

export default extension;

/** A binding carries only the site, model and control, not the block's other fields. */
function bindingFor(block: Block, index: number): Binding {
  const control = { kind: "parameter", index } as const;
  const { model } = block;
  switch (block.site) {
    case "slot":
      return { site: block.site, row: block.row, column: block.column, model, control };
    case "input":
    case "output":
      return { site: block.site, row: block.row, model, control };
    case "splitter":
    case "mixer":
      return { site: block.site, pair: block.pair, model, control };
  }
}
