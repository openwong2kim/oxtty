// The present path on a real GPU (Metal through ANGLE): panes take the shared surface's frames by transfer into a
// `bitmaprenderer` canvas, with no copy and no compaction, and still survive context loss. SwiftShader, which the
// other browser tests run on, keeps the copy into a 2D canvas, and so does Electron. Skipped where Chrome gives no
// GPU context.
//
// Chrome crashes the renderer when a context is lost on purpose between a transfer and the commit of that frame,
// so these tests lose the context only after a couple of frames have gone by (see the docs of render/gpu.rs).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

async function inPage(fn, arg, userAgent) {
  return withPage(
    'renderer.mjs',
    async (page) => {
      await page.waitForFunction(() => window.ready);
      return page.evaluate(fn, arg);
    },
    { gpu: true, userAgent },
  );
}

/** Chrome's user agent as Electron 41 sends it, which takes the copy path on a GPU too. */
const ELECTRON =
  'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/146.0.7680.188 Electron/41.10.6 Safari/537.36';

const TEXT = '\x1b[31mred\x1b[0m 한글 \x1b[42mgreen\x1b[0m\r\n\x1b[1mbold\x1b[0m │─┼─ WM 😀';

/** In the page: resolves after `n` frames. */
const FRAMES = `(n) => new Promise((resolve) => (function next(k) { k ? requestAnimationFrame(() => next(k - 1)) : resolve(); })(n))`;

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

const gpu = await inPage(() => {
  const p = new T.Pane(4, 1);
  // Read the renderer directly: where Chrome gives no GPU context there may be no renderer state to read.
  const present = p.term._rendererDebug()?.present;
  p.dispose();
  return present;
});
const skip = gpu === 'transfer' ? false : `no GPU context here (panes present by ${gpu})`;

/** In the page: a focused pane with a blinking cursor showing `text`, read with its cursor layer in one frame. */
async function layered([text, framesSrc]) {
  const frames = eval(framesSrc);
  const p = new T.Pane(30, 4, { cursorBlink: true });
  p.focus(true);
  await p.write(text);
  const { canvas, shown, region } = await p.layered();
  const { canvas: element, cursorLayer } = p.debug;
  const out = {
    present: p.debug.present,
    canvas: Array.from(canvas),
    shown: Array.from(shown),
    region,
    // A canvas has one kind of context: a bitmaprenderer canvas has no 2D context
    bitmaprenderer: [element.getContext('2d') === null, cursorLayer.getContext('2d') === null],
  };
  // Idle panes are not compacted on the transfer path: a transferred canvas has no buffers of its own
  const before = p.debug.compactions;
  await T.sleep(1500);
  await frames(2);
  out.compactions = p.debug.compactions - before;
  out.idle = T.diff(canvas, T.read(element));
  p.dispose();
  return out;
}

const diff = (a, b) => (a.length === b.length ? a.reduce((n, v, i) => n + (v !== b[i]), 0) : Math.max(a.length, b.length));

test('panes take frames by transfer and show what the copy path (Electron) shows, cursor layer included', { skip }, async () => {
  const transfer = await inPage(layered, [TEXT, FRAMES]);
  const copy = await inPage(layered, [TEXT, FRAMES], ELECTRON);
  assert.deepEqual([transfer.present, transfer.bitmaprenderer, transfer.compactions, transfer.idle], ['transfer', [true, true], 0, 0]);
  assert.deepEqual([copy.present, copy.bitmaprenderer], ['copy', [false, false]]);
  assert.deepEqual(transfer.region, copy.region);
  assert.ok(transfer.canvas.some((v, i) => i % 4 !== 3 && v > 0x80), 'the canvas shows text');
  assert.ok(diff(transfer.canvas, transfer.shown) > 0, 'the cursor layer shows the cursor');
  assert.deepEqual([diff(transfer.canvas, copy.canvas), diff(transfer.shown, copy.shown)], [0, 0]);
});

test('the shared surface shrinks to a pixel once every pane has idled for a second, and grows back for the next frame', { skip }, async () => {
  const r = await inPage(async ([text, framesSrc]) => {
    const frames = eval(framesSrc);
    const a = new T.Pane(30, 4);
    const b = new T.Pane(30, 4);
    await a.write(text);
    await b.write(text);
    const shown = await a.pixels();
    const surface = a.debug.gl.canvas;
    const size = () => [surface.width, surface.height].join('x');
    const drawn = size();
    // b keeps drawing while a idles
    for (let i = 0; i < 8; i++) {
      await T.sleep(200);
      await b.write(`\r${i}`);
      await frames(1);
    }
    const busy = size();
    await T.sleep(1500);
    const idle = size();
    const kept = T.diff(shown, T.read(a.canvas));
    await a.write('\x1b[2;1Hmore');
    const after = await a.pixels();
    const out = { drawn, busy, idle, kept, grown: size(), redrawn: T.diff(shown, after) > 0 };
    a.dispose();
    b.dispose();
    return out;
  }, [TEXT, FRAMES]);
  assert.notEqual(r.drawn, '1x1');
  assert.deepEqual(r, { drawn: r.drawn, busy: r.drawn, idle: '1x1', kept: 0, grown: r.drawn, redrawn: true });
});

test('a lost shared context that is restored, or replaced, redraws every pane as before', { skip }, async () => {
  const r = await inPage(async ([text, framesSrc, waitAll]) => {
    const frames = eval(framesSrc);
    const wait = eval(waitAll);
    const a = new T.Pane(20, 4);
    const b = new T.Pane(20, 4);
    await a.write(text);
    await b.write('\x1b[32mother pane\x1b[0m');
    const before = [await a.pixels(), await b.pixels()];
    const { gl } = a.debug;
    const ext = gl.getExtension('WEBGL_lose_context');
    await frames(2);
    a.takeRequests();
    b.takeRequests();
    ext.loseContext();
    await T.sleep(50);
    const lost = a.debug.state;
    ext.restoreContext();
    const restored = await wait([a, b], 2000);
    const diffRestored = [T.diff(before[0], await a.pixels()), T.diff(before[1], await b.pixels())];
    // Not restored within 3 s: a new context
    await frames(2);
    a.takeRequests();
    b.takeRequests();
    a.debug.gl.getExtension('WEBGL_lose_context').loseContext();
    const replaced = await wait([a, b], 5000);
    const out = {
      lost,
      restored: restored.map((s) => s.includes('all')),
      diffRestored,
      replaced: replaced.map((s) => s.includes('all')),
      newContext: a.debug.gl !== gl && a.debug.gl === b.debug.gl,
      present: [a.debug.present, b.debug.present],
      diffReplaced: [T.diff(before[0], await a.pixels()), T.diff(before[1], await b.pixels())],
    };
    a.dispose();
    b.dispose();
    return out;
  }, [TEXT, FRAMES, WAIT_ALL]);
  assert.deepEqual(r, {
    lost: 'lost',
    restored: [true, true],
    diffRestored: [0, 0],
    replaced: [true, true],
    newContext: true,
    present: ['transfer', 'transfer'],
    diffReplaced: [0, 0],
  });
});

test('a pane with display: none lets go of its frame and has it back when shown; the last dispose frees the context', { skip }, async () => {
  const r = await inPage(async ([text, framesSrc]) => {
    const frames = eval(framesSrc);
    const p = new T.Pane(30, 4);
    await p.write(text);
    const before = await p.pixels();
    p.container.style.display = 'none';
    await frames(3);
    const hidden = [p.debug.collapsed, p.canvas.width];
    p.container.style.display = '';
    await frames(3);
    const out = { hidden, diff: T.diff(before, T.read(p.canvas)) };
    // Many panes drawing every frame, then all disposed at once: the context is lost with no canvas left showing
    const panes = Array.from({ length: 17 }, () => new T.Pane(40, 1));
    for (let i = 0; i < 5; i++) {
      await Promise.all(panes.map((q, k) => q.write(`\r\x1b[3${(k % 7) + 1}mframe ${i}`)));
      await frames(1);
    }
    out.drawing = panes.every((q) => T.read(q.canvas).some((v, i) => i % 4 !== 3 && v > 0x80));
    const { gl } = p.debug;
    for (const q of panes) q.dispose();
    p.dispose();
    await frames(2);
    out.freed = gl.isContextLost();
    return out;
  }, [TEXT, FRAMES]);
  assert.deepEqual(r, { hidden: [true, 0], diff: 0, drawing: true, freed: true });
});
