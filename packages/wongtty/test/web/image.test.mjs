// Inline images (sixel, iTerm2 OSC 1337) against real xterm.js 6.1 with @xterm/addon-image 0.10.0-beta.331 and
// @xterm/addon-webgl: the same cells covered, the same cursor, the same pixels of the image layer, and the storage,
// eviction and memory behaviour.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

/** Runs `fn` in the image page; `fn` sees the helpers as `window.api`. */
async function inPage(fn, arg, options) {
  return withPage('image.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    return page.evaluate(fn, arg);
  }, options);
}

/** Same cursor and tiles on both, and the image layer's pixels within a channel step. */
function assertParity(r, label) {
  assert.deepEqual(r.state.cursor.ox, r.state.cursor.xt, `${label}: cursor`);
  assert.deepEqual(r.state.tiles.ox, r.state.tiles.xt, `${label}: tiles`);
  // An image placed at a pending wrap has no column left to draw in
  if (Object.keys(r.state.tiles.xt).length) assert.ok(r.pixels.compared > 0, `${label}: xterm.js drew the image`);
  assert.equal(r.pixels.off, 0, `${label}: pixels (largest difference ${r.pixels.max})`);
}

test('sixel images land on the same cells with the same cursor and pixels as @xterm/addon-image', async () => {
  const results = await inPage(async () => {
    const out = [];
    const run = async (label, body, at, options = {}, addon = {}) => {
      await api.setup(options, addon);
      await api.image('sixel', body, at);
      out.push({ label, state: api.state(), pixels: await api.compare() });
    };
    const halves = (x) => (x < 10 ? [100, 0, 100] : [0, 80, 20]);
    const stripes = (x, y) => (y % 3 ? [20, 40, 100] : [100, 90, 0]);
    await run('mid screen, text after', api.sixel(20, 30, halves), { prefix: 'ab\r\n\x1b[2;3H', suffix: 'Z' });
    await run('cut at the right edge', api.sixel(60, 20, stripes), { prefix: '\x1b[1;27H', suffix: 'Q' });
    await run('scrolls at the bottom', api.sixel(17, 70, stripes), { prefix: '\x1b[7;5H', suffix: '\r\nnext' });
    await run('pending wrap', api.sixel(16, 16, halves), { prefix: 'x'.repeat(30), suffix: 'Q' });
    await run('sixel scrolling off (DECSET 80)', api.sixel(30, 40, halves), { prefix: '\x1b[5;9H', suffix: 'Y' }, {}, { sixelScrolling: false });
    await run('transparent, odd sizes', api.sixel(13, 23, (x, y) => ((x + y) % 4 ? [0, 100, 100] : null)), { params: '0;1', prefix: '\x1b[3;3H' });
    await run('pen background fills', api.sixel(12, 12, (x, y) => (x > y ? [100, 100, 0] : null)), { prefix: '\x1b[44m', suffix: '\x1b[0m' });
    return out;
  });
  for (const r of results) assertParity(r, r.label);
});

test('iTerm2 PNG images are sized, placed and drawn like @xterm/addon-image', async () => {
  const results = await inPage(async () => {
    const out = [];
    const draw = (w, h) => (ctx) => {
      ctx.fillStyle = '#e03080';
      ctx.fillRect(0, 0, w, h);
      ctx.fillStyle = '#20c0f0';
      ctx.fillRect(w / 4, h / 4, w / 2, h / 2);
    };
    const run = async (label, payload, at, options = {}) => {
      await api.setup(options);
      await api.image('iip', payload, at);
      out.push({ label, state: api.state(), pixels: await api.compare() });
    };
    await run('natural size', await api.png(40, 34, draw(40, 34)), { prefix: 'ab\r\n\x1b[2;3H', suffix: 'Z' });
    await run('width in cells', await api.png(64, 48, draw(64, 48), ';width=5'), { prefix: '\x1b[1;4H' });
    await run('height in pixels, no aspect', await api.png(64, 48, draw(64, 48), ';width=50%;height=20px;preserveAspectRatio=0'), {});
    await run('taller than the screen is scaled down', await api.png(50, 400, draw(50, 400)), { prefix: '\x1b[3;1H' });
    await run('cursor right of an image at the edge', await api.png(80, 20, draw(80, 20)), { prefix: '\x1b[2;24H', suffix: 'W' });
    return out;
  });
  for (const r of results) assertParity(r, r.label);
});

test('text and images over tiles, erasing and scrolling remove and move tiles like the addon; trimmed images free', async () => {
  const r = await inPage(async () => {
    const steps = [];
    const snap = async (label) => steps.push({ label, state: api.state(), pixels: await api.compare() });
    await api.setup({ rows: 6, scrollback: 4 });
    const big = api.sixel(40, 40, (x, y) => (x < 20 ? [100, 10, 10] : [10, 10, 100]));
    await api.image('sixel', big, { prefix: '\x1b[2;2H' });
    await snap('placed');
    await api.writeBoth('\x1b[3;3HTXT\x1b[2;4H\x1b[1X');
    await snap('text and ECH over tiles');
    await api.image('sixel', api.sixel(12, 12, () => [0, 100, 0]), { prefix: '\x1b[3;4H' });
    await snap('a second image over the first');
    await api.writeBoth('\x1b[6;1H\r\n\r\n');
    await snap('scrolled two lines');
    await api.writeBoth('\x1b[2J');
    await snap('erased in display');
    await api.image('sixel', big, { prefix: '\x1b[1;1H' });
    await api.writeBoth('\r\n'.repeat(14));
    const gone = api.state();
    return { steps, gone };
  });
  for (const step of r.steps) {
    assert.deepEqual(step.state.cursor.ox, step.state.cursor.xt, `${step.label}: cursor`);
    assert.deepEqual(step.state.tiles.ox, step.state.tiles.xt, `${step.label}: tiles`);
    assert.equal(step.pixels.off, 0, `${step.label}: pixels (largest difference ${step.pixels.max})`);
  }
  assert.deepEqual(r.steps[1].state.usage.ox, r.steps[1].state.usage.xt, 'covered text keeps the image stored');
  // Trimmed off the scrollback: the end marker went, so did the image
  assert.equal(r.gone.usage.xt, 0);
  assert.equal(r.gone.usage.ox, 0);
  assert.equal(r.gone.debug.pageImages, 0);
  assert.equal(r.gone.debug.textures, 0);
});

test('the alternate screen, DECSTR and RIS in the same write as an image drop it in stream order like the addon', async () => {
  const r = await inPage(async () => {
    await api.setup();
    const image = api.sequence('sixel', api.sixel(20, 20, () => [100, 0, 0]));
    const usage = {};
    const step = async (label, data) => {
      await api.writeBoth(data);
      const state = api.state();
      usage[label] = { ...state.usage, pixels: state.debug.pixels };
    };
    await step('placed on the alternate screen, which is left', `\x1b[?1049h${image}\x1b[?1049l`);
    await step('placed after entering it', `\x1b[?1049h\x1b[H${image}`);
    await step('left again', '\x1b[?1049l');
    await step('placed, then DECSTR', `${image}\x1b[!p`);
    await step('placed, then RIS', `${image}\x1bc`);
    await step('RIS, then placed', `\x1bc${image}`);
    return { usage, state: api.state() };
  });
  for (const [label, u] of Object.entries(r.usage)) assert.equal(u.ox, u.xt, label);
  assert.equal(r.usage['placed after entering it'].pixels, 400);
  assert.equal(r.usage['left again'].pixels, 0);
  assert.equal(r.usage['RIS, then placed'].pixels, 400);
  assert.deepEqual(r.state.tiles.ox, r.state.tiles.xt);
});

test('storage limit evicts the oldest image first; its tiles show the placeholder like the addon', async () => {
  const r = await inPage(async () => {
    await api.setup({ cols: 60, rows: 30 }, { storageLimit: 0.5 });
    // 48000 pixels each: the third goes over the 125000 pixels of 0.5 MB
    const image = (c) => api.sixel(240, 200, () => c);
    const usage = [];
    await api.image('sixel', image([100, 0, 0]), { prefix: '\x1b[1;1H' });
    usage.push(api.state().usage);
    await api.image('sixel', image([0, 100, 0]), { prefix: '\x1b[1;31H' });
    usage.push(api.state().usage);
    await api.image('sixel', image([0, 0, 100]), { prefix: '\x1b[15;1H' });
    usage.push(api.state().usage);
    const state = api.state();
    const pixels = await api.compare();
    api.pair.oxAddon.showPlaceholder = false;
    api.pair.xtAddon.showPlaceholder = false;
    const hidden = await api.compare();
    return { usage, state, pixels, hidden, limit: state.limit };
  });
  assert.deepEqual(r.limit.ox, r.limit.xt);
  for (const [i, u] of r.usage.entries()) assert.equal(u.ox, u.xt, `usage after image ${i + 1}`);
  assert.equal(r.usage[2].ox, r.usage[1].ox, 'the oldest image was evicted');
  assert.deepEqual(r.state.tiles.ox, r.state.tiles.xt);
  assert.equal(r.pixels.off, 0, `placeholder and images (largest difference ${r.pixels.max})`);
  assert.equal(r.hidden.off, 0, 'without the placeholder');
  assert.ok(r.pixels.compared > r.hidden.compared, 'the placeholder was drawn');
});

test('DA1, XTSMGRAPHICS, the size reports and ReportCellSize answer like the addon', async () => {
  const r = await inPage(async () => {
    await api.setup({}, { pixelLimit: 1000000 });
    const out = {
      da1: await api.csi('\x1b[c'),
      colors: await api.csi('\x1b[?1;1S'),
      setColors: await api.csi('\x1b[?1;3;256S'),
      tooMany: await api.csi('\x1b[?1;3;5000S'),
      maxColors: await api.csi('\x1b[?1;4S'),
      geometry: await api.csi('\x1b[?2;1S'),
      maxGeometry: await api.csi('\x1b[?2;4S'),
      regis: await api.csi('\x1b[?3;1S'),
      winPixels: await api.csi('\x1b[14t'),
      cellPixels: await api.csi('\x1b[16t'),
      winChars: await api.csi('\x1b[18t'),
      windowOptions: [api.pair.ox.options.windowOptions, api.pair.xt.options.windowOptions],
    };
    const replies = { ox: [], xt: [] };
    api.pair.ox.onData((d) => replies.ox.push(d));
    api.pair.xt.onData((d) => replies.xt.push(d));
    await api.image('iip', 'ReportCellSize', {});
    out.cellSize = { ox: replies.ox.join(''), xt: replies.xt.join('') };
    return out;
  });
  for (const [key, value] of Object.entries(r)) {
    if (key === 'windowOptions') assert.deepEqual(value[0], value[1], key);
    else assert.equal(value.ox, value.xt, key);
  }
  assert.equal(r.da1.ox, '\x1b[?62;4;9;22c');
});

test('an @xterm/addon-image instance is absorbed, and wmux reaches its sixel handler through _handlers', async () => {
  const r = await inPage(async () => {
    await api.setup({}, { pixelLimit: 2 ** 23, storageLimit: 64, enableSizeReports: true }, 'xterm');
    const { oxAddon, ox } = api.pair;
    // src/shared/terminal/sixelCap.ts of wmux, as it wraps the handler
    const capSixelImageSize = (addon, pixelLimit) => {
      const handler = addon._handlers?.get('sixel');
      if (!handler || typeof handler.unhook !== 'function') return false;
      const unhook = handler.unhook;
      handler.unhook = (success) => {
        if (success && !handler._aborted) {
          const dec = handler._dec;
          if (!dec || !Number.isFinite(dec.width) || !Number.isFinite(dec.height) || dec.width * dec.height > pixelLimit) {
            handler._aborted = true;
            dec?.release();
            return true;
          }
        }
        return unhook.call(handler, success);
      };
      return true;
    };
    const capped = capSixelImageSize(oxAddon, 200);
    const keys = [...oxAddon._handlers.keys()];
    const before = ox._imageDebug().pageImages;
    await new Promise((r) => ox.write(api.sequence('sixel', api.sixel(30, 30, () => [100, 0, 0])), r));
    const tooLarge = ox._imageDebug().pageImages;
    await new Promise((r) => ox.write(api.sequence('sixel', api.sixel(10, 10, () => [100, 0, 0])), r));
    const small = ox._imageDebug().pageImages;
    const limit = oxAddon.storageLimit;
    let threw = false;
    try {
      oxAddon.storageLimit = 0.1;
    } catch (e) {
      threw = e instanceof RangeError;
    }
    const added = [];
    oxAddon.onImageAdded(() => added.push(1));
    await new Promise((r) => ox.write(api.sequence('sixel', api.sixel(8, 8, () => [0, 0, 100])), r));
    const windowOptions = ox.options.windowOptions;
    oxAddon.dispose();
    return {
      capped, keys, before, tooLarge, small, limit, threw, added: added.length, windowOptions,
      afterDispose: ox._imageDebug(), handlersAfter: oxAddon._handlers.size,
    };
  });
  assert.equal(r.capped, true);
  assert.deepEqual(r.keys, ['sixel', 'iip']);
  assert.equal(r.tooLarge, r.before, 'the cap dropped the image over its pixel limit');
  assert.equal(r.small, r.before + 1, 'a smaller image went through the original unhook');
  assert.equal(r.limit, 64);
  assert.ok(r.threw, 'storageLimit out of range throws');
  assert.equal(r.added, 1);
  assert.deepEqual(r.windowOptions, { getWinSizePixels: true, getCellSizePixels: true, getWinSizeChars: true });
  assert.equal(r.afterDispose.loaded, false);
  assert.equal(r.afterDispose.pixels, 0);
  assert.equal(r.handlersAfter, 0);
});

test('memory: 17 terminals, images in three; the others hold no image state, pixels stay out of the wasm memory', async () => {
  const r = await withPage('image.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    const session = await page.context().newCDPSession(page);
    // JS heap plus ArrayBuffer backing stores (where the pixels are), after a GC
    const memory = async () => {
      await session.send('HeapProfiler.collectGarbage');
      const usage = await session.send('Runtime.getHeapUsage');
      const wasm = await page.evaluate(async () => (await window.frontEnd()).memory.buffer.byteLength);
      return { heap: usage.usedSize, buffers: usage.backingStorageSize ?? 0, wasm };
    };
    await page.evaluate(async () => {
      const { Terminal, ImageAddon } = await import('wongtty/xterm');
      document.body.replaceChildren();
      window.terms = [];
      for (let i = 0; i < 17; i++) {
        const host = document.body.appendChild(document.createElement('div'));
        host.style.cssText = 'display: inline-block; width: 300px; height: 160px; overflow: hidden';
        const term = new Terminal({ cols: 30, rows: 8, scrollback: 50, fontFamily: 'Menlo, monospace', fontSize: 12 });
        term.open(host);
        if (i % 6 === 0) term.loadAddon(new ImageAddon());
        await new Promise((resolve) => term.write('line\r\n'.repeat(20), resolve));
        window.terms.push(term);
      }
      await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
    });
    const base = await memory();
    const placed = await page.evaluate(async () => {
      const body = api.sequence('sixel', api.sixel(1000, 1000, (x, y) => [((x >> 5) % 2) * 100, ((y >> 5) % 2) * 100, 50]));
      for (const term of window.terms.filter((_, i) => i % 6 === 0)) await new Promise((resolve) => term.write(body, resolve));
      await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      return window.terms.map((t) => t._imageDebug());
    });
    const withImages = await memory();
    const freed = await page.evaluate(async () => {
      for (const term of window.terms) await new Promise((resolve) => term.write('\r\n'.repeat(80), resolve));
      await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      return window.terms[0]._imageDebug();
    });
    const after = await memory();
    return { placed, base, withImages, after, freed };
  }, { viewport: { width: 2000, height: 1000 } });
  const loaded = r.placed.filter((d) => d.loaded).length;
  assert.equal(loaded, 3, 'only the terminals that loaded the addon have image state');
  assert.equal(r.placed[0].pageImages, 3);
  assert.equal(r.placed[0].pagePixels, 3 * 1000 * 1000);
  assert.ok(r.placed[0].textures <= 3, 'a texture per image on screen');
  assert.equal(r.freed.pageImages, 0, 'images trimmed off the scrollback are gone');
  assert.equal(r.freed.textures, 0);
  const mb = (b) => (b / 1048576).toFixed(1);
  const line = (m) => `heap ${mb(m.heap)} MB, buffers ${mb(m.buffers)} MB, front-end wasm ${mb(m.wasm)} MB`;
  console.log(`without images: ${line(r.base)}; 3 × 1000×1000: ${line(r.withImages)}; scrolled away: ${line(r.after)}`);
  const pixels = 3 * 1000 * 1000 * 4;
  assert.ok(r.withImages.buffers - r.base.buffers > pixels * 0.9, 'the pixels are in ArrayBuffers');
  assert.ok(r.after.buffers - r.base.buffers < pixels * 0.1, 'and leave with the images');
  assert.ok(r.withImages.wasm - r.base.wasm < 1 << 20, 'decoding grew the front-end wasm memory by less than 1 MB');
});
