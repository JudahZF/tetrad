import { describe, expect, it } from "vitest";
import { Reassembler, frames } from "@/lib/link/frames";
import { type Header, type Json, decode, encode } from "@/lib/link/message";
import type { LinkPort } from "@/lib/link/session";
import { sendFile } from "./client";
import { DemoClient } from "./demo";
import { LinkClient } from "./link";
import { type Fetch, RemoteClient, parseAddress } from "./remote";

type Call = { url: string; method: string; headers: Record<string, string>; body?: Uint8Array | string };

function fakeFetch(answer: (call: Call) => { status?: number; json?: unknown; bytes?: Uint8Array; headers?: Record<string, string> }) {
  const calls: Call[] = [];
  const fetchFn: Fetch = async (url, init) => {
    const call = { url, method: init.method, headers: init.headers, ...(init.body !== undefined ? { body: init.body } : {}) };
    calls.push(call);
    const reply = answer(call);
    return {
      status: reply.status ?? 200,
      headers: { get: (name: string) => reply.headers?.[name.toLowerCase()] ?? null },
      text: async () => JSON.stringify(reply.json ?? {}),
      arrayBuffer: async () => (reply.bytes ?? new Uint8Array()).slice().buffer,
    };
  };
  return { fetchFn, calls };
}

describe("Remote (Wi-Fi)", () => {
  it("takes an IPv4 address or the QR code's URL, not a host name", () => {
    expect(parseAddress("192.168.1.20")).toEqual({ host: "192.168.1.20", pin: null });
    expect(parseAddress(" http://10.0.0.7:8080/#pin=0427 ")).toEqual({ host: "10.0.0.7:8080", pin: "0427" });
    expect(parseAddress("quad-cortex.local")).toBeNull();
    expect(parseAddress("300.1.1.1")).toBeNull();
  });

  it("sends the PIN on every call and an empty body on a bodiless POST", async () => {
    const { fetchFn, calls } = fakeFetch(() => ({ json: { destination: 1, folders: [], irs: [] } }));
    const client = new RemoteClient("192.168.1.20", "0427", fetchFn);
    await client.libraryOp({ op: "link", id: "ir 1", folder: 3, linked: false });
    expect(calls[0]).toMatchObject({
      url: "http://192.168.1.20/api/library/link?id=ir%201&folder=3&linked=0",
      method: "POST",
      headers: { "x-tetrad-pin": "0427" },
      body: "",
    });
  });

  it("a refusal carries the unit's text; 401 asks for the PIN again", async () => {
    const { fetchFn } = fakeFetch((c) => (c.url.includes("irs") ? { status: 422, json: { error: "An IR name has 1 to 60 characters" } } : { status: 401, json: { error: "Wrong PIN" } }));
    const client = new RemoteClient("192.168.1.20", "1111", fetchFn);
    await expect(client.uploadIr("x", new Uint8Array(4))).rejects.toMatchObject({ message: "An IR name has 1 to 60 characters", pin: false, status: 422 });
    await expect(client.notices()).rejects.toMatchObject({ pin: true });
  });

  it("connects when the PIN is right even if the library is closed (422)", async () => {
    const closed = fakeFetch(() => ({ status: 422, json: { error: "The library is closed" } }));
    await expect(RemoteClient.connect("192.168.1.20", "1234", closed.fetchFn)).resolves.toBeInstanceOf(RemoteClient);
    const off = fakeFetch(() => ({ status: 503, json: { error: "Remote is off" } }));
    await expect(RemoteClient.connect("192.168.1.20", "1234", off.fetchFn)).rejects.toThrow("Remote is off");
  });

  it("a tab call answered with bad JSON is a refusal, though its status is 200", async () => {
    const { fetchFn, calls } = fakeFetch(() => ({ json: { error: "The extension answered with bad JSON" } }));
    const client = new RemoteClient("192.168.1.20", "1234", fetchFn);
    await expect(client.tabCall("midi-mapper", { op: "reset" })).rejects.toThrow("bad JSON");
    expect(calls[0]?.body).toBe('{"op":"reset"}');
  });

  it("a reply whose body stops coming times out", async () => {
    const fetchFn: Fetch = async (_url, init) => ({
      status: 200,
      headers: { get: () => null },
      text: () => new Promise((_, reject) => init.signal?.addEventListener("abort", () => reject(new Error("aborted")))),
      arrayBuffer: () => new Promise((_, reject) => init.signal?.addEventListener("abort", () => reject(new Error("aborted")))),
    });
    const client = new RemoteClient("192.168.1.20", "1234", fetchFn, 20);
    await expect(client.backupPiece(0)).rejects.toMatchObject({ refused: false, message: /did not answer/ });
  });

  it("takes the support file's name from Content-Disposition", async () => {
    const { fetchFn } = fakeFetch(() => ({ bytes: Uint8Array.of(0x50, 0x4b), headers: { "content-disposition": 'attachment; filename="tetrad-support-1FEB.zip"' } }));
    const file = await new RemoteClient("192.168.1.20", "1234", fetchFn).support();
    expect(file.name).toBe("tetrad-support-1FEB.zip");
    expect(Array.from(file.zip)).toEqual([0x50, 0x4b]);
  });
});

/** A unit behind the Link: `handle` answers each request header. */
function linkUnit(handle: (h: Header, body: Uint8Array) => Header | null, frameSize = 60) {
  let deliver: (f: Uint8Array) => void = () => {};
  const inbound = new Reassembler();
  const port: LinkPort & { push(h: Header): void } = {
    frameSize,
    async write(frame) {
      const whole = inbound.push(frame);
      if (!whole) return;
      const { header, body } = decode(whole);
      const reply = handle(header, body);
      if (reply) queueMicrotask(() => frames(encode({ ...reply, id: header.id ?? null }), frameSize).forEach((f) => deliver(f)));
    },
    listen(onFrame) {
      deliver = onFrame;
      return () => {};
    },
    async close() {},
    push(h) {
      frames(encode(h), frameSize).forEach((f) => deliver(f));
    },
  };
  return port;
}

describe("Tetrad Link (Bluetooth)", () => {
  const hello = (protocol: number, authorized = false) => ({ reply: "hello", tetrad: "0.1.0-alpha.10", protocol, name: "Quad Cortex 1FEB", authorized });

  it("reads hello, asks for the PIN, and has the preset only from protocol 2", async () => {
    let pinGiven = false;
    const unit = linkUnit((h): Header | null => {
      if (h.op === "hello") return hello(1);
      if (h.op === "auth") {
        if (h.pin !== "4711") return { error: "Wrong PIN" };
        pinGiven = true;
        return { reply: "auth", authorized: true };
      }
      return pinGiven ? { folder: "IRs" } : { error: "Give the PIN first (auth)" };
    });
    const client = await LinkClient.open(unit);
    expect(client.name).toBe("Quad Cortex 1FEB");
    expect(client.authorized).toBe(false);
    expect(client.has("extension-messages")).toBe(true);
    expect(client.has("preset")).toBe(false);
    await expect(client.status()).rejects.toMatchObject({ pin: true });
    await expect(client.auth("1234")).rejects.toMatchObject({ message: "Wrong PIN", pin: true });
    await client.auth("4711");
    await expect(client.status()).resolves.toEqual({ folder: "IRs" });

    const v2 = await LinkClient.open(linkUnit((h) => (h.op === "hello" ? hello(2, true) : null)));
    expect(v2.has("preset")).toBe(true);
  });

  it("sends Remote API requests through the remote op, with their query and body", async () => {
    const seen: [Json | undefined, Json | undefined, number][] = [];
    const unit = linkUnit((h, body): Header | null => {
      if (h.op === "hello") return hello(1, true);
      seen.push([h.method, h.path, body.length]);
      if (String(h.path).startsWith("/api/backup/file")) return { reply: "remote" };
      return String(h.path).includes("last=1") ? { name: "Plexi.nam", size: 102400 } : { received: 1 };
    });
    const client = await LinkClient.open(unit);
    const reply = await sendFile(client, "tetrad.nam-a2", "Plexi.nam", new Uint8Array(100 * 1024));
    expect(seen).toEqual([
      ["POST", "/api/files/chunk?id=tetrad.nam-a2&name=Plexi.nam&offset=0", 49152],
      ["POST", "/api/files/chunk?id=tetrad.nam-a2&name=Plexi.nam&offset=49152", 49152],
      ["POST", "/api/files/chunk?id=tetrad.nam-a2&name=Plexi.nam&offset=98304&last=1", 4096],
    ]);
    expect(reply).toEqual({ name: "Plexi.nam", size: 102400 });
    await client.backupPiece(0);
    expect(seen.at(-1)).toEqual(["GET", "/api/backup/file?offset=0", 0]);
  });

  it("sends hello again when the first one went while the link still paired", async () => {
    let hellos = 0;
    const unit = linkUnit((h) => {
      if (h.op !== "hello") return null;
      hellos++;
      return hellos === 1 ? null : hello(1, true);
    });
    const client = await LinkClient.open(unit, 20);
    expect(hellos).toBe(2);
    expect(client.name).toBe("Quad Cortex 1FEB");
  });

  it("says when the link was lost, but not when the app closed it", async () => {
    let fail: (e: Error) => void = () => {};
    const port: LinkPort = {
      frameSize: 60,
      async write(frame) {
        const { header } = decode(frame.subarray(1));
        queueMicrotask(() => deliver(frames(encode({ ...hello(1, true), id: header.id ?? null }), 60)));
      },
      listen(onFrame, onError) {
        deliver = (fs) => fs.forEach(onFrame);
        fail = onError;
        return () => {};
      },
      async close() {},
    };
    let deliver: (fs: Uint8Array[]) => void = () => {};
    const lost = await LinkClient.open(port);
    const seen: string[] = [];
    lost.onEvent((e) => e.kind === "closed" && seen.push(e.reason));
    fail(new Error("The unit disconnected"));
    expect(seen).toEqual(["The unit disconnected"]);

    const closed = await LinkClient.open(linkUnit((h) => (h.op === "hello" ? hello(1, true) : null)));
    closed.onEvent((e) => e.kind === "closed" && seen.push("closed by the app"));
    await closed.close();
    expect(seen).toEqual(["The unit disconnected"]);
  });

  it("passes on extension events and preset events", async () => {
    const unit = linkUnit((h) => (h.op === "hello" ? hello(2, true) : null));
    const client = await LinkClient.open(unit);
    const seen: string[] = [];
    client.onEvent((e) => seen.push(e.kind === "extension" ? `${e.extension}:${JSON.stringify(e.value)}` : e.kind));
    unit.push({ event: "extension", extension: "midi-mapper", value: { cc: 11 } });
    unit.push({ event: "preset", value: { loadId: 3, name: "x", place: "1A", scenes: [], scene: 0, grid: [], edited: false } });
    expect(seen).toEqual(['midi-mapper:{"cc":11}', "preset"]);
  });
});

describe("demo unit", () => {
  it("refuses an edit made on an older preset", async () => {
    const demo = new DemoClient(0);
    const preset = await demo.preset();
    await demo.presetEdit(preset.loadId, { kind: "bypass", at: { row: 0, column: 0 }, bypass: true });
    await expect(demo.presetEdit(preset.loadId, { kind: "scene", scene: 1 })).rejects.toThrow(/changed/);
    await demo.close();
  });

  it("takes file pieces in order only", async () => {
    const demo = new DemoClient(0);
    await expect(demo.fileChunk("tetrad.nam-a2", "New.nam", 10, false, new Uint8Array(4))).rejects.toThrow(/start/);
    await sendFile(demo, "tetrad.nam-a2", "New.nam", new Uint8Array(60 * 1024));
    const files = await demo.files();
    expect(files.extensions[1]?.files.find((f) => f.name === "New.nam")).toMatchObject({ size: 61440, state: "waiting" });
    await demo.close();
  });

  it("checks footswitch labels as the unit does", async () => {
    const demo = new DemoClient(0);
    const edit = (label: string) => demo.footswitchCommand({ op: "slot", scope: "global", page: 0, slot: 0, edit: { kind: "label", label } });
    await expect(edit("A label far too long")).rejects.toThrow(/16/);
    const view = await edit("Verse");
    expect(view.mode && "layouts" in view ? view.layouts.global[0]?.[0]?.label : null).toBe("Verse");
    await demo.close();
  });
});
