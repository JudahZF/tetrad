import { describe, expect, it } from "vitest";
import { Reassembler, frames } from "./frames";
import { type Header, decode, encode } from "./message";
import { type LinkPort, LinkSession } from "./session";

/** A unit on the far side of a fake radio: answers each request with `answer`. */
function fakePort(answer: (header: Header, body: Uint8Array) => { header: Header; body?: Uint8Array } | null, frameSize = 20) {
  let deliver: (frame: Uint8Array) => void = () => {};
  const inbound = new Reassembler();
  const written: Uint8Array[] = [];
  const port: LinkPort & { push: (header: Header) => void; written: Uint8Array[] } = {
    frameSize,
    written,
    async write(frame) {
      written.push(frame);
      const whole = inbound.push(frame);
      if (!whole) return;
      const { header, body } = decode(whole);
      const reply = answer(header, body);
      if (!reply) return;
      // Replies arrive later, cut to the same frame size.
      setTimeout(() => {
        for (const f of frames(encode({ ...reply.header, id: header.id ?? null }, reply.body), frameSize)) deliver(f);
      }, 1);
    },
    listen(onFrame) {
      deliver = onFrame;
      return () => {
        deliver = () => {};
      };
    },
    async close() {},
    push(header) {
      for (const f of frames(encode(header), frameSize)) deliver(f);
    },
  };
  return port;
}

describe("Tetrad Link session", () => {
  it("frames requests to the port size and matches replies by id", async () => {
    const port = fakePort((h, body) => ({ header: { reply: h.op ?? null, size: body.length } }));
    const session = new LinkSession(port);
    const [a, b] = await Promise.all([
      session.request("hello"),
      session.request("ir-upload", { name: "V30" }, new Uint8Array(100)),
    ]);
    expect(a.header).toMatchObject({ reply: "hello", id: 1 });
    expect(b.header).toMatchObject({ reply: "ir-upload", size: 100, id: 2 });
    expect(port.written.every((f) => f.length <= 20)).toBe(true);
  });

  it("a reply with error rejects as a refusal", async () => {
    const session = new LinkSession(fakePort(() => ({ header: { error: "Give the PIN first (auth)" } })));
    await expect(session.request("files")).rejects.toMatchObject({ refused: true, message: "Give the PIN first (auth)" });
  });

  it("a request with no reply times out, and the next one still goes", async () => {
    let first = true;
    const port = fakePort((h) => {
      if (first) {
        first = false;
        return null;
      }
      return { header: { reply: h.op ?? null } };
    });
    const session = new LinkSession(port, 20);
    await expect(session.request("tabs")).rejects.toMatchObject({ refused: false });
    await expect(session.request("tabs")).resolves.toMatchObject({ header: { reply: "tabs" } });
  });

  it("hands on extension events, which carry no id", async () => {
    const port = fakePort(() => null);
    const session = new LinkSession(port);
    const events: unknown[] = [];
    session.onEvent((e) => events.push(e));
    port.push({ event: "extension", extension: "midi-mapper", value: { cc: 11 } });
    expect(events).toEqual([{ event: "extension", extension: "midi-mapper", value: { cc: 11 } }]);
  });

  it("a link lost while a request goes out rejects it once, with nothing left unhandled", async () => {
    let lose: (error: Error) => void = () => {};
    const port: LinkPort = {
      frameSize: 20,
      async write() {
        lose(new Error("The unit disconnected"));
        throw new Error("not connected");
      },
      listen(_onFrame, onError) {
        lose = onError;
        return () => {};
      },
      async close() {},
    };
    const session = new LinkSession(port);
    await expect(session.request("hello")).rejects.toThrow(/write failed/);
  });

  it("closing rejects what still waits", async () => {
    const session = new LinkSession(fakePort(() => null));
    const waiting = session.request("support");
    await Promise.resolve();
    await session.close();
    await expect(waiting).rejects.toThrow(/closed/);
  });
});
