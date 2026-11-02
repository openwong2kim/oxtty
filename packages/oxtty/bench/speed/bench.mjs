// Speed of many terminals printing at once in a browser, as wmux shows them: oxtty/xterm against xterm.js with
// @xterm/addon-webgl and ghostty-web.
//
//   bash scripts/build-wasm.sh && bash scripts/build-web.sh   (repository root)
//   (cd packages/oxtty && npm ci && npm run bench:speed -- [out.json])
//
// Environment: PANES (comma list, default 17,20), DURATION in seconds per measurement (default 4), RUNS (default 3), KINDS
// (default oxtty,xterm,ghostty), WORKLOADS (default light-cells,dense-cells,unicode,cursor-motion,scrolling-region),
// MODES (default agent,flat).
//
// Each engine gets its own headless system Chrome on the real GPU (Metal through ANGLE, as bench/memory). For every
// pane count, workload and mode, the page builds that many terminals of 120×40 (10,000 lines of scrollback, all in
// view) on a freshly loaded page and keeps them all printing for DURATION seconds (page.mjs: 'agent' is 2 KiB per terminal every 50 ms, 'flat' is as
// fast as the write callbacks allow). The engines take turns measurement by measurement, so a load from elsewhere on
// the machine hits them alike; each line reports the median of RUNS and the spread (min–max).
//
// Columns: MiB/s of output parsed (write callbacks fired) over all terminals; write→frame latency p50 and p95 (ms);
// frames per second of the page; long tasks (count and ms per second); main-thread busy % (CDP Performance metric
// TaskDuration over the wall time).
import { createServer } from 'node:http';
import { readFile, writeFile } from 'node:fs/promises';
import { dirname, extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';
import { launch } from '../memory/common.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..', '..');
const env = (name, fallback) => (process.env[name] ?? fallback).split(',');
const PANES = env('PANES', '17,20').map(Number);
const SECONDS = Number(process.env.DURATION ?? 4);
const RUNS = Number(process.env.RUNS ?? 3);
const KINDS = env('KINDS', 'oxtty,xterm,ghostty');
const WORKLOADS = env('WORKLOADS', 'light-cells,dense-cells,unicode,cursor-motion,scrolling-region');
const MODES = env('MODES', 'agent,flat');
const COLS = 120;
const ROWS = 40;

const TYPES = { '.js': 'text/javascript', '.mjs': 'text/javascript', '.wasm': 'application/wasm', '.css': 'text/css' };
const IMPORTS = {
  'oxtty/xterm': '/dist/xterm.js',
  '@xterm/xterm': '/node_modules/@xterm/xterm/lib/xterm.mjs',
  '@xterm/addon-webgl': '/node_modules/@xterm/addon-webgl/lib/addon-webgl.mjs',
  '@xterm/addon-unicode11': '/node_modules/@xterm/addon-unicode11/lib/addon-unicode11.mjs',
  'ghostty-web': '/node_modules/ghostty-web/dist/ghostty-web.js',
};
const html = `<!doctype html><meta charset="utf-8"><link rel="icon" href="data:,">
<link rel="stylesheet" href="/node_modules/@xterm/xterm/css/xterm.css">
<script type="importmap">${JSON.stringify({ imports: IMPORTS })}</script>
<body style="margin:0;line-height:0"><script type="module" src="/bench/speed/page.mjs"></script>`;

const server = createServer(async (req, res) => {
  const path = normalize(new URL(req.url, 'http://x').pathname);
  if (path === '/') return res.writeHead(200, { 'content-type': 'text/html' }).end(html);
  if (path.includes('..')) return res.writeHead(404).end();
  try {
    const body = await readFile(join(root, path));
    res.writeHead(200, { 'content-type': TYPES[extname(path)] ?? 'application/octet-stream' }).end(body);
  } catch {
    res.writeHead(404).end();
  }
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));

/** A browser and page per engine, kept for the whole bench. */
async function open(kind) {
  const browser = await launch();
  const page = await browser.newPage({ viewport: { width: 4500, height: 2600 }, deviceScaleFactor: 1 });
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  await page.waitForFunction(() => window.ready, null, { timeout: 60000 });
  const cdp = await page.context().newCDPSession(page);
  await cdp.send('Performance.enable');
  return { kind, browser, page, cdp, errors };
}

const taskSeconds = async (cdp) => (await cdp.send('Performance.getMetrics')).metrics.find((m) => m.name === 'TaskDuration').value;

async function measure({ kind, page, cdp, errors }, panes, workload, mode) {
  // A fresh page each time: ghostty-web 0.4.0 can fail to open a terminal after others were disposed.
  await page.reload();
  await page.waitForFunction(() => window.ready, null, { timeout: 60000 });
  await cdp.send('Performance.enable');
  await page.evaluate(([k, n, c, r]) => window.setup(k, n, c, r), [kind, panes, COLS, ROWS]);
  const busy0 = await taskSeconds(cdp);
  const r = await page.evaluate(([w, m, s]) => window.run(w, m, s), [workload, mode, SECONDS]);
  const busy = (await taskSeconds(cdp)) - busy0;
  await page.evaluate(() => window.settle());
  const s = r.ms / 1000;
  return {
    mibPerSec: r.accepted / 1048576 / s,
    p50: r.latency.p50,
    p95: r.latency.p95,
    fps: r.frames / s,
    longTasks: r.longTasks.count / s,
    longTaskMs: r.longTasks.ms / s,
    // The evaluate round trips around the run count too, so this can pass 100 by a little.
    busy: (busy / s) * 100,
    errors: errors.length,
  };
}

const engines = [];
for (const kind of KINDS) {
  const e = await open(kind);
  if (kind === 'ghostty') {
    // ghostty-web is optional: drop it when it does not run here.
    try {
      await e.page.evaluate(([n, c, r]) => window.setup('ghostty', n, c, r), [1, COLS, ROWS]);
      if (e.errors.length) throw new Error(e.errors.join('; '));
    } catch (error) {
      console.log(`ghostty-web dropped: ${error.message}`);
      await e.browser.close();
      continue;
    }
  }
  engines.push(e);
}

const results = [];
try {
  for (const panes of PANES) {
    for (const workload of WORKLOADS) {
      for (const mode of MODES) {
        const runs = Object.fromEntries(engines.map((e) => [e.kind, []]));
        for (let run = 0; run < RUNS; run++) {
          for (const e of engines) runs[e.kind].push(await measure(e, panes, workload, mode));
        }
        for (const e of engines) {
          const rs = runs[e.kind];
          const stat = (key) => {
            const v = rs.map((x) => x[key]).filter((x) => x !== null).sort((a, b) => a - b);
            return v.length ? { median: v[Math.floor(v.length / 2)], min: v[0], max: v.at(-1) } : null;
          };
          const row = { panes, workload, mode, kind: e.kind };
          for (const key of ['mibPerSec', 'p50', 'p95', 'fps', 'longTasks', 'longTaskMs', 'busy', 'errors']) row[key] = stat(key);
          results.push(row);
          const f = (st, d = 1) => (st ? `${st.median.toFixed(d)} (${st.min.toFixed(d)}–${st.max.toFixed(d)})` : '-');
          console.log(
            `${String(panes).padStart(2)} ${workload.padEnd(16)} ${mode.padEnd(5)} ${e.kind.padEnd(7)} ` +
              `${f(row.mibPerSec)} MiB/s | p50 ${f(row.p50, 0)} p95 ${f(row.p95, 0)} ms | ${f(row.fps, 0)} fps | ` +
              `long ${f(row.longTasks)}/s ${f(row.longTaskMs, 0)} ms/s | busy ${f(row.busy, 0)}%` +
              (row.errors.max ? ` | ${row.errors.max} page errors` : ''),
          );
        }
      }
    }
  }
} finally {
  for (const e of engines) {
    for (const message of e.errors.slice(0, 5)) console.log(`${e.kind} page error: ${message}`);
    await e.browser.close();
  }
  server.close();
}

if (process.argv[2]) await writeFile(process.argv[2], JSON.stringify({ at: new Date().toISOString(), seconds: SECONDS, runs: RUNS, cols: COLS, rows: ROWS, results }, null, 2));
