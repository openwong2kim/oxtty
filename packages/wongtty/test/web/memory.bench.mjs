// Memory of many terminals on one page: wongtty/xterm against real xterm.js with @xterm/addon-webgl, after the same
// output. Not part of `npm run test:web`; run `node test/web/memory.bench.mjs [panes] [cols] [rows]`.
// Reports the JS heap (CDP Performance.getMetrics, after a GC), wongtty's front-end wasm memory, and the resident
// memory of Chrome's renderer and GPU processes (CDP SystemInfo.getProcessInfo + ps). The viewport holds every
// pane, so wongtty draws them all too (it skips panes out of view).
import { execFileSync } from 'node:child_process';
import { withPage } from './harness.mjs';

const [panes = 17, cols = 120, rows = 40] = process.argv.slice(2).map(Number);

function rss(pid) {
  try {
    return Number(execFileSync('ps', ['-o', 'rss=', '-p', String(pid)]).toString().trim()) * 1024;
  } catch {
    return 0;
  }
}

async function measure(kind) {
  return withPage('memory.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    const info = await page.evaluate(([k, n, c, r]) => window.setup(k, n, c, r), [kind, panes, cols, rows]);
    const cdp = await page.context().newCDPSession(page);
    await cdp.send('HeapProfiler.collectGarbage');
    await cdp.send('Performance.enable');
    const { metrics } = await cdp.send('Performance.getMetrics');
    const metric = (name) => metrics.find((m) => m.name === name)?.value ?? 0;
    const browser = await page.context().browser().newBrowserCDPSession();
    const { processInfo } = await browser.send('SystemInfo.getProcessInfo');
    const byType = {};
    for (const p of processInfo) byType[p.type] = (byType[p.type] ?? 0) + rss(p.id);
    const drawn = kind === 'wongtty' ? await page.evaluate(() => window.terms.filter((t) => t._rendererDebug().draws > 0).length) : panes;
    return { kind, ...info, drawn, jsHeap: metric('JSHeapUsedSize'), renderer: byType.renderer ?? 0, gpu: byType.GPU ?? byType.gpu ?? 0 };
  }, { viewport: { width: 4500, height: 4500 } });
}

const mb = (b) => (b / 1048576).toFixed(1);
console.log(`${panes} panes of ${cols}×${rows}`);
for (const kind of ['wongtty', 'xterm']) {
  const r = await measure(kind);
  console.log(`${kind}: drawn ${r.drawn}, canvases ${r.canvases}, JS heap ${mb(r.jsHeap)} MB, front-end wasm ${mb(r.frontEndWasm)} MB, renderer RSS ${mb(r.renderer)} MB, GPU RSS ${mb(r.gpu)} MB`);
}
