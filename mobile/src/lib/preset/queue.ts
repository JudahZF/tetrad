/**
 * Sends preset edits one at a time, each on the preset the last reply gave (its `loadId`). A
 * knob drag makes many edits of one parameter: while one is on its way, only the newest of
 * those waits, so a slow link (Bluetooth) never falls behind the finger.
 */

import { type Edit, type Preset, slotKey } from "./model";

export type Send = (loadId: number, edit: Edit) => Promise<Preset>;

/** An edit replaces the last one that waits when both have the same key. */
function key(edit: Edit): string | null {
  switch (edit.kind) {
    case "param":
      return `param:${slotKey(edit.at)}:${edit.param}`;
    case "bypass":
      return `bypass:${slotKey(edit.at)}`;
    case "scene":
      return "scene";
    default:
      return null;
  }
}

export class EditQueue {
  private waiting: Edit[] = [];
  private running = false;

  constructor(
    private preset: () => Preset | null,
    private readonly send: Send,
    private readonly onPreset: (preset: Preset) => void,
    private readonly onError: (error: unknown) => void,
    /** Nothing waits or is on its way any more. */
    private readonly onIdle: () => void = () => {},
  ) {}

  push(edit: Edit) {
    // Only the last: an edit after a scene change must not move before it.
    const last = this.waiting.at(-1);
    const k = key(edit);
    if (k !== null && last && key(last) === k) {
      this.waiting[this.waiting.length - 1] = edit;
      return;
    }
    this.waiting.push(edit);
    void this.run();
  }

  get busy() {
    return this.running || this.waiting.length > 0;
  }

  private async run() {
    if (this.running) return;
    this.running = true;
    try {
      for (let edit = this.waiting.shift(); edit; edit = this.waiting.shift()) {
        const preset = this.preset();
        if (!preset) break;
        try {
          this.onPreset(await this.send(preset.loadId, edit));
        } catch (error) {
          // A refused edit drops the ones queued after it: they were made on what it would have changed.
          this.waiting = [];
          this.onError(error);
        }
      }
    } finally {
      this.running = false;
      this.onIdle();
    }
  }
}
