/** Grid, block and scene edits on a [`Preset`]. Pure: each edit returns a new preset. */

import {
  type Block,
  COLUMNS,
  type Edit,
  type Param,
  type Preset,
  ROWS,
  SCENES,
  type Slot,
  blockAt,
  clamp,
} from "./model";

/** A block the unit can add: its parameters with their defaults. */
export interface BlockModel {
  model: string;
  name: string;
  category: Block["category"];
  description?: string;
  extension?: string;
  params: (Omit<Param, "value" | "scenes"> & { default: number })[];
}

export class EditError extends Error {}

function check(slot: Slot) {
  if (!Number.isInteger(slot.row) || slot.row < 0 || slot.row >= ROWS) {
    throw new EditError(`no grid row ${slot.row}`);
  }
  if (!Number.isInteger(slot.column) || slot.column < 0 || slot.column >= COLUMNS) {
    throw new EditError(`no grid column ${slot.column}`);
  }
  return slot;
}

function occupied(preset: Preset, slot: Slot): Block {
  const block = blockAt(preset, check(slot));
  if (!block) throw new EditError(`no block at row ${slot.row + 1}, column ${slot.column + 1}`);
  return block;
}

function withGrid(preset: Preset, change: (grid: (Block | null)[][]) => void): Preset {
  const grid = preset.grid.map((row) => [...row]);
  change(grid);
  return { ...preset, grid, edited: true, loadId: preset.loadId + 1 };
}

function put(grid: (Block | null)[][], slot: Slot, block: Block | null) {
  const row = grid[slot.row];
  if (row) row[slot.column] = block;
}

function withBlock(preset: Preset, at: Slot, change: (block: Block) => Block): Preset {
  const block = occupied(preset, at);
  return withGrid(preset, (grid) => put(grid, at, change(block)));
}

function withParam(block: Block, id: string, change: (param: Param) => Param): Block {
  if (!block.params.some((p) => p.id === id)) throw new EditError(`${block.name} has no parameter ${id}`);
  return { ...block, params: block.params.map((p) => (p.id === id ? change(p) : p)) };
}

export function newBlock(model: BlockModel): Block {
  return {
    model: model.model,
    name: model.name,
    category: model.category,
    ...(model.description ? { description: model.description } : {}),
    ...(model.extension ? { extension: model.extension } : {}),
    bypass: false,
    params: model.params.map(({ default: value, ...param }) => ({ ...param, value })),
  };
}

export function applyEdit(preset: Preset, edit: Edit, catalog: readonly BlockModel[]): Preset {
  switch (edit.kind) {
    case "move": {
      // A drop on another block swaps the two, so nothing is lost.
      const block = occupied(preset, edit.from);
      const there = blockAt(preset, check(edit.to));
      return withGrid(preset, (grid) => {
        put(grid, edit.from, there);
        put(grid, edit.to, block);
      });
    }
    case "copy": {
      const block = occupied(preset, edit.from);
      if (blockAt(preset, check(edit.to))) throw new EditError("copy a block to an empty slot");
      return withGrid(preset, (grid) => put(grid, edit.to, structuredClone(block)));
    }
    case "add": {
      // On an occupied slot the new block replaces the old one, as the unit's picker does.
      const model = catalog.find((m) => m.model === edit.model);
      if (!model) throw new EditError(`no block model ${edit.model}`);
      check(edit.to);
      return withGrid(preset, (grid) => put(grid, edit.to, newBlock(model)));
    }
    case "remove":
      occupied(preset, edit.at);
      return withGrid(preset, (grid) => put(grid, edit.at, null));
    case "bypass":
      return withBlock(preset, edit.at, (block) =>
        block.sceneBypass
          ? { ...block, sceneBypass: block.sceneBypass.map((b, i) => (i === preset.scene ? edit.bypass : b)) }
          : { ...block, bypass: edit.bypass },
      );
    case "bypass-scenes":
      return withBlock(preset, edit.at, ({ sceneBypass, ...block }) =>
        // Off keeps the current scene's bypass, as the unit does (tetrad-preset set_scene_mode).
        edit.on ? { ...block, sceneBypass: sceneBypass ?? Array(SCENES).fill(block.bypass) } : { ...block, bypass: sceneBypass?.[preset.scene] ?? block.bypass },
      );
    case "param":
      return withBlock(preset, edit.at, (block) =>
        withParam(block, edit.param, (param) => {
          const value = clamp(param, edit.value);
          return param.scenes
            ? { ...param, scenes: param.scenes.map((v, i) => (i === preset.scene ? value : v)) }
            : { ...param, value };
        }),
      );
    case "param-scenes":
      return withBlock(preset, edit.at, (block) =>
        withParam(block, edit.param, ({ scenes, ...param }) =>
          edit.on ? { ...param, scenes: scenes ?? Array(SCENES).fill(param.value) } : { ...param, value: scenes?.[preset.scene] ?? param.value },
        ),
      );
    case "scene":
      if (!Number.isInteger(edit.scene) || edit.scene < 0 || edit.scene >= SCENES) {
        throw new EditError(`no scene ${edit.scene}`);
      }
      // A scene change is not an edit of the preset: it stays saved.
      return { ...preset, scene: edit.scene, loadId: preset.loadId + 1 };
  }
}
