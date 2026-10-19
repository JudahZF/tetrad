// MIDI Mapper: maps CCs and notes on one reserved channel to block parameters and bypass.
//
// Learn: run "Learn mapping", send a CC or note-on, then touch a control on the device.
// Modes follow from the source and the control:
// - continuous: CC -> parameter. The parameter is set to value / 127.
// - switch: CC -> bypass, as in the Quad Cortex MIDI convention. A value of 64 or more
//   engages the block (bypassed = false); a value below 64 bypasses it.
// - toggle: note -> bypass or parameter. Each note-on (velocity > 0) flips bypass, or flips
//   the parameter between 0 and 1.

import type {
  Binding,
  Block,
  ControlChange,
  ControlKind,
  ErrorCode,
  ExtensionDefinition,
  JsonValue,
  MidiMessage,
  Scope,
  Site,
} from "@tetrad/extensions";

type Config = { channel: number; armed: boolean };

type Source = { kind: "cc"; controller: number } | { kind: "note"; key: number };
type Mode = "continuous" | "switch" | "toggle";
type Mapping = { source: Source; binding: Binding; mode: Mode };
type Input = { source: Source; value: number };
type Learn = { phase: "midi" } | { phase: "control"; source: Source };

const STORAGE_KEY = "mappings";
const MAX_MAPPINGS = 64;
const MAX_STATUS_LINES = 12;
/** Expected after preset changes and edits elsewhere; not worth a log line. */
const QUIET: readonly ErrorCode[] = ["staleContext", "unresolved", "scopeClosed"];

const extension: ExtensionDefinition<Config> = {
  async activate(ctx) {
    const { channel, armed } = ctx.config;
    let mappings = parseMappings(await ctx.storage.get(STORAGE_KEY));
    let learn: Learn | null = null;

    /** Runs one host action; expected refusals are dropped, others are logged. */
    const attempt = async (action: () => Promise<void>): Promise<void> => {
      try {
        await action();
      } catch (e) {
        if (!isQuiet(e)) ctx.warn(String(e));
      }
    };

    const showStatus = (scope: Scope): Promise<void> =>
      attempt(async () => {
        const blocks = await scope.controls.list();
        await scope.ui.setStatus(statusLines(blocks));
      });

    const statusLines = (blocks: readonly Block[]): string[] => {
      const lines: string[] = [];
      if (!armed) lines.push("Disarmed: MIDI input is ignored");
      if (learn?.phase === "midi") {
        lines.push(`Learning: send a CC or note on channel ${channel}`);
      } else if (learn?.phase === "control") {
        lines.push(`${sourceLabel(learn.source)} captured. Now touch a control on the device`);
      } else {
        const count = mappings.length === 1 ? "1 mapping" : `${mappings.length} mappings`;
        lines.push(`Channel ${channel}, ${count}`);
      }
      const room = MAX_STATUS_LINES - lines.length;
      const shown = mappings.length > room ? room - 1 : mappings.length;
      for (const m of mappings.slice(0, shown)) {
        lines.push(`${sourceLabel(m.source)} → ${targetLabel(blocks, m.binding)}`);
      }
      if (shown < mappings.length) lines.push(`…and ${mappings.length - shown} more`);
      return lines;
    };

    const save = async (scope: Scope, next: Mapping[]): Promise<void> => {
      await scope.storage.set(STORAGE_KEY, next);
      mappings = next;
    };

    const drive = async (m: Mapping, value: number, scope: Scope): Promise<void> => {
      const control = m.binding.control;
      switch (m.mode) {
        case "continuous":
          return scope.controls.set(m.binding, value / 127);
        case "switch":
          return scope.controls.setBypass(m.binding, value < 64);
        case "toggle": {
          if (control.kind === "bypass") return scope.controls.toggleBypass(m.binding);
          const block = findBlock(await scope.controls.list(), m.binding);
          const current = block?.parameters.find((p) => p.index === control.index)?.value ?? 0;
          return scope.controls.set(m.binding, current >= 0.5 ? 0 : 1);
        }
      }
    };

    ctx.midi.reserveChannel(channel);

    ctx.midi.onMessage(async (message, scope) => {
      if (!armed) return;
      const input = toInput(message);
      if (input === null) return;
      if (learn !== null) {
        // While learning, MIDI only picks the source; a second message replaces it.
        learn = { phase: "control", source: input.source };
        return showStatus(scope);
      }
      for (const m of mappings) {
        if (sameSource(m.source, input.source)) await attempt(() => drive(m, input.value, scope));
      }
    });

    ctx.controls.onChange(async (change: ControlChange, scope) => {
      if (learn?.phase !== "control") return;
      const source = learn.source;
      learn = null;
      const mapping: Mapping = { source, binding: change.binding, mode: modeFor(source, change.binding) };
      const rest = mappings.filter(
        (m) => !(sameSource(m.source, source) && sameBinding(m.binding, mapping.binding)),
      );
      if (rest.length >= MAX_MAPPINGS) {
        await attempt(() => scope.ui.setStatus([`Limit of ${MAX_MAPPINGS} mappings reached`]));
        return;
      }
      await save(scope, [...rest, mapping]);
      await showStatus(scope);
    });

    ctx.commands.register("learn", async (scope) => {
      learn = { phase: "midi" };
      await showStatus(scope);
    });
    ctx.commands.register("cancel", async (scope) => {
      learn = null;
      await showStatus(scope);
    });
    ctx.commands.register("undo", async (scope) => {
      await save(scope, mappings.slice(0, -1));
      await showStatus(scope);
    });
    ctx.commands.register("clear", async (scope) => {
      await scope.storage.delete(STORAGE_KEY);
      mappings = [];
      await showStatus(scope);
    });

    ctx.lifecycle.onRunning(showStatus);
    // Block names can change with the preset; mappings stay bound to their sites.
    ctx.lifecycle.onPresetLoaded(showStatus);
  },
};

export default extension;

function toInput(message: MidiMessage): Input | null {
  switch (message.kind) {
    case "cc":
      return { source: { kind: "cc", controller: message.controller }, value: message.value };
    case "noteOn":
      // Velocity 0 is a note-off by MIDI convention.
      return message.velocity > 0 ? { source: { kind: "note", key: message.key }, value: message.velocity } : null;
    default:
      return null;
  }
}

function modeFor(source: Source, binding: Binding): Mode {
  if (source.kind === "note") return "toggle";
  return binding.control.kind === "bypass" ? "switch" : "continuous";
}

function isQuiet(e: unknown): boolean {
  return e instanceof Error && "code" in e && QUIET.some((code) => code === e.code);
}

/** Unique per site, so it also serves as the site's identity. */
function siteLabel(site: Site): string {
  switch (site.site) {
    case "slot":
      return `Row ${site.row + 1} slot ${site.column + 1}`;
    case "input":
      return `Input ${site.row + 1}`;
    case "output":
      return `Output ${site.row + 1}`;
    case "splitter":
      return `Splitter ${site.pair}`;
    case "mixer":
      return `Mixer ${site.pair}`;
  }
}

const sourceLabel = (s: Source): string => (s.kind === "cc" ? `CC ${s.controller}` : `Note ${s.key}`);

const sameSource = (a: Source, b: Source): boolean =>
  a.kind === "cc" ? b.kind === "cc" && a.controller === b.controller : b.kind === "note" && a.key === b.key;

function sameBinding(a: Binding, b: Binding): boolean {
  const ca = a.control;
  const cb = b.control;
  const sameControl = ca.kind === "bypass" ? cb.kind === "bypass" : cb.kind === "parameter" && ca.index === cb.index;
  return sameControl && a.model === b.model && siteLabel(a) === siteLabel(b);
}

const findBlock = (blocks: readonly Block[], b: Binding): Block | undefined =>
  blocks.find((block) => block.model === b.model && siteLabel(block) === siteLabel(b));

function targetLabel(blocks: readonly Block[], binding: Binding): string {
  const block = findBlock(blocks, binding);
  if (block === undefined) return `${siteLabel(binding)} (not in this preset)`;
  const control = binding.control;
  if (control.kind === "bypass") return `${block.name}: Bypass`;
  const parameter = block.parameters.find((p) => p.index === control.index);
  return `${block.name}: ${parameter?.name ?? `Parameter ${control.index + 1}`}`;
}

// Stored mappings are untrusted JSON: anything malformed is dropped.

type JsonObject = { readonly [key: string]: JsonValue };

const isObject = (v: JsonValue | undefined): v is JsonObject =>
  typeof v === "object" && v !== null && !Array.isArray(v);

const isInt = (v: JsonValue | undefined, max: number): v is number =>
  typeof v === "number" && Number.isInteger(v) && v >= 0 && v <= max;

const isPair = (v: JsonValue | undefined): v is "upper" | "lower" => v === "upper" || v === "lower";

function parseMappings(stored: JsonValue): Mapping[] {
  const list: readonly JsonValue[] = Array.isArray(stored) ? stored : [];
  const out: Mapping[] = [];
  for (const item of list.slice(0, MAX_MAPPINGS)) {
    const mapping = isObject(item) ? parseMapping(item) : null;
    if (mapping !== null) out.push(mapping);
  }
  return out;
}

function parseMapping(v: JsonObject): Mapping | null {
  const { source, binding, mode } = v;
  if (!isObject(source) || !isObject(binding)) return null;
  const s = parseSource(source);
  const b = parseBinding(binding);
  if (s === null || b === null || mode !== modeFor(s, b)) return null;
  return { source: s, binding: b, mode: modeFor(s, b) };
}

function parseSource(v: JsonObject): Source | null {
  const { kind, controller, key } = v;
  if (kind === "cc" && isInt(controller, 127)) return { kind: "cc", controller };
  if (kind === "note" && isInt(key, 127)) return { kind: "note", key };
  return null;
}

function parseBinding(v: JsonObject): Binding | null {
  const { site, row, column, pair, model, control } = v;
  if (!isInt(model, 0xffff_ffff) || !isObject(control)) return null;
  const { kind, index } = control;
  let c: ControlKind;
  if (kind === "bypass") c = { kind: "bypass" };
  else if (kind === "parameter" && isInt(index, 0xffff)) c = { kind: "parameter", index };
  else return null;
  switch (site) {
    case "slot":
      return isInt(row, 3) && isInt(column, 7) ? { site, row, column, model, control: c } : null;
    case "input":
    case "output":
      return isInt(row, 3) ? { site, row, model, control: c } : null;
    case "splitter":
    case "mixer":
      return isPair(pair) ? { site, pair, model, control: c } : null;
    default:
      return null;
  }
}
