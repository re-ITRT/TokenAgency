// 生成 app-icon.png（1024×1024）——不依赖任何三方库，用 Node 内置 zlib 手写 PNG。
// 图案：深色底 + 蓝色圆环 + 中心白点（"中转节点"）
// 用法：node scripts/make-icon.mjs [outPath]

import { deflateSync } from "node:zlib";
import { writeFileSync } from "node:fs";

const SIZE = 1024;
const SS = 3; // 超采样倍数（抗锯齿）

const BG = [0x16, 0x1b, 0x22, 0xff];
const BLUE = [0x4c, 0x8d, 0xff, 0xff];
const WHITE = [0xf0, 0xf6, 0xfc, 0xff];

const cx = SIZE / 2;
const cy = SIZE / 2;
const rOuter = SIZE * 0.36;
const rInner = SIZE * 0.26;
const rDot = SIZE * 0.10;
// 外环上开四个缺口，像"四条线路"
const gapW = 0.16;

function sample(x, y) {
  const dx = x - cx;
  const dy = y - cy;
  const d = Math.hypot(dx, dy);

  if (d <= rDot) return WHITE;
  if (d >= rInner && d <= rOuter) {
    // 角度缺口（0/90/180/270 附近挖空）
    const a = Math.atan2(dy, dx);
    const quarter = Math.abs(((a / (Math.PI / 2)) % 2 + 2) % 2 - 1); // 0..1，1=在轴线上
    if (quarter > 1 - gapW) return BG;
    return BLUE;
  }
  return BG;
}

function crc32(buf) {
  let c;
  const table = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c;
  }
  let crc = -1;
  for (let i = 0; i < buf.length; i++) crc = (crc >>> 8) ^ table[(crc ^ buf[i]) & 0xff];
  return (crc ^ -1) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length, 0);
  const typeBuf = Buffer.from(type, "ascii");
  const body = Buffer.concat([typeBuf, data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([len, body, crc]);
}

const raw = Buffer.alloc(SIZE * (SIZE * 4 + 1));
let p = 0;
for (let y = 0; y < SIZE; y++) {
  raw[p++] = 0; // filter: none
  for (let x = 0; x < SIZE; x++) {
    let r = 0, g = 0, b = 0, a = 0;
    for (let sy = 0; sy < SS; sy++) {
      for (let sx = 0; sx < SS; sx++) {
        const c = sample(x + (sx + 0.5) / SS, y + (sy + 0.5) / SS);
        r += c[0]; g += c[1]; b += c[2]; a += c[3];
      }
    }
    const n = SS * SS;
    raw[p++] = Math.round(r / n);
    raw[p++] = Math.round(g / n);
    raw[p++] = Math.round(b / n);
    raw[p++] = Math.round(a / n);
  }
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 6; // color type RGBA
ihdr[10] = 0;
ihdr[11] = 0;
ihdr[12] = 0;

const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);

const out = process.argv[2] ?? "app-icon.png";
writeFileSync(out, png);
console.log(`wrote ${out} (${png.length} bytes, ${SIZE}x${SIZE})`);
