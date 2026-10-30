// @tetrad/extensions: TypeScript declarations for the Tetrad extension SDK, API 5.
// API 3 adds `pressed` on trigger firings. API 4 adds `footswitchMode` to the manifest; it has
// no TypeScript API. API 5 adds `{ leakyRelu: slope }` to network activations.
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
  /**
   * A press source only: `true` (the default) for a press, `false` for its release (a note-off).
   * Momentary mappings hold their value until the release.
   */
  readonly pressed?: boolean;
};

/** An asset file the user chose for a generated block, as UTF-8 text. */
export type Asset = {
  /** The file name, such as "Plexi.nam". */
  readonly name: string;
  readonly text: string;
};

/**
 * A qcdsp IR graph, as `BlockBuilder.finish` returns it. Tetrad validates it, replaces its
 * name, blob name and category from the manifest, and compiles it for the DSP.
 */
export type BlockIr = { readonly [key: string]: JsonValue };

/** A value in a block graph: an opaque handle that only its own builder accepts. */
export type Signal = number & { readonly __tetradSignal: true };

/** One persistent sample value. A read gives the value of the previous sample. */
export type State = { readonly signal: Signal };

/** A delay line, zero at the start. A read gives the sample written `samples` samples ago. */
export type Delay = { readonly signal: Signal };

/**
 * Builds a qcdsp IR graph, like qcdsp's `BlockBuilder`. The graph runs once per sample on
 * 32-bit floats in DSP units. A method throws on a bad value, on a signal from another
 * builder and after `finish`. Limits: 65536 nodes, 256 parameters, 4096 states, 4096 delays
 * and 16M delay samples in all.
 */
export interface BlockBuilder {
  /** The input sample. */
  input(): Signal;
  constant(value: number): Signal;
  /**
   * A control in DSP units; the unit shows it as 0..1 mapped onto `min..max`. Names are
   * unique in the block.
   */
  parameter(name: string, min: number, max: number, initial: number): Signal;
  state(initial: number): State;
  /** `samples` is a whole number, 1 or more. */
  delay(samples: number): Delay;
  add(lhs: Signal, rhs: Signal): Signal;
  sub(lhs: Signal, rhs: Signal): Signal;
  mul(lhs: Signal, rhs: Signal): Signal;
  min(lhs: Signal, rhs: Signal): Signal;
  max(lhs: Signal, rhs: Signal): Signal;
  /** `lhs * recip(rhs)`. */
  div(lhs: Signal, rhs: Signal): Signal;
  neg(input: Signal): Signal;
  abs(input: Signal): Signal;
  /**
   * `1 / input`, relative error about 2^-22. `|input|` must be about 1e-37..1e37: outside
   * that range the result can be infinite or NaN, which faults the block on the DSP.
   */
  recip(input: Signal): Signal;
  /** A clamped Padé tanh, within 1e-4 of tanh. */
  tanh(input: Signal): Signal;
  /** `0.5 + 0.5 * tanh(input / 2)`. */
  sigmoid(input: Signal): Signal;
  /** Sets the value of `state` for the next sample. At most one update per state. */
  updateState(state: State, value: Signal): void;
  /** Writes `value` into `delay` for this sample. At most one update per delay. */
  updateDelay(delay: Delay, value: Signal): void;
  /** Computes channel A only and copies it to channel B, at half the cost of stereo. */
  mono(): void;
  /**
   * Starts the block's network (IR 3): vector operations on channel buffers that run before the
   * graph, for convolutional models such as NAM WaveNets. A block has at most one network, and
   * a block with a network is mono.
   */
  network(): NetworkBuilder;
  /** Ends the graph with `output` as the block's output sample. */
  finish(output: Signal): BlockIr;
}

/** A buffer of channels in a network: an opaque handle that only its own network accepts. */
export type NetBuffer = number & { readonly __tetradNetBuffer: true };

/** Options of a convolution. Weights are ordered `[output][input][kernel]`. */
export type ConvOptions = {
  /** Taps, 1 to 16 (1 is a dense 1×1 layer). Default 1. */
  readonly kernel?: number;
  /** Samples between taps, 1 to 65536. Default 1. */
  readonly dilation?: number;
  readonly weights: readonly number[];
  /** One value per output channel. */
  readonly bias?: readonly number[];
  /** Add to the output buffer instead of replacing it. */
  readonly accumulate?: boolean;
};

/**
 * Builds a network. It runs once per sample: every operation reads and writes whole buffers.
 * A buffer is written before it is read, and a buffer whose history a convolution reads (kernel
 * above 1) does not change after that convolution. Limits: 1024 buffers, 64 channels per
 * buffer, 4096 operations, 2^20 weights.
 */
export interface NetworkBuilder {
  /** The block input: one channel. */
  input(): NetBuffer;
  buffer(channels: number): NetBuffer;
  /** `buffer *= parameter`, where `parameter` is a parameter signal of the same block. */
  gain(buffer: NetBuffer, parameter: Signal): void;
  /**
   * A causal dilated convolution: `output[o] (+)= bias[o] + Σ_i Σ_k weights[o][i][k] ·
   * input[i](t − (kernel − 1 − k) · dilation)`.
   */
  conv(input: NetBuffer, output: NetBuffer, options: ConvOptions): void;
  /**
   * In place; `tanh` is the same clamped Padé approximation as `BlockBuilder.tanh`.
   * `{ leakyRelu: s }` is `max(x, s · x)`, with `s` from 0 to 1.
   */
  activation(buffer: NetBuffer, fn: "tanh" | "hardtanh" | "relu" | { readonly leakyRelu: number }): void;
  /** `output += input` (same channel count). */
  add(input: NetBuffer, output: NetBuffer): void;
  /** `output = input` (same channel count). */
  copy(input: NetBuffer, output: NetBuffer): void;
  /** Ends the network; the signal is `buffer` (one channel) for each sample. */
  output(buffer: NetBuffer): Signal;
}

/**
 * Builds the IR of a generated block from one asset. It runs with a longer budget than other
 * handlers: 5 s in all and 2 s of CPU time for each piece of synchronous work.
 */
export type BlockProvider = (asset: Asset, scope: Scope) => BlockIr | Promise<BlockIr>;

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
  readonly ble: {
    /**
     * Pushes `value` to the Tetrad app that is connected over Bluetooth and has given the
     * PIN. Resolves `true` when it went out, `false` when no such app is connected. Needs the
     * `ble` capability (API 8).
     */
    notify(value: JsonValue): Promise<boolean>;
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
  readonly blocks: {
    /**
     * Registers the provider of a generated block (`"generated": true` in the manifest), such
     * as an amp built from a NAM model file. Tetrad calls it once for each asset the user
     * chooses and keeps the IR. What the provider throws shows as a failed build of that
     * asset. Needs the `blocks` capability.
     */
    provide(block: string, provider: BlockProvider): void;
    /** A new, empty IR builder. Works in `activate` and in handlers. */
    builder(): BlockBuilder;
  };
  readonly usb: {
    /**
     * Answers messages from a computer over the USB cable (`tetrad-usb ext <id> <json>`).
     * The handler's return value, as JSON, is the reply; nothing returned replies `null`, and
     * a throw replies with an error. One handler. Needs the `usb` capability.
     */
    onMessage(
      handler: (message: JsonValue, scope: Scope) => JsonValue | void | Promise<JsonValue | void>,
    ): void;
  };
  readonly remote: {
    /**
     * Answers the messages of the extension's tab on the Remote page (`remote` in the manifest,
     * `remote.html` in the package). The page calls `tetrad.send(message)` and gets the
     * handler's return value as JSON; nothing returned replies `null`, and a throw replies with
     * an error. One handler. Needs the `remote` capability.
     */
    onMessage(
      handler: (message: JsonValue, scope: Scope) => JsonValue | void | Promise<JsonValue | void>,
    ): void;
  };
  readonly ble: {
    /**
     * Answers messages from the Tetrad app over Bluetooth (its `extension` request). The
     * handler's return value, as JSON, is the reply; nothing returned replies `null`, and a
     * throw replies with an error. One handler. Needs the `ble` capability (API 8).
     */
    onMessage(
      handler: (message: JsonValue, scope: Scope) => JsonValue | void | Promise<JsonValue | void>,
    ): void;
  };
}

/** The default export of an extension module. `activate` may return a disposer. */
export interface ExtensionDefinition<C extends ConfigOf<C> = Config> {
  activate(ctx: ActivationContext<C>): void | (() => void) | Promise<void | (() => void)>;
}
