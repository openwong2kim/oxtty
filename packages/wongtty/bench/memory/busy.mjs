// GPU memory and draw calls while every terminal prints: wongtty/xterm against xterm.js with @xterm/addon-webgl.
//
//   (cd packages/wongtty && node bench/memory/busy.mjs [panes] [seconds] [dpr] [interval])   defaults: 17 12 1 50
//
// Each engine gets a fresh headless system Chrome on the real GPU (as bench.mjs) with `panes` terminals of 120×40
// (10,000 lines of scrollback, 1,000 lines of log output first; with MIXED=1, sizes from 92×30 to 120×40 as in a split
// layout). Then every terminal gets three lines of output
// every `interval` ms for `seconds`; near the end the GPU process's physical footprint is read (macOS `footprint`)
// with its largest region categories, and the WebGL draw calls, canvas `drawImage` calls and animation frames of
// the window are counted in the page. Then the GPU process again after the terminals idled IDLE_MS. With TYPING=1
// the first terminal is focused and gets an echoed character every 100 ms instead of output, and the echo latency
// is reported.
import { footprintDetail, launch, mb, pids, serve } from './common.mjs';

const [panes = 17, seconds = 12, dpr = 1, interval = 50] = process.argv.slice(2).map(Number);
/** How long the terminals idle after the output stops before the GPU process is read again. */
const IDLE_MS = 5000;
const server = await serve();

async function measure(kind) {
  const browser = await launch();
  try {
    const page = await browser.newPage({ viewport: { width: 4500, height: 4500 }, deviceScaleFactor: dpr });
    const errors = [];
    page.on('pageerror', (e) => errors.push(e.message));
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    await page.waitForFunction(() => window.ready, null, { timeout: 60000 });
    const browserCdp = await browser.newBrowserCDPSession();
    const gpuPid = async () => {
      const p = await pids(browserCdp);
      return (p.gpu ?? p.GPU)[0];
    };
    await page.waitForTimeout(1000);
    const empty = footprintDetail(await gpuPid());
    await page.evaluate(([k, n, mixed]) => {
      window.mixed = mixed;
      return window.setup(k, n, 120, 40, 1000, false);
    }, [kind, panes, !!process.env.MIXED]);
    await page.waitForTimeout(2000);
    const before = footprintDetail(await gpuPid());
    await page.evaluate(() => window.counts());
    await page.evaluate(([i, typing]) => window.startBusy(i, typing), [interval, !!process.env.TYPING]);
    await page.waitForTimeout(seconds * 1000 - 1000);
    const busy = footprintDetail(await gpuPid());
    await page.waitForTimeout(1000);
    const { frames, echoes } = await page.evaluate(() => window.stopBusy());
    const counts = await page.evaluate(() => window.counts());
    await page.waitForTimeout(IDLE_MS);
    const after = footprintDetail(await gpuPid());
    return { kind, errors, empty, before, busy, after, frames, echoes, counts };
  } finally {
    await browser.close();
  }
}

const cat = (f, e, k) => mb((f.categories[k] ?? 0) - (e.categories[k] ?? 0));
console.log(`${panes} panes of ${process.env.MIXED ? '92–120×30–40' : '120×40'} printing 3 lines every ${interval} ms for ${seconds} s, dpr ${dpr}`);
try {
  for (const kind of process.env.KINDS?.split(',') ?? ['wongtty', 'xterm']) {
    const r = await measure(kind);
    const { gl, drawImage, bitmap } = r.counts;
    console.log(
      `${kind.padEnd(8)} errors=${r.errors.length} | GPU process footprint ${mb(r.busy.total)} MB busy, ${mb(r.before.total)} before ` +
        `(over empty page +${mb(r.busy.total - r.empty.total)}: IOSurface +${cat(r.busy, r.empty, 'IOSurface')}, ` +
        `IOAccelerator (graphics) +${cat(r.busy, r.empty, 'IOAccelerator (graphics)')}) | ${r.frames} frames, ` +
        `WebGL draws ${gl} (${(gl / r.frames).toFixed(1)}/frame), drawImage ${drawImage} (${(drawImage / r.frames).toFixed(1)}/frame), ` +
        `transferFromImageBitmap ${bitmap} | idle ${IDLE_MS / 1000} s after: ${mb(r.after.total)} MB (+${mb(r.after.total - r.empty.total)})`,
    );
    if (r.echoes.length) {
      const ms = r.echoes.map((e) => e[0]).sort((a, b) => a - b);
      const at = (q) => ms[Math.min(ms.length - 1, Math.floor(ms.length * q))].toFixed(1);
      const frames = {};
      for (const [, f] of r.echoes) frames[f] = (frames[f] ?? 0) + 1;
      console.log(`  echo in the focused pane: ${ms.length} echoes, median ${at(0.5)} ms, p90 ${at(0.9)} ms, frames after the write ${JSON.stringify(frames)}`);
    }
    for (const e of r.errors) console.log(`  page error: ${e}`);
  }
} finally {
  server.close();
}
