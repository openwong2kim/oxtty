// Throughput and memory baseline: feeds the same output to oxtty (wasm), oxtty (native N-API) and @xterm/headless and compares.
//
//   bash scripts/build-wasm.sh && bash scripts/build-node.sh   (repository root)
//   node --expose-gc packages/oxtty/bench/throughput.mjs [out.json]
//
// Output is generated deterministically from vtebench-style scenarios (corpus.mjs: fixed seed, 8 MiB each).
// Terminal 120×40, 10,000 lines of scrollback, written 64 KiB at a time. Throughput is the median of 5 runs (MiB/s),
// memory is the RSS growth after GC in a fresh process per scenario and engine.
//
// Resize: with 10,000 and 100,000 lines of scrollback filled, 20 column changes (100 and 120 in turn), in a fresh
// process per engine. Reports the time per resize and the memory growth: WebAssembly memory for oxtty-wasm (it never
// shrinks, so this is the peak), RSS after GC otherwise.
import { spawnSync } from 'node:child_process';
import { existsSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { arch, cpus, platform } from 'node:os';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { cases, generate } from './corpus.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const NATIVE = join(here, '../../../crates/oxtty-node/oxtty.node');
const COLS = 120;
const ROWS = 40;
const SCROLLBACK = 10000;
const SIZE = 8 * 1024 * 1024;
const CHUNK = 64 * 1024;
const RUNS = 5;
const RESIZES = 20;
const RESIZE_SCROLLBACKS = [10000, 100000];

// oxtty's wrapper keeps the WebAssembly instance to itself; note its memory as it is instantiated.
let wasmMemory = null;
const instantiate = WebAssembly.instantiate;
WebAssembly.instantiate = async (...args) => {
  const result = await instantiate(...args);
  wasmMemory = (result.instance ?? result).exports.memory;
  return result;
};

const CASES = cases(COLS, ROWS);

function corpus(name) {
  const bytes = generate(name, SIZE, COLS, ROWS);
  const chunks = [];
  for (let i = 0; i < bytes.length; i += CHUNK) chunks.push(bytes.subarray(i, i + CHUNK));
  return { bytes: bytes.length, chunks };
}

async function engine(name) {
  if (name === 'oxtty-wasm') {
    const { init, Terminal } = await import('../dist/index.js');
    await init();
    return {
      make: (scrollback = SCROLLBACK) => new Terminal({ cols: COLS, rows: ROWS, scrollback }),
      feed: async (t, chunks) => { for (const c of chunks) t.write(c); },
      last: (t) => t.rowText(ROWS - 1),
      resize: (t, cols, rows) => t.resize(cols, rows),
      memory: () => wasmMemory.buffer.byteLength,
      dispose: (t) => t.dispose(),
    };
  }
  if (name === 'oxtty-native') {
    const { Terminal } = require(NATIVE);
    return {
      make: (scrollback = SCROLLBACK) => new Terminal(COLS, ROWS, scrollback),
      feed: async (t, chunks) => { for (const c of chunks) t.write(c); },
      last: (t) => t.rowText(ROWS - 1),
      resize: (t, cols, rows) => t.resize(cols, rows),
      dispose: () => {},
    };
  }
  const { Terminal } = require('@xterm/headless');
  const { Unicode11Addon } = require('@xterm/addon-unicode11');
  return {
    make: (scrollback = SCROLLBACK) => {
      const t = new Terminal({ cols: COLS, rows: ROWS, scrollback, allowProposedApi: true });
      t.loadAddon(new Unicode11Addon());
      t.unicode.activeVersion = '11';
      return t;
    },
    feed: (t, chunks) => new Promise((done) => {
      for (let i = 0; i < chunks.length; i++) t.write(chunks[i], i === chunks.length - 1 ? done : undefined);
    }),
    last: (t) => t.buffer.active.getLine(t.buffer.active.baseY + ROWS - 1)?.translateToString(true) ?? '',
    resize: (t, cols, rows) => t.resize(cols, rows),
    dispose: (t) => t.dispose(),
  };
}

const ENGINES = ['oxtty-wasm', ...(existsSync(NATIVE) ? ['oxtty-native'] : []), 'xterm-headless'];

// Child process: runs one engine on one scenario once and reports the RSS growth after GC.
if (process.argv[2] === '--memory') {
  const [, , , engineName, caseName] = process.argv;
  const e = await engine(engineName);
  const { chunks } = corpus(caseName);
  globalThis.gc();
  const before = process.memoryUsage().rss;
  const t = e.make();
  await e.feed(t, chunks);
  for (let i = 0; i < 4; i++) globalThis.gc();
  process.stdout.write(JSON.stringify({ rss: process.memoryUsage().rss - before, last: e.last(t) }));
  process.exit(0);
}

// Child process: fills one engine's scrollback, then resizes it and reports the time and the memory growth.
if (process.argv[2] === '--resize') {
  const [, , , engineName, scrollback] = process.argv;
  const e = await engine(engineName);
  const { chunks } = corpus('light-cells');
  const t = e.make(Number(scrollback));
  await e.feed(t, chunks);
  for (let i = 0; i < 4; i++) globalThis.gc();
  const memory = () => e.memory?.() ?? process.memoryUsage().rss;
  const before = memory();
  const start = performance.now();
  for (let i = 0; i < RESIZES; i++) e.resize(t, i % 2 === 0 ? 100 : COLS, ROWS);
  const ms = (performance.now() - start) / RESIZES;
  for (let i = 0; i < 4; i++) globalThis.gc();
  process.stdout.write(JSON.stringify({ ms, grew: memory() - before }));
  process.exit(0);
}

const results = [];
for (const caseName of Object.keys(CASES)) {
  const { bytes, chunks } = corpus(caseName);
  const lasts = {};
  for (const engineName of ENGINES) {
    const e = await engine(engineName);
    const times = [];
    for (let run = 0; run <= RUNS; run++) {
      const t = e.make();
      const start = performance.now();
      await e.feed(t, chunks);
      const ms = performance.now() - start;
      if (run > 0) times.push(ms); // the first run is warm-up
      lasts[engineName] = e.last(t);
      e.dispose(t);
    }
    times.sort((a, b) => a - b);
    const ms = times[Math.floor(times.length / 2)];
    const child = spawnSync(process.execPath, ['--expose-gc', fileURLToPath(import.meta.url), '--memory', engineName, caseName], { encoding: 'utf8' });
    const memory = child.status === 0 ? JSON.parse(child.stdout).rss : null;
    results.push({ case: caseName, engine: engineName, ms: +ms.toFixed(1), mibPerSec: +((bytes / 1048576) / (ms / 1000)).toFixed(1), rssMiB: memory === null ? null : +(memory / 1048576).toFixed(1) });
    console.log(`${caseName.padEnd(17)} ${engineName.padEnd(15)} ${ms.toFixed(0).padStart(6)} ms ${String(results.at(-1).mibPerSec).padStart(7)} MiB/s  rss +${results.at(-1).rssMiB} MiB`);
  }
  // Given the same output, the last line must match (fairness check).
  const distinct = new Set(Object.values(lasts));
  if (distinct.size > 1) console.log(`  ! ${caseName}: last row differs between engines`, lasts);
}

const resizes = [];
for (const scrollback of RESIZE_SCROLLBACKS) {
  for (const engineName of ENGINES) {
    const child = spawnSync(process.execPath, ['--expose-gc', fileURLToPath(import.meta.url), '--resize', engineName, String(scrollback)], { encoding: 'utf8' });
    if (child.status !== 0) {
      console.log(`resize ${scrollback} ${engineName}: failed`, child.stderr);
      continue;
    }
    const { ms, grew } = JSON.parse(child.stdout);
    resizes.push({ scrollback, engine: engineName, msPerResize: +ms.toFixed(1), grewMiB: +(grew / 1048576).toFixed(1) });
    console.log(`resize ${String(scrollback).padStart(6)}    ${engineName.padEnd(15)} ${ms.toFixed(1).padStart(6)} ms/resize  memory +${resizes.at(-1).grewMiB} MiB`);
  }
}

const out = process.argv[2];
if (out) {
  writeFileSync(out, JSON.stringify({ at: new Date().toISOString(), node: process.version, platform: platform(), arch: arch(), cpu: cpus()[0]?.model, cols: COLS, rows: ROWS, scrollback: SCROLLBACK, bytesPerCase: SIZE, chunk: CHUNK, runs: RUNS, results, resizes }, null, 2));
}
