// The present path on a real GPU (Metal through ANGLE): panes take the shared surface's frames by transfer into a
// `bitmaprenderer` canvas, with no copy and no compaction, and still survive context loss. SwiftShader, which the
// other browser tests run on, keeps the copy into a 2D canvas. Skipped where Chrome gives no GPU context.
//
// Chrome crashes the renderer when a context is lost on purpose between a transfer and the commit of that frame,
// so these tests lose the context only after a couple of frames have gone by (see the docs of render/gpu.rs).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

async function inPage(fn, arg) {
  return withPage(
    'renderer.mjs',
    async (page) => {
      await page.waitForFunction(() => window.ready);
      return page.evaluate(fn, arg);
    },
    { gpu: true },
  );
}

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
  const present = p.debug.present;
  p.dispose();
  return present;
});
const skip = gpu === 'transfer' ? false : `no GPU context here (panes present by ${gpu})`;

test('panes take frames by transfer, cursor layer included, and draw what the copy path draws', { skip }, async () => {
  const r = await inPage(async ([text, framesSrc]) => {
    const frames = eval(framesSrc);
    const p = new T.Pane(30, 4, { cursorBlink: true });
    p.focus(true);
    await p.write(text);
    const pixels = await p.pixels();
    const { canvas, cursorLayer } = p.debug;
    const out = {
      present: p.debug.present,
      // A canvas has one kind of context: this one is a bitmaprenderer
      canvas2d: canvas.getContext('2d') === null && canvas.getContext('bitmaprenderer') !== null,
      layer2d: cursorLayer.getContext('2d') === null,
      lit: pixels.some((v, i) => i % 4 !== 3 && v > 0x80),
    };
    // Idle panes are not compacted: a transferred canvas has no buffers of its own
    const before = p.debug.compactions;
    await T.sleep(1500);
    await frames(2);
    out.compactions = p.debug.compactions - before;
    out.same = T.diff(pixels, T.read(canvas));
    p.dispose();
    return out;
  }, [TEXT, FRAMES]);
  assert.deepEqual(r, { present: 'transfer', canvas2d: true, layer2d: true, lit: true, compactions: 0, same: 0 });
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
