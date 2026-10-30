/**
 * The Tetrad Link (docs/bluetooth.md): every Remote API request through the `remote` op, the
 * messages to extensions with the `ble` capability, and, from Link protocol 2, the live preset
 * (docs/preset-protocol.md).
 */

import { type LinkEvent, LinkError, type LinkPort, LinkSession } from "@/lib/link/session";
import type { Json } from "@/lib/link/message";
import type { BlockModel } from "@/lib/preset/edit";
import type { Edit, Preset } from "@/lib/preset/model";
import { type Feature, UnitError } from "./client";
import { type Fetch, RemoteClient } from "./remote";

/** The first Link protocol with the preset ops. */
export const PRESET_PROTOCOL = 2;

/** At about 8 KB/s a 48 KiB file piece takes several seconds, and the support file more. */
const LINK_TIMEOUT = 120_000;

/**
 * A request sent while a new connection still pairs gets no reply (docs/bluetooth.md), and the
 * phone's pairing prompt waits for a tap: `hello` goes again every 5 s, for up to a minute.
 */
const HELLO_WAIT = 5_000;
const HELLO_TRIES = 12;

export interface Hello {
  tetrad: string;
  protocol: number;
  name: string;
  authorized: boolean;
}

const asJson = (value: unknown) => JSON.parse(JSON.stringify(value ?? null)) as Json;

/** The reply header without the link's own fields. */
function fields<R>(header: Record<string, Json>): R {
  const { id: _id, reply: _reply, ...rest } = header;
  return rest as R;
}

/**
 * A Remote API request as a `remote` op. The link has its own PIN, so the unit takes no PIN
 * header, and a refusal comes back as `{ error }` with no status.
 */
function remoteOp(session: LinkSession): Fetch {
  return async (url, init) => {
    const path = url.replace(/^http:\/\/[^/]*/, "");
    const body = typeof init.body === "string" ? new TextEncoder().encode(init.body) : init.body;
    const reply = await session.request("remote", { method: init.method, path }, body, LINK_TIMEOUT).catch((error: unknown) => {
      throw wrap(error);
    });
    return {
      status: 200,
      headers: { get: () => null },
      text: async () => JSON.stringify(fields(reply.header)),
      arrayBuffer: async () => reply.body.slice().buffer,
    };
  };
}

export class LinkClient extends RemoteClient {
  override readonly transport = "bluetooth" as const;
  protected override readonly features: ReadonlySet<Feature>;
  private readonly offEvents: () => void;
  private readonly offClosed: () => void;

  private constructor(
    private readonly session: LinkSession,
    readonly hello: Hello,
  ) {
    super("link", "", remoteOp(session), LINK_TIMEOUT);
    this.version = hello.tetrad;
    const features: Feature[] = ["extension-messages"];
    if (hello.protocol >= PRESET_PROTOCOL) features.push("preset");
    this.features = new Set(features);
    this.offEvents = session.onEvent((event) => this.onLinkEvent(event));
    this.offClosed = session.onClosed((reason) => this.emit({ kind: "closed", reason }));
  }

  override get name() {
    return this.hello.name;
  }

  get authorized() {
    return this.hello.authorized;
  }

  static async open(port: LinkPort, helloWait = HELLO_WAIT): Promise<LinkClient> {
    const session = new LinkSession(port);
    try {
      for (let tries = 1; ; tries++) {
        try {
          const { header } = await session.request("hello", {}, undefined, helloWait);
          return new LinkClient(session, fields<Hello>(header));
        } catch (error) {
          if (!(error instanceof LinkError && error.timedOut) || tries >= HELLO_TRIES) throw error;
        }
      }
    } catch (error) {
      await session.close();
      throw wrap(error);
    }
  }

  /** The App PIN of the unit's Bluetooth page; the unit remembers it for this pairing. */
  async auth(pin: string) {
    await this.op("auth", { pin });
    this.hello.authorized = true;
  }

  /** One of the link's own ops (not a Remote API request). */
  private async op<R>(op: string, header: Record<string, Json> = {}, timeoutMs?: number): Promise<R> {
    try {
      return fields<R>((await this.session.request(op, header, undefined, timeoutMs)).header);
    } catch (error) {
      throw wrap(error);
    }
  }

  private onLinkEvent(event: LinkEvent) {
    if (event.event === "extension" && event.extension) {
      this.emit({ kind: "extension", extension: event.extension, value: event.value ?? null });
    } else if (event.event === "preset" && event.value && typeof event.value === "object") {
      this.emit({ kind: "preset", preset: event.value as unknown as Preset });
    }
  }

  /** Closed by the app: no "closed" event, which is for a link that was lost. */
  override async close() {
    this.offEvents();
    this.offClosed();
    await this.session.close();
  }

  override async extensionMessage(extension: string, message: unknown) {
    return (await this.op<{ value: unknown }>("extension", { extension, message: asJson(message) })).value;
  }
  override async preset() {
    return (await this.op<{ preset: Preset }>("preset")).preset;
  }
  override async presetEdit(loadId: number, edit: Edit) {
    return (await this.op<{ preset: Preset }>("preset-edit", { loadId, edit: asJson(edit) })).preset;
  }
  override async presetSave() {
    return (await this.op<{ preset: Preset }>("preset-save")).preset;
  }
  override async blocks() {
    return (await this.op<{ blocks: BlockModel[] }>("blocks", {}, 60_000)).blocks;
  }
}

function wrap(error: unknown): UnitError {
  if (error instanceof UnitError) return error;
  if (error instanceof LinkError) {
    return new UnitError(error.message, error.refused, error.refused && /PIN/.test(error.message));
  }
  return new UnitError(String(error), false);
}
