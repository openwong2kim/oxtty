// Usage (packages/wongtty): MODES=copy,perpane,xtermlike,perpane+ro RUNS=3 DPR=2 N=15 node bench/memory/present-probe/probe.mjs
// Runs the present-path probe in headless system Chrome on the real GPU; reports GPU process footprint.
//
// Modes and their '+' flags are listed in probe-page.js. RUNS repeats every mode, interleaved (the order rotates per
// run), and prints the medians at the end. The dpr is the screen scale factor of a 4500x4500 window
// (--force-device-scale-factor); SCALE=emulate emulates it in the page instead, where ResizeObserver's
// device-pixel-content-box reports CSS pixels. ARGS adds Chrome switches (ARGS=--disable-features=SkiaGraphite).
// Other env: N panes (17), BUSY ms (8000), INTERVAL ms (50), BUSYN busy panes (all), PAGE_COMPACT (page modes).
import { footprintDetail, launch, mb, pids } from '../common.mjs';
import { readFileSync } from 'node:fs';
import { execSync } from 'node:child_process';
import { createServer } from 'node:http';
import { chromium } from 'playwright-core';

const script = readFileSync(new URL('./probe-page.js', import.meta.url), 'utf8');
const modes = (process.env.MODES ?? 'copy,page,pagep,perpane,copy2').split(',');
const n = Number(process.env.N ?? 17), dpr = Number(process.env.DPR ?? 1), runs = Number(process.env.RUNS ?? 1);
const BUSY = Number(process.env.BUSY ?? 8000), interval = Number(process.env.INTERVAL ?? 50);
const extraArgs = process.env.ARGS ? process.env.ARGS.split(' ') : [];
const EMULATE = process.env.SCALE === 'emulate';
// The GPU process of a fresh browser drops its startup surfaces about 5 s after launch; the empty page is read after.
const SETTLE = Number(process.env.SETTLE ?? 6000);
const IOS = 'IOSurface', GFX = 'IOAccelerator (graphics)', ACC = 'IOAccelerator';
const cat = (f, k) => f.categories[k] ?? 0;
const server = createServer((q, r) => r.writeHead(200, { 'content-type': 'text/html' }).end('<!doctype html><body style="margin:0">'));
await new Promise((res) => server.listen(0, '127.0.0.1', res));
const results = [];
console.log(`n=${n} dpr=${dpr} (${EMULATE ? 'emulated' : 'screen scale factor'}) busy ${BUSY} ms every ${interval} ms${extraArgs.length ? ` args ${extraArgs.join(' ')}` : ''}`);
for (let run = 0; run < runs; run++) for (let i = 0; i < modes.length; i++) {
  const mode = modes[(run + i) % modes.length];
  const browser = EMULATE && !extraArgs.length
    ? await launch()
    : await chromium.launch({
        channel: 'chrome',
        headless: true,
        args: ['--use-angle=metal', '--enable-gpu', '--ignore-gpu-blocklist', ...(EMULATE ? [] : [`--force-device-scale-factor=${dpr}`, '--window-size=4500,4500']), ...extraArgs],
      });
  try {
    const page = await browser.newPage(EMULATE ? { viewport: { width: 4500, height: 4500 }, deviceScaleFactor: dpr } : { viewport: null });
    await page.goto(`http://127.0.0.1:${server.address().port}/`);
    await page.addScriptTag({ content: script });
    const cdp = await browser.newBrowserCDPSession();
    const gpu = async () => { const p = await pids(cdp); return (p.gpu ?? p.GPU)[0]; };
    await page.waitForTimeout(SETTLE);
    const empty = footprintDetail(await gpu());
    await page.evaluate((v) => { window.pageCompact = v; }, process.env.PAGE_COMPACT === 'unconf' ? 'unconf' : !!process.env.PAGE_COMPACT);
    const info = await page.evaluate((o) => window.runProbe(o), { mode, n, w: 868, h: 560, interval, dpr, cols: 120, rows: 40, busyN: process.env.BUSYN ? Number(process.env.BUSYN) : undefined });
    await page.waitForTimeout(1500);
    const before = footprintDetail(await gpu());
    await page.evaluate(() => window.probe.counts());
    await page.evaluate(() => window.probe.start());
    await page.waitForTimeout(BUSY);
    const busy = footprintDetail(await gpu());
    const c = await page.evaluate(() => window.probe.counts());
    const px = await page.evaluate(() => window.probe.mpx());
    await page.evaluate(() => window.probe.stop());
    await page.waitForTimeout(1200);
    await page.evaluate(() => window.probe.compact());
    await page.waitForTimeout(4000);
    const idle = footprintDetail(await gpu());
    const load = execSync('sysctl -n vm.loadavg').toString().trim();
    results.push({ mode, empty, busy, idle });
    console.log(
      `${mode.padEnd(16)} | empty ${mb(empty.total)} before ${mb(before.total)} busy ${mb(busy.total)} idle ${mb(idle.total)} | ` +
        `busy +${mb(busy.total - empty.total)} (IOSurface ${mb(cat(busy, IOS))} +${mb(cat(busy, IOS) - cat(empty, IOS))}, ` +
        `IOAccel(gfx) ${mb(cat(busy, GFX))} +${mb(cat(busy, GFX) - cat(empty, GFX))}, IOAccel +${mb(cat(busy, ACC) - cat(empty, ACC))}) | ` +
        `idle +${mb(idle.total - empty.total)} (IOSurface +${mb(cat(idle, IOS) - cat(empty, IOS))}) | ` +
        `backing ${px.canvas.toFixed(1)} Mpx, GL buffers ${px.gl.toFixed(1)} Mpx, lost ${px.lost} | ` +
        `frames ${c.frames} draws ${c.draws} presents ${c.presents}${info.pageCanvas ? ` page ${info.pageCanvas.w}x${info.pageCanvas.h}` : ''} | load ${load}`,
    );
  } finally { await browser.close(); }
}
server.close();
if (runs > 1) {
  const median = (v) => [...v].sort((a, b) => a - b)[v.length >> 1];
  for (const mode of modes) {
    const rs = results.filter((r) => r.mode === mode);
    const m = (f) => mb(median(rs.map(f)));
    console.log(
      `median of ${rs.length} ${mode.padEnd(16)} busy ${m((r) => r.busy.total)} MB (+${m((r) => r.busy.total - r.empty.total)}) | ` +
        `IOSurface ${m((r) => cat(r.busy, IOS))} (+${m((r) => cat(r.busy, IOS) - cat(r.empty, IOS))}) | ` +
        `IOAccel(gfx) ${m((r) => cat(r.busy, GFX))} (+${m((r) => cat(r.busy, GFX) - cat(r.empty, GFX))}) | ` +
        `idle +${m((r) => r.idle.total - r.empty.total)}`,
    );
  }
}
