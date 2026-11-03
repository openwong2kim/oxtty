// wongtty (wasm) against libghostty-vt (wasm, from ghostty-web) and xterm.js headless in Node, on the throughput.mjs
// corpus: MiB/s (median of 5 runs, engines in turn within each run), and from a fresh process per engine and
// scenario the peak RSS growth, the lines of scrollback kept and the visible screen text (compared with wongtty's).
//
//   bash scripts/build-wasm.sh && (cd bench/engines && npm ci)   (repository root)
//   node bench/engines/corpus.mjs <dir>
//   node --expose-gc bench/engines/node.mjs <dir> [out.json]
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { CASES, CHUNK, COLS, ROWS, SCROLLBACK } from './corpus.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const RUNS = 5;
// ghostty-web's scrollbackLimit is in bytes (its xterm.js-style `scrollback: 10000` becomes 10,000 bytes, one page
// of a few hundred lines). 16 MiB keeps at least 10,000 lines of every scenario here.
const GHOSTTY_SCROLLBACK_BYTES = 16 * 1024 * 1024;

async function engine(name) {
  if (name === 'wongtty-wasm') {
    const { init, Terminal } = await import('../../packages/wongtty/dist/index.js');
    await init();
    return {
      make: () => new Terminal({ cols: COLS, rows: ROWS, scrollback: SCROLLBACK }),
      feed: async (t, chunks) => { for (const c of chunks) t.write(c); },
      screen: (t) => Array.from({ length: ROWS }, (_, y) => t.rowText(y)),
      history: (t) => t.historyLength,
      dispose: (t) => t.dispose(),
    };
  }
  if (name === 'ghostty-web') {
    // ghostty-web loads its wasm with fetch in a browser; in Node, instantiate it here with the same import.
    const { Ghostty } = await import('ghostty-web');
    const bytes = readFileSync(join(here, 'node_modules/ghostty-web/ghostty-vt.wasm'));
    const { instance } = await WebAssembly.instantiate(bytes, { env: { log: () => {} } });
    const ghostty = new Ghostty(instance);
    return {
      make: () => ghostty.createTerminal(COLS, ROWS, { scrollbackLimit: GHOSTTY_SCROLLBACK_BYTES }),
      feed: async (t, chunks) => { for (const c of chunks) t.write(c); },
      // Read in a fresh process only: rows a later terminal does not touch keep an earlier terminal's cells in
      // ghostty-web's render state.
      screen: (t) => Array.from({ length: ROWS }, (_, y) => {
        let text = '';
        t.getLine(y).forEach((cell, x) => {
          if (cell.width === 0) return; // the spacer after a wide character
          if (cell.grapheme_len > 0) text += t.getGraphemeString(y, x);
          else text += cell.codepoint ? String.fromCodePoint(cell.codepoint) : ' ';
        });
        return text.trimEnd();
      }),
      history: (t) => t.getScrollbackLength(),
      dispose: (t) => t.free(),
    };
  }
  const { Terminal } = require('@xterm/headless');
  const { Unicode11Addon } = require('@xterm/addon-unicode11');
  return {
    make: () => {
      const t = new Terminal({ cols: COLS, rows: ROWS, scrollback: SCROLLBACK, allowProposedApi: true });
      t.loadAddon(new Unicode11Addon());
      t.unicode.activeVersion = '11';
      return t;
    },
    feed: (t, chunks) => new Promise((done) => {
      for (let i = 0; i < chunks.length; i++) t.write(chunks[i], i === chunks.length - 1 ? done : undefined);
    }),
    // Typed trailing spaces trimmed too, as the others do.
    screen: (t) => Array.from({ length: ROWS }, (_, y) => (t.buffer.active.getLine(t.buffer.active.baseY + y)?.translateToString(true) ?? '').trimEnd()),
    history: (t) => t.buffer.normal.length - ROWS,
    dispose: (t) => t.dispose(),
  };
}

// ENGINES and CASES (comma-separated) pick the engines and scenarios; by default all of them.
const ENGINES = (process.env.ENGINES ?? 'wongtty-wasm,ghostty-web,xterm-headless').split(',');
const RUN_CASES = process.env.CASES ? process.env.CASES.split(',') : CASES;

function load(dir, name) {
  const bytes = readFileSync(join(dir, `${name}.vt`));
  const chunks = [];
  for (let i = 0; i < bytes.length; i += CHUNK) chunks.push(bytes.subarray(i, i + CHUNK));
  return { bytes, chunks };
}

// Child process: one engine, one scenario.
if (process.argv[2] === '--memory') {
  const [, , , engineName, caseName, dir] = process.argv;
  const e = await engine(engineName);
  const { chunks } = load(dir, caseName);
  globalThis.gc();
  const before = process.memoryUsage().rss;
  const peakBefore = process.resourceUsage().maxRSS * 1024;
  const t = e.make();
  await e.feed(t, chunks);
  const peak = process.resourceUsage().maxRSS * 1024 - peakBefore;
  for (let i = 0; i < 4; i++) globalThis.gc();
  const retained = process.memoryUsage().rss - before;
  process.stdout.write(JSON.stringify({ peak, retained, history: e.history(t), screen: e.screen(t) }));
  process.exit(0);
}

const dir = process.argv[2];
if (!dir) throw new Error('usage: node --expose-gc node.mjs <corpus-dir> [out.json]');
const engines = {};
for (const name of ENGINES) engines[name] = await engine(name);

const results = [];
for (const caseName of RUN_CASES) {
  const { bytes, chunks } = load(dir, caseName);
  const mib = bytes.length / 1048576;
  const times = Object.fromEntries(ENGINES.map((n) => [n, []]));
  const crashed = {};
  // Run 0 is warm-up. Each run starts with a different engine, so drift from other load spreads over all.
  for (let run = 0; run <= RUNS; run++) {
    for (let k = 0; k < ENGINES.length; k++) {
      const name = ENGINES[(run + k) % ENGINES.length];
      if (crashed[name]) continue;
      const e = engines[name];
      const t = e.make();
      const start = performance.now();
      try {
        await e.feed(t, chunks);
      } catch (err) {
        // ghostty-web 0.4.0 traps on some grapheme scenarios; its instance is not reused after that.
        crashed[name] = String(err.message ?? err).slice(0, 80);
        engines[name] = await engine(name);
        continue;
      }
      const ms = performance.now() - start;
      if (run > 0) times[name].push(mib / (ms / 1000));
      e.dispose(t);
    }
  }
  const screens = {};
  for (const name of ENGINES) {
    if (crashed[name]) {
      results.push({ case: caseName, engine: name, crashed: crashed[name] });
      console.log(`${caseName.padEnd(17)} ${name.padEnd(15)} crashed: ${crashed[name]}`);
      continue;
    }
    const s = times[name].sort((a, b) => a - b);
    const child = spawnSync(process.execPath, ['--expose-gc', fileURLToPath(import.meta.url), '--memory', name, caseName, dir], { encoding: 'utf8', maxBuffer: 1 << 24 });
    const memory = child.status === 0 ? JSON.parse(child.stdout) : { screen: [] };
    screens[name] = memory.screen;
    const first = screens[ENGINES[0]] ?? [];
    const differ = screens[name].flatMap((row, i) => (row === first[i] ? [] : [i]));
    const screen = differ.length === 0 ? 'same' : `${differ.length} rows differ; row ${differ[0]}: ${JSON.stringify(screens[name][differ[0]].slice(0, 60))} vs wongtty ${JSON.stringify((first[differ[0]] ?? '').slice(0, 60))}`;
    const row = { case: caseName, engine: name, mibPerSec: +s[s.length >> 1].toFixed(1), min: +s[0].toFixed(1), max: +s.at(-1).toFixed(1), peakBytes: memory.peak ?? null, retainedBytes: memory.retained ?? null, history: memory.history ?? null, screen };
    results.push(row);
    const m = (b) => (b === null ? '?' : (b / 1048576).toFixed(1).padStart(6));
    console.log(`${caseName.padEnd(17)} ${name.padEnd(15)} ${row.mibPerSec.toFixed(1).padStart(7)} MiB/s (${row.min.toFixed(1).padStart(6)}–${row.max.toFixed(1).padStart(6)})  rss peak +${m(row.peakBytes)} MiB  retained +${m(row.retainedBytes)} MiB  history ${String(row.history).padStart(5)}  screen ${screen}`);
  }
}

const out = process.argv[3];
if (out) writeFileSync(out, JSON.stringify({ at: new Date().toISOString(), node: process.version, results }, null, 2));
