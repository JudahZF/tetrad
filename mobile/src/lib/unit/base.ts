/** What every client shares: the feature set, the event listeners, and refusals for what a transport lacks. */

import type { BlockModel } from "@/lib/preset/edit";
import type { Edit, Preset } from "@/lib/preset/model";
import { type Feature, type Transport, type UnitEvent, Unsupported } from "./client";

export abstract class BaseClient {
  abstract readonly transport: Transport;
  abstract readonly name: string;
  version: string | null = null;
  protected abstract readonly features: ReadonlySet<Feature>;
  private readonly listeners = new Set<(event: UnitEvent) => void>();

  has(feature: Feature) {
    return this.features.has(feature);
  }

  onEvent(listener: (event: UnitEvent) => void) {
    this.listeners.add(listener);
    return () => {
      this.listeners.delete(listener);
    };
  }

  protected emit(event: UnitEvent) {
    for (const listener of this.listeners) listener(event);
  }

  /** A rejected promise, so callers see the refusal where they await. */
  protected lacks(feature: Feature): Promise<never> {
    return Promise.reject(new Unsupported(feature, this.transport));
  }

  async close() {}

  extensionMessage(_e: string, _m: unknown): Promise<unknown> {
    return this.lacks("extension-messages");
  }
  preset(): Promise<Preset> {
    return this.lacks("preset");
  }
  presetEdit(_loadId: number, _edit: Edit): Promise<Preset> {
    return this.lacks("preset");
  }
  presetSave(): Promise<Preset> {
    return this.lacks("preset");
  }
  blocks(): Promise<BlockModel[]> {
    return this.lacks("preset");
  }
}
