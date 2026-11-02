// The WebGL2 renderer through the Terminal API in headless Chrome (ported from the TypeScript front end's
// renderer.test.mjs at 0510a7d).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

/** Runs `fn` in the renderer page; `fn` sees the helpers as `window.T`. */
async function inPage(fn, arg, options) {
  return withPage('renderer.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    return page.evaluate(fn, arg);
  }, options);
}

test('draws cell backgrounds, text and the theme background', async () => {
  const r = await inPage(async () => {
    const p = new T.Pane(10, 3, { theme: { background: '#102030' } });
    await p.write('\x1b[41m  \x1b[0m\x1b[42mAB\x1b[0m\x1b[?25l');
    const d = p.term.dimensions;
    const out = {
      red: await p.cellColor(0, 0),
      green: await p.cellColor(2, 0, 0.05, 0.05),
      empty: await p.cellColor(5, 1),
      canvas: [p.canvas.width, p.canvas.height],
      cell: d.device.cell,
      screen: [p.term.element.querySelector('.xterm-screen').style.width, d.css.canvas.width],
      lit: (await p.pixels()).some((v, i) => i % 4 === 0 && v > 0xa0),
    };
    p.dispose();
    return out;
  });
  assert.deepEqual(r.red, [0xcc, 0, 0]);
  assert.deepEqual(r.green, [0x4e, 0x9a, 0x06]);
  assert.deepEqual(r.empty, [0x10, 0x20, 0x30]);
  assert.deepEqual(r.canvas, [r.cell.width * 10, r.cell.height * 3]);
  assert.equal(r.screen[0], `${r.screen[1]}px`);
  assert.ok(r.lit, 'glyphs are drawn');
});

test('font, line height and letter spacing options re-measure; a theme change redraws and answers OSC 11', async () => {
  const r = await inPage(async () => {
    const p = new T.Pane(10, 3);
    await p.write('\x1b[41m \x1b[0m\x1b[?25l');
    const changes = [];
    p.term.onDimensionsChange((d) => changes.push(d.css.cell.width));
    const dims = () => JSON.parse(JSON.stringify(p.term.dimensions));
    const base = dims();
    p.takeRequests();
    p.term.options.fontSize = 28;
    const requests = p.takeRequests();
    const big = { ...dims(), canvas: [p.canvas.width, p.canvas.height], requests };
    p.term.options = { fontSize: 14, lineHeight: 1.5 };
    const tall = dims();
    p.term.options = { lineHeight: 1, letterSpacing: 2 };
    const spaced = dims();
    const replies = [];
    p.term.onData((d) => replies.push(d));
    p.term.options = { letterSpacing: 0, theme: { background: '#203040', red: '#00ff00' } };
    await p.write('\x1b]11;?\x07');
    const out = { base, big, tall, spaced, changes, replies, red: await p.cellColor(0, 0), empty: await p.cellColor(5, 1), fontSize: p.term.options.fontSize };
    p.dispose();
    return out;
  });
  const { base, big, tall, spaced } = r;
  assert.ok(Math.abs(big.device.char.width - 2 * base.device.char.width) <= 1, 'char width follows the font size');
  assert.deepEqual(big.canvas, [big.device.cell.width * 10, big.device.cell.height * 3]);
  assert.ok(big.requests.includes('all'));
  assert.equal(tall.device.cell.height, Math.floor(base.device.char.height * 1.5));
  assert.equal(tall.device.char.top, Math.round((tall.device.cell.height - tall.device.char.height) / 2));
  assert.equal(spaced.device.cell.width, base.device.char.width + 2);
  assert.equal(spaced.device.char.left, 1);
  assert.equal(r.changes.length, 6, 'onDimensionsChange fires for each cell size change');
  assert.deepEqual(r.red, [0, 0xff, 0]);
  assert.deepEqual(r.empty, [0x20, 0x30, 0x40]);
  assert.equal(r.fontSize, 14);
  assert.match(r.replies.join(''), /2020\/3030\/4040|20\/30\/40/i);
});

test('terminals with the same options share an atlas; it is disposed once no terminal holds it', async () => {
  const r = await inPage(async () => {
    const live = () => new T.Pane(1, 1).debug.liveAtlases;
    const before = (() => {
      const p = new T.Pane(1, 1, { fontSize: 9 });
      const n = p.debug.liveAtlases;
      p.dispose();
      return n - 1;
    })();
    const a = new T.Pane(10, 2);
    const b = new T.Pane(10, 2);
    await a.write('a');
    await b.write('b');
    const shared = a.debug.atlas === b.debug.atlas;
    const counts = [a.debug.liveAtlases - before];
    a.term.options.fontSize = 20;
    const second = a.debug.atlas;
    counts.push(a.debug.liveAtlases - before);
    a.term.options.fontSize = 22;
    counts.push(a.debug.liveAtlases - before);
    const replaced = a.debug.atlas !== second;
    a.dispose();
    counts.push(b.debug.liveAtlases - before);
    b.dispose();
    const probe = new T.Pane(10, 2, { fontSize: 9 });
    counts.push(probe.debug.liveAtlases - before - 1);
    probe.dispose();
    void live;
    return { shared, counts, replaced };
  });
  assert.deepEqual(r, { shared: true, counts: [1, 2, 2, 1, 0], replaced: true });
});

const CURSOR_THEME = { theme: { background: '#000000', cursor: '#ff0000', cursorAccent: '#000000' } };

test('cursor: DECSCUSR shape over the cursorStyle default, cursorInactiveStyle when unfocused', async () => {
  const r = await inPage(async (theme) => {
    const red = (c) => c[0] === 0xff && c[1] === 0 && c[2] === 0;
    const p = new T.Pane(10, 2, theme);
    // Where red shows in the cursor cell: centre, left edge, bottom edge, top edge
    const probe = async () => {
      const points = [[0.5, 0.5], [0.02, 0.5], [0.5, 0.97], [0.5, 0.02]];
      let out = '';
      for (const [dx, dy] of points) out += red(await p.cellColor(2, 0, dx, dy)) ? 1 : 0;
      return out;
    };
    p.focus(true);
    await p.write('ab');
    const out = { block: await probe() };
    await p.write('\x1b[6 q');
    out.bar = await probe();
    await p.write('\x1b[4 q');
    out.underline = await probe();
    p.term.options.cursorStyle = 'bar';
    // A steady block the program asked for beats the option; `CSI 0 SP q` hands the cursor back to it
    await p.write('\x1b[2 q');
    out.steadyBlock = await probe();
    await p.write('\x1b[0 q');
    out.optionBar = await probe();
    p.focus(false);
    out.outline = await probe();
    p.term.options.cursorInactiveStyle = 'none';
    out.none = await probe();
    await p.write('\x1b[?25l');
    p.focus(true);
    out.hidden = await probe();
    p.dispose();
    return out;
  }, CURSOR_THEME);
  assert.deepEqual(r, { block: '1111', bar: '0100', underline: '0010', steadyBlock: '1111', optionBar: '0100', outline: '0111', none: '0000', hidden: '0000' });
});

test('cursor blink is timed by the renderer and flips the cursor layer', async () => {
  const r = await inPage(async (theme) => {
    const p = new T.Pane(10, 3, { ...theme, cursorBlink: true });
    await p.write('\r\nab');
    // Blinks at 600 ms intervals from the focus: watch for hidden then shown again, with room for a loaded machine
    p.focus(true);
    await p.layered();
    const t0 = performance.now();
    const shown = () => p.debug.cursorLayer?.style.opacity === '1';
    p.takeRequests();
    const seen = [shown()];
    while (seen.length < 3 && performance.now() - t0 < 5000) {
      await T.sleep(50);
      const s = shown();
      if (s !== seen[seen.length - 1]) seen.push(s);
    }
    const requests = p.takeRequests().filter((x) => x !== 'all');
    // Unfocused, the cursor stops blinking and stays shown in the canvas
    p.focus(false);
    await p.layered();
    p.takeRequests();
    await T.sleep(700);
    const unfocused = p.takeRequests().filter((x) => x !== 'all').length;
    const layer = p.debug.cursorLayer;
    p.dispose();
    return { seen, requests, unfocused, layer };
  }, CURSOR_THEME);
  assert.deepEqual(r.seen, [true, false, true]);
  assert.deepEqual(r.requests, []);
  assert.equal(r.unfocused, 0);
  assert.equal(r.layer, null);
});

test('selection colours the selected cells and redraws only the affected rows', async () => {
  const r = await inPage(async () => {
    const p = new T.Pane(10, 4, { theme: { background: '#000000', selectionBackground: '#ff000080', selectionInactiveBackground: '#0000ff80' } });
    p.focus(true);
    await p.write('abcdef\r\nghijkl\r\nmnopqr\x1b[?25l');
    // Cell top-left corners (away from glyphs): 1 when selection-coloured
    const map = async () => {
      const rows = [];
      for (const y of [0, 1, 2, 3]) {
        let row = '';
        for (const x of [0, 1, 2, 3, 4, 5, 6, 7]) {
          const c = await p.cellColor(x, y, 0.05, 0.05);
          row += c[0] > 0x30 || c[2] > 0x30 ? 1 : 0;
        }
        rows.push(row);
      }
      return rows.join('|');
    };
    const out = {};
    p.takeRequests();
    p.term._setRendererSelection({ start: [2, 0], end: [3, 1], columnSelect: false });
    out.requests = p.takeRequests();
    out.linear = await map();
    p.term._setRendererSelection({ start: [1, 1], end: [4, 3], columnSelect: true });
    out.column = await map();
    p.focus(false);
    out.inactiveBlue = (await p.cellColor(2, 2, 0.05, 0.05))[2] > 0x30;
    p.takeRequests();
    p.term._setRendererSelection(undefined);
    out.clearRequests = p.takeRequests();
    out.cleared = await map();
    p.dispose();
    return out;
  });
  assert.deepEqual(r.requests, [[0, 1]]);
  assert.equal(r.linear, '00111111|11100000|00000000|00000000');
  assert.equal(r.column, '00000000|01110000|01110000|01110000');
  assert.equal(r.inactiveBlue, true);
  assert.equal(r.cleared, '00000000|00000000|00000000|00000000');
  assert.deepEqual(r.clearRequests, [[1, 2, 3]]);
});

test('highlights paint background, foreground, border and underline under the selection', async () => {
  const r = await inPage(async () => {
    const p = new T.Pane(10, 3, { theme: { background: '#000000', foreground: '#808080', selectionBackground: '#0000ff' } });
    await p.write('\x1b[?25lXXXXXXXXXX\r\nabcdefghij');
    p.takeRequests();
    p.term._setHighlights('search', [
      { row: 0, start: 1, end: 3, background: '#00ff00' },
      { row: 1, start: 4, end: 6, border: '#ff0000' },
    ]);
    const requests = p.takeRequests();
    p.term._setHighlights('link', [{ row: 0, start: 6, end: 8, foreground: '#ffff00', underline: true }]);
    const out = {
      requests,
      bg: await p.cellColor(1, 0, 0.05, 0.05),
      plain: await p.cellColor(0, 0, 0.05, 0.05),
      borderTop: await p.cellColor(4, 1, 0.5, 0),
      borderLeft: await p.cellColor(4, 1, 0, 0.5),
      inside: await p.cellColor(4, 1, 0.3, 0.3),
      underline: await p.cellColor(6, 0, 0.5, 0.99),
    };
    // The yellow foreground: some pixel of cell 6 is yellowish
    const data = await p.pixels();
    const d = p.term.dimensions.device;
    let yellow = 0;
    for (let y = 0; y < d.cell.height - 2; y++) {
      for (let x = 6 * d.cell.width; x < 7 * d.cell.width; x++) {
        const i = (y * p.canvas.width + x) * 4;
        if (data[i] > 0x80 && data[i + 1] > 0x80 && data[i + 2] < 0x40) yellow++;
      }
    }
    out.yellow = yellow;
    // The selection draws over the highlight background
    p.term._setRendererSelection({ start: [0, 0], end: [5, 0], columnSelect: false });
    p.focus(true);
    out.selected = await p.cellColor(1, 0, 0.05, 0.05);
    p.term._setHighlights('search', []);
    p.term._setRendererSelection(undefined);
    out.cleared = await p.cellColor(1, 0, 0.05, 0.05);
    p.dispose();
    return out;
  });
  assert.deepEqual(r.requests, [[0, 1]]);
  assert.deepEqual(r.bg, [0, 0xff, 0]);
  assert.deepEqual(r.plain, [0, 0, 0]);
  assert.deepEqual(r.borderTop, [0xff, 0, 0]);
  assert.deepEqual(r.borderLeft, [0xff, 0, 0]);
  assert.deepEqual(r.inside, [0, 0, 0]);
  assert.deepEqual(r.underline, [0xff, 0xff, 0]);
  assert.ok(r.yellow > 5, `yellow glyph pixels: ${r.yellow}`);
  assert.ok(r.selected[2] > 0x40 && r.selected[1] < 0x40, `selection over the highlight: ${r.selected}`);
  assert.deepEqual(r.cleared, [0, 0, 0]);
});

test('a devicePixelRatio change re-measures, resizes the canvas and fires onDimensionsChange', async () => {
  const r = await withPage('renderer.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    await page.evaluate(async () => {
      window.p = new T.Pane(10, 2);
      await p.write('dpr');
      window.changes = 0;
      p.term.onDimensionsChange(() => changes++);
      window.before = JSON.parse(JSON.stringify(p.term.dimensions));
      p.takeRequests();
    });
    const cdp = await page.context().newCDPSession(page);
    await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1000, height: 700, deviceScaleFactor: 2, mobile: false });
    // Emulation changes devicePixelRatio but sends none of the signals a real zoom does (window resize, a new device
    // pixel size for the canvas), so send the resize a zoom would
    await page.evaluate(() => window.dispatchEvent(new Event('resize')));
    await page.waitForFunction(() => changes > 0, null, { timeout: 5000 });
    return page.evaluate(() => ({ changes, before, after: p.term.dimensions, canvas: [p.canvas.width, p.canvas.height], requests: p.takeRequests() }));
  });
  assert.equal(r.changes, 1);
  assert.deepEqual(r.after.css.canvas, r.before.css.canvas);
  assert.ok(Math.abs(r.after.device.char.width - 2 * r.before.device.char.width) <= 1);
  assert.deepEqual(r.canvas, [r.after.device.canvas.width, r.after.device.canvas.height]);
  assert.ok(r.requests.includes('all'));
});

test('fonts that finish loading re-measure, clear fallback glyphs and fire onDimensionsChange', async () => {
  const r = await inPage(async () => {
    const p = new T.Pane(10, 2, { fontFamily: 'WongttyTestFont, monospace' });
    await p.write('WWW');
    await p.pixels();
    const before = p.term.dimensions.css.cell.width;
    let changes = 0;
    p.term.onDimensionsChange(() => changes++);
    p.takeRequests();
    // A wide proportional font under a new name, so the cell width visibly changes once it is in
    const face = new FontFace('WongttyTestFont', 'local("Arial"), local("Helvetica"), local("DejaVu Sans"), local("Liberation Sans")');
    document.fonts.add(face);
    await face.load();
    for (let n = 0; n < 50 && !changes; n++) await T.sleep(20);
    const out = { before, after: p.term.dimensions.css.cell.width, changes, requests: p.takeRequests() };
    p.dispose();
    return out;
  });
  assert.ok(r.after > r.before, `${r.before} → ${r.after}`);
  assert.equal(r.changes, 1);
  assert.ok(r.requests.includes('all'));
});

test('dispose releases the atlas, the canvas and the blink timers; the last terminal frees the shared context', async () => {
  const r = await inPage(async () => {
    const p = new T.Pane(10, 2, { cursorBlink: true });
    await p.write('x');
    p.focus(true);
    const debug = p.debug;
    const { canvas, gl } = debug;
    const atlases = debug.liveAtlases;
    await T.sleep(100);
    let frames = 0;
    const raf = window.requestAnimationFrame;
    window.requestAnimationFrame = (cb) => (frames++, raf(cb));
    p.dispose();
    await T.sleep(1500);
    window.requestAnimationFrame = raf;
    const probe = new T.Pane(1, 1, { fontSize: 9 });
    const after = probe.debug.liveAtlases - 1;
    const newContext = probe.debug.gl !== gl;
    probe.dispose();
    return { atlasFreed: after === atlases - 1, lost: gl.isContextLost(), removed: !canvas.isConnected, frames, newContext };
  });
  assert.deepEqual(r, { atlasFreed: true, lost: true, removed: true, frames: 0, newContext: true });
});
