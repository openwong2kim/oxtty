// The corpus of packages/wongtty/bench/throughput.mjs (its corpus.mjs: the same six scenarios, the same seed, 8 MiB
// each), written to one file per scenario so the Rust bench and node.mjs read the same bytes.
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
export const CASES = Object.keys(cases(COLS, ROWS));

const dir = process.argv[2];
if (dir && process.argv[1].endsWith('corpus.mjs')) {
  mkdirSync(dir, { recursive: true });
  for (const name of CASES) {
    const bytes = generate(name, SIZE, COLS, ROWS);
    writeFileSync(join(dir, `${name}.vt`), bytes);
    console.log(`${name.padEnd(17)} ${bytes.length} bytes sha256 ${createHash('sha256').update(bytes).digest('hex').slice(0, 16)}`);
  }
}
