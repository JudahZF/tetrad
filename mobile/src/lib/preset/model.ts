/**
 * The preset as the app edits it: 8 scenes and a 4 × 8 Grid of blocks. Positions follow the unit
 * (crates/tetrad-preset/src/edit.rs): a slot is a row and a column.
 */

import type { IconName } from "@/ui/icon-assets";

export const ROWS = 4;
export const COLUMNS = 8;
export const SCENES = 8;

/** Block categories with the Grid's ring colour and glyph (tetrad-assets `block`, tetrad-ui `icon`). */
export const CATEGORIES = {
  amp: { name: "Amp", colour: "#ff2727", icon: "amp" },
  drive: { name: "Drive", colour: "#ff7000", icon: "overdrive" },
  cab: { name: "Cab", colour: "#6954ff", icon: "cab" },
  irLoader: { name: "IR Loader", colour: "#6954ff", icon: "ir-loader" },
  dynamics: { name: "Dynamics", colour: "#45f862", icon: "compressor" },
  eq: { name: "EQ", colour: "#0a74e0", icon: "eq" },
  delay: { name: "Delay", colour: "#00ffdd", icon: "delay" },
  reverb: { name: "Reverb", colour: "#00ffdd", icon: "reverb" },
  modulation: { name: "Modulation", colour: "#3500f1", icon: "modulation" },
  pitch: { name: "Pitch", colour: "#ffd236", icon: "pitch" },
  filter: { name: "Filter", colour: "#87daff", icon: "filter" },
  synth: { name: "Synth", colour: "#e44a5d", icon: "synth" },
  wah: { name: "Wah", colour: "#959595", icon: "wah" },
  capture: { name: "Neural Capture", colour: "#959595", icon: "neural-capture" },
  utility: { name: "Utility", colour: "#959595", icon: "utility" },
  fxLoop: { name: "FX Loop", colour: "#959595", icon: "fx-loop" },
  looper: { name: "Looper", colour: "#ff2727", icon: "looper-reels" },
} as const satisfies Record<string, { name: string; colour: string; icon: IconName }>;

export type Category = keyof typeof CATEGORIES;

/** Stock default scene colours (edit.rs `DEFAULT_SCENE_COLORS`). */
export const SCENE_COLOURS = [
  "#ff2727", "#0a74e0", "#ffd236", "#ff02c2", "#45f862", "#ff7000", "#6954ff", "#00ffdd",
] as const;

export interface Param {
  id: string;
  name: string;
  value: number;
  min: number;
  max: number;
  step: number;
  unit: string;
  /** A value per scene, when the parameter changes with the scene. */
  scenes?: number[];
}

export interface Block {
  model: string;
  name: string;
  category: Category;
  /** Shown under the name in the block list, for example the modelled gear. */
  description?: string;
  /** The extension that supplies the block, when it is not a stock block. */
  extension?: string;
  bypass: boolean;
  /** Bypass per scene, when the block's bypass changes with the scene. */
  sceneBypass?: boolean[];
  params: Param[];
}

export interface Scene {
  name: string;
  colour: string;
}

export interface Slot {
  row: number;
  column: number;
}

export interface Preset {
  /** New at each load and each edit: an edit made on an older preset is refused. */
  loadId: number;
  name: string;
  /** Bank and letter, for example "12B". */
  place: string;
  scenes: Scene[];
  scene: number;
  /** `grid[row][column]`. */
  grid: (Block | null)[][];
  /** Edits since the last save. */
  edited: boolean;
}

export type Edit =
  | { kind: "move"; from: Slot; to: Slot }
  | { kind: "copy"; from: Slot; to: Slot }
  | { kind: "add"; to: Slot; model: string }
  | { kind: "remove"; at: Slot }
  | { kind: "bypass"; at: Slot; bypass: boolean }
  | { kind: "param"; at: Slot; param: string; value: number }
  | { kind: "param-scenes"; at: Slot; param: string; on: boolean }
  | { kind: "bypass-scenes"; at: Slot; on: boolean }
  | { kind: "scene"; scene: number };

export const slotKey = (slot: Slot) => `${slot.row}:${slot.column}`;

/** The slot of a `slotKey`, or null. */
export function parseSlot(text: string | undefined): Slot | null {
  const [row, column] = (text ?? "").split(":").map(Number);
  if (row === undefined || column === undefined || !Number.isInteger(row) || !Number.isInteger(column)) return null;
  return row >= 0 && row < ROWS && column >= 0 && column < COLUMNS ? { row, column } : null;
}

export function blockAt(preset: Preset, slot: Slot): Block | null {
  return preset.grid[slot.row]?.[slot.column] ?? null;
}

/** The value the unit plays now: the scene's value when the parameter has scene values. */
export function liveValue(param: Param, scene: number): number {
  return param.scenes?.[scene] ?? param.value;
}

export function liveBypass(block: Block, scene: number): boolean {
  return block.sceneBypass?.[scene] ?? block.bypass;
}

export const clamp = (param: Param, value: number) => {
  const stepped = Math.round((value - param.min) / param.step) * param.step + param.min;
  return Math.min(param.max, Math.max(param.min, Number(stepped.toFixed(6))));
};
