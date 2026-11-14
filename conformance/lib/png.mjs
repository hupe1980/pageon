// A small PNG decoder for tests (8-bit RGB/RGBA, non-interlaced: what pageon writes). No
// dependency beyond Node's zlib.
import { inflateSync } from 'node:zlib';

/** Decodes a PNG buffer to `{ width, height, pixel(x, y) → [r, g, b, a] }`. */
export function decodePng(buf) {
  if (buf.readUInt32BE(0) !== 0x89504e47) throw new Error('not a PNG');
  let off = 8;
  let width = 0, height = 0, channels = 0;
  const idat = [];
  while (off < buf.length) {
    const len = buf.readUInt32BE(off);
    const type = buf.toString('latin1', off + 4, off + 8);
    const data = buf.subarray(off + 8, off + 8 + len);
    if (type === 'IHDR') {
      width = data.readUInt32BE(0);
      height = data.readUInt32BE(4);
      const color = data[9];
      if (data[8] !== 8 || data[12] !== 0 || (color !== 2 && color !== 6)) throw new Error('unsupported PNG');
      channels = color === 6 ? 4 : 3;
    } else if (type === 'IDAT') idat.push(data);
    else if (type === 'IEND') break;
    off += 12 + len;
  }
  const raw = inflateSync(Buffer.concat(idat));
  const stride = width * channels;
  const px = Buffer.alloc(stride * height);
  for (let y = 0; y < height; y++) {
    const filter = raw[y * (stride + 1)];
    const line = raw.subarray(y * (stride + 1) + 1, (y + 1) * (stride + 1));
    for (let x = 0; x < stride; x++) {
      const a = x >= channels ? px[y * stride + x - channels] : 0;
      const b = y > 0 ? px[(y - 1) * stride + x] : 0;
      const c = x >= channels && y > 0 ? px[(y - 1) * stride + x - channels] : 0;
      let v = line[x];
      if (filter === 1) v += a;
      else if (filter === 2) v += b;
      else if (filter === 3) v += (a + b) >> 1;
      else if (filter === 4) {
        const p = a + b - c;
        const pa = Math.abs(p - a), pb = Math.abs(p - b), pc = Math.abs(p - c);
        v += pa <= pb && pa <= pc ? a : pb <= pc ? b : c;
      }
      px[y * stride + x] = v & 0xff;
    }
  }
  return {
    width,
    height,
    pixel(x, y) {
      const i = y * stride + x * channels;
      return [px[i], px[i + 1], px[i + 2], channels === 4 ? px[i + 3] : 255];
    },
  };
}

/** Whether `[r, g, b]` is within 8 of `want` on every channel. */
export const near = (p, want) => want.every((v, i) => Math.abs(p[i] - v) <= 8);
