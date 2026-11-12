/**
 * The unit's replies and commands, as the Remote API (docs/remote.md) and the Tetrad Link send them.
 * Field names are the unit's; see docs/remote-api.md for each source line.
 */

export interface StatusReply {
  /** The IR saving destination. */
  folder: string;
}

export interface IrAddedReply {
  name: string;
  folder: string;
}

export interface IrLibrary {
  destination: number;
  /** Writable IR folders, the root first. */
  folders: IrFolder[];
  /** User IRs (factory IRs are not listed). */
  irs: IrEntry[];
}
export interface IrFolder {
  id: number;
  name: string;
  parent: number;
  root: boolean;
}
export interface IrEntry {
  id: string;
  name: string;
  date: string;
  folders: number[];
}
export type LibraryOp =
  | { op: "rename"; id: string; name: string }
  | { op: "delete"; id: string }
  | { op: "link"; id: string; folder: number; linked: boolean }
  | { op: "destination"; folder: number }
  | { op: "folder"; name: string; parent?: number };

export interface FilesReply {
  extensions: ExtensionFiles[];
}
export interface ExtensionFiles {
  /** Extension id, e.g. "tetrad.nam". */
  id: string;
  name: string;
  /** What the files are, e.g. "NAM model". */
  label: string;
  /** Allowed file extensions with the dot, lower case. */
  extensions: string[];
  maxBytes: number;
  files: ExtensionFile[];
}
export interface ExtensionFile {
  name: string;
  size: number;
  state: "ready" | "waiting" | "failed";
  detail: string;
}
export type FileChunkReply = { received: number } | { name: string; size: number };

export interface ExtensionTab {
  id: string;
  title: string;
}

export interface SupportFile {
  name: string;
  zip: Uint8Array;
}

export interface Notice {
  /** Seconds before the reply. */
  ago: number;
  /** A label from the unit, such as "Dsp". A newer Tetrad can send values this app does not know. */
  subsystem: string;
  detail: string;
}
export interface NoticesReply {
  /** Newest last. */
  notices: Notice[];
  dropped: number;
  repeats: number;
}

export type BackupState = "idle" | "creating" | "ready" | "receiving" | "received" | "restoring" | "restored";
export interface BackupStatus {
  state: BackupState;
  name: string | null;
  size: number | null;
  received: number | null;
  error: string | null;
}

export interface ExtensionsList {
  installed: InstalledExtension[];
  available: AvailableExtension[];
  sources: ExtensionSource[];
  status: string | null;
  busy: boolean;
}
export interface InstalledExtension {
  /** "owner/repo:extension.id" */
  key: string;
  name: string;
  version: string;
  description: string;
  source: string;
  enabled: boolean;
  state: "off" | "starting" | "running" | "faulted";
  fault: string | null;
  update: string | null;
  duplicates: string[];
  commands: { id: string; label: string }[];
  settings: ExtensionSettingView[];
  status: string[];
  blocks: { name: string; detail: string; ready: boolean }[];
  log: string[];
  themes: string[];
  mode: { name: string; detail: string } | null;
  app: string | null;
}
export type ExtensionSettingView =
  | { key: string; label: string; kind: "choice"; options: string[]; value: number }
  | { key: string; label: string; kind: "toggle"; value: boolean }
  | { key: string; label: string; kind: "number"; value: number; min: number; max: number };
export interface AvailableExtension {
  source: string;
  id: string;
  name: string;
  version: string;
  description: string;
  duplicates: string[];
  /** It brings a program with full access to the unit: ask for consent first. */
  app: boolean;
}
export interface ExtensionSource {
  repository: string;
  default: boolean;
  enabled: boolean;
  status: string;
}
export type ExtensionCommand =
  | { op: "refresh" }
  | { op: "install"; source: string; id: string }
  | { op: "update"; key: string }
  | { op: "enable"; key: string }
  | { op: "disable"; key: string }
  | { op: "retry"; key: string }
  | { op: "remove"; key: string }
  | { op: "run"; key: string; command: string }
  | { op: "setting"; key: string; setting: string; value: number | boolean };

export type PaletteRole =
  | "background" | "canvas" | "panel" | "surface" | "rowSelected" | "section" | "divider"
  | "scrollbar" | "text" | "muted" | "faint" | "cable" | "accent" | "accentFill" | "onAccent"
  | "action" | "danger" | "warning";
export type UnitPalette = Record<PaletteRole, string>;
export interface ThemesList {
  active: string | null;
  fields: { key: PaletteRole; label: string; help: string; derived: boolean }[];
  themes: ThemeEntry[];
}
/** The Update channel of Settings › Tetrad › Tetrad Updates. */
export type ReleaseChannel = "stable" | "nightly";
/** GET /api/updates and POST /api/updates/channel. */
export interface UpdatesView {
  channel: ReleaseChannel;
  /** The running Tetrad. */
  version: string;
  /** The running Tetrad is a nightly build. */
  nightly: boolean;
}
export interface ThemeEntry {
  id: string;
  name: string;
  source: "builtin" | "file" | "extension";
  origin: string;
  error: string | null;
  palette: UnitPalette | null;
  base: string | null;
  colors: Partial<UnitPalette> | null;
}

// ---------- mappings ----------

export type MappingScope = "preset" | "global";
export interface PerScope<T> {
  preset: T;
  global: T;
}
export interface RowRef {
  scope: MappingScope;
  id: string;
}
export interface MappingsView {
  preset: string;
  rows: PerScope<MappingRow[]>;
  triggers: PerScope<TriggerChoice[]>;
  actions: PerScope<ActionGroup[]>;
  focus: RowRef | null;
  learning: RowRef | null;
  notice: string | null;
}
export type MappingTriggerView =
  | { kind: "presetLoaded" }
  | { kind: "expression"; pedal: number }
  | { kind: "cc"; number: number }
  | { kind: "note"; number: number }
  | { kind: "extension"; key: string; trigger: string; source: string };
export type ModeKind = "onPress" | "follow" | "relative" | "set" | "toggle" | "momentary" | "step";
export interface ModeView {
  kind: ModeKind;
  label: string;
  detail: string;
}
export type FieldKey = "value" | "a" | "b" | "amount" | "wrap" | "min" | "max" | "pickup" | "encoding" | "low" | "high";
export interface ValueField {
  id: FieldKey;
  label: string;
  text: string;
}
export interface MappingRow {
  /** Opaque: send it back unchanged, never store it. */
  id: string;
  trigger: MappingTriggerView;
  triggerLabel: string;
  triggerDetail: string;
  learns: boolean;
  /** Opaque action key; null = no action yet. */
  action: string | null;
  actionLabel: string;
  actionDetail: string;
  sendMidi: boolean;
  mode: ModeView | null;
  modes: ModeView[];
  fields: ValueField[];
  summary: string;
  response: {
    low: number;
    high: number;
    line: [number, number] | null;
    level: number | null;
    current: number | null;
    waiting: boolean;
  } | null;
}
export type TriggerChoice =
  | { kind: "presetLoaded" | "expression" | "midi"; label: string }
  | { kind: "extension"; key: string; trigger: string; label: string };
export interface ActionGroup {
  label: string;
  actions: { key: string; label: string }[];
}
export type MappingTriggerInput =
  | { kind: "presetLoaded" }
  | { kind: "expression"; pedal: number }
  | { kind: "cc"; number: number }
  | { kind: "note"; number: number }
  | { kind: "extension"; key: string; trigger: string };
export type MappingCommand =
  | { op: "add"; scope: MappingScope }
  | { op: "trigger"; scope: MappingScope; id: string; trigger: MappingTriggerInput }
  | { op: "action"; scope: MappingScope; id: string; action: string }
  | { op: "mode"; scope: MappingScope; id: string; mode: ModeKind }
  | { op: "step"; scope: MappingScope; id: string; field: FieldKey; ticks: number }
  | { op: "learn"; scope: MappingScope; id: string; on: boolean }
  | { op: "remove"; scope: MappingScope; id: string };

// ---------- footswitch (Custom mode) ----------

export type LayoutScope = "global" | "preset";
export type LooperButton =
  | "recordOverdub" | "playStop" | "undo" | "reverse" | "halfSpeed" | "oneShot" | "punchIn" | "duplicate";
export type SystemAction =
  | "nextPreset" | "previousPreset" | "nextScene" | "previousScene"
  | "tuner" | "tapTempo" | "nextMode" | "gigView"
  | { scene: number }
  | { mode: "preset" | "scene" | "stomp" | "custom" }
  | { looper: LooperButton }
  | { customPage: "next" | "previous" | { go: number } }
  | { customSlot: number };
/** A label from the unit, such as "Preset" or "Tuner". A newer Tetrad can send values this app does not know. */
export type SystemGroup = string;
/** A part that a preset leaves out is null: the unit sends what the preset has. */
type MidiPart = number | null;
export type MidiMessage =
  | { kind: "cc"; channel: number; values: [MidiPart, MidiPart, MidiPart] }
  | { kind: "ccToggle"; channel: number; values: [MidiPart, MidiPart, MidiPart] }
  | { kind: "pc"; channel: number; values: [MidiPart, MidiPart, MidiPart] };
export type SlotActionView =
  | { kind: "system"; action: SystemAction }
  | { kind: "block"; row: number; column: number; parameter: number | null }
  | { kind: "loadPreset" }
  | { kind: "sendMidi" }
  | { kind: "command"; key: string; command: string }
  /** THIS PRESET only: the switch plays nothing, not the ALL PRESETS switch. */
  | { kind: "nothing" };
/** One action of a switch, with a mode and values as a Mappings row has. */
export interface SlotActionRow {
  action: SlotActionView;
  label: string;
  detail: string;
  available: boolean;
  mode: ModeView | null;
  /** Empty: the mode is fixed. */
  modes: ModeView[];
  /** A switch gets value, a, b, amount and wrap. */
  fields: ValueField[];
  summary: string;
  /** Send MIDI only. */
  messages: MidiMessage[];
}
export interface SlotView {
  /** The first action; null = empty. */
  action: SlotActionView | null;
  /** Up to MAX_SLOT_ACTIONS, run in order. */
  actions: SlotActionRow[];
  label: string;
  customLabel: string | null;
  /** The first action's detail, with " · +N" when more follow. */
  detail: string;
  colour: string;
  swatch: number | null;
  engaged: boolean;
  available: boolean;
}
/** One page: 10 slots, and the combos of adjacent switches (a combo shows as a slot). */
export interface LayoutPage {
  slots: SlotView[];
  combos: (SlotView & { switches: [number, number] })[];
}
export interface FootswitchBlock {
  row: number;
  column: number;
  name: string;
  location: string;
  parameters: { index: number; name: string }[];
}
/** The Custom mode is part of Tetrad: `mode` is always there ("Custom", extension "Tetrad"). */
export interface FootswitchView {
  mode: { name: string; extension: string; pages: number; page: number; active: boolean };
  preset: string;
  presetLoaded: boolean;
  /** 8 LED colours. */
  colours: string[];
  /** `preset` is null only when no preset is loaded. A THIS PRESET press, double press or combo
   *  without actions plays the ALL PRESETS one ("switch by switch"). */
  layouts: { global: LayoutPage[]; preset: LayoutPage[] | null };
  systems: { group: SystemGroup; label: string; action: SystemAction }[];
  blocks: FootswitchBlock[];
  commands: { key: string; command: string; label: string }[];
  setlists: string[];
  presets?: { setlist: string; slots: { slot: number; label: string }[] };
}
export type SlotActionInput =
  | { kind: "system"; action: SystemAction }
  | { kind: "block"; row: number; column: number; parameter?: number | null }
  | { kind: "loadPreset" }
  | { kind: "preset"; setlist: string; slot: number }
  | { kind: "sendMidi" }
  | { kind: "command"; key: string; command: string }
  | { kind: "nothing" };
/** `index` names an action of the slot (0 without it); `action` at the number of actions adds one. */
export type SlotEdit =
  | { kind: "action"; index?: number; action: SlotActionInput }
  | { kind: "remove"; index: number }
  | { kind: "mode"; index?: number; mode: ModeKind }
  /** ticks -127..=127, not 0. */
  | { kind: "step"; index?: number; field: FieldKey; ticks: number }
  | { kind: "messages"; index?: number; messages: MidiMessage[] }
  | { kind: "clear" }
  | { kind: "label"; label: string }
  | { kind: "colour"; colour: number | null }
  | { kind: "copyTo"; page: number; slot: number }
  | { kind: "moveTo"; page: number; slot: number };
export type FootswitchCommand =
  | { op: "slot"; scope: LayoutScope; page: number; slot: number; edit: SlotEdit }
  | { op: "copyPage"; scope: LayoutScope; from: number; to: number }
  | { op: "presets"; setlist: string };

/** The most actions a switch runs. */
export const MAX_SLOT_ACTIONS = 8;
/** Slot index per switch: A–H, then Down and Tap. */
export const SLOT_NAMES = ["A", "B", "C", "D", "E", "F", "G", "H", "Down", "Tap"] as const;
/** On-screen order: A B C D Down / E F G H Tap. */
export const SLOT_ORDER = [0, 1, 2, 3, 8, 4, 5, 6, 7, 9] as const;
