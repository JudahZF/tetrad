// MIDI Mapper: sends MIDI on one reserved channel to Mappings as triggers.
//
// It declares one trigger, `midi`. Each message fires one source of it:
// - CC n: `cc:n`, continuous, value / 127.
// - note-on k with velocity > 0: `note:k`, a press with velocity / 127.
// - note-off k (or note-on with velocity 0): `note:k`, the release of that press.
// - program change p: `pc:p`, a press with value 1.
// Mappings decide what each source does.

import type { ExtensionDefinition, MidiMessage, TriggerFiring } from "@tetrad/extensions";

type Config = { channel: number };

const TRIGGER = "midi";

const extension: ExtensionDefinition<Config> = {
  activate(ctx) {
    const { channel } = ctx.config;
    ctx.midi.reserveChannel(channel);
    ctx.triggers.declare({ id: TRIGGER, label: `Ch ${channel}` });

    ctx.midi.onMessage(async (message, scope) => {
      await scope.triggers.fire(toFiring(message));
    });
    ctx.lifecycle.onRunning((scope) => scope.ui.setStatus([`Channel ${channel} · sends triggers to Mappings`]));
  },
};

export default extension;

function toFiring(message: MidiMessage): TriggerFiring {
  switch (message.kind) {
    case "cc": {
      const { controller, value } = message;
      return { trigger: TRIGGER, source: `cc:${controller}`, label: `CC ${controller}`, value: value / 127, continuous: true };
    }
    case "noteOn":
    case "noteOff": {
      // Velocity 0 is a note-off by MIDI convention.
      const { key, velocity } = message;
      const pressed = message.kind === "noteOn" && velocity > 0;
      return { trigger: TRIGGER, source: `note:${key}`, label: `Note ${key}`, value: velocity / 127, continuous: false, pressed };
    }
    case "pc": {
      const { program } = message;
      return { trigger: TRIGGER, source: `pc:${program}`, label: `PC ${program + 1}`, value: 1, continuous: false };
    }
  }
}
