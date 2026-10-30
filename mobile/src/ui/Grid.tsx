/**
 * The Grid as the unit draws it: 4 rows of 8 slots between the In and Out buttons, a cable
 * along each row that has blocks, and block tiles in their category colours. A tap selects a
 * block (or adds one in an empty slot), a drag moves it, and a hold starts a copy.
 */

import { useMemo, useRef, useState } from "react";
import { Pressable, View } from "react-native";
import Svg, { Line, Rect } from "react-native-svg";
import { COLUMNS, type Preset, ROWS, type Slot, liveBypass } from "@/lib/preset/model";
import { BlockTile } from "./BlockTile";
import { T } from "./kit";
import { usePalette } from "./theme";

const IO = [
  ["In 1", "Multi Out"],
  ["Prev. Row", "Out 1/2"],
  ["", ""],
  ["", ""],
] as const;

const PAD = 8;
const GAP = 4;
const ROW_GAP = 12;
const HOLD_MS = 450;

interface Geometry {
  io: number;
  tile: number;
  x(column: number): number;
  y(row: number): number;
  /** The slot under a point, or null. */
  at(x: number, y: number): Slot | null;
  height: number;
}

function geometry(width: number): Geometry {
  const io = 26;
  const tile = Math.min(44, Math.floor((width - 2 * PAD - 2 * io - 9 * GAP) / COLUMNS));
  const span = 2 * io + COLUMNS * tile + 9 * GAP;
  const left = (width - span) / 2;
  const x = (column: number) => left + io + GAP + column * (tile + GAP);
  const y = (row: number) => PAD + row * (tile + ROW_GAP);
  return {
    io,
    tile,
    x,
    y,
    height: 2 * PAD + ROWS * tile + (ROWS - 1) * ROW_GAP,
    at(px, py) {
      const column = Math.floor((px - x(0) + GAP / 2) / (tile + GAP));
      const row = Math.floor((py - PAD + ROW_GAP / 2) / (tile + ROW_GAP));
      return row >= 0 && row < ROWS && column >= 0 && column < COLUMNS ? { row, column } : null;
    },
  };
}

const same = (a: Slot | null, b: Slot | null) => !!a && !!b && a.row === b.row && a.column === b.column;

export function Grid({
  preset,
  selected,
  copying,
  onSelect,
  onEmpty,
  onMove,
  onHold,
}: {
  preset: Preset;
  selected: Slot | null;
  /** The block being copied: empty slots take it on a tap. */
  copying: Slot | null;
  onSelect: (slot: Slot) => void;
  onEmpty: (slot: Slot) => void;
  onMove: (from: Slot, to: Slot) => void;
  onHold: (slot: Slot) => void;
}) {
  const palette = usePalette();
  const [width, setWidth] = useState(0);
  const g = useMemo(() => geometry(width), [width]);
  const [drag, setDrag] = useState<{ from: Slot; x: number; y: number; over: Slot | null } | null>(null);
  // Read in touch handlers only: where the finger came down, and the hold timer.
  const touch = useRef<{ slot: Slot | null; x: number; y: number; moved: boolean; held: boolean; timer?: ReturnType<typeof setTimeout> }>({
    slot: null, x: 0, y: 0, moved: false, held: false,
  });
  const blockAtSlot = (slot: Slot | null) => (slot ? (preset.grid[slot.row]?.[slot.column] ?? null) : null);

  const grant = (x: number, y: number) => {
    const slot = g.at(x, y);
    const t: typeof touch.current = { slot, x, y, moved: false, held: false };
    if (slot && blockAtSlot(slot)) {
      t.timer = setTimeout(() => {
        t.held = true;
        onHold(slot);
      }, HOLD_MS);
    }
    touch.current = t;
  };
  const move = (x: number, y: number) => {
    const t = touch.current;
    if (!t.slot || t.held || !blockAtSlot(t.slot)) return;
    if (!t.moved && Math.hypot(x - t.x, y - t.y) < 6) return;
    t.moved = true;
    clearTimeout(t.timer);
    setDrag({ from: t.slot, x, y, over: g.at(x, y) });
  };
  const release = (x: number, y: number) => {
    const t = touch.current;
    clearTimeout(t.timer);
    setDrag(null);
    if (!t.slot || t.held) return;
    if (t.moved) {
      const to = g.at(x, y);
      if (to && !same(to, t.slot)) onMove(t.slot, to);
    } else if (blockAtSlot(t.slot)) onSelect(t.slot);
    else onEmpty(t.slot);
  };
  const cancel = () => {
    clearTimeout(touch.current.timer);
    setDrag(null);
  };

  const rows = preset.grid;
  const dragged = drag ? rows[drag.from.row]?.[drag.from.column] : null;

  return (
    <View
      onLayout={(e) => setWidth(e.nativeEvent.layout.width)}
      style={{ backgroundColor: palette.canvas, borderRadius: 12, height: width ? g.height : 200 }}
      accessibilityLabel="Grid"
    >
      {width > 0 && (
        <>
          <Svg width={width} height={g.height} style={{ position: "absolute" }}>
            {rows.map((row, r) =>
              row.some(Boolean) ? (
                <Line
                  key={r}
                  x1={g.x(0) - GAP - g.io / 2}
                  x2={g.x(COLUMNS - 1) + g.tile + GAP + g.io / 2}
                  y1={g.y(r) + g.tile / 2}
                  y2={g.y(r) + g.tile / 2}
                  stroke={palette.cable}
                  strokeWidth={2}
                />
              ) : null,
            )}
            {drag?.over && (
              <Rect
                x={g.x(drag.over.column) - 1}
                y={g.y(drag.over.row) - 1}
                width={g.tile + 2}
                height={g.tile + 2}
                rx={6}
                fill="none"
                stroke={palette.accent}
                strokeWidth={2}
              />
            )}
          </Svg>
          {IO.map(([left, right], r) =>
            [left, right].map((label, side) => (
              <View
                key={`${r}-${side}`}
                style={{
                  position: "absolute",
                  left: side === 0 ? g.x(0) - GAP - g.io : g.x(COLUMNS - 1) + g.tile + GAP,
                  top: g.y(r),
                  width: g.io,
                  height: g.tile,
                  borderRadius: 5,
                  backgroundColor: palette.surface,
                  alignItems: "center",
                  justifyContent: "center",
                }}
              >
                <T size={label ? 7.5 : 13} weight="semibold" tone={label ? "text" : "muted"} center style={{ lineHeight: 9 }}>
                  {label || "+"}
                </T>
              </View>
            )),
          )}
          {rows.map((row, r) =>
            row.map((block, c) => {
              const slot = { row: r, column: c };
              const hidden = drag && same(drag.from, slot);
              return (
                <View
                  key={`${r}:${c}`}
                  style={{ position: "absolute", left: g.x(c), top: g.y(r), opacity: hidden ? 0.25 : 1 }}
                  accessibilityRole="button"
                  accessibilityLabel={block ? `${block.name}, row ${r + 1}, column ${c + 1}` : `Empty slot, row ${r + 1}, column ${c + 1}`}
                >
                  {block ? (
                    <BlockTile category={block.category} size={g.tile} bypassed={liveBypass(block, preset.scene)} selected={same(slot, selected)} />
                  ) : (
                    <View
                      style={{
                        width: g.tile,
                        height: g.tile,
                        borderRadius: 5,
                        borderWidth: 1,
                        borderStyle: "dashed",
                        borderColor: copying ? palette.accent : "#2a2a2a",
                      }}
                    />
                  )}
                </View>
              );
            }),
          )}
          {dragged && drag && (
            <View pointerEvents="none" style={{ position: "absolute", left: drag.x - g.tile / 2, top: drag.y - g.tile / 2, transform: [{ scale: 1.15 }] }}>
              <BlockTile category={dragged.category} size={g.tile} />
            </View>
          )}
          {/* One responder over the whole Grid: it knows the slot under the finger. */}
          <View
            onStartShouldSetResponder={() => true}
            onMoveShouldSetResponder={() => true}
            onResponderTerminationRequest={() => false}
            onResponderGrant={(e) => grant(e.nativeEvent.locationX, e.nativeEvent.locationY)}
            onResponderMove={(e) => move(e.nativeEvent.locationX, e.nativeEvent.locationY)}
            onResponderRelease={(e) => release(e.nativeEvent.locationX, e.nativeEvent.locationY)}
            onResponderTerminate={cancel}
            style={{ position: "absolute", left: 0, top: 0, right: 0, bottom: 0 }}
          />
        </>
      )}
    </View>
  );
}

/** A scene strip: eight buttons in the scene colours. */
export function SceneStrip({ preset, onScene }: { preset: Preset; onScene: (scene: number) => void }) {
  const palette = usePalette();
  return (
    <View style={{ flexDirection: "row", gap: 5 }}>
      {preset.scenes.map((scene, i) => {
        const on = i === preset.scene;
        return (
          <Pressable
            key={i}
            onPress={() => onScene(i)}
            accessibilityRole="button"
            accessibilityLabel={`Scene ${"ABCDEFGH"[i]}, ${scene.name}`}
            accessibilityState={{ selected: on }}
            style={{
              flex: 1,
              height: 32,
              borderRadius: 6,
              alignItems: "center",
              justifyContent: "center",
              backgroundColor: on ? scene.colour : palette.surface,
              borderBottomWidth: 3,
              borderBottomColor: scene.colour,
            }}
          >
            <T size={13} weight="bold" color={on ? "#000" : palette.muted}>
              {"ABCDEFGH"[i]}
            </T>
          </Pressable>
        );
      })}
    </View>
  );
}
