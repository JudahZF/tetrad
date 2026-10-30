import { describe, expect, it } from "vitest";
import { fromBase64, toBase64 } from "./base64";
import { END, Reassembler, START, frames } from "./frames";
import { decode, encode } from "./message";

describe("Tetrad Link codec", () => {
  it("matches the unit's message layout: u32 LE header length, JSON, body", () => {
    const message = encode({ id: 9, op: "ir-upload", name: "V30" }, new Uint8Array([82, 73, 70, 70]));
    const json = '{"id":9,"op":"ir-upload","name":"V30"}';
    expect(Array.from(message.subarray(0, 4))).toEqual([json.length, 0, 0, 0]);
    expect(new TextDecoder().decode(message.subarray(4, 4 + json.length))).toBe(json);
    const { header, body } = decode(message);
    expect(header).toEqual({ id: 9, op: "ir-upload", name: "V30" });
    expect(Array.from(body)).toEqual([82, 73, 70, 70]);
  });

  it("refuses a header that runs past the message", () => {
    const message = encode({ id: 1 });
    expect(() => decode(message.subarray(0, message.length - 1))).toThrow();
  });

  // The same cases as crates/tetrad-ble/src/link.rs.
  it("cuts messages into frames and joins them again", () => {
    const message = Uint8Array.from({ length: 256 }, (_, i) => i);
    const cut = frames(message, 20);
    expect(cut).toHaveLength(14);
    expect(cut.every((f) => f.length <= 20)).toBe(true);
    expect(cut[0]?.[0]).toBe(START);
    expect(cut[13]?.[0]).toBe(END);
    const r = new Reassembler();
    const joined = cut.map((f) => r.push(f)).filter((m) => m !== null);
    expect(joined).toEqual([message]);
    expect(frames(new Uint8Array(), 20)).toEqual([Uint8Array.of(START | END)]);
  });

  it("base64 round-trips BLE values of every length", () => {
    for (let n = 0; n < 8; n++) {
      const bytes = Uint8Array.from({ length: n }, (_, i) => 250 - i * 37);
      expect(toBase64(bytes)).toBe(btoa(String.fromCharCode(...bytes)));
      expect(fromBase64(toBase64(bytes))).toEqual(bytes);
    }
  });

  it("refuses a frame out of place, and a new START begins again", () => {
    const r = new Reassembler(8);
    expect(() => r.push(Uint8Array.of(0, 1, 2))).toThrow(/no START/);
    expect(() => r.push(new Uint8Array())).toThrow(/empty/);
    expect(r.push(Uint8Array.of(START, 0))).toBeNull();
    expect(() => r.push(new Uint8Array(10))).toThrow(/longer/);
    expect(r.push(Uint8Array.of(START | END, 7))).toEqual(Uint8Array.of(7));
  });
});
