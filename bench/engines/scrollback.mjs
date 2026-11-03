// Operations on a full scrollback in Node: wongtty (wasm), wongtty (N-API) and xterm.js headless swallow one corpus
// scenario (10,000 lines of history at 120x40), then one operation runs in a fresh process: reflowing resizes,
// scrolling the view while reading every visible cell, search over the whole buffer, the whole buffer's text, the
// serialized buffer, clearing the history. Reported: median latency, peak RSS growth during the operation, RSS kept.
//
//   node --expose-gc bench/engines/scrollback.mjs <corpus-dir> [runs]
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const MAIN = fileURLToPath(new URL('../..', import.meta.url)).replace(/\/$/, '');
const require = createRequire(`${MAIN}/bench/engines/package.json`);
const requirePkg = createRequire(`${MAIN}/packages/wongtty/package.json`);
const COLS = 120, ROWS = 40, NARROW = 100, SCROLLBACK = 10000, CHUNK = 64 * 1024;
const OPS = ['fill', 'resize-shrink', 'resize-grow', 'resize-rows', 'scroll-page', 'scroll-jump', 'search-absent', 'search-common', 'text', 'serialize', 'clear'];
const needle = (c) => (c === 'light-cells' ? 'module_44' : c === 'unicode' ? '漢' : 'R7');

async function engine(name) {
  if (name === 'wongtty-wasm' || name === 'wongtty-node') {
    let t;
    if (name === 'wongtty-wasm') {
      // Keep the instance's linear memory in sight: it never shrinks, so its growth is what an operation costs for good.
      const instantiate = WebAssembly.instantiate;
      WebAssembly.instantiate = async (...a) => { const r = await instantiate(...a); globalThis.wongttyMemory = (r.instance ?? r).exports.memory; return r; };
      const { init, Terminal } = await import(`${MAIN}/packages/wongtty/dist/index.js`);
      await init();
      t = new Terminal({ cols: COLS, rows: ROWS, scrollback: SCROLLBACK });
    } else {
      const { Terminal } = require(`${MAIN}/crates/wongtty-node/wongtty.node`);
      t = new Terminal(COLS, ROWS, SCROLLBACK);
    }
    const hist = () => (name === 'wongtty-wasm' ? t.historyLength : t.historyLength);
    const read = () => {
      const off = t.displayOffset;
      let sum = 0;
      for (let y = 0; y < ROWS; y++) {
        const cells = name === 'wongtty-wasm' ? t.rowCellsView(y - off) : t.rowCells(y - off);
        for (let i = 0; i < cells.length; i += 4) sum = (sum + cells[i] + cells[i + 1]) | 0;
      }
      return sum;
    };
    return {
      feed: async (chunks) => { for (const c of chunks) t.write(name === 'wongtty-node' ? Buffer.from(c.buffer, c.byteOffset, c.byteLength) : c); },
      resize: (c, r) => t.resize(c, r),
      history: hist,
      frame: (d) => { t.scrollDisplay(d); t.takeDamage?.(); return read(); },
      frameAt: (o) => { t.scrollDisplay(o - t.displayOffset); return read(); },
      count: (q) => t.searchRows(q, {}, -hist(), ROWS - 1).length,
      text: () => { let s = ''; for (let l = -hist(); l < ROWS; l++) s += t.rowText(l) + '\n'; return s.length; },
      serialize: () => { const s = t.ansiSnapshot(); return s.payload?.length ?? -1; },
      clear: () => t.write(name === 'wongtty-node' ? Buffer.from('\x1b[3J') : '\x1b[3J'),
    };
  }
  const { Terminal } = require('@xterm/headless');
  const { Unicode11Addon } = require('@xterm/addon-unicode11');
  const { SerializeAddon } = requirePkg('@xterm/addon-serialize');
  const t = new Terminal({ cols: COLS, rows: ROWS, scrollback: SCROLLBACK, allowProposedApi: true });
  t.loadAddon(new Unicode11Addon());
  t.unicode.activeVersion = '11';
  const ser = new SerializeAddon();
  t.loadAddon(ser);
  const b = () => t.buffer.active;
  const cell = b().getNullCell();
  const read = () => {
    let sum = 0;
    const top = b().viewportY;
    for (let y = 0; y < ROWS; y++) {
      const line = b().getLine(top + y);
      for (let x = 0; x < COLS; x++) {
        line.getCell(x, cell);
        sum = (sum + cell.getCode() + cell.getFgColor() + cell.getBgColor()) | 0;
      }
    }
    return sum;
  };
  const write = (s) => new Promise((done) => t.write(s, done));
  return {
    feed: (chunks) => new Promise((done) => { for (let i = 0; i < chunks.length; i++) t.write(chunks[i], i === chunks.length - 1 ? done : undefined); }),
    resize: (c, r) => t.resize(c, r),
    history: () => b().length - ROWS,
    frame: (d) => { t.scrollLines(-d); return read(); },
    frameAt: (o) => { t.scrollToLine(b().baseY - o); return read(); },
    // What @xterm/addon-search 0.17 does per logical line: the wrapped rows' text joined, lowercased, then indexOf.
    count: (q) => {
      q = q.toLowerCase();
      let n = 0;
      const len = b().length;
      for (let y = 0; y < len;) {
        let text = b().getLine(y).translateToString(true);
        let k = y + 1;
        while (k < len && b().getLine(k).isWrapped) text += b().getLine(k++).translateToString(true);
        text = text.toLowerCase();
        for (let at = text.indexOf(q); at >= 0; at = text.indexOf(q, at + q.length)) n++;
        y = k;
      }
      return n;
    },
    text: () => { let s = ''; const len = b().length; for (let y = 0; y < len; y++) s += b().getLine(y).translateToString(true) + '\n'; return s.length; },
    serialize: () => ser.serialize({ scrollback: SCROLLBACK }).length,
    clear: () => write('\x1b[3J'),
  };
}

const mem = () => process.memoryUsage();
const maxRss = () => process.resourceUsage().maxRSS * 1024;

if (process.argv[2] === '--one') {
  const [, , , name, caseName, op, dir] = process.argv;
  const bytes = readFileSync(`${dir}/${caseName}.vt`);
  const chunks = [];
  for (let i = 0; i < bytes.length; i += CHUNK) chunks.push(bytes.subarray(i, i + CHUNK));
  globalThis.gc();
  const rss0 = mem().rss;
  const e = await engine(name);
  const t0 = performance.now();
  await e.feed(chunks);
  const fillMs = performance.now() - t0;
  globalThis.gc();
  const rssFill = mem().rss;
  const wasmFill = globalThis.wongttyMemory?.buffer.byteLength ?? 0;
  const peakBefore = maxRss();
  const history = e.history();
  let times = [], frames = 1, result = 0;
  const time = async (f) => { const s = performance.now(); await f(); times.push(performance.now() - s); };
  switch (op) {
    case 'fill': times.push(fillMs); break;
    case 'resize-shrink': case 'resize-grow':
      for (let i = 0; i < +(process.env.RESIZE_CYCLES ?? 3); i++) {
        const s = performance.now(); e.resize(NARROW, ROWS); const a = performance.now() - s;
        const g = performance.now(); e.resize(COLS, ROWS); const bb = performance.now() - g;
        times.push(op === 'resize-shrink' ? a : bb);
      }
      break;
    case 'resize-rows':
      for (let i = 0; i < 3; i++) { await time(() => e.resize(COLS, ROWS - 10)); await time(() => e.resize(COLS, ROWS)); }
      break;
    case 'scroll-page': {
      const pages = Math.floor(history / ROWS);
      frames = 2 * pages;
      await time(() => { for (let i = 0; i < pages; i++) result += e.frame(ROWS); for (let i = 0; i < pages; i++) result += e.frame(-ROWS); });
      break;
    }
    case 'scroll-jump': {
      frames = 500;
      let x = 12345;
      await time(() => { for (let i = 0; i < frames; i++) { x = (x * 1103515245 + 12345) >>> 0; result += e.frameAt(x % history); } });
      break;
    }
    case 'search-absent': for (let i = 0; i < 3; i++) await time(() => { result = e.count('qqzzxx'); }); break;
    case 'search-common': for (let i = 0; i < 3; i++) await time(() => { result = e.count(needle(caseName)); }); break;
    case 'text': for (let i = 0; i < 3; i++) await time(() => { result = e.text(); }); break;
    case 'serialize': for (let i = 0; i < 3; i++) await time(() => { result = e.serialize(); }); break;
    case 'clear': await time(() => e.clear()); break;
  }
  const peak = maxRss() - peakBefore;
  for (let i = 0; i < 3; i++) globalThis.gc();
  const kept = mem().rss - rssFill;
  times.sort((a, b) => a - b);
  process.stdout.write(JSON.stringify({ ms: times[times.length >> 1] / frames, peak, kept, wasmFill, wasmGrow: (globalThis.wongttyMemory?.buffer.byteLength ?? 0) - wasmFill, fillRss: rssFill - rss0, history, result }));
  process.exit(0);
}

const dir = process.argv[2];
const runs = +(process.argv[3] ?? 3);
const ENGINES = (process.env.ENGINES ?? 'wongtty-wasm,wongtty-node,xterm-headless').split(',');
const cases = (process.env.CASES ?? 'light-cells,dense-cells,unicode').split(',');
const ops = (process.env.OPS ?? OPS.join(',')).split(',');
const med = (v) => { const s = [...v].sort((a, b) => a - b); return s[s.length >> 1]; };
const mib = (b) => (b / 1048576).toFixed(2).padStart(7);
console.log('case op engine | median ms (per frame for scroll) [min-max] | peak RSS MiB | kept RSS MiB | fill RSS MiB | wasm mem MiB after fill | wasm grow MiB | history | result');
for (const c of cases) for (const op of ops) {
  const res = Object.fromEntries(ENGINES.map((n) => [n, []]));
  for (let run = 0; run < runs; run++) for (let k = 0; k < ENGINES.length; k++) {
    const name = ENGINES[(run + k) % ENGINES.length];
    const child = spawnSync(process.execPath, ['--expose-gc', fileURLToPath(import.meta.url), '--one', name, c, op, dir], { encoding: 'utf8', maxBuffer: 1 << 24 });
    if (child.status !== 0) { console.error(name, c, op, child.stderr.slice(0, 400)); continue; }
    res[name].push(JSON.parse(child.stdout));
  }
  for (const name of ENGINES) {
    const r = res[name];
    if (!r.length) continue;
    const ms = r.map((x) => x.ms);
    console.log(`${c.padEnd(12)} ${op.padEnd(14)} ${name.padEnd(15)} | ${med(ms).toFixed(3).padStart(9)} [${Math.min(...ms).toFixed(3)}-${Math.max(...ms).toFixed(3)}] | ${mib(med(r.map((x) => x.peak)))} | ${mib(med(r.map((x) => x.kept)))} | ${mib(med(r.map((x) => x.fillRss)))} | ${mib(med(r.map((x) => x.wasmFill)))} | ${mib(med(r.map((x) => x.wasmGrow)))} | ${String(r[0].history).padStart(5)} | ${r[0].result}`);
  }
}
