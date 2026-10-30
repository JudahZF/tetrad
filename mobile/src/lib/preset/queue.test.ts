import { describe, expect, it } from "vitest";
import { DEMO_CATALOG } from "@/lib/demo/catalog";
import { demoPreset } from "@/lib/demo/data";
import { applyEdit } from "./edit";
import type { Edit, Preset } from "./model";
import { EditQueue } from "./queue";

describe("edit queue", () => {
  it("sends one edit at a time and keeps only the newest value of a dragged knob", async () => {
    let current: Preset = demoPreset();
    const sent: Edit[] = [];
    let release: () => void = () => {};
    const send = async (loadId: number, edit: Edit) => {
      expect(loadId).toBe(current.loadId);
      sent.push(edit);
      await new Promise<void>((r) => (release = r));
      return applyEdit(current, edit, DEMO_CATALOG);
    };
    let idle = 0;
    const waited: number[] = [];
    const queue = new EditQueue(
      () => current,
      send,
      (p, waiting) => {
        current = p;
        waited.push(waiting.length);
      },
      (e) => {
        throw e;
      },
      () => idle++,
    );
    const at = { row: 0, column: 2 };
    for (const value of [6.3, 6.4, 6.5, 6.6]) queue.push({ kind: "param", at, param: "master", value });
    await Promise.resolve();
    expect(sent).toHaveLength(1);
    release();
    await new Promise((r) => setTimeout(r, 0));
    release();
    await new Promise((r) => setTimeout(r, 0));
    expect(sent.map((e) => (e.kind === "param" ? e.value : null))).toEqual([6.3, 6.6]);
    // The first reply comes while 6.6 waits: the screen keeps showing it.
    expect(waited).toEqual([1, 0]);
    expect(queue.busy).toBe(false);
    expect(idle).toBe(1);
    expect(current.grid[0]?.[2]?.params.find((p) => p.id === "master")?.value).toBe(6.6);
  });

  it("keeps a scene change between the edits made before and after it", async () => {
    let current: Preset = demoPreset();
    const sent: Edit[] = [];
    let release: () => void = () => {};
    const send = async (_: number, edit: Edit) => {
      sent.push(edit);
      await new Promise<void>((r) => (release = r));
      return applyEdit(current, edit, DEMO_CATALOG);
    };
    const queue = new EditQueue(() => current, send, (p) => (current = p), (e) => {
      throw e;
    });
    const at = { row: 0, column: 2 };
    queue.push({ kind: "bypass", at, bypass: true });
    queue.push({ kind: "param", at, param: "master", value: 7 });
    queue.push({ kind: "scene", scene: 1 });
    queue.push({ kind: "param", at, param: "master", value: 9 });
    for (let i = 0; i < 4; i++) {
      release();
      await new Promise((r) => setTimeout(r, 0));
    }
    expect(sent.map((e) => (e.kind === "param" ? e.value : e.kind))).toEqual(["bypass", 7, "scene", 9]);
  });

  it("a refusal drops what waited after it and reports it", async () => {
    let current: Preset = demoPreset();
    const errors: unknown[] = [];
    const send = async (_: number, edit: Edit) => {
      if (edit.kind === "remove") throw new Error("refused");
      return applyEdit(current, edit, DEMO_CATALOG);
    };
    const queue = new EditQueue(() => current, send, (p) => (current = p), (e) => errors.push(e));
    queue.push({ kind: "remove", at: { row: 3, column: 3 } });
    queue.push({ kind: "scene", scene: 4 });
    await new Promise((r) => setTimeout(r, 0));
    expect(errors).toHaveLength(1);
    expect(current.scene).toBe(0);
  });
});
