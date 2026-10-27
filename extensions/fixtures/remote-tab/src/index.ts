// Example: a tab on the Remote page. `remote.html` sends messages with `tetrad.send(...)`; the
// handler below answers them. This one keeps a note in the extension's storage.

import type { ExtensionDefinition, JsonValue } from "@tetrad/extensions";

const extension: ExtensionDefinition = {
  activate(ctx) {
    ctx.remote.onMessage(async (message, scope) => {
      const request = message as { readonly op?: string; readonly text?: string };
      if (request.op === "save") {
        await scope.storage.set("note", request.text ?? "");
      } else if (request.op !== "load") {
        throw new Error(`unknown request ${String(request.op)}`);
      }
      const note: JsonValue = await scope.storage.get("note");
      return { text: typeof note === "string" ? note : "" };
    });
  },
};

export default extension;
