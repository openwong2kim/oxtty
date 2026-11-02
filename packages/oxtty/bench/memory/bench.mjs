// Memory of many terminals on one page with a real GPU: oxtty/xterm against xterm.js with @xterm/addon-webgl.
//
//   bash scripts/build-wasm.sh && bash scripts/build-web.sh   (repository root)
//   (cd packages/oxtty && npm ci && npm run bench:memory -- [panes] [lines])
//
// Each engine gets a fresh headless system Chrome (Metal through ANGLE, GPU blocklist ignored), `panes` terminals of
// 120×40 with 10,000 lines of scrollback, each fed `lines` lines of coloured log output (default 17 and 10,000).
// Reports the JS heap after a GC (CDP Performance.getMetrics) and the resident memory the renderer and GPU processes
// gained over the empty page (CDP SystemInfo.getProcessInfo + ps). Not part of the test gate.
//
// Then the idle phase, once with `cursorBlink` on and once off: the first terminal takes focus, and after
// IDLE_MS the GPU process's physical footprint is read (macOS `footprint`; RSS misses memory Chromium marks
// purgeable once nothing redraws) along with the WebGL draw calls and canvas `drawImage` calls of that window.
import { footprint, launch, mb, rss, serve } from './common.mjs';

const [panes = 17, lines = 10000] = process.argv.slice(2).map(Number);
/** How long the focused terminal idles before the GPU process is read; the first second settles the focus. */
const SETTLE_MS = 1000;
const IDLE_MS = 10000;
const server = await serve();

/** Resident bytes per Chrome process type ('renderer', 'GPU', ...). */
async function byType(cdp) {
  const sums = {};
  for (const p of (await cdp.send('SystemInfo.getProcessInfo')).processInfo) sums[p.type] = (sums[p.type] ?? 0) + rss(p.id);
  return sums;
}
const gpuOf = (sums) => sums.GPU ?? sums.gpu ?? 0;

async function gpuPid(cdp) {
  return (await cdp.send('SystemInfo.getProcessInfo')).processInfo.find((p) => /^gpu$/i.test(p.type))?.id;
}

async function measure(kind, blink) {
  const browser = await launch();
  try {
    const page = await browser.newPage({ viewport: { width: 4500, height: 4500 } });
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    await page.waitForFunction(() => window.ready, null, { timeout: 60000 });
    const browserCdp = await browser.newBrowserCDPSession();
    const base = await byType(browserCdp);
    const info = await page.evaluate(([k, n, l, b]) => window.setup(k, n, 120, 40, l, b), [kind, panes, lines, blink]);
    const cdp = await page.context().newCDPSession(page);
    await cdp.send('HeapProfiler.collectGarbage');
    await page.waitForTimeout(1500);
    await cdp.send('Performance.enable');
    const heap = (await cdp.send('Performance.getMetrics')).metrics.find((m) => m.name === 'JSHeapUsedSize').value;
    const now = await byType(browserCdp);
    const focused = await page.evaluate(() => window.idle());
    await page.waitForTimeout(SETTLE_MS);
    await page.evaluate(() => window.counts());
    await page.waitForTimeout(IDLE_MS);
    const idle = await byType(browserCdp);
    const pid = await gpuPid(browserCdp);
    return {
      kind,
      blink,
      ...info,
      errors,
      heap,
      renderer: now.renderer - base.renderer,
      rendererTotal: now.renderer,
      gpu: gpuOf(now) - gpuOf(base),
      gpuTotal: gpuOf(now),
      focused,
      idleCounts: await page.evaluate(() => window.counts()),
      idleGpu: gpuOf(idle),
      idleGpuFootprint: pid ? footprint(pid) : 0,
    };
  } finally {
    await browser.close();
  }
}

console.log(`${panes} panes of 120×40, ${lines} lines each`);
try {
  const idle = [];
  for (const kind of ['oxtty', 'xterm']) {
    for (const blink of [true, false]) {
      const r = await measure(kind, blink);
      idle.push(r);
      if (!blink) continue;
      console.log(
        `${kind.padEnd(6)} gpu="${r.gpuName}" canvases=${r.canvases} errors=${r.errors.length} | JS heap ${mb(r.heap)} MB | ` +
          `renderer +${mb(r.renderer)} MB (total ${mb(r.rendererTotal)}) | GPU process +${mb(r.gpu)} MB (total ${mb(r.gpuTotal)})`,
      );
      for (const e of r.errors) console.log(`  page error: ${e}`);
    }
  }
  console.log(`idle ${IDLE_MS / 1000} s with the first terminal focused`);
  for (const r of idle) {
    console.log(
      `${r.kind.padEnd(6)} blink=${r.blink ? 'on ' : 'off'} focused=${r.focused} | GPU process footprint ${mb(r.idleGpuFootprint)} MB, ` +
        `RSS ${mb(r.idleGpu)} MB | WebGL draws ${r.idleCounts.gl}, drawImage ${r.idleCounts.drawImage}`,
    );
  }
} finally {
  server.close();
}
