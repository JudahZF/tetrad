/**
 * The run-time shape of each reply from the unit. The app and Tetrad are released separately:
 * a reply that does not match is an error message, not a crash in a screen. Each schema is
 * typed against its interface in `types.ts`, so the compiler finds a difference.
 *
 * `z.object` ignores fields it does not know: a newer Tetrad may add fields.
 */

import { z } from "zod";
import type { Hello as HelloReply } from "./link";
import type * as T from "./types";

export const StatusReply: z.ZodType<T.StatusReply> = z.object({ folder: z.string() });

export const IrAddedReply: z.ZodType<T.IrAddedReply> = z.object({ name: z.string(), folder: z.string() });

export const IrLibrary: z.ZodType<T.IrLibrary> = z.object({
  destination: z.number(),
  folders: z.array(z.object({ id: z.number(), name: z.string(), parent: z.number(), root: z.boolean() })),
  irs: z.array(z.object({ id: z.string(), name: z.string(), date: z.string(), folders: z.array(z.number()) })),
});

export const FilesReply: z.ZodType<T.FilesReply> = z.object({
  extensions: z.array(
    z.object({
      id: z.string(),
      name: z.string(),
      label: z.string(),
      extensions: z.array(z.string()),
      maxBytes: z.number(),
      files: z.array(
        z.object({ name: z.string(), size: z.number(), state: z.enum(["ready", "waiting", "failed"]), detail: z.string() }),
      ),
    }),
  ),
});

export const FileChunkReply: z.ZodType<T.FileChunkReply> = z.union([
  z.object({ received: z.number() }),
  z.object({ name: z.string(), size: z.number() }),
]);

export const TabsReply: z.ZodType<{ tabs: T.ExtensionTab[] }> = z.object({
  tabs: z.array(z.object({ id: z.string(), title: z.string() })),
});

export const TabPageReply: z.ZodType<{ html: string }> = z.object({ html: z.string() });

/** 200 can still carry `{ error }` when the extension answers with bad JSON. */
export const TabCallReply: z.ZodType<{ result?: unknown; error?: string }> = z.object({
  result: z.unknown().optional(),
  error: z.string().optional(),
});

export const NoticesReply: z.ZodType<T.NoticesReply> = z.object({
  // A label that the app only shows stays a string, so a newer Tetrad can add one.
  notices: z.array(z.object({ ago: z.number(), subsystem: z.string(), detail: z.string() })),
  dropped: z.number(),
  repeats: z.number(),
});

export const BackupStatus: z.ZodType<T.BackupStatus> = z.object({
  state: z.enum(["idle", "creating", "ready", "receiving", "received", "restoring", "restored"]),
  name: z.string().nullable(),
  size: z.number().nullable(),
  received: z.number().nullable(),
  error: z.string().nullable(),
});

const ExtensionSettingView: z.ZodType<T.ExtensionSettingView> = z.discriminatedUnion("kind", [
  z.object({ key: z.string(), label: z.string(), kind: z.literal("choice"), options: z.array(z.string()), value: z.number() }),
  z.object({ key: z.string(), label: z.string(), kind: z.literal("toggle"), value: z.boolean() }),
  z.object({ key: z.string(), label: z.string(), kind: z.literal("number"), value: z.number(), min: z.number(), max: z.number() }),
]);

export const ExtensionsList: z.ZodType<T.ExtensionsList> = z.object({
  installed: z.array(
    z.object({
      key: z.string(),
      name: z.string(),
      version: z.string(),
      description: z.string(),
      source: z.string(),
      enabled: z.boolean(),
      state: z.enum(["off", "starting", "running", "faulted"]),
      fault: z.string().nullable(),
      update: z.string().nullable(),
      duplicates: z.array(z.string()),
      commands: z.array(z.object({ id: z.string(), label: z.string() })),
      settings: z.array(ExtensionSettingView),
      status: z.array(z.string()),
      blocks: z.array(z.object({ name: z.string(), detail: z.string(), ready: z.boolean() })),
      log: z.array(z.string()),
      themes: z.array(z.string()),
      mode: z.object({ name: z.string(), detail: z.string() }).nullable(),
      app: z.string().nullable(),
    }),
  ),
  available: z.array(
    z.object({
      source: z.string(),
      id: z.string(),
      name: z.string(),
      version: z.string(),
      description: z.string(),
      duplicates: z.array(z.string()),
      app: z.boolean(),
    }),
  ),
  sources: z.array(z.object({ repository: z.string(), default: z.boolean(), enabled: z.boolean(), status: z.string() })),
  status: z.string().nullable(),
  busy: z.boolean(),
});

const PaletteRole = z.enum([
  "background", "canvas", "panel", "surface", "rowSelected", "section", "divider",
  "scrollbar", "text", "muted", "faint", "cable", "accent", "accentFill", "onAccent",
  "action", "danger", "warning",
]) satisfies z.ZodType<T.PaletteRole>;

export const ThemesList: z.ZodType<T.ThemesList> = z.object({
  active: z.string().nullable(),
  fields: z.array(z.object({ key: PaletteRole, label: z.string(), help: z.string(), derived: z.boolean() })),
  themes: z.array(
    z.object({
      id: z.string(),
      name: z.string(),
      source: z.enum(["builtin", "file", "extension"]),
      origin: z.string(),
      error: z.string().nullable(),
      palette: z.record(PaletteRole, z.string()).nullable(),
      base: z.string().nullable(),
      colors: z.partialRecord(PaletteRole, z.string()).nullable(),
    }),
  ),
});

// ---------- mappings ----------

const ModeView: z.ZodType<T.ModeView> = z.object({
  kind: z.enum(["onPress", "follow", "relative", "set", "toggle", "momentary", "step"]),
  label: z.string(),
  detail: z.string(),
});

const ValueField: z.ZodType<T.ValueField> = z.object({
  id: z.enum(["value", "a", "b", "amount", "wrap", "min", "max", "pickup", "encoding", "low", "high"]),
  label: z.string(),
  text: z.string(),
});

const MappingScope: z.ZodType<T.MappingScope> = z.enum(["preset", "global"]);
const RowRef: z.ZodType<T.RowRef> = z.object({ scope: MappingScope, id: z.string() });

const MappingRow: z.ZodType<T.MappingRow> = z.object({
  id: z.string(),
  trigger: z.discriminatedUnion("kind", [
    z.object({ kind: z.literal("presetLoaded") }),
    z.object({ kind: z.literal("expression"), pedal: z.number() }),
    z.object({ kind: z.literal("cc"), number: z.number() }),
    z.object({ kind: z.literal("note"), number: z.number() }),
    z.object({ kind: z.literal("extension"), key: z.string(), trigger: z.string(), source: z.string() }),
  ]),
  triggerLabel: z.string(),
  triggerDetail: z.string(),
  learns: z.boolean(),
  action: z.string().nullable(),
  actionLabel: z.string(),
  actionDetail: z.string(),
  sendMidi: z.boolean(),
  mode: ModeView.nullable(),
  modes: z.array(ModeView),
  fields: z.array(ValueField),
  summary: z.string(),
  response: z
    .object({
      low: z.number(),
      high: z.number(),
      line: z.tuple([z.number(), z.number()]).nullable(),
      level: z.number().nullable(),
      current: z.number().nullable(),
      waiting: z.boolean(),
    })
    .nullable(),
});

const TriggerChoice: z.ZodType<T.TriggerChoice> = z.discriminatedUnion("kind", [
  z.object({ kind: z.enum(["presetLoaded", "expression", "midi"]), label: z.string() }),
  z.object({ kind: z.literal("extension"), key: z.string(), trigger: z.string(), label: z.string() }),
]);

const ActionGroup: z.ZodType<T.ActionGroup> = z.object({
  label: z.string(),
  actions: z.array(z.object({ key: z.string(), label: z.string() })),
});

export const MappingsView: z.ZodType<T.MappingsView> = z.object({
  preset: z.string(),
  rows: z.object({ preset: z.array(MappingRow), global: z.array(MappingRow) }),
  triggers: z.object({ preset: z.array(TriggerChoice), global: z.array(TriggerChoice) }),
  actions: z.object({ preset: z.array(ActionGroup), global: z.array(ActionGroup) }),
  focus: RowRef.nullable(),
  learning: RowRef.nullable(),
  notice: z.string().nullable(),
});

// ---------- footswitch (Custom mode) ----------

const SystemAction: z.ZodType<T.SystemAction> = z.union([
  z.enum(["nextPreset", "previousPreset", "nextScene", "previousScene", "tuner", "tapTempo", "nextMode", "gigView"]),
  z.object({ scene: z.number() }),
  z.object({ mode: z.enum(["preset", "scene", "stomp", "custom"]) }),
  z.object({
    looper: z.enum(["recordOverdub", "playStop", "undo", "reverse", "halfSpeed", "oneShot", "punchIn", "duplicate"]),
  }),
  z.object({ customPage: z.union([z.enum(["next", "previous"]), z.object({ go: z.number() })]) }),
  z.object({ customSlot: z.number() }),
]);

const SlotActionView: z.ZodType<T.SlotActionView> = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("system"), action: SystemAction }),
  z.object({ kind: z.literal("block"), row: z.number(), column: z.number(), parameter: z.number().nullable() }),
  z.object({ kind: z.literal("loadPreset") }),
  z.object({ kind: z.literal("sendMidi") }),
  z.object({ kind: z.literal("command"), key: z.string(), command: z.string() }),
  z.object({ kind: z.literal("nothing") }),
]);

const MidiMessage: z.ZodType<T.MidiMessage> = z.discriminatedUnion("kind", [
  z.object({ kind: z.literal("cc"), channel: z.number(), values: z.tuple([z.number(), z.number(), z.number().nullable()]) }),
  z.object({ kind: z.literal("ccToggle"), channel: z.number(), values: z.tuple([z.number(), z.number(), z.number()]) }),
  z.object({ kind: z.literal("pc"), channel: z.number(), values: z.tuple([z.number().nullable(), z.number().nullable(), z.number()]) }),
]);

const SlotView = z.object({
  action: SlotActionView.nullable(),
  actions: z.array(
    z.object({
      action: SlotActionView,
      label: z.string(),
      detail: z.string(),
      available: z.boolean(),
      mode: ModeView.nullable(),
      modes: z.array(ModeView),
      fields: z.array(ValueField),
      summary: z.string(),
      messages: z.array(MidiMessage),
    }),
  ),
  label: z.string(),
  customLabel: z.string().nullable(),
  detail: z.string(),
  colour: z.string(),
  swatch: z.number().nullable(),
  engaged: z.boolean(),
  available: z.boolean(),
}) satisfies z.ZodType<T.SlotView>;

const LayoutPage: z.ZodType<T.LayoutPage> = z.object({
  slots: z.array(SlotView),
  combos: z.array(SlotView.extend({ switches: z.tuple([z.number(), z.number()]) })),
});

export const FootswitchView: z.ZodType<T.FootswitchView> = z.object({
  mode: z.object({ name: z.string(), extension: z.string(), pages: z.number(), page: z.number(), active: z.boolean() }),
  preset: z.string(),
  presetLoaded: z.boolean(),
  colours: z.array(z.string()),
  layouts: z.object({ global: z.array(LayoutPage), preset: z.array(LayoutPage).nullable() }),
  systems: z.array(
    z.object({
      // A label that the app only shows stays a string, so a newer Tetrad can add one.
      group: z.string(),
      label: z.string(),
      action: SystemAction,
    }),
  ),
  blocks: z.array(
    z.object({
      row: z.number(),
      column: z.number(),
      name: z.string(),
      location: z.string(),
      parameters: z.array(z.object({ index: z.number(), name: z.string() })),
    }),
  ),
  commands: z.array(z.object({ key: z.string(), command: z.string(), label: z.string() })),
  setlists: z.array(z.string()),
  /** Only in the answer to a `presets` command. */
  presets: z
    .object({ setlist: z.string(), slots: z.array(z.object({ slot: z.number(), label: z.string() })) })
    .optional(),
});

// ---------- Tetrad Link ----------

export const Hello: z.ZodType<HelloReply> = z.object({
  tetrad: z.string(),
  protocol: z.number(),
  name: z.string(),
  authorized: z.boolean(),
});
