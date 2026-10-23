// Runner glue: builds the SDK objects over the runner's native transport.
// Evaluated once as a script before the extension module. The runner calls the two globals
// it leaves (`__tetrad_activate`, `__tetrad_dispatch`) and deletes nothing else from scope.
(() => {
  "use strict";
  const native = globalThis.__tetrad_native;
  delete globalThis.__tetrad_native;

  const handlers = {
    midi: [],
    commands: new Map(),
    running: [],
    controlChanged: [],
    presetLoaded: [],
    blocks: new Map(),
    usb: undefined,
  };
  const registrations = {
    midiChannels: [],
    commands: [],
    controlChanges: false,
    triggers: [],
    blocks: [],
    usb: false,
  };
  let activating = true;
  let disposer;

  const call = (method, scope, params) =>
    native.call(method, scope, JSON.stringify(params === undefined ? null : params)).then(
      (json) => JSON.parse(json),
    );

  const text = (args) => args.map((a) => (typeof a === "string" ? a : JSON.stringify(a))).join(" ");
  const fn = (name, value) => {
    if (typeof value !== "function") throw new TypeError(`${name} must be a function`);
    return value;
  };
  const duringActivation = (name) => {
    if (!activating) throw new Error(`${name} is only allowed during activate()`);
  };
  const message = (e) => (e instanceof Error ? e.message : String(e)).slice(0, 1024);

  // Mutating APIs exist only on a scope. A scope closes when its handler settles; the host
  // then refuses its calls with ScopeClosed, so a detached promise cannot write later.
  const makeScope = (id) => {
    const c = (method, params) => call(method, id, params);
    return Object.freeze({
      id,
      controls: Object.freeze({
        list: () => c("controls.list"),
        set: (binding, value) => c("controls.set", { binding, value }),
        setBypass: (binding, bypassed) => c("controls.setBypass", { binding, bypassed }),
        toggleBypass: (binding) => c("controls.toggleBypass", { binding }),
      }),
      midi: Object.freeze({ send: (message) => c("midi.send", message) }),
      storage: Object.freeze({
        get: (key) => c("storage.get", { key }),
        set: (key, value) => c("storage.set", { key, value }),
        delete: (key) => c("storage.delete", { key }),
      }),
      ui: Object.freeze({ setStatus: (lines) => c("ui.setStatus", { lines }) }),
      triggers: Object.freeze({ fire: (firing) => c("triggers.fire", firing) }),
    });
  };

  // The IR builder mirrors qcdsp's BlockBuilder and emits its JSON IR (qcdsp graph.rs). Nodes
  // live in flat arrays. A signal is the number `graph * NODE_SPAN + node`, so a signal from
  // another builder fails the range check. Limits are qcdsp's; Tetrad validates again.
  const MAX_NODES = 65536;
  const MAX_PARAMETERS = 256;
  const MAX_MEMORIES = 4096;
  const MAX_DELAY_SAMPLES = 16 * 1024 * 1024;
  const NODE_SPAN = 2 * MAX_NODES;
  // Op codes index OPS: 0 input, 1 constant, 2..4 indexed reads, 5..10 binary, 11..15 unary,
  // 16 the network output.
  const OPS = ["input", "constant", "parameter", "state", "delay", "add", "sub", "mul", "min",
    "max", "div", "neg", "abs", "recip", "tanh", "sigmoid", "network"];
  const OP_DIV = 10;
  const OP_RECIP = 13;
  const OP_NETWORK = 16;
  // qcdsp network.rs limits.
  const MAX_BUFFERS = 1024;
  const MAX_CHANNELS = 64;
  const MAX_NETWORK_OPS = 4096;
  // Weights and biases of all convolutions of a network together (qcdsp `MAX_WEIGHTS`).
  const MAX_NETWORK_WEIGHTS = 1 << 20;
  const MAX_KERNEL = 16;
  const MAX_DILATION = 1 << 16;
  const ACTIVATIONS = ["tanh", "hardtanh", "relu"];
  let nextGraph = 1;
  // qcdsp reads every value as an f32; JSON keeps the shorter f64 text.
  const f32 = (what, value) => {
    if (typeof value !== "number" || !Number.isFinite(Math.fround(value))) {
      throw new RangeError(`${what} must be a finite 32-bit float`);
    }
    return value;
  };

  const makeBuilder = () => {
    const graph = nextGraph++;
    const ops = [0];
    const as = [0];
    const bs = [0];
    const parameters = [];
    const states = [];
    const delays = [];
    let isMono = false;
    let finished = false;
    let network = null;
    let delaySamples = 0;
    const live = () => {
      if (finished) throw new Error("the block builder is finished");
    };
    const node = (signal) => {
      const index = signal - graph * NODE_SPAN;
      if (!Number.isInteger(signal) || index < 0 || index >= ops.length) {
        throw new TypeError("signal belongs to another block builder");
      }
      return index;
    };
    const push = (op, a, b) => {
      live();
      if (ops.length >= MAX_NODES) throw new RangeError(`a block has at most ${MAX_NODES} nodes`);
      ops.push(op);
      as.push(a);
      bs.push(b);
      return graph * NODE_SPAN + ops.length - 1;
    };
    const binary = (op) => (lhs, rhs) => push(op, node(lhs), node(rhs));
    const unary = (op) => (input) => push(op, node(input), 0);
    // A state or delay handle of this builder; `op` is its read node's op code.
    const memory = (handle, op, list, what) => {
      const index = node(handle === null || typeof handle !== "object" ? undefined : handle.signal);
      if (ops[index] !== op) throw new TypeError(`not a ${what} of this block builder`);
      const entry = list[as[index]];
      if (entry.update !== null) throw new Error(`the ${what} already has an update`);
      return entry;
    };
    const emit = (i) => {
      const op = ops[i];
      if (op === 0) return { op: "input" };
      if (op === OP_NETWORK) return { op: "network" };
      if (op === 1) return { op: "constant", value: as[i] };
      if (op <= 4) return { op: OPS[op], index: as[i] };
      if (op <= OP_DIV) return { op: "binary", kind: OPS[op], lhs: as[i], rhs: bs[i] };
      return { op: OPS[op], input: as[i] };
    };
    return Object.freeze({
      input: () => (live(), graph * NODE_SPAN),
      constant: (value) => push(1, f32("constant", value), 0),
      parameter: (name, min, max, initial) => {
        live();
        if (typeof name !== "string" || name.trim() === "") {
          throw new TypeError("parameter name must be a non-empty string");
        }
        const [lo, hi, value] = [f32("min", min), f32("max", max), f32("default", initial)];
        const [flo, fhi, fvalue] = [lo, hi, value].map(Math.fround);
        // min < max as f32: the unit divides by max - min.
        if (!(flo < fhi) || fvalue < flo || fvalue > fhi || !Number.isFinite(Math.fround(fhi - flo))) {
          throw new RangeError(`parameter ${name} needs min < max and min <= default <= max`);
        }
        if (parameters.some((p) => p.name === name)) throw new Error(`parameter ${name} exists`);
        if (parameters.length >= MAX_PARAMETERS) {
          throw new RangeError(`a block has at most ${MAX_PARAMETERS} parameters`);
        }
        const signal = push(2, parameters.length, 0);
        parameters.push({ name, min: lo, max: hi, default: value });
        return signal;
      },
      state: (initial) => {
        const value = f32("state", initial);
        if (states.length >= MAX_MEMORIES) throw new RangeError(`at most ${MAX_MEMORIES} states`);
        const signal = push(3, states.length, 0);
        states.push({ initial: value, update: null });
        return Object.freeze({ signal });
      },
      delay: (samples) => {
        if (!Number.isInteger(samples) || samples < 1 || delaySamples + samples > MAX_DELAY_SAMPLES) {
          throw new RangeError(`delay lengths are whole samples, at most ${MAX_DELAY_SAMPLES} in all`);
        }
        if (delays.length >= MAX_MEMORIES) throw new RangeError(`at most ${MAX_MEMORIES} delays`);
        const signal = push(4, delays.length, 0);
        delays.push({ initial: samples, update: null });
        delaySamples += samples;
        return Object.freeze({ signal });
      },
      add: binary(5),
      sub: binary(6),
      mul: binary(7),
      min: binary(8),
      max: binary(9),
      div: binary(OP_DIV),
      neg: unary(11),
      abs: unary(12),
      recip: unary(OP_RECIP),
      tanh: unary(14),
      sigmoid: unary(15),
      updateState: (state, value) => {
        live();
        const update = node(value);
        memory(state, 3, states, "state").update = update;
      },
      updateDelay: (delay, value) => {
        live();
        const update = node(value);
        memory(delay, 4, delays, "delay").update = update;
      },
      mono: () => {
        live();
        isMono = true;
      },
      // IR 3: a network of vector operations that runs before the graph (qcdsp network.rs).
      // A buffer handle is `graph * NODE_SPAN + MAX_NODES + buffer`: the upper half of the span,
      // which signals never use, so another builder's buffer and any signal are refused. Buffer 0
      // is the block input. `output` ends the network and
      // returns the signal of its output for each sample. A block has one network and is mono.
      network: () => {
        live();
        if (network !== null) throw new Error("a block has at most one network");
        const buffers = [1];
        const netOps = [];
        let ended = false;
        let weightCount = 0;
        network = { buffers, ops: netOps, output: 0 };
        const bufferBase = graph * NODE_SPAN + MAX_NODES;
        const unfinished = () => {
          live();
          if (ended) throw new Error("the network is finished");
        };
        const open = () => {
          unfinished();
          if (netOps.length >= MAX_NETWORK_OPS) {
            throw new RangeError(`a network has at most ${MAX_NETWORK_OPS} operations`);
          }
        };
        const buffer = (handle) => {
          const index = handle - bufferBase;
          if (!Number.isInteger(handle) || index < 0 || index >= buffers.length) {
            throw new TypeError("not a buffer of this network");
          }
          return index;
        };
        const floats = (what, values) => {
          if (!Array.isArray(values)) throw new TypeError(`${what} must be an array of numbers`);
          return values.map((value) => f32(what, value));
        };
        return Object.freeze({
          input: () => (live(), bufferBase),
          buffer: (channels) => {
            open();
            if (!Number.isInteger(channels) || channels < 1 || channels > MAX_CHANNELS) {
              throw new RangeError(`a buffer has 1 to ${MAX_CHANNELS} channels`);
            }
            if (buffers.length >= MAX_BUFFERS) throw new RangeError(`at most ${MAX_BUFFERS} buffers`);
            buffers.push(channels);
            return bufferBase + buffers.length - 1;
          },
          gain: (target, parameter) => {
            open();
            const index = node(parameter);
            if (ops[index] !== 2) throw new TypeError("gain takes a parameter of this block builder");
            netOps.push({ op: "gain", buffer: buffer(target), parameter: as[index] });
          },
          conv: (input, output, options) => {
            open();
            const { kernel = 1, dilation = 1, weights, bias, accumulate = false } = options ?? {};
            if (!Number.isInteger(kernel) || kernel < 1 || kernel > MAX_KERNEL ||
              !Number.isInteger(dilation) || dilation < 1 || dilation > MAX_DILATION) {
              throw new RangeError(`kernel 1 to ${MAX_KERNEL}, dilation 1 to ${MAX_DILATION}`);
            }
            const [from, to] = [buffer(input), buffer(output)];
            const count = buffers[to] * (buffers[from] * kernel + (bias === undefined ? 0 : 1));
            if (weightCount + count > MAX_NETWORK_WEIGHTS) {
              throw new RangeError(`a network has at most ${MAX_NETWORK_WEIGHTS} weights and biases`);
            }
            const w = floats("weights", weights);
            if (w.length !== buffers[from] * buffers[to] * kernel) {
              throw new RangeError("weights need output channels × input channels × kernel values");
            }
            const op = { op: "conv", input: from, output: to, kernel, dilation, weights: w };
            if (bias !== undefined) {
              op.bias = floats("bias", bias);
              if (op.bias.length !== buffers[to]) throw new RangeError("bias needs one value per output channel");
            }
            if (accumulate) op.accumulate = true;
            netOps.push(op);
            weightCount += count;
          },
          activation: (target, fn) => {
            open();
            if (!ACTIVATIONS.includes(fn)) throw new TypeError(`activation is one of ${ACTIVATIONS.join(", ")}`);
            netOps.push({ op: "activation", buffer: buffer(target), function: fn });
          },
          add: (input, output) => {
            open();
            netOps.push({ op: "add", input: buffer(input), output: buffer(output) });
          },
          copy: (input, output) => {
            open();
            netOps.push({ op: "copy", input: buffer(input), output: buffer(output) });
          },
          output: (target) => {
            unfinished();
            network.output = buffer(target);
            ended = true;
            isMono = true;
            return push(OP_NETWORK, 0, 0);
          },
        });
      },
      finish: (output) => {
        live();
        const out = node(output);
        finished = true;
        // The oldest IR version that holds the block, as qcdsp's `finish` writes.
        let version = network !== null ? 3 : isMono ? 2 : 1;
        const nodes = new Array(ops.length);
        for (let i = 0; i < ops.length; i++) {
          if (version < 2 && (ops[i] === OP_DIV || ops[i] >= OP_RECIP)) version = 2;
          nodes[i] = emit(i);
        }
        // Placeholders: Tetrad replaces blob name, name and category from the manifest.
        const ir = { version, blob_name: "block", name: "Block", category: "utility" };
        if (isMono) ir.channels = "mono";
        Object.assign(ir, { parameters, states, delays, nodes, output: out });
        if (network !== null) ir.network = network;
        return ir;
      },
    });
  };

  const makeContext = (extension, config) =>
    Object.freeze({
      extension: Object.freeze(extension),
      config: Object.freeze(config),
      log: (...args) => native.log("info", text(args)),
      warn: (...args) => native.log("warn", text(args)),
      error: (...args) => native.log("error", text(args)),
      midi: Object.freeze({
        reserveChannel(channel) {
          duringActivation("midi.reserveChannel");
          if (!Number.isInteger(channel) || channel < 1 || channel > 16) {
            throw new RangeError("MIDI channel must be an integer 1..16");
          }
          if (!registrations.midiChannels.includes(channel)) registrations.midiChannels.push(channel);
        },
        onMessage(handler) {
          duringActivation("midi.onMessage");
          handlers.midi.push(fn("handler", handler));
        },
      }),
      commands: Object.freeze({
        register(id, handler) {
          duringActivation("commands.register");
          if (typeof id !== "string" || id.length === 0 || id.length > 64) {
            throw new TypeError("command id must be a non-empty string of at most 64 characters");
          }
          if (handlers.commands.has(id)) throw new Error(`command ${id} is already registered`);
          handlers.commands.set(id, fn("handler", handler));
          registrations.commands.push(id);
        },
      }),
      controls: Object.freeze({
        list: () => call("controls.list", null),
        onChange(handler) {
          duringActivation("controls.onChange");
          handlers.controlChanged.push(fn("handler", handler));
          registrations.controlChanges = true;
        },
      }),
      lifecycle: Object.freeze({
        onRunning(handler) {
          duringActivation("lifecycle.onRunning");
          handlers.running.push(fn("handler", handler));
        },
        onPresetLoaded(handler) {
          duringActivation("lifecycle.onPresetLoaded");
          handlers.presetLoaded.push(fn("handler", handler));
        },
      }),
      storage: Object.freeze({ get: (key) => call("storage.get", null, { key }) }),
      usb: Object.freeze({
        onMessage(handler) {
          duringActivation("usb.onMessage");
          if (handlers.usb) throw new Error("usb.onMessage is already registered");
          handlers.usb = fn("handler", handler);
          registrations.usb = true;
        },
      }),
      // The host checks the same rules again (`TriggerSpec::is_valid`).
      triggers: Object.freeze({
        declare(trigger) {
          duringActivation("triggers.declare");
          const { id, label } = trigger ?? {};
          if (typeof id !== "string" || !/^[a-z0-9-]{1,32}$/.test(id)) {
            throw new TypeError("trigger id must be 1 to 32 characters of [a-z0-9-]");
          }
          if (typeof label !== "string" || label.length === 0 || label.length > 48) {
            throw new TypeError("trigger label must be 1 to 48 characters");
          }
          if (registrations.triggers.some((t) => t.id === id)) {
            throw new Error(`trigger ${id} is already declared`);
          }
          if (registrations.triggers.length >= 8) throw new RangeError("at most 8 triggers");
          registrations.triggers.push({ id, label });
        },
      }),
      // The host checks the capability and that the manifest declares a generated block.
      blocks: Object.freeze({
        provide(block, provider) {
          duringActivation("blocks.provide");
          if (typeof block !== "string" || !/^[a-z0-9-]{1,32}$/.test(block)) {
            throw new TypeError("block id must be 1 to 32 characters of [a-z0-9-]");
          }
          if (handlers.blocks.has(block)) throw new Error(`block ${block} already has a provider`);
          handlers.blocks.set(block, fn("provider", provider));
          registrations.blocks.push(block);
        },
        builder: makeBuilder,
      }),
    });

  globalThis.__tetrad_activate = async (definition, extensionJson, configJson) => {
    if (definition === null || typeof definition !== "object" || typeof definition.activate !== "function") {
      throw new TypeError("the module's default export must be an object with activate(ctx)");
    }
    const ctx = makeContext(JSON.parse(extensionJson), JSON.parse(configJson));
    try {
      const result = await definition.activate(ctx);
      if (typeof result === "function") disposer = result;
    } finally {
      activating = false;
    }
    return JSON.stringify(registrations);
  };

  globalThis.__tetrad_dispatch = (scopeId, eventJson) => {
    const event = JSON.parse(eventJson);
    const scope = makeScope(scopeId);
    // The JSON text of a USB handler's return value: the reply to the computer.
    let result = null;
    let run;
    switch (event.kind) {
      case "running":
        run = handlers.running.map((h) => () => h(scope));
        break;
      case "midi":
        run = handlers.midi.map((h) => () => h(Object.freeze(event.message), scope));
        break;
      case "command": {
        const h = handlers.commands.get(event.id);
        run = h ? [() => h(scope)] : [];
        break;
      }
      case "controlChanged":
        run = handlers.controlChanged.map((h) => () => h(Object.freeze(event.change), scope));
        break;
      case "presetLoaded":
        run = handlers.presetLoaded.map((h) => () => h(scope));
        break;
      case "buildBlock":
        run = [() => buildBlock(event, scope)];
        break;
      case "usb": {
        const h = handlers.usb;
        run = h
          ? [
              async () => {
                const reply = await h(Object.freeze(event.message), scope);
                result = JSON.stringify(reply === undefined ? null : reply);
              },
            ]
          : [];
        break;
      }
      default:
        run = [];
    }
    (async () => {
      for (const h of run) await h();
    })().then(
      () => native.done(scopeId, null, result),
      (e) => native.done(scopeId, String(e && e.stack ? `${e}\n${e.stack}` : e), null),
    );
  };

  // Runs a block provider and sends its IR, or what it threw, to the host in the event's scope.
  const buildBlock = async ({ block, variant, asset }, scope) => {
    const publish = (result) => call("blocks.publish", scope.id, { block, variant, ...result });
    let ir;
    try {
      const provider = handlers.blocks.get(block);
      if (!provider) throw new Error(`no provider for block ${block}`);
      ir = await provider(Object.freeze({ name: asset.name, text: asset.text }), scope);
      if (ir === null || typeof ir !== "object" || Array.isArray(ir)) {
        throw new TypeError("a block provider must return a BlockIr object");
      }
    } catch (e) {
      return publish({ error: message(e) });
    }
    try {
      await publish({ ir });
    } catch (e) {
      // Refused before it reached the host (not JSON, or over the frame size).
      if (e === null || typeof e !== "object" || e.code !== "limitExceeded") throw e;
      await publish({ error: `the IR was not sent: ${message(e)}` });
    }
  };

  globalThis.__tetrad_dispose = () => {
    if (disposer) disposer();
  };
})();
