// @tetrad/extensions: TypeScript declarations for the Tetrad extension SDK, API 1.
//
// Types only. The runner provides every value at run time and has no module loader, so
// import with `import type { ... } from "@tetrad/extensions"` and bundle to one ES module
// whose default export is an ExtensionDefinition.

/** A JSON value; everything that crosses to the host is JSON. */
export type JsonValue =
  | null
  | boolean
  | number
  | string
  | readonly JsonValue[]
  | { readonly [key: string]: JsonValue };

/** Why the host refused a call. `staleContext` and `scopeClosed` are expected cancellations. */
export type ErrorCode =
  | "unknownMethod"
  | "invalidParams"
  | "permissionDenied"
  | "notRunning"
  | "scopeClosed"
  | "staleContext"
  | "unresolved"
  | "unsupported"
  | "limitExceeded"
  | "failed";

/** The rejection of every host call. There is no runtime class: test `code`, not `instanceof`. */
export interface HostError extends Error {
  readonly code: ErrorCode;
}

export type Pair = "upper" | "lower";

/** One block site of the loaded preset. Grid rows count 0..3, columns 0..7. */
export type Site =
  | { readonly site: "slot"; readonly row: number; readonly column: number }
  | { readonly site: "input"; readonly row: number }
  | { readonly site: "output"; readonly row: number }
  | { readonly site: "splitter"; readonly pair: Pair }
  | { readonly site: "mixer"; readonly pair: Pair };

export type ControlKind =
  | { readonly kind: "parameter"; readonly index: number }
  | { readonly kind: "bypass" };

/**
 * A saved reference to one control: a site, the model expected there and what to drive.
 * It never follows a block that moves; a different model at the site is `unresolved`.
 */
export type Binding = Site & { readonly model: number; readonly control: ControlKind };

export type Parameter = {
  readonly index: number;
  readonly name: string;
  /** Normalized 0..1. */
  readonly value: number;
};

/** One block of the loaded preset, as `controls.list` returns it. */
export type Block = Site & {
  readonly model: number;
  readonly name: string;
  readonly bypassed: boolean;
  readonly parameters: readonly Parameter[];
};

/** A native edit (touch, footswitch, Cortex Control). Extensions never hear edits an extension made. */
export type ControlChange = {
  readonly binding: Binding;
  /** Normalized 0..1 for a parameter; 0 or 1 (bypassed) for bypass. */
  readonly value: number;
};

/** A channel voice message. Channels count 1..16; data bytes 0..127. */
export type MidiMessage =
  | { readonly kind: "cc"; readonly channel: number; readonly controller: number; readonly value: number }
  | { readonly kind: "pc"; readonly channel: number; readonly program: number }
  | { readonly kind: "noteOn"; readonly channel: number; readonly key: number; readonly velocity: number }
  | { readonly kind: "noteOff"; readonly channel: number; readonly key: number; readonly velocity: number };

export type ExtensionInfo = { readonly id: string; readonly version: string };

/** A trigger the extension offers to Mappings. Mappings show it as "<extension name> · <label>". */
export type TriggerSpec = {
  /** 1..32 characters of [a-z0-9-], unique in the extension. */
  readonly id: string;
  /** 1..48 characters, such as "Ch 3". */
  readonly label: string;
};

/** One firing of a declared trigger. Mappings match on `trigger` and `source`. */
export type TriggerFiring = {
  /** The ID of a declared trigger. */
  readonly trigger: string;
  /** What fired inside the trigger, 1..64 characters, such as "cc:20". */
  readonly source: string;
  /** Display text of the source, at most 48 characters, such as "CC 20". */
  readonly label: string;
  /** 0..1; the host clamps other finite values. */
  readonly value: number;
  /** `true` for a continuous source (a CC); `false` for a press (a note, a PC). */
  readonly continuous: boolean;
};

/** Settings values by key, as declared in the manifest. */
export type Config = { readonly [key: string]: JsonValue };

/** Constrains a config type to JSON values; interfaces work as well as type aliases. */
export type ConfigOf<C> = { readonly [K in keyof C]: JsonValue };

/** A handler runs to completion before the next event; it may be async. */
export type Handler<Args extends unknown[]> = (...args: Args) => void | Promise<void>;

/**
 * The mutating API of one event. It closes when its handler settles; later calls reject
 * with `scopeClosed`, and calls after a preset change reject with `staleContext`.
 */
export interface Scope {
  readonly id: number;
  readonly controls: {
    list(): Promise<Block[]>;
    /** `value` is normalized 0..1. */
    set(binding: Binding, value: number): Promise<void>;
    setBypass(binding: Binding, bypassed: boolean): Promise<void>;
    toggleBypass(binding: Binding): Promise<void>;
  };
  readonly midi: {
    send(message: MidiMessage): Promise<void>;
  };
  readonly storage: {
    /** `null` when the key is not set. */
    get(key: string): Promise<JsonValue>;
    set(key: string, value: JsonValue): Promise<void>;
    delete(key: string): Promise<void>;
  };
  readonly ui: {
    /** Replaces the status lines on the extension's settings page. */
    setStatus(lines: readonly string[]): Promise<void>;
  };
  readonly triggers: {
    /** Sends one firing to Mappings. Needs the `triggers` capability. */
    fire(firing: TriggerFiring): Promise<void>;
  };
}

/**
 * The read and register API of `activate`. Registration methods throw after `activate`
 * returns, and the host refuses `controls.list` and `storage.get` from here once activation
 * has finished: handlers use their scope. Nothing here can change the device.
 */
export interface ActivationContext<C extends ConfigOf<C> = Config> {
  readonly extension: ExtensionInfo;
  readonly config: Readonly<C>;
  log(...args: unknown[]): void;
  warn(...args: unknown[]): void;
  error(...args: unknown[]): void;
  readonly midi: {
    /** Takes channel 1..16 exclusively; its voice messages go only to this extension. */
    reserveChannel(channel: number): void;
    onMessage(handler: Handler<[message: MidiMessage, scope: Scope]>): void;
  };
  readonly commands: {
    /** `id` is 1..64 characters; the manifest gives its button label. */
    register(id: string, handler: Handler<[scope: Scope]>): void;
  };
  readonly controls: {
    list(): Promise<Block[]>;
    onChange(handler: Handler<[change: ControlChange, scope: Scope]>): void;
  };
  readonly lifecycle: {
    /** Runs once after activation; the first scope that may write. */
    onRunning(handler: Handler<[scope: Scope]>): void;
    /** A different preset is loaded; every earlier target is stale. */
    onPresetLoaded(handler: Handler<[scope: Scope]>): void;
  };
  readonly storage: {
    get(key: string): Promise<JsonValue>;
  };
  readonly triggers: {
    /** Offers a trigger to Mappings; at most 8. Needs the `triggers` capability. */
    declare(trigger: TriggerSpec): void;
  };
}

/** The default export of an extension module. `activate` may return a disposer. */
export interface ExtensionDefinition<C extends ConfigOf<C> = Config> {
  activate(ctx: ActivationContext<C>): void | (() => void) | Promise<void | (() => void)>;
}
