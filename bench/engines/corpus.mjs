// The corpus of packages/oxtty/bench/throughput.mjs, shared by the engine comparison: the same six scenarios, the
// same seed, 8 MiB each. Imported by node.mjs; run directly it writes one file per scenario for the Rust bench.
//
//   node bench/engines/corpus.mjs <dir>
import { mkdirSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

export const COLS = 120;
export const ROWS = 40;
export const SCROLLBACK = 10000;
export const SIZE = 8 * 1024 * 1024;
export const CHUNK = 64 * 1024;

function rng(seed) {
  let s = seed >>> 0;
  return () => ((s = (s * 1664525 + 1013904223) >>> 0) / 2 ** 32);
}

const ESC = '\x1b';
export const CASES = {
  // Lines like a build log: mostly plain text, occasionally colored.
  'light-cells': (r) => `${r() < 0.2 ? `${ESC}[3${1 + Math.floor(r() * 6)}m` : ''}[${(r() * 1e6).toFixed(0)}] compiling module_${Math.floor(r() * 9999)}.rs ok${ESC}[0m\r\n`,
  // A different truecolor foreground and background per cell.
  'dense-cells': (r) => {
    let line = '';
    for (let i = 0; i < COLS; i++) {
      const c = () => Math.floor(r() * 256);
      line += `${ESC}[38;2;${c()};${c()};${c()};48;2;${c()};${c()};${c()}m${String.fromCharCode(33 + Math.floor(r() * 90))}`;
    }
    return `${line}${ESC}[0m\r\n`;
  },
  // Lines of Hangul, Han characters and emoji.
  unicode: (r) => {
    const pool = ['가', '나', '한', '글', '漢', '字', '😀', '🚀', 'é', 'a', ' '];
    let line = '';
    for (let i = 0; i < 50; i++) line += pool[Math.floor(r() * pool.length)];
    return `${line}\r\n`;
  },
  // TUI redraw: move to random positions and write short text (no scrolling).
  'cursor-motion': (r) => `${ESC}[${1 + Math.floor(r() * ROWS)};${1 + Math.floor(r() * (COLS - 10))}H${ESC}[3${Math.floor(r() * 8)}m${'#'.repeat(1 + Math.floor(r() * 8))}`,
  // Scroll only the lower region (DECSTBM).
  'scrolling-region': (r) => `${ESC}[20;40r${ESC}[40;1Hline ${Math.floor(r() * 1e6)}\n`,
  // Keep streaming short plain lines to fill the scrollback.
  'scroll-flood': (r) => `${Math.floor(r() * 1e9)}\r\n`,
};

export function corpus(name) {
  const r = rng(0x5eed);
  const make = CASES[name];
  const parts = [];
  let size = 0;
  while (size < SIZE) {
    const s = make(r);
    parts.push(s);
    size += Buffer.byteLength(s);
  }
  const bytes = Buffer.from(parts.join(''));
  const chunks = [];
  for (let i = 0; i < bytes.length; i += CHUNK) chunks.push(bytes.subarray(i, i + CHUNK));
  return { bytes, chunks };
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const dir = process.argv[2];
  if (!dir) throw new Error('usage: node corpus.mjs <dir>');
  mkdirSync(dir, { recursive: true });
  for (const name of Object.keys(CASES)) {
    const { bytes } = corpus(name);
    writeFileSync(join(dir, `${name}.vt`), bytes);
    console.log(`${name.padEnd(17)} ${bytes.length} bytes sha256 ${createHash('sha256').update(bytes).digest('hex').slice(0, 16)}`);
  }
}
