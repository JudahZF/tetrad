/**
 * A unit that lives in the app: every feature, with the state of the mockup. It answers as the
 * unit does (full views after each command, refusals as text), so the app can be tried, shown
 * and tested with no Quad Cortex.
 */

import { DEMO_CATALOG } from "@/lib/demo/catalog";
import {
  DEMO_ACTIONS,
  type DemoMapping,
  MODES,
  demoExtensions,
  demoFiles,
  demoFootswitch,
  demoLibrary,
  demoMappings,
  demoNotices,
  demoPreset,
  demoTabs,
  demoThemes,
} from "@/lib/demo/data";
import { EditError, applyEdit } from "@/lib/preset/edit";
import type { Edit, Preset } from "@/lib/preset/model";
import { BaseClient } from "./base";
import { type Feature, type UnitClient, UnitError } from "./client";
import * as T from "./types";

const ALL: ReadonlySet<Feature> = new Set<Feature>(["preset", "extension-messages"]);

const refuse = (text: string): never => {
  throw new UnitError(text);
};

/** An empty zip (end of central directory only): what the demo hands out as its support file. */
const EMPTY_ZIP = Uint8Array.from([0x50, 0x4b, 0x05, 0x06, ...Array(18).fill(0)]);

export class DemoClient extends BaseClient implements UnitClient {
  readonly transport = "demo" as const;
  readonly name = "Demo Quad Cortex";
  protected readonly features = ALL;
  private presetState: Preset = demoPreset();
  private readonly lib = demoLibrary();
  private readonly fileList = demoFiles();
  private readonly ext = demoExtensions();
  private readonly themeList = demoThemes();
  private readonly fs = demoFootswitch();
  private readonly maps = demoMappings();
  private nextMapping = 2;
  private learning: T.RowRef | null = null;
  private notice: string | null = null;
  private focus: T.RowRef | null = null;
  private backupState: T.BackupStatus = { state: "idle", name: null, size: null, received: null, error: null };
  private readonly pending = new Map<string, Uint8Array[]>();
  private readonly timers = new Set<ReturnType<typeof setTimeout>>();

  constructor(private readonly latencyMs = 120) {
    super();
    this.version = "0.1.0-alpha.9 (demo)";
  }

  private later(ms: number, run: () => void) {
    const timer = setTimeout(() => {
      this.timers.delete(timer);
      run();
    }, ms);
    this.timers.add(timer);
  }

  private async answer<R>(make: () => R): Promise<R> {
    if (this.latencyMs > 0) await new Promise((resolve) => setTimeout(resolve, this.latencyMs));
    return structuredClone(make());
  }

  override async close() {
    for (const timer of this.timers) clearTimeout(timer);
    this.timers.clear();
  }

  // ---------- IRs ----------

  status() {
    return this.answer(() => ({ folder: this.folderName(this.lib.destination) }));
  }

  private folderName(id: number) {
    return this.lib.folders.find((f) => f.id === id)?.name ?? "IRs";
  }

  uploadIr(name: string, wav: Uint8Array) {
    return this.answer(() => {
      const clean = name.replace(/\s+/g, " ").trim();
      if (clean.length < 1 || clean.length > 60) refuse("An IR name has 1 to 60 characters");
      if (wav.length < 46 || String.fromCharCode(...wav.subarray(0, 4)) !== "RIFF") refuse("Not an IR the unit plays");
      const folders = [...new Set([1, this.lib.destination])];
      this.lib.irs.unshift({ id: `ir-${Date.now()}-${this.lib.irs.length}`, name: clean, date: new Date().toISOString().slice(0, 19) + "Z", folders });
      return { name: clean, folder: this.folderName(this.lib.destination) };
    });
  }

  library() {
    return this.answer(() => this.lib);
  }

  libraryOp(op: T.LibraryOp) {
    return this.answer(() => {
      const ir = "id" in op ? this.lib.irs.find((i) => i.id === op.id) : undefined;
      switch (op.op) {
        case "rename":
          if (!ir) refuse("That IR is gone");
          if (op.name.trim().length < 4 || op.name.trim().length > 60) refuse("A name has 4 to 60 characters");
          if (ir) ir.name = op.name.trim();
          break;
        case "delete":
          this.lib.irs = this.lib.irs.filter((i) => i.id !== op.id);
          break;
        case "link":
          if (!ir) refuse("That IR is gone");
          if (op.folder === 1 && !op.linked) refuse("Every IR stays in the IR root");
          if (ir) ir.folders = op.linked ? [...new Set([...ir.folders, op.folder])] : ir.folders.filter((f) => f !== op.folder);
          break;
        case "destination":
          if (!this.lib.folders.some((f) => f.id === op.folder)) refuse("No such folder");
          this.lib.destination = op.folder;
          break;
        case "folder": {
          const name = op.name.trim();
          if (name.length < 4 || name.length > 60) refuse("A name has 4 to 60 characters");
          const id = Math.max(...this.lib.folders.map((f) => f.id)) + 1;
          this.lib.folders.push({ id, name, parent: op.parent ?? 1, root: false });
          break;
        }
      }
      return this.lib;
    });
  }

  // ---------- extension files ----------

  files() {
    return this.answer(() => this.fileList);
  }

  fileChunk(extension: string, name: string, offset: number, last: boolean, bytes: Uint8Array) {
    return this.answer((): T.FileChunkReply => {
      const ext = this.fileList.extensions.find((e) => e.id === extension) ?? refuse(`No extension ${extension} takes files`);
      if (!ext.extensions.some((x) => name.toLowerCase().endsWith(x))) refuse(`${ext.name} takes ${ext.extensions.join(", ")} files`);
      const key = `${extension}/${name}`;
      const parts = offset === 0 ? [] : (this.pending.get(key) ?? refuse("Send the file from its start"));
      const have = parts.reduce((n, p) => n + p.length, 0);
      if (offset !== have) refuse(`Out of order: the unit has ${have} bytes`);
      parts.push(bytes.slice());
      this.pending.set(key, parts);
      const size = have + bytes.length;
      if (size > ext.maxBytes) refuse(`${name} is larger than 3 MiB`);
      if (!last) return { received: size };
      this.pending.delete(key);
      if (size === 0) refuse(`${name} is empty`);
      ext.files = ext.files.filter((f) => f.name !== name);
      ext.files.push({ name, size, state: "waiting", detail: "" });
      this.later(1500, () => {
        const file = ext.files.find((f) => f.name === name);
        if (file) file.state = "ready";
      });
      return { name, size };
    });
  }

  async fileDelete(extension: string, name: string) {
    await this.answer(() => {
      const ext = this.fileList.extensions.find((e) => e.id === extension);
      if (ext) ext.files = ext.files.filter((f) => f.name !== name);
    });
  }

  // ---------- tabs and extensions ----------

  tabs() {
    return this.answer(() => demoTabs().tabs);
  }

  tabPage(extension: string) {
    return this.answer(() => demoTabs().html[extension] ?? refuse("That extension has no page"));
  }

  tabCall(_extension: string, message: unknown) {
    return this.answer(() => (message && typeof message === "object" && "op" in message && message.op === "reset" ? { learned: 0 } : null));
  }

  override extensionMessage(extension: string, message: unknown) {
    return this.answer(() => ({ extension, echo: message }));
  }

  support() {
    return this.answer(() => ({ name: "tetrad-support-demo.zip", zip: EMPTY_ZIP }));
  }

  notices() {
    return this.answer(demoNotices);
  }

  extensions() {
    return this.answer(() => this.ext);
  }

  extensionCommand(command: T.ExtensionCommand) {
    return this.answer(() => {
      const find = (key: string) => this.ext.installed.find((e) => e.key === key) ?? refuse(`${key} is not installed`);
      switch (command.op) {
        case "refresh":
          this.ext.sources.forEach((s) => (s.status = "Up to date"));
          break;
        case "install": {
          const offer = this.ext.available.find((a) => a.source === command.source && a.id === command.id) ?? refuse(`${command.source} does not offer ${command.id}; refresh the list`);
          this.ext.available = this.ext.available.filter((a) => a !== offer);
          this.ext.installed.push({
            key: `${offer.source}:${offer.id}`, name: offer.name, version: offer.version, description: offer.description,
            source: offer.source, enabled: true, state: "starting", fault: null, update: null, duplicates: [], commands: [],
            settings: [], status: [], blocks: [], log: [], themes: [], mode: null, app: offer.app ? `Play ${offer.name}` : null,
          });
          // As on the unit, the install is a job: the list is busy, then the extension starts.
          this.ext.busy = true;
          this.ext.status = `Installing ${offer.name} ${offer.version}…`;
          const key = `${offer.source}:${offer.id}`;
          this.later(1500, () => {
            this.ext.busy = false;
            this.ext.status = `${offer.name} ${offer.version} installed`;
            const installed = this.ext.installed.find((e) => e.key === key);
            if (installed) installed.state = "running";
          });
          break;
        }
        case "update": {
          const e = find(command.key);
          if (!e.update) refuse(`${e.name} is up to date`);
          e.version = e.update ?? e.version;
          e.update = null;
          this.ext.status = `${e.name} updated to ${e.version}`;
          break;
        }
        case "enable":
        case "disable": {
          const e = find(command.key);
          e.enabled = command.op === "enable";
          e.state = e.enabled ? "running" : "off";
          break;
        }
        case "retry":
          find(command.key).state = "running";
          break;
        case "remove":
          this.ext.installed = this.ext.installed.filter((e) => e.key !== command.key);
          break;
        case "run": {
          const e = find(command.key);
          const cmd = e.commands.find((c) => c.id === command.command) ?? refuse(`${e.name} has no command ${command.command}`);
          e.log = [...e.log, `Ran: ${cmd.label}`].slice(-20);
          break;
        }
        case "setting": {
          const e = find(command.key);
          e.settings = e.settings.map((s) => (s.key === command.setting ? ({ ...s, value: command.value } as T.ExtensionSettingView) : s));
          break;
        }
      }
      return this.ext;
    });
  }

  // ---------- themes and backup ----------

  themes() {
    return this.answer(() => this.themeList);
  }

  themeSelect(id: string) {
    return this.answer(() => {
      if (!this.themeList.themes.some((t) => t.id === id)) refuse(`No theme ${id}`);
      this.themeList.active = id;
      return this.themeList;
    });
  }

  backup() {
    return this.answer(() => this.backupState);
  }

  backupCreate() {
    return this.answer(() => {
      if (this.backupState.state === "received") refuse("A backup waits on the unit: restore or discard it first");
      this.backupState = { state: "creating", name: null, size: null, received: null, error: null };
      this.later(1500, () => {
        this.backupState = { state: "ready", name: "QC-Backup-demo.tar.gz", size: DEMO_BACKUP.length, received: null, error: null };
      });
      return this.backupState;
    });
  }

  backupPiece(offset: number) {
    return this.answer(() => {
      if (this.backupState.state !== "ready") refuse("No backup is ready");
      return DEMO_BACKUP.slice(offset, offset + 512 * 1024);
    });
  }

  backupUpload(name: string, offset: number, last: boolean, bytes: Uint8Array) {
    return this.answer(() => {
      const have = offset === 0 ? 0 : (this.backupState.received ?? 0);
      if (offset !== have) refuse(`Out of order: the unit has ${have} bytes`);
      if (offset === 0 && bytes[0] !== 0x1f) refuse("A backup is a .tar.gz file");
      const size = have + bytes.length;
      this.backupState = last
        ? { state: "received", name, size, received: null, error: null }
        : { state: "receiving", name, size: null, received: size, error: null };
      return this.backupState;
    });
  }

  backupDiscard() {
    return this.answer(() => {
      this.backupState = { state: "idle", name: null, size: null, received: null, error: null };
      return this.backupState;
    });
  }

  // ---------- mappings ----------

  private mappingRow(m: DemoMapping): T.MappingRow {
    const action = DEMO_ACTIONS.flatMap((g) => g.actions.map((a) => ({ ...a, group: g.label }))).find((a) => a.key === m.action);
    const trigger = triggerText(m.trigger);
    const learns = m.trigger.kind === "cc" || m.trigger.kind === "note" || m.trigger.kind === "extension";
    const isParameter = m.action?.includes("Parameter") ?? false;
    const modes: T.ModeKind[] =
      m.trigger.kind === "cc" ? (isParameter ? ["follow", "relative", "set", "toggle", "step"] : ["follow", "set", "toggle"])
      : m.trigger.kind === "presetLoaded" ? ["set", "toggle"]
      : isParameter ? ["set", "toggle", "momentary", "step"] : ["onPress", "toggle", "momentary"];
    const text = (f: DemoMapping["fields"][number]) => `${f.value}${f.unit ? ` ${f.unit}` : ""}`;
    return {
      id: m.id,
      trigger: m.trigger,
      triggerLabel: trigger.label,
      triggerDetail: trigger.detail,
      learns,
      action: m.action,
      actionLabel: action ? (action.group === "System" || action.group === "MIDI out" ? action.label : `${action.group.split(" · ")[0]} · ${action.label}`) : "",
      actionDetail: action?.group ?? "",
      sendMidi: m.action === "SendMidi",
      mode: m.mode ? MODES[m.mode] : null,
      modes: m.action ? modes.map((k) => MODES[k]) : [],
      fields: m.fields.map((f) => ({ id: f.id, label: f.label, text: text(f) })),
      summary: m.fields.length === 0 ? "" : m.mode === "toggle" ? m.fields.map(text).join(" ↔ ") : m.fields.map((f) => `${f.label} ${text(f)}`).join(" · "),
      response: null,
    };
  }

  private mappingView(): T.MappingsView {
    const rows = (scope: T.MappingScope) => this.maps[scope].map((m) => this.mappingRow(m));
    return {
      preset: this.presetState.name,
      rows: { preset: rows("preset"), global: rows("global") },
      triggers: {
        preset: [
          { kind: "presetLoaded", label: "Preset loaded" },
          { kind: "stomp", label: "Footswitch" },
          { kind: "expression", label: "Expression pedal" },
          { kind: "midi", label: "MIDI (QC channel)" },
        ],
        global: [{ kind: "midi", label: "MIDI (QC channel)" }],
      },
      actions: { preset: DEMO_ACTIONS, global: DEMO_ACTIONS.filter((g) => g.label === "System") },
      focus: this.focus,
      learning: this.learning,
      notice: this.notice,
    };
  }

  mappings() {
    return this.answer(() => this.mappingView());
  }

  mappingCommand(command: T.MappingCommand) {
    return this.answer(() => {
      const list = this.maps[command.scope];
      const row = "id" in command ? (list.find((m) => m.id === command.id) ?? refuse(`No mapping ${command.id} in this list now; reload the page`)) : null;
      this.notice = null;
      switch (command.op) {
        case "add": {
          const id = `Tetrad(${this.nextMapping++})`;
          list.push({ id, trigger: command.scope === "global" ? { kind: "cc", number: 20 } : { kind: "stomp", stomp: 0 }, action: null, mode: null, fields: [] });
          this.focus = { scope: command.scope, id };
          break;
        }
        case "trigger":
          if (row) row.trigger = command.trigger.kind === "extension" ? { ...command.trigger, source: "" } : command.trigger;
          break;
        case "action":
          if (row) {
            if (!this.mappingView().actions[command.scope].some((g) => g.actions.some((a) => a.key === command.action))) refuse("That action is not in this list");
            row.action = command.action;
            row.mode = row.trigger.kind === "cc" ? "follow" : command.action.includes("Parameter") ? "set" : "onPress";
            row.fields = command.action.includes("Parameter") ? [{ id: "value", label: "Value", value: 50, unit: "%" }] : [];
          }
          break;
        case "mode":
          if (row) {
            row.mode = command.mode;
            row.fields =
              command.mode === "toggle" ? [{ id: "a", label: "A", value: 25, unit: "%" }, { id: "b", label: "B", value: 75, unit: "%" }]
              : command.mode === "follow" ? [{ id: "low", label: "Low", value: 0, unit: "" }, { id: "high", label: "High", value: 127, unit: "" }]
              : command.mode === "step" ? [{ id: "amount", label: "Amount", value: 1, unit: "" }]
              : row.action?.includes("Parameter") ? [{ id: "value", label: "Value", value: 50, unit: "%" }] : [];
          }
          break;
        case "step":
          if (row) {
            const field = row.fields.find((f) => f.id === command.field) ?? refuse(`This mapping has no ${command.field}`);
            const max = field.id === "low" || field.id === "high" ? 127 : field.unit === "%" ? 100 : 10;
            field.value = Math.min(max, Math.max(field.id === "amount" ? -10 : 0, field.value + command.ticks));
          }
          break;
        case "learn":
          if (row) {
            this.learning = command.on ? { scope: command.scope, id: row.id } : null;
            if (command.on) {
              this.later(2500, () => {
                if (this.learning?.id !== row.id) return;
                row.trigger = row.trigger.kind === "note" ? { kind: "note", number: 52 } : { kind: "cc", number: 21 };
                this.learning = null;
              });
            }
          }
          break;
        case "remove":
          this.maps[command.scope] = list.filter((m) => m !== row);
          break;
      }
      if (row && command.op !== "remove") this.focus = { scope: command.scope, id: row.id };
      return this.mappingView();
    });
  }

  // ---------- footswitches ----------

  footswitch() {
    return this.answer(() => this.fs);
  }

  private layout(scope: T.LayoutScope) {
    const layout = scope === "global" ? this.fs.layouts.global : this.fs.layouts.preset;
    return layout ?? refuse("This preset uses the layout of all presets");
  }

  footswitchCommand(command: T.FootswitchCommand) {
    return this.answer(() => {
      switch (command.op) {
        case "presetLayout":
          this.fs.layouts.preset = command.own ? structuredClone(this.fs.layouts.global) : null;
          break;
        case "copyPage": {
          const layout = this.layout(command.scope);
          const from = layout[command.from] ?? refuse(`There is no page ${command.from + 1}`);
          if (!layout[command.to]) refuse(`There is no page ${command.to + 1}`);
          layout[command.to] = structuredClone(from);
          break;
        }
        case "presets":
          return { ...this.fs, presets: { setlist: command.setlist, slots: ["1A Clean", "4B Worship", "12B Plexi Crunch"].map((label, slot) => ({ slot, label })) } };
        case "slot": {
          const layout = this.layout(command.scope);
          const page = layout[command.page] ?? refuse(`There is no page ${command.page + 1}`);
          const slot = page[command.slot] ?? refuse(`There is no switch ${command.slot + 1}`);
          const edit = command.edit;
          const at = (p: number, s: number) => layout[p]?.[s] ?? refuse(`There is no switch ${s + 1}`);
          switch (edit.kind) {
            case "clear":
              page[command.slot] = { ...slot, action: null, label: "", customLabel: null, detail: "", colour: "#3a3a3a", swatch: null, value: null, momentary: null, messages: [], engaged: false };
              break;
            case "label":
              if (edit.label.length > 16) refuse("A label has at most 16 characters");
              slot.customLabel = edit.label || null;
              slot.label = edit.label || describe(slot.action, this.fs);
              break;
            case "colour":
              if (edit.colour !== null && (edit.colour < 0 || edit.colour > 7)) refuse("There are eight colours");
              slot.swatch = edit.colour;
              slot.colour = edit.colour === null ? "#3a3a3a" : (this.fs.colours[edit.colour] ?? slot.colour);
              break;
            case "value":
              if (slot.action?.kind !== "block" || slot.action.parameter === null) refuse("Only a parameter action has a value");
              slot.value = Math.min(100, Math.max(0, Math.round(edit.value)));
              break;
            case "momentary":
              if (slot.action?.kind !== "block" || slot.action.parameter !== null) refuse("Only a bypass action can be momentary");
              slot.momentary = edit.momentary;
              slot.detail = edit.momentary ? "Bypass · momentary" : "Bypass · latching";
              break;
            case "messages":
              if (slot.action?.kind !== "sendMidi") refuse("Choose Send MIDI first");
              slot.messages = edit.messages;
              break;
            case "action": {
              const a = edit.action;
              const view: T.SlotActionView =
                a.kind === "preset" ? { kind: "loadPreset" }
                : a.kind === "block" ? { kind: "block", row: a.row, column: a.column, parameter: a.parameter ?? null }
                : a;
              if (view.kind === "block" && !this.fs.blocks.some((b) => b.row === view.row && b.column === view.column)) refuse("There is no block there");
              if (view.kind === "system" && typeof view.action === "object" && "customSlot" in view.action) refuse("A slot cannot press another slot");
              slot.action = view;
              slot.value = view.kind === "block" && view.parameter !== null ? 50 : null;
              slot.momentary = view.kind === "block" && view.parameter === null ? false : null;
              slot.label = slot.customLabel ?? describe(view, this.fs);
              slot.detail = view.kind === "block" ? (view.parameter === null ? "Bypass · latching" : "Parameter · 50 %") : view.kind === "loadPreset" ? `Load ${a.kind === "preset" ? a.setlist : this.fs.preset}` : "";
              break;
            }
            case "copyTo": {
              at(edit.page, edit.slot);
              const target = layout[edit.page];
              if (target) target[edit.slot] = structuredClone(slot);
              break;
            }
            case "moveTo": {
              const other = at(edit.page, edit.slot);
              const target = layout[edit.page];
              if (target) target[edit.slot] = slot;
              page[command.slot] = other;
              break;
            }
          }
          break;
        }
      }
      return this.fs;
    });
  }

  // ---------- the preset ----------

  override preset() {
    return this.answer(() => this.presetState);
  }

  override presetEdit(loadId: number, edit: Edit) {
    return this.answer(() => {
      if (loadId !== this.presetState.loadId) refuse("The preset changed on the unit: try again");
      try {
        this.presetState = applyEdit(this.presetState, edit, DEMO_CATALOG);
      } catch (error) {
        if (error instanceof EditError) refuse(error.message);
        throw error;
      }
      this.later(0, () => this.emit({ kind: "preset", preset: structuredClone(this.presetState) }));
      return this.presetState;
    });
  }

  override presetSave() {
    return this.answer(() => {
      this.presetState = { ...this.presetState, edited: false, loadId: this.presetState.loadId + 1 };
      return this.presetState;
    });
  }

  override blocks() {
    return this.answer(() => [...DEMO_CATALOG]);
  }
}

function triggerText(t: T.MappingTriggerView): { label: string; detail: string } {
  switch (t.kind) {
    case "presetLoaded":
      return { label: "Preset loaded", detail: "When the preset loads" };
    case "stomp":
      return { label: `Stomp ${T.SLOT_NAMES[t.stomp] ?? t.stomp}`, detail: "Footswitch press" };
    case "expression":
      return { label: `EXP ${t.pedal}`, detail: "Expression pedal" };
    case "cc":
      return { label: `CC ${t.number}`, detail: "MIDI · QC channel" };
    case "note":
      return { label: `Note ${t.number}`, detail: "MIDI · QC channel" };
    case "extension":
      return { label: t.source || "Learn…", detail: t.key };
  }
}

function describe(action: T.SlotActionView | null, fs: T.FootswitchMode): string {
  if (!action) return "";
  switch (action.kind) {
    case "system":
      return fs.systems.find((s) => JSON.stringify(s.action) === JSON.stringify(action.action))?.label ?? "System";
    case "block": {
      const block = fs.blocks.find((b) => b.row === action.row && b.column === action.column);
      const param = block?.parameters.find((p) => p.index === action.parameter);
      return param ? param.name : (block?.name ?? "Block");
    }
    case "loadPreset":
      return fs.preset;
    case "sendMidi":
      return "Send MIDI";
    case "command":
      return fs.commands.find((c) => c.key === action.key && c.command === action.command)?.label.split(" · ").pop() ?? "Command";
  }
}

/** 600 KiB that start like a gzip file, so a download needs two pieces. */
const DEMO_BACKUP = (() => {
  const bytes = new Uint8Array(600 * 1024);
  bytes.set([0x1f, 0x8b, 0x08, 0x00]);
  for (let i = 4; i < bytes.length; i++) bytes[i] = (i * 31) & 0xff;
  return bytes;
})();
