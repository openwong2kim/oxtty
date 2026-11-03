// Startup, scrollback scrolling, resize, keystroke echo latency and CPU (renderer + GPU process) under agent load,
// for wongtty/xterm, xterm.js + WebGL and ghostty-web, in headless system Chrome on the real GPU.
//
//   SCEN=startup,scroll,resize,echo,cpu RUNS=5 node bench/speed/extra.mjs [out.json]
//
// Env: KINDS (wongtty, xterm, ghostty; wongtty@<dir> loads packages/wongtty/<dir> instead of dist), SCEN (startup,
// cold, scroll, resize, echo, cpu), RUNS (kinds interleaved per run), PANES (pane counts for scroll/resize/echo/cpu),
// NS (pane counts for startup and cold, default 1,20), LOADS (corpora for cpu).
import { createServer } from 'node:http';
import { readFile, writeFile } from 'node:fs/promises';
import { dirname, extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';
import { footprint, launch } from '../memory/common.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..', '..');
const env = (name, fallback) => (process.env[name] ?? fallback).split(',');
const KINDS = env('KINDS', 'wongtty,xterm,ghostty');
const SCEN = env('SCEN', 'startup,scroll,resize,echo,cpu');
const RUNS = Number(process.env.RUNS ?? 5);
const PANES = env('PANES', '17').map(Number);
const NS = env('NS', '1,20').map(Number);
const COLS = 120;
const ROWS = 40;

const TYPES = { '.js': 'text/javascript', '.mjs': 'text/javascript', '.wasm': 'application/wasm', '.css': 'text/css' };
const IMPORTS = {
  'wongtty/xterm': '/dist/xterm.js',
  '@xterm/xterm': '/node_modules/@xterm/xterm/lib/xterm.mjs',
  '@xterm/addon-webgl': '/node_modules/@xterm/addon-webgl/lib/addon-webgl.mjs',
  '@xterm/addon-unicode11': '/node_modules/@xterm/addon-unicode11/lib/addon-unicode11.mjs',
  'ghostty-web': '/node_modules/ghostty-web/dist/ghostty-web.js',
};
const page = (dist) => `<!doctype html><meta charset="utf-8"><link rel="icon" href="data:,">
<link rel="stylesheet" href="/node_modules/@xterm/xterm/css/xterm.css">
<script type="importmap">${JSON.stringify({ imports: { ...IMPORTS, 'wongtty/xterm': `/${dist}/xterm.js` } })}</script>
<body style="margin:0;line-height:0"><script type="module" src="/bench/speed/extra-page.mjs"></script>`;
const server = createServer(async (req, res) => {
  const path = normalize(new URL(req.url, 'http://x').pathname);
  if (path === '/') return res.writeHead(200, { 'content-type': 'text/html' }).end(page(new URL(req.url, 'http://x').searchParams.get('dist') ?? 'dist'));
  if (path.includes('..')) return res.writeHead(404).end();
  try {
    res.writeHead(200, { 'content-type': TYPES[extname(path)] ?? 'application/octet-stream' }).end(await readFile(join(root, path)));
  } catch {
    res.writeHead(404).end();
  }
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
// A kind 'wongtty@<dir>' is wongtty loaded from packages/wongtty/<dir> instead of dist (a prototype build).
const url = (kind) => {
  const [k, dist = 'dist', flag] = kind.split('@');
  return `http://127.0.0.1:${server.address().port}/?kind=${k}&dist=${dist}${flag ? `&${flag}=1` : ''}`;
};

const browsers = {};
for (const kind of KINDS) {
  const browser = await launch();
  const bcdp = await browser.newBrowserCDPSession();
  const page = await browser.newPage({ viewport: { width: 4500, height: 2600 }, deviceScaleFactor: 1 });
  const errors = [];
  page.on('pageerror', (e) => errors.push(e.message));
  page.on('console', (m) => { if (/WebGL/.test(m.text())) errors.push('console: ' + m.text().slice(0, 120)); });
  browsers[kind] = { browser, bcdp, page, errors };
}
async function fresh(kind) {
  const b = browsers[kind];
  await b.page.goto(url(kind));
  await b.page.waitForFunction(() => window.ready, null, { timeout: 60000 });
  return b;
}
async function cpu(bcdp, mem = false) {
  const out = { renderer: 0, gpu: 0, rendererMiB: 0, gpuMiB: 0 };
  for (const p of (await bcdp.send('SystemInfo.getProcessInfo')).processInfo) {
    if (p.type === 'renderer') {
      out.renderer += p.cpuTime;
      if (mem) out.rendererMiB += footprint(p.id) / 1048576;
    }
    if (p.type === 'GPU') {
      out.gpu += p.cpuTime;
      if (mem) out.gpuMiB += footprint(p.id) / 1048576;
    }
  }
  return out;
}

const med = (v) => {
  const s = v.filter((x) => x !== null && x !== undefined).sort((a, b) => a - b);
  return s.length ? { median: s[s.length >> 1], min: s[0], max: s.at(-1) } : null;
};
const fmt = (st, d = 1) => (st ? `${st.median.toFixed(d)} (${st.min.toFixed(d)}–${st.max.toFixed(d)})` : '-');
const results = [];
function report(name, rowsByKind, keys) {
  for (const kind of Object.keys(rowsByKind)) {
    const rs = rowsByKind[kind];
    const row = { scen: name, kind };
    for (const k of keys) row[k] = med(rs.map((r) => k.split('.').reduce((o, p) => o?.[p], r)));
    results.push(row);
    console.log(`${name.padEnd(28)} ${kind.padEnd(8)} ` + keys.map((k) => `${k} ${fmt(row[k])}`).join(' | '));
  }
}
async function scenario(name, keys, fn) {
  const rows = Object.fromEntries(KINDS.map((k) => [k, []]));
  for (let run = 0; run < RUNS; run++) {
    for (let i = 0; i < KINDS.length; i++) {
      const kind = KINDS[(run + i) % KINDS.length];
      try {
        rows[kind].push(await fn(kind));
      } catch (e) {
        console.log(`${name} ${kind} failed: ${e.message.split('\n')[0]}`);
      }
    }
  }
  report(name, rows, keys);
}

try {
  if (SCEN.includes('startup')) {
    for (const n of NS) {
      await scenario(`startup n=${n}`, ['initMs', 'buildMs', 'firstFrameMs', 'sinceNavMs'], async (kind) => {
        const { page } = await fresh(kind);
        const initMs = await page.evaluate(() => window.initMs);
        const r = await page.evaluate(([n, c, r]) => window.setup(n, c, r), [n, COLS, ROWS]);
        return { initMs, ...r };
      });
    }
  }
  if (SCEN.includes('cold')) {
    // A new browser context per run: empty HTTP and compiled-code caches, as a first launch.
    for (const n of NS) {
      await scenario(`cold startup n=${n}`, ['initMs', 'buildMs', 'parsedMs', 'firstFrameMs', 'sinceNavMs'], async (kind) => {
        const context = await browsers[kind].browser.newContext({ viewport: { width: 4500, height: 2600 }, deviceScaleFactor: 1 });
        try {
          const page = await context.newPage();
          await page.goto(url(kind));
          await page.waitForFunction(() => window.ready, null, { timeout: 60000 });
          const initMs = await page.evaluate(() => window.initMs);
          const r = await page.evaluate(([n, c, r]) => window.setup(n, c, r), [n, COLS, ROWS]);
          return { initMs, ...r };
        } finally {
          await context.close();
        }
      });
    }
  }
  if (SCEN.includes('scroll')) {
    for (const n of PANES.includes(1) ? PANES : [1, ...PANES]) {
      const cases = process.env.SCROLLS ? [[1, 40, 'lines'], [n, 40, 'lines'], [n, 0, 'jump']] : [[1, 3, 'lines'], [1, 0, 'jump'], ...(n > 1 ? [[n, 3, 'lines']] : [])];
      for (const [which, step, mode] of cases) {
        await scenario(`scroll n=${n} scrolled=${which} ${mode} ${step}`, ['work.p50', 'work.p95', 'gap.p50', 'gap.p95', 'fillMs'], async (kind) => {
          const { page } = await fresh(kind);
          await page.evaluate(([n, c, r]) => window.setup(n, c, r), [n, COLS, ROWS]);
          const f = await page.evaluate(() => window.fill('light-cells', 1200000));
          const r = await page.evaluate(([w, s, m]) => window.scroll(w, s, 120, m), [which, step, mode]);
          return { ...r, fillMs: f.ms };
        });
      }
    }
  }
  if (SCEN.includes('resize')) {
    for (const n of [1, ...PANES]) {
      await scenario(`resize n=${n} 10k lines`, ['call.p50', 'call.p95', 'toFrame.p50', 'toFrame.p95'], async (kind) => {
        const { page } = await fresh(kind);
        await page.evaluate(([n, c, r]) => window.setup(n, c, r), [n, COLS, ROWS]);
        await page.evaluate(() => window.fill('light-cells', 1200000));
        return page.evaluate(([r]) => window.resize(100, 120, r, 10), [ROWS]);
      });
    }
  }
  if (SCEN.includes('echo')) {
    for (const n of [1, ...PANES]) {
      for (const load of n > 1 ? ['idle', 'light-cells', 'dense-cells'] : ['idle']) {
        await scenario(`echo n=${n} others=${load}`, ['p50', 'p95', 'max', 'n', 'data', 'parsed', 'frame'], async (kind) => {
          const { page } = await fresh(kind);
          await page.evaluate(([n, c, r]) => window.setup(n, c, r), [n, COLS, ROWS]);
          await page.evaluate(() => window.armEcho());
          if (load !== 'idle') await page.evaluate(([l]) => window.agent(l, 2048, 50), [load]);
          await page.waitForTimeout(300);
          for (let i = 0; i < 40; i++) {
            await page.keyboard.press('a');
            await page.waitForTimeout(23 + (i % 5) * 7);
          }
          await page.waitForTimeout(200);
          const r = await page.evaluate(() => window.echoStats());
          if (load !== 'idle') await page.evaluate(() => window.stopAgent());
          return r;
        });
      }
    }
  }
  if (SCEN.includes('cpu')) {
    for (const n of PANES) {
      for (const load of env('LOADS', 'light-cells,dense-cells,unicode')) {
        await scenario(`cpu n=${n} agent ${load}`, ['renderer', 'gpu', 'total', 'gpuMiB', 'work.p50', 'gap.mean', 'pres.full', 'pres.region', 'pres.mpx', 'secs'], async (kind) => {
          const { page, bcdp } = await fresh(kind);
          await page.evaluate(([n, c, r]) => window.setup(n, c, r), [n, COLS, ROWS]);
          await page.evaluate(([l]) => window.agent(l, 2048, 50, true), [load]);
          await page.waitForTimeout(500);
          const d0 = await page.evaluate(() => ({ ...window.drawStats }));
          const c0 = await cpu(bcdp);
          const t0 = Date.now();
          let sampler = null;
          if (process.env.SAMPLE) {
            const gpuPid = (await bcdp.send('SystemInfo.getProcessInfo')).processInfo.find((p) => p.type === 'GPU').id;
            const { spawn } = await import('node:child_process');
            const out = `${process.env.SAMPLE}-${kind.replace(/[@/]/g, '_')}-${Date.now()}.txt`;
            sampler = new Promise((r) => spawn('sample', [String(gpuPid), '3', '-mayDie', '-file', out], { stdio: 'ignore' }).on('exit', r));
          }
          const fr = await page.evaluate(() => window.frames(240));
          if (sampler) await sampler;
          const c1 = await cpu(bcdp, true);
          const s = (Date.now() - t0) / 1000;
          const d1 = await page.evaluate(() => ({ ...window.drawStats }));
          if (process.env.SHOT) await page.screenshot({ path: `${process.env.SHOT}-${kind.replace(/[@/]/g, '_')}.png`, clip: { x: 0, y: 0, width: 1800, height: 600 } });
          await page.evaluate(() => window.stopAgent());
          const pres = { full: (d1.full - d0.full) / s, region: (d1.region - d0.region) / s, mpx: (d1.px - d0.px) / s / 1e6 };
          const renderer = ((c1.renderer - c0.renderer) / s) * 100;
          const gpu = ((c1.gpu - c0.gpu) / s) * 100;
          return { renderer, gpu, total: renderer + gpu, rendererMiB: c1.rendererMiB, gpuMiB: c1.gpuMiB, ...fr, pres, secs: s };
        });
      }
    }
  }
} finally {
  for (const kind of KINDS) {
    for (const m of browsers[kind].errors.slice(0, 3)) console.log(`${kind} page error: ${m}`);
    await browsers[kind].browser.close();
  }
  server.close();
}
if (process.argv[2]) await writeFile(process.argv[2], JSON.stringify({ at: new Date().toISOString(), runs: RUNS, results }, null, 2));
