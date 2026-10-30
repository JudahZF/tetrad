import { describe, expect, it } from "vitest";
import { DEMO_CATALOG } from "@/lib/demo/catalog";
import { EditError, applyEdit } from "./edit";
import { COLUMNS, type Preset, ROWS, SCENE_COLOURS, blockAt, liveBypass, liveValue } from "./model";

const empty = (): Preset => ({
  loadId: 1,
  name: "Test",
  place: "1A",
  scenes: SCENE_COLOURS.map((colour, i) => ({ name: `Scene ${"ABCDEFGH"[i]}`, colour })),
  scene: 0,
  grid: Array.from({ length: ROWS }, () => Array<null>(COLUMNS).fill(null)),
  edited: false,
});

const edit = (preset: Preset, ...edits: Parameters<typeof applyEdit>[1][]) =>
  edits.reduce((p, e) => applyEdit(p, e, DEMO_CATALOG), preset);

describe("preset edits", () => {
  it("adds, moves (swapping onto a block), copies and removes blocks", () => {
    const a = { row: 0, column: 0 };
    const b = { row: 0, column: 1 };
    const c = { row: 1, column: 0 };
    let p = edit(empty(), { kind: "add", to: a, model: "amp.brit-2203" }, { kind: "add", to: b, model: "delay.simple" });
    expect(p.edited).toBe(true);
    p = edit(p, { kind: "move", from: a, to: b });
    expect(blockAt(p, a)?.name).toBe("Simple Delay");
    expect(blockAt(p, b)?.name).toBe("Brit 2203");
    p = edit(p, { kind: "copy", from: b, to: c });
    expect(blockAt(p, c)?.name).toBe("Brit 2203");
    expect(blockAt(p, c)).not.toBe(blockAt(p, b));
    expect(() => edit(p, { kind: "copy", from: a, to: c })).toThrow(EditError);
    p = edit(p, { kind: "remove", at: a });
    expect(blockAt(p, a)).toBeNull();
  });

  it("refuses slots outside the grid and missing blocks", () => {
    expect(() => edit(empty(), { kind: "add", to: { row: 4, column: 0 }, model: "amp.brit-2203" })).toThrow(/row/);
    expect(() => edit(empty(), { kind: "remove", at: { row: 0, column: 0 } })).toThrow(/no block/);
    expect(() => edit(empty(), { kind: "add", to: { row: 0, column: 0 }, model: "nope" })).toThrow(/model/);
  });

  it("each edit gives a new load id, so an edit on an old preset can be told apart", () => {
    const p = edit(empty(), { kind: "add", to: { row: 0, column: 0 }, model: "amp.brit-2203" });
    expect(p.loadId).toBe(2);
  });

  it("parameters clamp to their range and step", () => {
    const at = { row: 0, column: 0 };
    let p = edit(empty(), { kind: "add", to: at, model: "amp.brit-2203" });
    p = edit(p, { kind: "param", at, param: "gain", value: 12 });
    expect(blockAt(p, at)?.params.find((x) => x.id === "gain")?.value).toBe(10);
    p = edit(p, { kind: "param", at, param: "gain", value: 3.04 });
    expect(blockAt(p, at)?.params.find((x) => x.id === "gain")?.value).toBe(3);
    expect(() => edit(p, { kind: "param", at, param: "nope", value: 1 })).toThrow(/parameter/);
  });

  it("a parameter with scene values changes only in the current scene", () => {
    const at = { row: 0, column: 0 };
    let p = edit(empty(), { kind: "add", to: at, model: "amp.brit-2203" }, { kind: "param-scenes", at, param: "gain", on: true });
    p = edit(p, { kind: "scene", scene: 1 }, { kind: "param", at, param: "gain", value: 7.4 });
    const gain = blockAt(p, at)?.params.find((x) => x.id === "gain");
    expect(gain && liveValue(gain, 1)).toBe(7.4);
    expect(gain && liveValue(gain, 0)).toBe(6.2);
  });

  it("bypass per scene follows the scene", () => {
    const at = { row: 0, column: 0 };
    let p = edit(empty(), { kind: "add", to: at, model: "delay.simple" }, { kind: "bypass-scenes", at, on: true });
    p = edit(p, { kind: "scene", scene: 2 }, { kind: "bypass", at, bypass: true });
    const block = blockAt(p, at);
    expect(block && liveBypass(block, 2)).toBe(true);
    expect(block && liveBypass(block, 0)).toBe(false);
  });

  it("a scene change keeps the preset saved", () => {
    const p = edit(empty(), { kind: "scene", scene: 3 });
    expect(p.scene).toBe(3);
    expect(p.edited).toBe(false);
  });
});
