/**
 * The unit's replies and commands, as the Remote API (PR #42) and the Tetrad Link send them.
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

export type Subsystem =
  | "Platform" | "Splash" | "Settings" | "Library" | "Dsp" | "Preset" | "Ui" | "Control"
  | "Input" | "Midi" | "Services" | "Capture" | "Power" | "Extensions" | "Remote" | "Usb"
  | "Bluetooth";
export interface Notice {
  /** Seconds before the reply. */
  ago: number;
  subsystem: Subsystem;
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
  | { kind: "stomp"; stomp: number }
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
  | { kind: "presetLoaded" | "stomp" | "expression" | "midi"; label: string }
  | { kind: "extension"; key: string; trigger: string; label: string };
export interface ActionGroup {
  label: string;
  actions: { key: string; label: string }[];
}
export type MappingTriggerInput =
  | { kind: "presetLoaded" }
  | { kind: "stomp"; stomp: number }
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
export type SystemGroup = "Preset" | "Scene" | "Tuner" | "Tempo" | "Mode" | "Gig View" | "Looper" | "Page";
export type MidiMessage =
  | { kind: "cc"; channel: number; values: [number, number, number | null] }
  | { kind: "ccToggle"; channel: number; values: [number, number, number] }
  | { kind: "pc"; channel: number; values: [number | null, number | null, number] };
export type SlotActionView =
  | { kind: "system"; action: SystemAction }
  | { kind: "block"; row: number; column: number; parameter: number | null }
  | { kind: "loadPreset" }
  | { kind: "sendMidi" }
  | { kind: "command"; key: string; command: string };
export interface SlotView {
  action: SlotActionView | null;
  label: string;
  customLabel: string | null;
  detail: string;
  colour: string;
  swatch: number | null;
  engaged: boolean;
  available: boolean;
  value: number | null;
  momentary: boolean | null;
  messages: MidiMessage[];
}
export interface FootswitchBlock {
  row: number;
  column: number;
  name: string;
  location: string;
  parameters: { index: number; name: string }[];
}
export interface FootswitchMode {
  mode: { name: string; extension: string; pages: number; page: number; active: boolean };
  preset: string;
  presetLoaded: boolean;
  /** 8 LED colours. */
  colours: string[];
  layouts: { global: SlotView[][]; preset: SlotView[][] | null };
  systems: { group: SystemGroup; label: string; action: SystemAction }[];
  blocks: FootswitchBlock[];
  commands: { key: string; command: string; label: string }[];
  setlists: string[];
  presets?: { setlist: string; slots: { slot: number; label: string }[] };
}
export type FootswitchView = { mode: null } | FootswitchMode;
export type SlotActionInput =
  | { kind: "system"; action: SystemAction }
  | { kind: "block"; row: number; column: number; parameter?: number | null }
  | { kind: "loadPreset" }
  | { kind: "preset"; setlist: string; slot: number }
  | { kind: "sendMidi" }
  | { kind: "command"; key: string; command: string };
export type SlotEdit =
  | { kind: "action"; action: SlotActionInput }
  | { kind: "clear" }
  | { kind: "label"; label: string }
  | { kind: "colour"; colour: number | null }
  | { kind: "value"; value: number }
  | { kind: "momentary"; momentary: boolean }
  | { kind: "messages"; messages: MidiMessage[] }
  | { kind: "copyTo"; page: number; slot: number }
  | { kind: "moveTo"; page: number; slot: number };
export type FootswitchCommand =
  | { op: "slot"; scope: LayoutScope; page: number; slot: number; edit: SlotEdit }
  | { op: "copyPage"; scope: LayoutScope; from: number; to: number }
  | { op: "presetLayout"; own: boolean }
  | { op: "presets"; setlist: string };

/** Slot index per switch: A–H, then Down and Tap. */
export const SLOT_NAMES = ["A", "B", "C", "D", "E", "F", "G", "H", "Down", "Tap"] as const;
/** On-screen order: A B C D Down / E F G H Tap. */
export const SLOT_ORDER = [0, 1, 2, 3, 8, 4, 5, 6, 7, 9] as const;
