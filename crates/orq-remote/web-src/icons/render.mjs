// Renders the app icons in ../../web/icons from the shapes in orq.svg. Run once after changing the
// icon (node crates/orq-remote/web-src/icons/render.mjs, from any folder); the PNGs are checked in, so
// nothing here runs when the server builds or starts. No dependencies: shapes are drawn with 4x4
// supersampling and the PNG is written with node's zlib. `--out-dir <dir>` writes them elsewhere, as
// the test that compares a fresh render with the checked-in files does.
import { deflateSync } from "node:zlib";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const flag = process.argv.indexOf("--out-dir");
if (flag !== -1 && !process.argv[flag + 1]) throw new Error("--out-dir needs a folder");
const out = flag === -1 ? resolve(here, "../../web/icons") : resolve(process.argv[flag + 1]);

// Kept in step with orq.svg (a 512 unit square).
const SIDEBAR = [0x15, 0x1b, 0x23];
const ACCENT = [0x60, 0xa5, 0xfa];
const PRIMARY = [0xdb, 0xea, 0xfe];
const CORNER = 112;
const RING = 150;
const RING_WIDTH = 56;
const DOT = 44;

// The colour at a point in icon units, or null outside the icon. `rounded` gives the rounded square of
// the SVG; without it the background fills the whole square, as maskable and Apple icons need (the
// platform cuts its own shape). `scale` shrinks the artwork about the centre to keep it in the safe zone.
function colourAt(x, y, rounded, scale) {
  if (rounded) {
    const cx = Math.min(Math.max(x, CORNER), 512 - CORNER);
    const cy = Math.min(Math.max(y, CORNER), 512 - CORNER);
    if ((x - cx) ** 2 + (y - cy) ** 2 > CORNER ** 2) return null;
  }
  const d = Math.hypot(x - 256, y - 256) / scale;
  if (d <= DOT) return PRIMARY;
  if (Math.abs(d - RING) <= RING_WIDTH / 2) return ACCENT;
  return SIDEBAR;
}

function render(size, rounded, scale) {
  const samples = 4;
  const pixels = Buffer.alloc(size * size * 4);
  for (let py = 0; py < size; py++) {
    for (let px = 0; px < size; px++) {
      let [r, g, b, a] = [0, 0, 0, 0];
      for (let sy = 0; sy < samples; sy++) {
        for (let sx = 0; sx < samples; sx++) {
          const x = ((px + (sx + 0.5) / samples) / size) * 512;
          const y = ((py + (sy + 0.5) / samples) / size) * 512;
          const colour = colourAt(x, y, rounded, scale);
          if (!colour) continue;
          r += colour[0];
          g += colour[1];
          b += colour[2];
          a += 1;
        }
      }
      const at = (py * size + px) * 4;
      if (a > 0) {
        pixels[at] = Math.round(r / a);
        pixels[at + 1] = Math.round(g / a);
        pixels[at + 2] = Math.round(b / a);
      }
      pixels[at + 3] = Math.round((a / (samples * samples)) * 255);
    }
  }
  return png(size, pixels);
}

const CRC_TABLE = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});

function crc32(bytes) {
  let c = 0xffffffff;
  for (const byte of bytes) c = CRC_TABLE[(c ^ byte) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length);
  const body = Buffer.concat([Buffer.from(type, "ascii"), data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body));
  return Buffer.concat([length, body, crc]);
}

function png(size, rgba) {
  const header = Buffer.alloc(13);
  header.writeUInt32BE(size, 0);
  header.writeUInt32BE(size, 4);
  header[8] = 8; // bit depth
  header[9] = 6; // RGBA
  const rows = Buffer.alloc(size * (size * 4 + 1));
  for (let y = 0; y < size; y++) {
    rows[y * (size * 4 + 1)] = 0; // no filter
    rgba.copy(rows, y * (size * 4 + 1) + 1, y * size * 4, (y + 1) * size * 4);
  }
  const signature = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
  return Buffer.concat([signature, chunk("IHDR", header), chunk("IDAT", deflateSync(rows, { level: 9 })), chunk("IEND", Buffer.alloc(0))]);
}

mkdirSync(out, { recursive: true });
writeFileSync(resolve(out, "icon-192.png"), render(192, true, 1));
writeFileSync(resolve(out, "icon-512.png"), render(512, true, 1));
// The maskable safe zone is the centre circle of 80% of the width; the artwork is shrunk well inside it.
writeFileSync(resolve(out, "icon-maskable-512.png"), render(512, false, 0.72));
writeFileSync(resolve(out, "apple-touch-icon.png"), render(180, false, 0.85));
