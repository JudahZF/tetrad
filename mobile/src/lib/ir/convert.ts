/**
 * An audio file to the IR the unit takes, as the Remote page makes it (docs/remote-api.md §5):
 * decoded, resampled to 48 kHz, mixed to one channel (Web Audio's down-mix rules), the first
 * 1,024 samples, written as a mono 32-bit float WAV with the stock 46-byte header. No gain, no
 * fade, no normalising: the page does none.
 */

export const RATE = 48_000;
export const SAMPLES = 1024;
export const MAX_NAME = 60;

export interface Decoded {
  rate: number;
  /** One Float32Array per channel. */
  channels: Float32Array[];
}

export interface Converted {
  wav: Uint8Array;
  /** "48 kHz stereo → mono, first 1024 samples": what the unit gets. */
  detail: string;
}

export class IrError extends Error {}

const ascii = (bytes: Uint8Array, at: number) => String.fromCharCode(...bytes.subarray(at, at + 4));

/** WAV (RIFF/RF64 not needed) and AIFF/AIFC, integer PCM and float. */
export function decode(bytes: Uint8Array): Decoded {
  if (bytes.length >= 12 && ascii(bytes, 0) === "RIFF" && ascii(bytes, 8) === "WAVE") return decodeWav(bytes);
  if (bytes.length >= 12 && ascii(bytes, 0) === "FORM" && ["AIFF", "AIFC"].includes(ascii(bytes, 8))) return decodeAiff(bytes);
  throw new IrError("Not an audio file (WAV or AIFF)");
}

function decodeWav(bytes: Uint8Array): Decoded {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let format: { tag: number; channels: number; rate: number; bits: number } | null = null;
  for (let at = 12; at + 8 <= bytes.length; ) {
    const id = ascii(bytes, at);
    const size = view.getUint32(at + 4, true);
    const body = at + 8;
    if (id === "fmt ") {
      let tag = view.getUint16(body, true);
      // WAVE_FORMAT_EXTENSIBLE: the real tag is the first two bytes of the sub-format GUID.
      if (tag === 0xfffe && size >= 40) tag = view.getUint16(body + 24, true);
      format = { tag, channels: view.getUint16(body + 2, true), rate: view.getUint32(body + 4, true), bits: view.getUint16(body + 14, true) };
    } else if (id === "data") {
      if (!format) throw new IrError("A WAV file with its data before its format");
      const end = Math.min(body + size, bytes.length);
      return { rate: format.rate, channels: pcm(view, body, end, format.channels, format.bits, format.tag === 3, true) };
    }
    at = body + size + (size & 1);
  }
  throw new IrError("A WAV file with no audio data");
}

/** The 80-bit IEEE extended float of an AIFF sample rate. */
function extended(view: DataView, at: number): number {
  const exponent = view.getUint16(at) & 0x7fff;
  const hi = view.getUint32(at + 2);
  const lo = view.getUint32(at + 6);
  return (hi * 2 ** 32 + lo) * 2 ** (exponent - 16383 - 63);
}

function decodeAiff(bytes: Uint8Array): Decoded {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let common: { channels: number; bits: number; rate: number; float: boolean; little: boolean } | null = null;
  for (let at = 12; at + 8 <= bytes.length; ) {
    const id = ascii(bytes, at);
    const size = view.getUint32(at + 4);
    const body = at + 8;
    if (id === "COMM") {
      const compression = size >= 22 ? ascii(bytes, body + 18) : "NONE";
      if (!["NONE", "sowt", "fl32", "FL32", "fl64", "FL64"].includes(compression)) {
        throw new IrError(`AIFF compression ${compression} is not supported`);
      }
      const float = compression.toLowerCase().startsWith("fl");
      common = {
        channels: view.getUint16(body),
        bits: float ? (compression.endsWith("64") ? 64 : 32) : view.getUint16(body + 6),
        rate: extended(view, body + 8),
        float,
        little: compression === "sowt",
      };
    } else if (id === "SSND") {
      if (!common) throw new IrError("An AIFF file with its sound before its format");
      const offset = view.getUint32(body);
      const start = body + 8 + offset;
      const end = Math.min(body + size, bytes.length);
      return { rate: common.rate, channels: pcm(view, start, end, common.channels, common.bits, common.float, common.little) };
    }
    at = body + size + (size & 1);
  }
  throw new IrError("An AIFF file with no sound data");
}

function pcm(view: DataView, start: number, end: number, count: number, bits: number, float: boolean, little: boolean): Float32Array[] {
  if (count < 1) throw new IrError("An audio file with no channels");
  const width = bits / 8;
  if (!Number.isInteger(width) || width < 1 || width > 8) throw new IrError(`${bits}-bit audio is not supported`);
  const frames = Math.floor((end - start) / (width * count));
  const channels = Array.from({ length: count }, () => new Float32Array(frames));
  const read = (at: number): number => {
    if (float) {
      if (width === 4) return view.getFloat32(at, little);
      if (width === 8) return view.getFloat64(at, little);
      throw new IrError(`${bits}-bit float audio is not supported`);
    }
    switch (width) {
      case 1:
        return (view.getUint8(at) - 128) / 128; // 8-bit WAV is unsigned
      case 2:
        return view.getInt16(at, little) / 32768;
      case 3: {
        const [b0, b1, b2] = little ? [view.getUint8(at), view.getUint8(at + 1), view.getUint8(at + 2)] : [view.getUint8(at + 2), view.getUint8(at + 1), view.getUint8(at)];
        return (((b2 << 24) | (b1 << 16) | (b0 << 8)) >> 8) / 8388608;
      }
      case 4:
        return view.getInt32(at, little) / 2147483648;
      default:
        throw new IrError(`${bits}-bit integer audio is not supported`);
    }
  };
  for (let f = 0; f < frames; f++) {
    for (let c = 0; c < count; c++) {
      const channel = channels[c];
      if (channel) channel[f] = read(start + (f * count + c) * width);
    }
  }
  return channels;
}

/** Web Audio's "speakers" down-mix to mono: 1, 2, 4 and 6 channels; other counts keep channel 0. */
export function toMono(channels: Float32Array[]): Float32Array {
  const [l, r, c3, c4, c5, c6] = channels;
  if (!l) throw new IrError("An audio file with no channels");
  const out = new Float32Array(l.length);
  for (let i = 0; i < l.length; i++) {
    const at = (ch: Float32Array | undefined) => ch?.[i] ?? 0;
    out[i] =
      channels.length === 2 ? 0.5 * (at(l) + at(r))
      : channels.length === 4 ? 0.25 * (at(l) + at(r) + at(c3) + at(c4))
      // 5.1: L R C LFE SL SR
      : channels.length === 6 ? Math.SQRT1_2 * (at(l) + at(r)) + at(c3) + 0.5 * (at(c5) + at(c6))
      : at(l);
  }
  return out;
}

/** The first `count` samples of `input` at `to` Hz: a Blackman-windowed sinc. */
export function resample(input: Float32Array, from: number, to: number, count: number): Float32Array {
  const frames = Math.min(count, Math.floor((input.length * to) / from));
  if (from === to) return input.slice(0, frames);
  const ratio = from / to;
  const cutoff = Math.min(1, to / from);
  const half = Math.ceil(32 / cutoff);
  const out = new Float32Array(frames);
  for (let n = 0; n < frames; n++) {
    const centre = n * ratio;
    const first = Math.ceil(centre - half);
    let sum = 0;
    for (let k = first; k <= centre + half; k++) {
      const x = input[k];
      if (x === undefined) continue;
      const t = k - centre;
      const sinc = t === 0 ? 1 : Math.sin(Math.PI * cutoff * t) / (Math.PI * cutoff * t);
      const w = (t + half) / (2 * half);
      const window = 0.42 - 0.5 * Math.cos(2 * Math.PI * w) + 0.08 * Math.cos(4 * Math.PI * w);
      sum += x * cutoff * sinc * window;
    }
    out[n] = sum;
  }
  return out;
}

/** The stock IR WAV: RIFF, an 18-byte IEEE-float fmt chunk, the data chunk at byte 38. */
export function encodeWav(samples: Float32Array): Uint8Array {
  const bytes = new Uint8Array(46 + samples.length * 4);
  const view = new DataView(bytes.buffer);
  const tag = (at: number, text: string) => [...text].forEach((c, i) => view.setUint8(at + i, c.charCodeAt(0)));
  tag(0, "RIFF");
  view.setUint32(4, 38 + samples.length * 4, true);
  tag(8, "WAVE");
  tag(12, "fmt ");
  view.setUint32(16, 18, true);
  view.setUint16(20, 3, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, RATE, true);
  view.setUint32(28, RATE * 4, true);
  view.setUint16(32, 4, true);
  view.setUint16(34, 32, true);
  view.setUint16(36, 0, true);
  tag(38, "data");
  view.setUint32(42, samples.length * 4, true);
  samples.forEach((sample, i) => view.setFloat32(46 + i * 4, sample, true));
  return bytes;
}

export function convert(bytes: Uint8Array): Converted {
  const decoded = decode(bytes);
  const source = decoded.channels[0]?.length ?? 0;
  if (source === 0) throw new IrError("An audio file with no samples");
  const mono = toMono(decoded.channels);
  const samples = resample(mono, decoded.rate, RATE, SAMPLES);
  const count = decoded.channels.length;
  const channels = count === 1 ? "mono" : `${count === 2 ? "stereo" : `${count} channels`} → mono`;
  const cut = (source * RATE) / decoded.rate > SAMPLES ? `, first ${SAMPLES} samples` : "";
  const rate = decoded.rate === RATE ? "48 kHz" : `${decoded.rate / 1000} → 48 kHz`;
  return { wav: encodeWav(samples), detail: `${rate} ${channels}${cut}` };
}

/** The file name without its extension, as the unit shows names: at most 60 UTF-16 units. */
export function irName(fileName: string): string {
  let name = fileName.replace(/\.[^.]*$/, "").replace(/\s+/g, " ").trim();
  if (name.length > MAX_NAME) {
    name = name.slice(0, MAX_NAME);
    const last = name.charCodeAt(name.length - 1);
    if (last >= 0xd800 && last <= 0xdbff) name = name.slice(0, -1);
  }
  return name;
}

/** An extension file's name as the unit takes it (docs/remote-api.md §8), or null. */
export function extensionFileName(fileName: string, extensions: readonly string[]): string | null {
  const extension = extensions.find((x) => fileName.toLowerCase().endsWith(x));
  if (!extension) return null;
  const stem = fileName
    .slice(0, fileName.length - extension.length)
    .replace(/[^A-Za-z0-9 ._-]/g, "")
    .replace(/\s+/g, " ")
    .trim()
    .replace(/^\.+/, "")
    .slice(0, 48)
    .trim();
  return stem ? `${stem}${extension}` : null;
}
