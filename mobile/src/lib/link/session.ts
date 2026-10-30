/**
 * Requests and replies over the Tetrad Link (docs/bluetooth.md). The port carries frames; this
 * layer cuts messages into frames, matches replies to requests by `id`, and hands on the
 * events that extensions push. The unit answers one request at a time, in order, so requests
 * wait for the one before them.
 */

import { Reassembler, frames } from "./frames";
import { type Header, type Json, type Message, decode, encode } from "./message";

/** The radio side: frames to and from the unit's GATT characteristics. */
export interface LinkPort {
  /** Room for one frame: the ATT MTU less 3. */
  readonly frameSize: number;
  write(frame: Uint8Array): Promise<void>;
  /** Starts the delivery of notified frames; returns the stop. */
  listen(onFrame: (frame: Uint8Array) => void, onError: (error: Error) => void): () => void;
  close(): Promise<void>;
}

export class LinkError extends Error {
  constructor(
    message: string,
    /** The unit refused the request (a reply with `error`), as opposed to a lost link. */
    readonly refused: boolean,
    /** No reply came in time; the link may still be there. */
    readonly timedOut = false,
  ) {
    super(message);
  }
}

export interface LinkEvent {
  event: string;
  extension?: string;
  value?: Json;
}

interface Pending {
  resolve: (message: Message) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

export class LinkSession {
  private nextId = 1;
  private readonly pending = new Map<number, Pending>();
  private queue: Promise<unknown> = Promise.resolve();
  private readonly reassembler = new Reassembler();
  private readonly stop: () => void;
  private readonly listeners = new Set<(event: LinkEvent) => void>();
  private readonly endings = new Set<(reason: string) => void>();
  private closed = false;

  constructor(
    private readonly port: LinkPort,
    private readonly timeoutMs = 20_000,
  ) {
    this.stop = port.listen(
      (frame) => this.onFrame(frame),
      (error) => this.fail(error),
    );
  }

  /** Sends `op` with its fields and body; resolves with the reply, rejects with a `LinkError`. */
  request(op: string, fields: Header = {}, body?: Uint8Array, timeoutMs = this.timeoutMs): Promise<Message> {
    const run = () => this.send(op, fields, body, timeoutMs);
    const next = this.queue.then(run, run);
    this.queue = next.catch(() => undefined);
    return next;
  }

  onEvent(listener: (event: LinkEvent) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /** The link ended: closed here, or lost (the unit went away). */
  onClosed(listener: (reason: string) => void): () => void {
    this.endings.add(listener);
    return () => this.endings.delete(listener);
  }

  async close() {
    if (this.closed) return;
    this.fail(new LinkError("The Bluetooth link closed", false));
    await this.port.close();
  }

  private async send(op: string, fields: Header, body: Uint8Array | undefined, timeoutMs: number) {
    if (this.closed) throw new LinkError("The Bluetooth link closed", false);
    const id = this.nextId++;
    const reply = new Promise<Message>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new LinkError(`The unit did not answer (${op})`, false, true));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
    });
    // A timeout or a lost link can reject it while the frames still go out, before the await.
    reply.catch(() => undefined);
    try {
      for (const frame of frames(encode({ ...fields, id, op }, body), this.port.frameSize)) {
        await this.port.write(frame);
      }
    } catch (error) {
      const pending = this.pending.get(id);
      if (pending) {
        clearTimeout(pending.timer);
        this.pending.delete(id);
      }
      throw new LinkError(`Bluetooth write failed: ${String(error)}`, false);
    }
    const message = await reply;
    const refusal = message.header.error;
    if (typeof refusal === "string") throw new LinkError(refusal, true);
    return message;
  }

  private onFrame(frame: Uint8Array) {
    let message: Message;
    try {
      const whole = this.reassembler.push(frame);
      if (!whole) return;
      message = decode(whole);
    } catch {
      return; // A broken message: its request times out.
    }
    const { id, event } = message.header;
    if (typeof id === "number") {
      const pending = this.pending.get(id);
      if (!pending) return; // A late reply after a timeout.
      clearTimeout(pending.timer);
      this.pending.delete(id);
      pending.resolve(message);
    } else if (typeof event === "string") {
      const { extension, value } = message.header;
      const out: LinkEvent = { event, ...(typeof extension === "string" ? { extension } : {}), ...(value !== undefined ? { value } : {}) };
      for (const listener of this.listeners) listener(out);
    }
  }

  private fail(error: Error) {
    if (this.closed) return;
    this.closed = true;
    this.stop();
    for (const ending of this.endings) ending(error.message);
    for (const [, pending] of this.pending) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    this.pending.clear();
  }
}
