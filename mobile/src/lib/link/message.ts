/**
 * The message format of the Tetrad Link (and the USB link, docs/usb.md): the length of a JSON
 * header as a little-endian u32, the JSON header, then a binary body.
 */

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type Header = { [key: string]: Json };

export interface Message {
  header: Header;
  body: Uint8Array;
}

const utf8 = new TextEncoder();
const fromUtf8 = new TextDecoder();

export function encode(header: Header, body: Uint8Array = new Uint8Array()): Uint8Array {
  const json = utf8.encode(JSON.stringify(header));
  const out = new Uint8Array(4 + json.length + body.length);
  new DataView(out.buffer).setUint32(0, json.length, true);
  out.set(json, 4);
  out.set(body, 4 + json.length);
  return out;
}

export function decode(message: Uint8Array): Message {
  if (message.length < 4) throw new Error("a Tetrad message shorter than its header length");
  const view = new DataView(message.buffer, message.byteOffset, message.byteLength);
  const length = view.getUint32(0, true);
  if (4 + length > message.length) throw new Error("a Tetrad message header past its end");
  const header: unknown = JSON.parse(fromUtf8.decode(message.subarray(4, 4 + length)));
  if (typeof header !== "object" || header === null || Array.isArray(header)) {
    throw new Error("a Tetrad message header that is not an object");
  }
  return { header: header as Header, body: message.slice(4 + length) };
}
