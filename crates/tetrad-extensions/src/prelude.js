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
    usb: undefined,
  };
  const registrations = {
    midiChannels: [],
    commands: [],
    controlChanges: false,
    triggers: [],
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

  globalThis.__tetrad_dispose = () => {
    if (disposer) disposer();
  };
})();
