/**
 * Remote over Wi-Fi (docs/remote.md, docs/remote-api.md): plain HTTP to the unit's IPv4 address,
 * the PIN in `x-tetrad-pin`, one request per connection, JSON replies and `{ error }` refusals.
 * The Link client sends the same requests over Bluetooth through its own `Fetch`.
 */

import { BaseClient } from "./base";
import { z } from "zod";
import { type Feature, type Transport, type UnitClient, UnitError } from "./client";
import * as S from "./schemas";
import type * as T from "./types";

export interface FetchReply {
  status: number;
  headers: { get(name: string): string | null };
  text(): Promise<string>;
  arrayBuffer(): Promise<ArrayBuffer>;
}
export type Fetch = (
  url: string,
  init: { method: string; headers: Record<string, string>; body?: Uint8Array | string; signal?: AbortSignal },
) => Promise<FetchReply>;

const PIN_CHANGED = "The unit has a new PIN. Connect again and enter it.";
const UNKNOWN_REPLY = "The unit sent a reply that this app does not know. Update the app or Tetrad.";

const IPV4 = /^(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)(\.(25[0-5]|2[0-4]\d|1\d\d|[1-9]?\d)){3}$/;

/**
 * The unit's address from what the user typed or pasted: an IPv4 address with an optional port,
 * or the Remote page's URL (its QR code opens `http://<ip>/#pin=NNNN`). The unit refuses names,
 * so `.local` and other host names are refused here.
 */
export function parseAddress(input: string): { host: string; pin: string | null } | null {
  const text = input.trim().replace(/^https?:\/\//i, "");
  const pin = /#pin=(\d{4})$/.exec(text)?.[1] ?? null;
  const host = text.replace(/[/#?].*$/, "");
  const [ip, port, extra] = host.split(":");
  if (extra !== undefined || !ip || !IPV4.test(ip)) return null;
  if (port !== undefined && !/^\d{1,5}$/.test(port)) return null;
  return { host, pin };
}

export class RemoteClient extends BaseClient implements UnitClient {
  readonly transport: Transport = "wifi";
  protected readonly features: ReadonlySet<Feature> = new Set();
  /** The unit refused the PIN once. Each more request with it counts as a wrong PIN there. */
  private pinRefused = false;

  constructor(
    readonly host: string,
    private readonly pin: string,
    private readonly fetchFn: Fetch,
    private readonly timeoutMs = 12_000,
  ) {
    super();
  }

  get name() {
    return this.host;
  }

  /** Opens Remote: the PIN is good when the status call is not refused for it. */
  static async connect(host: string, pin: string, fetchFn: Fetch): Promise<RemoteClient> {
    const client = new RemoteClient(host, pin, fetchFn);
    try {
      await client.status();
    } catch (error) {
      // 422: the library is closed, but the PIN is right (docs/remote-api.md §9).
      if (!(error instanceof UnitError && error.status === 422)) throw error;
    }
    return client;
  }

  /** One request; `read` takes the body of a good reply, inside the timeout too. */
  private async call<R>(
    method: "GET" | "POST",
    path: string,
    query: Record<string, string | number | boolean | undefined>,
    body: Uint8Array | string | undefined,
    read: (reply: FetchReply) => Promise<R>,
  ): Promise<R> {
    if (this.pinRefused) throw new UnitError(PIN_CHANGED, true, true, 401);
    const params = Object.entries(query)
      .filter((entry): entry is [string, string | number | boolean] => entry[1] !== undefined)
      .map(([k, v]) => `${k}=${encodeURIComponent(typeof v === "boolean" ? (v ? "1" : "0") : String(v))}`)
      .join("&");
    const url = `http://${this.host}${path}${params ? `?${params}` : ""}`;
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), this.timeoutMs);
    let status: number;
    let refusal: string;
    try {
      const reply = await this.fetchFn(url, {
        method,
        headers: { "x-tetrad-pin": this.pin },
        // A POST needs Content-Length, even when empty: an empty string sends 0.
        ...(method === "POST" ? { body: body ?? "" } : {}),
        signal: controller.signal,
      });
      // A link that stops in the middle of a download must not wait forever.
      if (reply.status === 200) return await read(reply);
      status = reply.status;
      refusal = await reply.text();
    } catch (error) {
      if (error instanceof UnitError) throw error;
      throw new UnitError(
        controller.signal.aborted ? "The unit did not answer. Is it on the same Wi-Fi?" : `Could not reach the unit: ${String(error)}`,
        false,
      );
    } finally {
      clearTimeout(timer);
    }
    let text = `The unit refused (${status})`;
    try {
      const parsed: unknown = JSON.parse(refusal);
      if (parsed && typeof parsed === "object" && "error" in parsed && typeof parsed.error === "string") text = parsed.error;
    } catch {}
    if (status === 401) {
      this.pinRefused = true;
      this.emit({ kind: "closed", reason: PIN_CHANGED });
    }
    throw new UnitError(text, true, status === 401, status);
  }

  private json<R>(schema: z.ZodType<R>, method: "GET" | "POST", path: string, query: Record<string, string | number | boolean | undefined> = {}, body?: Uint8Array | string): Promise<R> {
    return this.call(method, path, query, body, async (reply) => {
      const parsed = schema.safeParse(JSON.parse(await reply.text()));
      if (!parsed.success) throw new UnitError(UNKNOWN_REPLY, false);
      return parsed.data;
    });
  }

  status() {
    return this.json(S.StatusReply, "GET", "/api/status");
  }
  uploadIr(name: string, wav: Uint8Array) {
    return this.json(S.IrAddedReply, "POST", "/api/irs", { name }, wav);
  }
  library() {
    return this.json(S.IrLibrary, "GET", "/api/library");
  }
  libraryOp(op: T.LibraryOp) {
    const { op: kind, ...query } = op;
    return this.json(S.IrLibrary, "POST", `/api/library/${kind}`, query);
  }
  files() {
    return this.json(S.FilesReply, "GET", "/api/files");
  }
  fileChunk(extension: string, name: string, offset: number, last: boolean, bytes: Uint8Array) {
    return this.json(S.FileChunkReply, "POST", "/api/files/chunk", { id: extension, name, offset, last: last || undefined }, bytes);
  }
  async fileDelete(extension: string, name: string) {
    await this.json(z.unknown(), "POST", "/api/files/delete", { id: extension, name });
  }
  async tabs() {
    return (await this.json(S.TabsReply, "GET", "/api/tabs")).tabs;
  }
  async tabPage(extension: string) {
    return (await this.json(S.TabPageReply, "GET", "/api/tabs/page", { id: extension })).html;
  }
  async tabCall(extension: string, message: unknown) {
    // 200 can still carry { error } when the extension answers with bad JSON.
    const reply = await this.json(S.TabCallReply, "POST", "/api/tabs/call", { id: extension }, JSON.stringify(message ?? null));
    if (typeof reply.error === "string") throw new UnitError(reply.error);
    return reply.result;
  }
  support() {
    return this.call("GET", "/api/support", {}, undefined, async (reply) => {
      const disposition = reply.headers.get("content-disposition") ?? "";
      const name = /filename="([^"]+)"/.exec(disposition)?.[1] ?? "tetrad-support.zip";
      return { name, zip: new Uint8Array(await reply.arrayBuffer()) };
    });
  }
  notices() {
    return this.json(S.NoticesReply, "GET", "/api/notices");
  }
  backup() {
    return this.json(S.BackupStatus, "GET", "/api/backup");
  }
  backupCreate() {
    return this.json(S.BackupStatus, "POST", "/api/backup/create");
  }
  backupPiece(offset: number) {
    return this.call("GET", "/api/backup/file", { offset }, undefined, async (reply) => new Uint8Array(await reply.arrayBuffer()));
  }
  backupUpload(name: string, offset: number, last: boolean, bytes: Uint8Array) {
    return this.json(S.BackupStatus, "POST", "/api/backup/upload", { name, offset, last: last || undefined }, bytes);
  }
  backupDiscard() {
    return this.json(S.BackupStatus, "POST", "/api/backup/discard");
  }
  extensions() {
    return this.json(S.ExtensionsList, "GET", "/api/extensions");
  }
  extensionCommand(command: T.ExtensionCommand) {
    return this.json(S.ExtensionsList, "POST", "/api/extensions/command", {}, JSON.stringify(command));
  }
  themes() {
    return this.json(S.ThemesList, "GET", "/api/themes");
  }
  themeSelect(id: string) {
    return this.json(S.ThemesList, "POST", "/api/themes/select", { id });
  }
  mappings() {
    return this.json(S.MappingsView, "GET", "/api/mappings");
  }
  mappingCommand(command: T.MappingCommand) {
    return this.json(S.MappingsView, "POST", "/api/mappings/command", {}, JSON.stringify(command));
  }
  footswitch() {
    return this.json(S.FootswitchView, "GET", "/api/footswitch");
  }
  footswitchCommand(command: T.FootswitchCommand) {
    return this.json(S.FootswitchView, "POST", "/api/footswitch/command", {}, JSON.stringify(command));
  }
}
