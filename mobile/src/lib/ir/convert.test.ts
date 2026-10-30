import { describe, expect, it } from "vitest";
import { IrError, convert, decode, encodeWav, extensionFileName, irName, resample, toMono } from "./convert";

/** A PCM WAV with `channels` interleaved, as a DAW writes it. */
function wav16(rate: number, channels: number[][]): Uint8Array {
  const frames = channels[0]?.length ?? 0;
  const bytes = new Uint8Array(44 + frames * channels.length * 2);
  const v = new DataView(bytes.buffer);
  const tag = (at: number, t: string) => [...t].forEach((c, i) => v.setUint8(at + i, c.charCodeAt(0)));
  tag(0, "RIFF");
  v.setUint32(4, bytes.length - 8, true);
  tag(8, "WAVE");
  tag(12, "fmt ");
  v.setUint32(16, 16, true);
  v.setUint16(20, 1, true);
  v.setUint16(22, channels.length, true);
  v.setUint32(24, rate, true);
  v.setUint16(34, 16, true);
  tag(36, "data");
  v.setUint32(40, frames * channels.length * 2, true);
  for (let f = 0; f < frames; f++) channels.forEach((ch, c) => v.setInt16(44 + (f * channels.length + c) * 2, Math.round((ch[f] ?? 0) * 32767), true));
  return bytes;
}

describe("IR conversion", () => {
  it("writes the stock header the unit checks, byte for byte", () => {
    const out = encodeWav(new Float32Array([0.5, -0.25]));
    const v = new DataView(out.buffer);
    expect(out.length).toBe(54);
    expect(String.fromCharCode(...out.subarray(0, 4))).toBe("RIFF");
    expect(v.getUint32(4, true)).toBe(out.length - 8);
    expect([v.getUint32(16, true), v.getUint16(20, true), v.getUint16(22, true), v.getUint32(24, true), v.getUint16(34, true)]).toEqual([18, 3, 1, 48000, 32]);
    expect(String.fromCharCode(...out.subarray(38, 42))).toBe("data");
    expect(v.getFloat32(46, true)).toBe(0.5);
  });

  it("a 48 kHz stereo file: ½ L + ½ R, the first 1,024 samples, no gain", () => {
    const left = Array.from({ length: 2000 }, (_, i) => (i === 0 ? 0.8 : 0));
    const right = Array.from({ length: 2000 }, (_, i) => (i === 0 ? 0.2 : i === 1 ? 0.4 : 0));
    const { wav, detail } = convert(wav16(48000, [left, right]));
    const v = new DataView(wav.buffer);
    expect(wav.length).toBe(46 + 4096);
    expect(v.getFloat32(46, true)).toBeCloseTo(0.5, 3);
    expect(v.getFloat32(50, true)).toBeCloseTo(0.2, 3);
    expect(detail).toBe("48 kHz stereo → mono, first 1024 samples");
  });

  it("a short mono file stays short", () => {
    const { wav, detail } = convert(wav16(48000, [[0.1, 0.2, 0.3]]));
    expect(wav.length).toBe(46 + 12);
    expect(detail).toBe("48 kHz mono");
  });

  it("44.1 kHz resamples to 48 kHz and keeps a tone's level", () => {
    const tone = Array.from({ length: 4410 }, (_, i) => 0.5 * Math.sin((2 * Math.PI * 1000 * i) / 44100));
    const decoded = decode(wav16(44100, [tone]));
    const out = resample(toMono(decoded.channels), 44100, 48000, 1024);
    expect(out.length).toBe(1024);
    const peak = Math.max(...out.subarray(200, 800).map(Math.abs));
    expect(peak).toBeGreaterThan(0.48);
    expect(peak).toBeLessThan(0.52);
  });

  it("mixes 5.1 as Web Audio does and keeps only channel 0 of other counts", () => {
    const ch = (x: number) => new Float32Array([x]);
    expect(toMono([ch(1), ch(1), ch(1), ch(9), ch(1), ch(1)])[0]).toBeCloseTo(Math.SQRT2 + 1 + 1, 5);
    expect(toMono([ch(0.3), ch(1), ch(1)])[0]).toBeCloseTo(0.3, 5);
  });

  it("refuses what is not audio", () => {
    expect(() => convert(new TextEncoder().encode("not a wav file at all"))).toThrow(IrError);
  });

  it("names: no extension, one space, at most 60 units", () => {
    expect(irName("V30  Mix   57.wav")).toBe("V30 Mix 57");
    expect(irName(`${"x".repeat(70)}.wav`)).toHaveLength(60);
    expect(extensionFileName("Plexi (crunch)+.NAM", [".nam"])).toBe("Plexi crunch.nam");
    expect(extensionFileName("model.txt", [".nam"])).toBeNull();
  });
});
