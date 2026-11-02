// What the renderer process memory of many oxtty/xterm terminals is made of.
//
//   (cd packages/oxtty && node bench/memory/breakdown.mjs [panes,...] [lines] [dpr,...])   defaults: 1,17 10000 1,2
//
// For each pane count and devicePixelRatio, a fresh headless system Chrome on the real GPU (as bench.mjs) opens the
// page, builds the terminals (120×40, 10,000 lines of scrollback, fed `lines` lines of log output) and reports, after
// a GC: the physical footprint (macOS `footprint`) of the renderer and the GPU process over the empty page and
// their largest region categories, the WebAssembly memory of the engine and of the front end, the JS heap (CDP Performance.getMetrics),
// DOM nodes (CDP Memory.getDOMCounters) and canvas backing stores (width × height × 4, attached to the page, detached
// such as the atlas pages, and OffscreenCanvas). Then the footprint again with every terminal `display: none`, and
// after every terminal is disposed. With two pane counts, the per-pane cost is the difference over the extra panes.
import { footprintDetail, launch, mb1, pids, serve } from './common.mjs';

const args = process.argv.slice(2);
const paneCounts = (args[0] ?? '1,17').split(',').map(Number);
const lines = Number(args[1] ?? 10000);
const ratios = (args[2] ?? '1,2').split(',').map(Number);
/** Lets the renderer settle (purge, decommit) before its footprint is read. */
const SETTLE_MS = 4000;
const server = await serve();

async function rendererFootprint(browserCdp) {
  const renderer = (await pids(browserCdp)).renderer ?? [];
  // The page's renderer is the largest one
  return renderer.map(footprintDetail).sort((a, b) => b.total - a.total)[0] ?? { total: 0, categories: {} };
}

async function gpuFootprint(browserCdp) {
  const gpu = (await pids(browserCdp)).gpu ?? (await pids(browserCdp)).GPU ?? [];
  return gpu.length ? footprintDetail(gpu[0]) : { total: 0, categories: {} };
}

async function settle(page, cdp) {
  await cdp.send('HeapProfiler.collectGarbage');
  await page.waitForTimeout(SETTLE_MS);
}

async function measure(panes, dpr) {
  const browser = await launch();
  try {
    const page = await browser.newPage({ viewport: { width: 4500, height: 4500 }, deviceScaleFactor: dpr });
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    await page.waitForFunction(() => window.ready, null, { timeout: 60000 });
    const browserCdp = await browser.newBrowserCDPSession();
    const cdp = await page.context().newCDPSession(page);
    await cdp.send('Performance.enable');
    await settle(page, cdp);
    const empty = await rendererFootprint(browserCdp);
    const emptyGpu = await gpuFootprint(browserCdp);
    await page.evaluate(([n, l]) => window.setup('oxtty', n, 120, 40, l, false), [panes, lines]);
    await settle(page, cdp);
    const full = await rendererFootprint(browserCdp);
    const fullGpu = await gpuFootprint(browserCdp);
    const metrics = Object.fromEntries((await cdp.send('Performance.getMetrics')).metrics.map((m) => [m.name, m.value]));
    const dom = await cdp.send('Memory.getDOMCounters');
    const parts = await page.evaluate(() => window.breakdown());
    await page.evaluate(() => window.hide(true));
    await settle(page, cdp);
    const hidden = await rendererFootprint(browserCdp);
    const hiddenGpu = await gpuFootprint(browserCdp);
    await page.evaluate(() => window.hide(false));
    await page.evaluate(() => window.dispose());
    await settle(page, cdp);
    const disposed = await rendererFootprint(browserCdp);
    const disposedGpu = await gpuFootprint(browserCdp);
    const afterDispose = await page.evaluate(() => window.breakdown());
    return { panes, dpr, errors, empty, full, metrics, dom, parts, hidden, disposed, afterDispose, emptyGpu, fullGpu, hiddenGpu, disposedGpu };
  } finally {
    await browser.close();
  }
}

/** Categories that grew the most from `a` to `b`. */
function grown(a, b, n = 6) {
  return Object.keys(b.categories)
    .map((k) => [k, b.categories[k] - (a.categories[k] ?? 0)])
    .filter(([, d]) => Math.abs(d) >= 1048576)
    .sort((x, y) => y[1] - x[1])
    .slice(0, n)
    .map(([k, d]) => `${k} ${d > 0 ? '+' : ''}${mb1(d)}`)
    .join(', ');
}

const wasmOf = (parts, name) => parts.wasm.filter((w) => w.name === name).reduce((s, w) => s + w.bytes, 0);

console.log(`oxtty, 120×40 panes, ${lines} lines each; footprint over the empty page, MB`);
try {
  for (const dpr of ratios) {
    const results = [];
    for (const panes of paneCounts) {
      const r = await measure(panes, dpr);
      results.push(r);
      const c = r.parts.canvas;
      console.log(
        `dpr ${dpr} panes ${String(panes).padStart(2)} | renderer +${mb1(r.full.total - r.empty.total)} ` +
          `(hidden +${mb1(r.hidden.total - r.empty.total)}, disposed +${mb1(r.disposed.total - r.empty.total)}) | ` +
          `wasm engine ${mb1(wasmOf(r.parts, 'engine'))}, front end ${mb1(wasmOf(r.parts, 'front end'))} | ` +
          `JS heap ${mb1(r.metrics.JSHeapUsedSize)} used / ${mb1(r.metrics.JSHeapTotalSize)} | ` +
          `canvases attached ${c.attachedCount} ${mb1(c.attached)}, detached ${c.detachedCount} ${mb1(c.detached)}, ` +
          `offscreen ${c.offscreenCount} ${mb1(c.offscreen)} | DOM nodes ${r.dom.nodes}, listeners ${r.dom.jsEventListeners} | ` +
          `errors ${r.errors.length}`,
      );
      console.log(
        `  GPU process +${mb1(r.fullGpu.total - r.emptyGpu.total)} (hidden +${mb1(r.hiddenGpu.total - r.emptyGpu.total)}, ` +
          `disposed +${mb1(r.disposedGpu.total - r.emptyGpu.total)}), grew: ${grown(r.emptyGpu, r.fullGpu)}`,
      );
      console.log(`  grew: ${grown(r.empty, r.full)}`);
      console.log(`  hidden vs shown: ${grown(r.full, r.hidden)}`);
      console.log(`  disposed vs shown: ${grown(r.full, r.disposed)}`);
      for (const e of r.errors) console.log(`  page error: ${e}`);
    }
    if (results.length >= 2) {
      const [a, b] = [results[0], results.at(-1)];
      const n = b.panes - a.panes;
      const per = (f) => mb1((f(b) - f(a)) / n);
      const fixed = (f) => mb1(f(a) - (a.panes * (f(b) - f(a))) / n);
      const total = (r) => r.full.total - r.empty.total;
      const gpu = (r) => r.fullGpu.total - r.emptyGpu.total;
      const engine = (r) => wasmOf(r.parts, 'engine');
      const front = (r) => wasmOf(r.parts, 'front end');
      const heap = (r) => r.metrics.JSHeapUsedSize;
      const attached = (r) => r.parts.canvas.attached;
      const detached = (r) => r.parts.canvas.detached + r.parts.canvas.offscreen;
      console.log(
        `dpr ${dpr} per pane: renderer ${per(total)}, GPU process ${per(gpu)}, engine wasm ${per(engine)}, front-end wasm ${per(front)}, ` +
          `JS heap ${per(heap)}, attached canvases ${per(attached)}, detached/offscreen ${per(detached)}`,
      );
      console.log(
        `dpr ${dpr} fixed:    renderer ${fixed(total)}, GPU process ${fixed(gpu)}, engine wasm ${fixed(engine)}, front-end wasm ${fixed(front)}, ` +
          `JS heap ${fixed(heap)}, attached canvases ${fixed(attached)}, detached/offscreen ${fixed(detached)}`,
      );
    }
  }
} finally {
  server.close();
}
