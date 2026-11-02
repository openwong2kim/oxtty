// The page's one shared WebGL2 context: loss and restore for every terminal at once, many terminals on a page,
// and panes out of view that draw nothing (ported in part from the TypeScript front end's renderer and
// terminal-webgl tests at 0510a7d).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

async function inPage(fn, arg) {
  return withPage('renderer.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    return page.evaluate(fn, arg);
  });
}

/** In the page: waits until every pane has asked for a full redraw, up to `ms`. */
const WAIT_ALL = `async (panes, ms) => {
  const seen = panes.map(() => []);
  const t0 = performance.now();
  while (performance.now() - t0 < ms && !seen.every((s) => s.includes('all'))) {
    await T.sleep(20);
    panes.forEach((p, i) => seen[i].push(...p.takeRequests()));
  }
  return seen;
}`;

test('a lost shared context that is restored redraws every terminal as before', async () => {
  const r = await inPage(async (waitAll) => {
    const wait = eval(waitAll);
    const a = new T.Pane(20, 4);
    const b = new T.Pane(20, 4);
    await a.write('\x1b[31mhello \x1b[44m한글\x1b[0m 😀 \x1b[4;58;2;255;0;0mline\x1b[0m');
    await b.write('\x1b[32mother pane\x1b[0m');
    const before = [await a.pixels(), await b.pixels()];
    const { gl, canvas } = a.debug;
    const shared = gl === b.debug.gl;
    const ext = gl.getExtension('WEBGL_lose_context');
    a.takeRequests();
    b.takeRequests();
    ext.loseContext();
    await T.sleep(50);
    const lost = a.debug.state;
    await a.write('!');
    await a.pixels();
    ext.restoreContext();
    const seen = await wait([a, b], 2000);
    await a.write('\b \b');
    const out = {
      shared,
      lost,
      redrawn: seen.map((s) => s.includes('all')),
      state: a.debug.state,
      sameContext: gl === a.debug.gl,
      sameCanvas: canvas === a.canvas,
      diff: [T.diff(before[0], await a.pixels()), T.diff(before[1], await b.pixels())],
    };
    a.dispose();
    b.dispose();
    return out;
  }, WAIT_ALL);
  assert.deepEqual(r, { shared: true, lost: 'lost', redrawn: [true, true], state: 'live', sameContext: true, sameCanvas: true, diff: [0, 0] });
});

test('a lost context that is not restored within 3 s is replaced, and every terminal draws again', async () => {
  const r = await inPage(async (waitAll) => {
    const wait = eval(waitAll);
    const a = new T.Pane(20, 4);
    const b = new T.Pane(20, 4);
    await a.write('\x1b[32mrecreated\x1b[0m');
    await b.write('\x1b[33mtoo\x1b[0m');
    const before = [await a.pixels(), await b.pixels()];
    const { gl } = a.debug;
    a.takeRequests();
    b.takeRequests();
    gl.getExtension('WEBGL_lose_context').loseContext();
    const t0 = performance.now();
    const seen = await wait([a, b], 5000);
    const after = Math.round(performance.now() - t0);
    const out = {
      after,
      redrawn: seen.map((s) => s.includes('all')),
      replaced: a.debug.gl !== gl && a.debug.gl === b.debug.gl,
      diff: [T.diff(before[0], await a.pixels()), T.diff(before[1], await b.pixels())],
    };
    a.dispose();
    b.dispose();
    return out;
  }, WAIT_ALL);
  assert.ok(r.after >= 3000 && r.after < 4000, `recreated after ${r.after} ms`);
  assert.deepEqual([r.redrawn, r.replaced, r.diff], [[true, true], true, [0, 0]]);
});

for (const count of [17, 32]) {
  test(`${count} terminals on one page share one context and all draw`, async () => {
    const r = await inPage(async (n) => {
      const panes = [];
      for (let i = 0; i < n; i++) {
        const p = new T.Pane(12, 1);
        await p.write(`pane ${i}`);
        panes.push(p);
      }
      await T.sleep(100);
      let drawing = '';
      for (const p of panes) {
        const px = await p.pixels();
        drawing += px.some((v, i) => i % 4 !== 3 && v > 0x80) ? 1 : 0;
      }
      const gl = panes[0].debug.gl;
      const out = { drawing, shared: panes.every((p) => p.debug.gl === gl), lost: gl.isContextLost(), states: [...new Set(panes.map((p) => p.debug.state))] };
      for (const p of panes) p.dispose();
      return out;
    }, count);
    assert.deepEqual(r, { drawing: '1'.repeat(count), shared: true, lost: false, states: ['live'] });
  });
}

test('panes out of view or idle draw nothing; a pane coming into view draws what it missed', async () => {
  const r = await inPage(async () => {
    const shown = new T.Pane(20, 2);
    const hidden = new T.Pane(20, 2);
    await shown.write('visible');
    await hidden.write('first');
    await shown.pixels();
    await hidden.pixels();
    // Idle: nothing changes, nothing is drawn
    const idle = [shown.debug.draws, hidden.debug.draws];
    await T.sleep(300);
    const idleAfter = [shown.debug.draws, hidden.debug.draws];
    hidden.container.style.display = 'none';
    await T.sleep(200);
    const hiddenDraws = hidden.debug.draws;
    for (let i = 0; i < 5; i++) {
      await hidden.write(`\r\nline ${i}`);
      await hidden.pixels();
    }
    const whileHidden = hidden.debug.draws - hiddenDraws;
    const visibleWhileHidden = hidden.debug.visible;
    hidden.container.style.display = '';
    await T.sleep(200);
    const afterShow = hidden.debug.draws - hiddenDraws;
    // What it drew matches a full rebuild
    const now = await hidden.pixels();
    hidden.term.options.drawBoldTextInBrightColors = false;
    hidden.term.options.drawBoldTextInBrightColors = true;
    const rebuilt = await hidden.pixels();
    const zero = new T.Pane(20, 2);
    zero.container.style.cssText = 'width: 0; height: 0; overflow: hidden';
    await zero.write('zero');
    await T.sleep(200);
    const zeroVisible = zero.debug.visible;
    const out = { idle: [idleAfter[0] - idle[0], idleAfter[1] - idle[1]], whileHidden, visibleWhileHidden, afterShow, diff: T.diff(now, rebuilt), zeroVisible };
    shown.dispose();
    hidden.dispose();
    zero.dispose();
    return out;
  });
  assert.deepEqual(r.idle, [0, 0]);
  assert.equal(r.whileHidden, 0);
  assert.equal(r.visibleWhileHidden, false);
  assert.ok(r.afterShow >= 1, `draws after showing: ${r.afterShow}`);
  assert.equal(r.diff, 0);
  assert.equal(r.zeroVisible, false);
});

test('a real terminal draws coloured Hangul in the frame onRender reports, and answers a cursor position request', async () => {
  const r = await inPage(async () => {
    const p = new T.Pane(20, 3, { theme: { background: '#000000' } });
    const data = [];
    p.term.onData((d) => data.push(d));
    const drawn = new Promise((resolve) => {
      p.term.onRender(() => {
        const pixels = T.read(p.canvas);
        let lit = 0;
        let red = 0;
        for (let i = 0; i < pixels.length; i += 4) {
          if (pixels[i] || pixels[i + 1] || pixels[i + 2]) lit++;
          if (pixels[i] > 150 && pixels[i + 1] < 100 && pixels[i + 2] < 100) red++;
        }
        if (red) resolve({ lit, red });
      });
    });
    await p.write('\x1b[31m한글\x1b[0m ok\x1b[6n');
    const out = { data, ...(await drawn) };
    p.dispose();
    return out;
  });
  assert.deepEqual(r.data, ['\x1b[1;8R']);
  assert.ok(r.red > 20, `red glyph pixels: ${r.red}`);
  assert.ok(r.lit > r.red, `lit pixels: ${r.lit}`);
});
