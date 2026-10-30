/**
 * Tetrad Link frames (crates/tetrad-ble/src/link.rs): one flags byte, then a piece of a
 * message. A message runs from a START frame to an END frame.
 */

export const START = 0x01;
export const END = 0x02;
/** The largest message the unit takes from the app. */
export const MAX_MESSAGE = 64 * 1024;

/** The frames of `message`, each at most `max` bytes with its flags byte. */
export function frames(message: Uint8Array, max: number): Uint8Array[] {
  const room = Math.max(max - 1, 1);
  const count = Math.max(Math.ceil(message.length / room), 1);
  return Array.from({ length: count }, (_, i) => {
    const piece = message.subarray(i * room, (i + 1) * room);
    const frame = new Uint8Array(1 + piece.length);
    frame[0] = (i === 0 ? START : 0) | (i === count - 1 ? END : 0);
    frame.set(piece, 1);
    return frame;
  });
}

/**
 * Joins frames into messages. A START drops a message that did not end. Replies from the unit
 * can be larger than what the app may send, so the limit is the caller's.
 */
export class Reassembler {
  private parts: Uint8Array[] | null = null;
  private length = 0;

  constructor(private readonly limit = 8 * 1024 * 1024) {}

  /** One frame; returns a complete message, or null. Throws on a frame out of place. */
  push(frame: Uint8Array): Uint8Array | null {
    const flags = frame[0];
    if (flags === undefined) throw new Error("an empty Tetrad Link frame");
    if (flags & START) {
      this.parts = [];
      this.length = 0;
    }
    if (this.parts === null) throw new Error("a Tetrad Link frame with no START before it");
    const piece = frame.subarray(1);
    if (this.length + piece.length > this.limit) {
      this.parts = null;
      throw new Error(`a Tetrad Link message longer than ${this.limit} bytes`);
    }
    this.parts.push(piece);
    this.length += piece.length;
    if (!(flags & END)) return null;
    const message = new Uint8Array(this.length);
    let at = 0;
    for (const part of this.parts) {
      message.set(part, at);
      at += part.length;
    }
    this.parts = null;
    return message;
  }
}
