import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

// Unit tests cover the code that needs no React Native: the link codec, the clients against
// fakes, the demo unit, the preset edits and the IR conversion.
export default defineConfig({
  resolve: { alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) } },
  test: { include: ["src/**/*.test.ts"] },
});
