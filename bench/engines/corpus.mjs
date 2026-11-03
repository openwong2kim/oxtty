// The corpus of packages/wongtty/bench/throughput.mjs (its corpus.mjs: the same six scenarios, the same seed, 8 MiB
// each), written to one file per scenario so the Rust bench and node.mjs read the same bytes, plus three 4 MiB
// scenarios of the phase-1 coverage pass (agent-emoji, long-lines, graphemes; their md5s are fixed, see EXTRA_MD5).
//
//   node bench/engines/corpus.mjs <dir>
import { mkdirSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import { join } from 'node:path';
import { cases, generate } from '../../packages/wongtty/bench/corpus.mjs';

export const COLS = 120;
export const ROWS = 40;
export const SCROLLBACK = 10000;
export const SIZE = 8 * 1024 * 1024;
export const CHUNK = 64 * 1024;
export const EXTRA_SIZE = 4 * 1024 * 1024;
export const EXTRA_MD5 = {
  'agent-emoji': 'b28290f6f04766afc92bf5882b31d2c0',
  'long-lines': 'c01894e62bbed783d86ccb53546b7ed6',
  graphemes: '9605e770071b09250c29b2a2d62752c1',
};
export const CASES = [...Object.keys(cases(COLS, ROWS)), ...Object.keys(EXTRA_MD5)];

// The 32-bit LCG of the phase-1 generators, kept as written there (its float rounding is part of the bytes).
const lcg = (seed) => {
  let s = seed;
  return () => (s = (s * 1103515245 + 12345) >>> 0) / 4294967296;
};

/** The three extra scenarios, by name. long-lines and graphemes share one generator, in that order. */
export function extra() {
  const SIZE = EXTRA_SIZE;
  // agent-emoji: log lines of 40-80 lowercase letters with one emoji cluster at each end (VS16, skin tone, ZWJ).
  let r = lcg(7);
  const marks = ['✔️', '👍🏽', '⚠️', '🧑‍💻', 'é'];
  let out = [];
  let n = 0;
  while (n < SIZE) {
    let l = '';
    const len = 40 + Math.floor(r() * 40);
    while (l.length < len) l += String.fromCharCode(97 + Math.floor(r() * 26)) + (r() < 0.15 ? ' ' : '');
    l = marks[Math.floor(r() * marks.length)] + ' ' + l + ' ' + marks[Math.floor(r() * marks.length)] + '\r\n';
    out.push(l);
    n += Buffer.byteLength(l);
  }
  const agent = Buffer.from(out.join('')).subarray(0, SIZE);
  // long-lines: lines of 2,000-8,000 bytes, no LF inside, a little SGR, like a minified bundle or one-line JSON log.
  r = lcg(12345);
  const words = ['"id"', '"name"', '"value"', 'true', 'false', 'null', '"items"', '[', ']', '{', '}', ':', ',', '12345', '"https://example.com/a/b"'];
  out = [];
  n = 0;
  while (n < SIZE) {
    const len = 2000 + Math.floor(r() * 6000);
    let line = '';
    while (line.length < len) {
      const w = words[Math.floor(r() * words.length)];
      line += r() < 0.05 ? `\x1b[3${1 + Math.floor(r() * 6)}m${w}\x1b[0m` : w;
    }
    line += '\r\n';
    out.push(line);
    n += line.length;
  }
  const long = Buffer.from(out.join('').slice(0, SIZE));
  // graphemes: ZWJ families, skin tones, flags, VS16, combining accents (Latin, Hangul jamo), short lines.
  const g = ['👨‍👩‍👧‍👦', '👍🏽', '🇰🇷', '🇯🇵', '❤️', '☺️', 'é', 'ñ', 'á̂', '각', 'ok', ' ', 'x', '🏳️‍🌈', '1️⃣'];
  out = [];
  n = 0;
  while (n < SIZE) {
    let line = '';
    const cnt = 20 + Math.floor(r() * 20);
    for (let i = 0; i < cnt; i++) line += g[Math.floor(r() * g.length)];
    line += '\r\n';
    out.push(line);
    n += Buffer.byteLength(line);
  }
  const graphemes = Buffer.from(out.join('')).subarray(0, SIZE);
  return { 'agent-emoji': agent, 'long-lines': long, graphemes };
}

const dir = process.argv[2];
if (dir && process.argv[1].endsWith('corpus.mjs')) {
  mkdirSync(dir, { recursive: true });
  const more = extra();
  for (const name of CASES) {
    const bytes = more[name] ?? generate(name, SIZE, COLS, ROWS);
    writeFileSync(join(dir, `${name}.vt`), bytes);
    const md5 = createHash('md5').update(bytes).digest('hex');
    if (EXTRA_MD5[name] && md5 !== EXTRA_MD5[name]) throw new Error(`${name}: md5 ${md5}, expected ${EXTRA_MD5[name]}`);
    console.log(`${name.padEnd(17)} ${bytes.length} bytes md5 ${md5}`);
  }
}
