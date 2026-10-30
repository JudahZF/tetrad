/** The demo unit's starting state: the preset, library, files, mappings and footswitches of the mockup. */

import { newBlock } from "@/lib/preset/edit";
import { COLUMNS, type Preset, ROWS, SCENE_COLOURS } from "@/lib/preset/model";
import { BUILTIN_THEMES } from "@/lib/themes";
import type * as T from "@/lib/unit/types";
import { catalogModel } from "./catalog";

export function demoPreset(): Preset {
  const grid: Preset["grid"] = Array.from({ length: ROWS }, () => Array(COLUMNS).fill(null));
  const place = (row: number, column: number, model: string, change: (b: ReturnType<typeof newBlock>) => void = () => {}) => {
    const block = newBlock(catalogModel(model));
    change(block);
    const r = grid[row];
    if (r) r[column] = block;
  };
  place(0, 0, "dyn.opto-comp");
  place(0, 1, "drive.green-808");
  place(0, 2, "amp.brit-2203", (b) => {
    const gain = b.params.find((p) => p.id === "gain");
    if (gain) gain.scenes = [6.2, 7.4, 6.2, 6.2, 6.2, 6.2, 6.2, 6.2];
  });
  place(0, 3, "cab.412-brit-v30");
  place(0, 5, "eq.parametric-8");
  place(0, 6, "delay.simple", (b) => {
    b.sceneBypass = [true, false, false, true, true, false, true, true];
  });
  place(0, 7, "reverb.plate");
  place(1, 1, "mod.chorus");
  place(1, 2, "wah.crybaby");
  place(1, 4, "capture.plexi-crunch", (b) => {
    b.bypass = true;
  });
  place(1, 6, "ir.loader");
  return {
    loadId: 1,
    name: "Plexi Crunch",
    place: "12B",
    scenes: ["Intro", "Verse", "Chorus", "Lead", "Bridge", "Clean", "Drive", "Outro"].map((name, i) => ({
      name,
      colour: SCENE_COLOURS[i] ?? "#ffffff",
    })),
    scene: 0,
    grid,
    edited: false,
  };
}

export const demoLibrary = (): T.IrLibrary => ({
  destination: 2,
  folders: [
    { id: 1, name: "IRs", parent: 0, root: true },
    { id: 2, name: "Cabs › Marshall", parent: 1, root: false },
    { id: 3, name: "Bass", parent: 1, root: false },
  ],
  irs: [
    { id: "ir-1", name: "Greenback SM57 Cap", date: "2026-10-27T18:02:11Z", folders: [1, 2] },
    { id: "ir-2", name: "G12H 4x12 Ribbon", date: "2026-10-27T18:03:40Z", folders: [1, 2] },
    { id: "ir-3", name: "Ampeg 8x10 Room", date: "2026-10-12T09:44:05Z", folders: [1, 3] },
  ],
});

export const demoFiles = (): T.FilesReply => ({
  extensions: [
    {
      id: "tetrad.nam-a1", name: "Neural Amp Modeler A1", label: "NAM model", extensions: [".nam"], maxBytes: 3_145_728,
      files: [{ name: "Fender Deluxe Clean.nam", size: 312_404, state: "ready", detail: "" }],
    },
    {
      id: "tetrad.nam-a2", name: "Neural Amp Modeler A2", label: "NAM model", extensions: [".nam"], maxBytes: 3_145_728,
      files: [
        { name: "Deluxe Clean.nam", size: 1_204_112, state: "ready", detail: "" },
        { name: "JCM800 Edge.nam", size: 1_198_880, state: "ready", detail: "" },
        { name: "Old A1 model.nam", size: 288_400, state: "failed", detail: "This is an A1 model: Neural Amp Modeler A1 plays it" },
      ],
    },
  ],
});

export const demoNotices = (): T.NoticesReply => ({
  notices: [
    { ago: 3600, subsystem: "Extensions", detail: "Neural Amp Modeler A2 0.2.0 started" },
    { ago: 1800, subsystem: "Bluetooth", detail: "A phone paired: iPhone" },
    { ago: 42, subsystem: "Remote", detail: "IR added: Greenback SM57 Cap (Cabs › Marshall)" },
  ],
  dropped: 0,
  repeats: 0,
});

export const demoExtensions = (): T.ExtensionsList => ({
  installed: [
    {
      key: "JudahZF/tetrad-extensions:tetrad.nam-a2", name: "Neural Amp Modeler A2", version: "0.2.0",
      description: "Plays NAM A2 models as amp blocks.", source: "JudahZF/tetrad-extensions", enabled: true,
      state: "running", fault: null, update: null, duplicates: [], commands: [], settings: [], status: ["2 models ready"],
      blocks: [{ name: "Deluxe Clean", detail: "A2", ready: true }, { name: "JCM800 Edge", detail: "A2", ready: true }],
      log: [], themes: [], mode: null, app: null,
    },
    {
      key: "JudahZF/tetrad-extensions:midi-mapper", name: "MIDI Mapper", version: "0.3.0",
      description: "Maps MIDI from any channel to actions.", source: "JudahZF/tetrad-extensions", enabled: true,
      state: "running", fault: null, update: "0.3.1", duplicates: [],
      commands: [{ id: "reset", label: "Reset learned CCs" }],
      settings: [
        { key: "channel", label: "Channel", kind: "number", value: 3, min: 1, max: 16 },
        { key: "thru", label: "MIDI thru", kind: "toggle", value: false },
      ],
      status: [], blocks: [], log: ["Listening on channel 3"], themes: [], mode: null, app: null,
    },
    {
      key: "JudahZF/tetrad-extensions:custom-footswitch", name: "Custom Footswitch Mode", version: "1.0.0",
      description: "A fourth footswitch mode with four pages.", source: "JudahZF/tetrad-extensions", enabled: true,
      state: "running", fault: null, update: null, duplicates: [], commands: [], settings: [], status: [], blocks: [],
      log: [], themes: [], mode: { name: "Custom", detail: "4 pages of 10 switches" }, app: null,
    },
  ],
  available: [
    { source: "JudahZF/tetrad-extensions", id: "doom", name: "DOOM", version: "1.0.0", description: "Play DOOM on the unit's screen.", duplicates: [], app: true },
  ],
  sources: [{ repository: "JudahZF/tetrad-extensions", default: true, enabled: true, status: "Up to date" }],
  status: null,
  busy: false,
});

type Field = { id: T.FieldKey; label: string; value: number; unit: string };

/** A mapping row of the demo, with numbers behind its value texts. */
export interface DemoMapping {
  id: string;
  trigger: T.MappingTriggerView;
  action: string | null;
  mode: T.ModeKind | null;
  fields: Field[];
}

export const MODES: Record<T.ModeKind, T.ModeView> = {
  onPress: { kind: "onPress", label: "On press", detail: "Runs once on each press" },
  follow: { kind: "follow", label: "Follow", detail: "The CC sets the value" },
  relative: { kind: "relative", label: "Relative", detail: "An endless encoder steps the value" },
  set: { kind: "set", label: "Set", detail: "Each press sets the value" },
  toggle: { kind: "toggle", label: "Toggle", detail: "Each press swaps A and B" },
  momentary: { kind: "momentary", label: "Momentary", detail: "Held: the value" },
  step: { kind: "step", label: "Step", detail: "Each press moves the value" },
};

export const DEMO_ACTIONS: T.ActionGroup[] = [
  {
    label: "System",
    actions: [
      { key: "System(NextPreset)", label: "Next preset" },
      { key: "System(PreviousPreset)", label: "Previous preset" },
      ...[..."ABCDEFGH"].map((s, i) => ({ key: `System(Scene(${i}))`, label: `Scene ${s}` })),
      { key: "System(Tuner)", label: "Tuner" },
      { key: "System(TapTempo)", label: "Tap tempo" },
    ],
  },
  {
    label: "Plate Verb · Row 1 slot 8",
    actions: [
      { key: "Block { position: GridPosition { row: 0, column: 7 }, target: Bypass }", label: "Bypass" },
      { key: "Block { position: GridPosition { row: 0, column: 7 }, target: Parameter(2) }", label: "Mix" },
    ],
  },
  {
    label: "Brit 2203 · Row 1 slot 3",
    actions: [
      { key: "Block { position: GridPosition { row: 0, column: 2 }, target: Bypass }", label: "Bypass" },
      { key: "Block { position: GridPosition { row: 0, column: 2 }, target: Parameter(0) }", label: "Gain" },
    ],
  },
  { label: "MIDI out", actions: [{ key: "SendMidi", label: "Send MIDI" }] },
];

export const demoMappings = (): { preset: DemoMapping[]; global: DemoMapping[] } => ({
  preset: [
    {
      id: "Stomp { stomp: 1 }", trigger: { kind: "stomp", stomp: 1 },
      action: "Block { position: GridPosition { row: 0, column: 7 }, target: Parameter(2) }", mode: "toggle",
      fields: [{ id: "a", label: "A", value: 25, unit: "%" }, { id: "b", label: "B", value: 60, unit: "%" }],
    },
    {
      id: "Tetrad(0)", trigger: { kind: "cc", number: 11 },
      action: "Block { position: GridPosition { row: 0, column: 2 }, target: Parameter(0) }", mode: "follow",
      fields: [
        { id: "low", label: "Low", value: 0, unit: "" }, { id: "high", label: "High", value: 127, unit: "" },
        { id: "min", label: "Min", value: 3, unit: "" }, { id: "max", label: "Max", value: 8, unit: "" },
      ],
    },
    { id: "MidiOut(OnLoad)", trigger: { kind: "presetLoaded" }, action: "SendMidi", mode: "set", fields: [] },
  ],
  global: [{ id: "Tetrad(1)", trigger: { kind: "note", number: 48 }, action: "System(Tuner)", mode: "momentary", fields: [] }],
});

const COLOURS = ["#45f862", "#0a74e0", "#ff7000", "#ff02c2", "#87daff", "#6954ff", "#ffd236", "#ff2727"];

const slot = (label: string, swatch: number | null, action: T.SlotActionView | null, detail: string, extra: Partial<T.SlotView> = {}): T.SlotView => ({
  action, label, customLabel: null, detail, colour: swatch === null ? "#3a3a3a" : (COLOURS[swatch] ?? "#3a3a3a"),
  swatch, engaged: false, available: true, value: null, momentary: null, messages: [], ...extra,
});

/** Page 1 as in the mockup; pages 2–4 start with the page keys only. */
export function demoFootswitch(): T.FootswitchMode {
  const pageKeys = (n: number) => [
    slot("‹ Page", null, { kind: "system", action: { customPage: "previous" } }, `Page ${n === 0 ? 4 : n}`),
    slot("Page ›", null, { kind: "system", action: { customPage: "next" } }, `Page ${n === 3 ? 1 : n + 2}`),
  ];
  const empty = () => slot("", null, null, "");
  const first: T.SlotView[] = [
    slot("Scene A", 0, { kind: "system", action: { scene: 0 } }, "Scene", { engaged: true }),
    slot("Scene B", 1, { kind: "system", action: { scene: 1 } }, "Scene"),
    slot("Scene C", 2, { kind: "system", action: { scene: 2 } }, "Scene"),
    slot("Delay", 4, { kind: "block", row: 0, column: 6, parameter: null }, "Bypass · momentary", { engaged: true, momentary: true, customLabel: "Delay" }),
    slot("Worship 4B", 6, { kind: "loadPreset" }, "Load 4B", { customLabel: "Worship 4B" }),
    slot("Synth", 5, { kind: "command", key: "JudahZF/tetrad-extensions:midi-mapper", command: "reset" }, "MIDI Mapper · Reset"),
    slot("Rec / Overdub", 7, { kind: "system", action: { looper: "recordOverdub" } }, "Looper"),
    slot("Swell", 3, { kind: "block", row: 0, column: 7, parameter: 2 }, "Mix · 80 %", { value: 80, customLabel: "Swell" }),
    ...pageKeys(0),
  ];
  const others = [1, 2, 3].map((n) => [...Array.from({ length: 8 }, empty), ...pageKeys(n)]);
  return {
    mode: { name: "Custom", extension: "Custom Footswitch Mode", pages: 4, page: 0, active: true },
    preset: "12B Plexi Crunch",
    presetLoaded: true,
    colours: COLOURS,
    layouts: { global: [first, ...others], preset: null },
    systems: [
      { group: "Preset", label: "Next preset", action: "nextPreset" },
      { group: "Preset", label: "Previous preset", action: "previousPreset" },
      ...[..."ABCDEFGH"].map((s, i) => ({ group: "Scene" as const, label: `Scene ${s}`, action: { scene: i } })),
      { group: "Tuner", label: "Tuner", action: "tuner" },
      { group: "Tempo", label: "Tap tempo", action: "tapTempo" },
      { group: "Looper", label: "Record / Overdub", action: { looper: "recordOverdub" } },
      { group: "Looper", label: "Play / Stop", action: { looper: "playStop" } },
      { group: "Page", label: "Next page", action: { customPage: "next" } },
      { group: "Page", label: "Previous page", action: { customPage: "previous" } },
    ],
    blocks: [
      { row: 0, column: 2, name: "Brit 2203", location: "Row 1 slot 3", parameters: [{ index: 0, name: "Gain" }, { index: 5, name: "Master" }] },
      { row: 0, column: 6, name: "Simple Delay", location: "Row 1 slot 7", parameters: [{ index: 0, name: "Time" }, { index: 2, name: "Mix" }] },
      { row: 0, column: 7, name: "Plate Verb", location: "Row 1 slot 8", parameters: [{ index: 2, name: "Mix" }] },
    ],
    commands: [{ key: "JudahZF/tetrad-extensions:midi-mapper", command: "reset", label: "MIDI Mapper · Reset learned CCs" }],
    setlists: ["Sunday Set"],
  };
}

const TAB_HTML = `<!doctype html><meta name=viewport content="width=device-width">
<style>body{font-family:system-ui;background:#121212;color:#fff;padding:16px}button{background:#45f862;border:0;border-radius:8px;padding:10px 14px;font-weight:600}</style>
<h3>MIDI Mapper</h3><p>Learned CCs: <b id=n>2</b></p><button onclick="tetrad.send({op:'reset'}).then(r=>n.textContent=r.learned)">Reset learned CCs</button>`;

export const demoTabs = (): { tabs: T.ExtensionTab[]; html: Record<string, string> } => ({
  tabs: [{ id: "midi-mapper", title: "MIDI Mapper" }],
  html: { "midi-mapper": TAB_HTML },
});

export const demoThemes = (): T.ThemesList => ({
  active: "stock",
  fields: [],
  themes: BUILTIN_THEMES.map((t) => ({
    id: t.id, name: t.name, source: "builtin", origin: "Built-in", error: null, palette: t.palette, base: null, colors: {},
  })),
});
