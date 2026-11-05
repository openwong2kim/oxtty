// What a pane's canvas holds in the GPU process: an idle pane redraws its last frame once into a fresh canvas
// (Chromium drops the extra buffers a busy 2D canvas collected), and a pane that takes no room has no pixels.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

async function inPage(fn, arg) {
  return withPage('renderer.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    return page.evaluate(fn, arg);
  });
}

const TEXT = '\x1b[31mred\x1b[0m 한글 \x1b[42mgreen\x1b[0m\r\n\x1b[1mbold\x1b[0m │─┼─ WM';

// The copy path only: a pane that takes frames by transfer (on a GPU, see renderer-transfer.test.mjs) has no
// buffers to drop
test('a pane idle for a second redraws its last frame once into a fresh canvas, with the same pixels', async (t) => {
  const r = await inPage(async (text) => {
    const p = new T.Pane(30, 4, { cursorBlink: true });
    p.focus(true);
    await p.write(text);
    const before = await p.pixels();
    const start = p.debug;
    p.takeRequests();
    // Output every 300 ms keeps the pane busy: no compaction, unless a loaded machine left a gap of a second
    let gaps = 0;
    for (let i = 0, last = performance.now(); i < 5; i++) {
      await T.sleep(300);
      await p.write('.');
      if (performance.now() - last > 950) gaps++;
      last = performance.now();
    }
    const busy = p.debug.compactions;
    const shown = await p.pixels();
    await T.sleep(1500);
    const idle = p.debug;
    const out = {
      busy: busy - start.compactions <= gaps,
      idle: idle.compactions - busy,
      requests: p.takeRequests().filter((x) => x === 'all'),
      same: T.diff(shown, T.read(idle.canvas)),
      size: [idle.canvas.width, idle.canvas.height].join('x') === [start.canvas.width, start.canvas.height].join('x'),
      changed: T.diff(before, shown) > 0,
      present: p.debug.present,
    };
    p.dispose();
    return out;
  }, TEXT);
  if (r.present !== 'copy') return t.skip(`panes present by ${r.present}`);
  assert.deepEqual(r, { busy: true, idle: 1, requests: [], same: 0, size: true, changed: true, present: 'copy' });
});

/** In the page: resolves after `n` frames. */
const FRAMES = `(n) => new Promise((resolve) => (function next(k) { k ? requestAnimationFrame(() => next(k - 1)) : resolve(); })(n))`;

// The shared surface keeps buffers of the last frame's size, and on the copy path also the copy a pane's canvas read
// of it: one full-window pane at devicePixelRatio 2 held two 17 MB buffers next to its canvas's one. On the copy path
// it waits five seconds, so a keystroke after a short pause does not wait for new buffers
test('on the copy path too, the shared surface shrinks to a pixel once every pane has idled for five seconds', async (t) => {
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
    const paused = size();
    await T.sleep(5000);
    const idle = size();
    const kept = T.diff(shown, T.read(a.canvas));
    await a.write('\x1b[2;1Hmore');
    const after = await a.pixels();
    const out = { present: a.debug.present, drawn, busy, paused, idle, kept, grown: size(), redrawn: T.diff(shown, after) > 0 };
    a.dispose();
    b.dispose();
    return out;
  }, [TEXT, FRAMES]);
  if (r.present !== 'copy') return t.skip(`panes present by ${r.present}`);
  assert.notEqual(r.drawn, '1x1');
  assert.deepEqual(r, { present: 'copy', drawn: r.drawn, busy: r.drawn, paused: r.drawn, idle: '1x1', kept: 0, grown: r.drawn, redrawn: true });
});

test('a pane that takes no room right after it drew still lets the shared surface shrink', async () => {
  const r = await inPage(async ([text, framesSrc]) => {
    const frames = eval(framesSrc);
    const p = new T.Pane(30, 4);
    await p.write(text);
    const before = await p.pixels();
    const surface = p.debug.gl.canvas;
    const size = () => [surface.width, surface.height].join('x');
    const drawn = size();
    p.container.style.display = 'none';
    await T.sleep(6500);
    const hidden = size();
    p.container.style.display = '';
    await frames(3);
    const out = { drawn, hidden, diff: T.diff(before, T.read(p.canvas)) };
    p.dispose();
    return out;
  }, [TEXT, FRAMES]);
  assert.notEqual(r.drawn, '1x1');
  assert.deepEqual(r, { drawn: r.drawn, hidden: '1x1', diff: 0 });
});

test('a pane with display: none has no canvas pixels, and has its last frame back before the page paints it again', async () => {
  const r = await inPage(async (text) => {
    const frames = (n) => new Promise((resolve) => (n > 0 ? requestAnimationFrame(() => frames(n - 1).then(resolve)) : resolve()));
    const p = new T.Pane(30, 4);
    await p.write(text);
    const before = await p.pixels();
    const { width, height } = p.canvas;
    p.container.style.display = 'none';
    await frames(3);
    const hidden = [p.debug.collapsed, p.canvas.width, p.canvas.height];
    // Resize observers run in the order they were made, the pane's first, and all before the paint
    let restored = null;
    const observer = new ResizeObserver(() => {
      if (p.term.element.clientWidth > 0 && restored === null) restored = T.read(p.canvas);
    });
    observer.observe(p.term.element.querySelector('.xterm-screen'));
    await frames(1);
    p.container.style.display = '';
    await frames(3);
    observer.disconnect();
    const after = await p.pixels();
    const out = {
      hidden,
      restoredSame: restored && T.diff(before, restored),
      afterSame: T.diff(before, after),
      shown: [p.debug.collapsed, p.canvas.width === width, p.canvas.height === height],
    };
    p.dispose();
    return out;
  }, TEXT);
  assert.deepEqual(r, { hidden: [true, 0, 0], restoredSame: 0, afterSame: 0, shown: [false, true, true] });
});

test('a frame makes one WebGL draw call per layer that has something to draw', async () => {
  const r = await inPage(async (text) => {
    const p = new T.Pane(30, 4, { theme: { background: '#101010', cursor: '#ff0000' } });
    await p.write(text);
    await p.pixels();
    let calls = 0;
    const proto = WebGL2RenderingContext.prototype;
    const original = proto.drawElementsInstanced;
    proto.drawElementsInstanced = function (...args) {
      calls++;
      return original.apply(this, args);
    };
    const count = async () => {
      calls = 0;
      p.term.options.drawBoldTextInBrightColors = false;
      p.term.options.drawBoldTextInBrightColors = true;
      await p.pixels();
      return calls;
    };
    // Backgrounds, glyphs and the cursor; no highlight or link overlay
    const out = { cursor: await count() };
    await p.write('\x1b[?25l');
    out.hidden = await count();
    proto.drawElementsInstanced = original;
    p.dispose();
    return out;
  }, TEXT);
  assert.deepEqual(r, { cursor: 3, hidden: 2 });
});
