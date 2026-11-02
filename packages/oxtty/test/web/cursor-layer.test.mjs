// The blinking cursor's layer: the pane's canvas is drawn with the cursor hidden, the layer over it with the
// cursor shown, and the blink only flips the layer's opacity (no WebGL draw while idle).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

async function inPage(fn, arg, options) {
  return withPage('renderer.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    return page.evaluate(fn, arg);
  }, options);
}

const THEME = { theme: { background: '#101010', foreground: '#c0c0c0', cursor: '#ff0000', cursorAccent: '#0000ff' } };

test('both blink phases show the pixels a steady cursor and a hidden one draw, for every cursor style', async () => {
  const r = await inPage(async (theme) => {
    const text = '\x1b[31mred\x1b[0m \x1b[42mgreen\x1b[0m 한글 \x1b[3mitalic\x1b[0m\r\n\x1b[1mbold\x1b[0m xyz WM';
    // Cursor on a narrow glyph, on a wide one, past the text; DECSCUSR blinking and steady shapes
    const cases = [
      { style: 'block', move: '\x1b[4D' },
      { style: 'block', move: '\x1b[1;11H' },
      { style: 'block', move: '' },
      { style: 'underline', move: '\x1b[4D' },
      { style: 'bar', move: '\x1b[4D', cursorWidth: 3 },
      { style: 'bar', move: '\x1b[2;1H', cursorWidth: 3 },
      { style: 'block', move: '\x1b[4D', unfocused: true },
      { style: 'block', move: '\x1b[4D', program: ['\x1b[5 q', '\x1b[6 q'] },
      { style: 'block', move: '\x1b[4D', program: ['\x1b[3 q', '\x1b[4 q'] },
    ];
    const out = [];
    for (const c of cases) {
      const options = { ...theme, cursorStyle: c.style, cursorWidth: c.cursorWidth ?? 1 };
      const blinking = new T.Pane(30, 3, { ...options, cursorBlink: !c.program });
      const steady = new T.Pane(30, 3, options);
      for (const [p, mode] of [[blinking, c.program?.[0]], [steady, c.program?.[1]]]) {
        await p.write(text + c.move + (mode ?? ''));
        p.focus(!c.unfocused);
      }
      const a = await blinking.layered();
      const shown = (await steady.layered()).canvas;
      await steady.write('\x1b[?25l');
      const hidden = (await steady.layered()).canvas;
      out.push({
        layer: a.layer !== null,
        cursorDrawn: T.diff(shown, hidden) > 0,
        shownPhase: T.diff(a.shown, shown),
        hiddenPhase: T.diff(a.canvas, c.unfocused ? shown : hidden),
      });
      blinking.dispose();
      steady.dispose();
    }
    return out;
  }, THEME);
  for (const [i, c] of r.entries()) {
    const unfocused = i === 6;
    assert.deepEqual(c, { layer: !unfocused, cursorDrawn: true, shownPhase: 0, hiddenPhase: 0 }, `case ${i}`);
  }
});

test('an idle blink draws nothing: no WebGL draw, no drawImage, no redraw request; the layer flips its opacity', async () => {
  const r = await inPage(async (theme) => {
    const p = new T.Pane(20, 3, { ...theme, cursorBlink: true });
    await p.write('abc');
    p.focus(true);
    await p.layered();
    // Past the one redraw that compacts the canvas of a pane that stopped drawing
    await T.sleep(1200);
    const layer = p.debug.cursorLayer;
    const counts = { gl: 0, drawImage: 0 };
    const wrap = (proto, names, key) => {
      for (const name of names) {
        const original = proto[name];
        proto[name] = function (...args) {
          counts[key]++;
          return original.apply(this, args);
        };
      }
    };
    wrap(WebGL2RenderingContext.prototype, ['drawArrays', 'drawElements', 'drawArraysInstanced', 'drawElementsInstanced'], 'gl');
    wrap(CanvasRenderingContext2D.prototype, ['drawImage'], 'drawImage');
    const opacities = [layer.style.opacity];
    new MutationObserver(() => {
      if (layer.style.opacity !== opacities.at(-1)) opacities.push(layer.style.opacity);
    }).observe(layer, { attributes: true, attributeFilter: ['style'] });
    const before = p.debug;
    p.takeRequests();
    await T.sleep(1500);
    const after = p.debug;
    const out = {
      counts,
      opacities,
      draws: after.draws - before.draws,
      layerDraws: after.layerDraws - before.layerDraws,
      requests: p.takeRequests(),
      sameLayer: after.cursorLayer === layer,
    };
    p.dispose();
    return out;
  }, THEME);
  assert.deepEqual(r.counts, { gl: 0, drawImage: 0 });
  assert.equal(r.draws, 0);
  assert.equal(r.layerDraws, 0);
  assert.deepEqual(r.requests, []);
  assert.ok(r.sameLayer);
  // 600 ms phases: at least shown → hidden → shown within 1.5 s
  assert.ok(r.opacities.length >= 3, JSON.stringify(r.opacities));
  assert.ok(r.opacities.every((o, i) => i === 0 || o !== r.opacities[i - 1]) && r.opacities.includes('0'));
});

test('the layer follows cursor moves, scrolling, resizes and font changes', async () => {
  const r = await inPage(async (theme) => {
    const p = new T.Pane(20, 4, { ...theme, cursorBlink: true, scrollback: 100 });
    p.focus(true);
    const at = async () => {
      const { region, layer } = await p.layered();
      const d = p.term.dimensions.device.cell;
      const el = p.debug.cursorLayer;
      const css = p.term.dimensions.css.cell;
      return {
        layer,
        // Region origin in cells and size in cells (fractions mean it is off the grid)
        cell: region && [region[0] / d.width, region[1] / d.height, region[2] / d.width, region[3] / d.height],
        // The element sits at the region's CSS position
        placed: el ? Math.abs(parseFloat(el.style.left) - (region[0] / d.width) * css.width) < 0.01 && Math.abs(parseFloat(el.style.top) - (region[1] / d.height) * css.height) < 0.01 : null,
      };
    };
    const out = {};
    await p.write('abcdef');
    out.start = await at();
    await p.write('\r\n\r\nxy');
    out.moved = await at();
    await p.write('\x1b[1;1H');
    out.home = await at();
    // The cursor scrolled out of view: no layer; back in view: a layer again
    await p.write('\x1b[4;1H' + '\r\n'.repeat(10) + 'zz');
    out.bottom = await at();
    p.term.scrollLines(-2);
    out.scrolled = await at();
    p.term.scrollToBottom();
    out.back = await at();
    p.term.resize(10, 6);
    await p.write('\x1b[3;9H');
    out.resized = await at();
    p.term.options.fontSize = 20;
    out.font = await at();
    out.cellChanged = p.term.dimensions.css.cell.width;
    const shown = await p.layered();
    p.term.options.cursorBlink = false;
    out.steady = (await p.layered()).canvas;
    out.shownMatches = T.diff(shown.shown, out.steady) === 0;
    delete out.steady;
    out.blinkOff = (await at()).layer;
    p.dispose();
    return out;
  }, THEME);
  assert.deepEqual(r.start.cell, [5, 0, 6, 2]);
  assert.deepEqual(r.moved.cell, [1, 1, 6, 3]);
  assert.deepEqual(r.home.cell, [0, 0, 5, 2]);
  assert.deepEqual(r.bottom.cell, [1, 2, 6, 2]);
  assert.equal(r.scrolled.layer, null);
  assert.deepEqual(r.back.cell, [1, 2, 6, 2]);
  assert.deepEqual(r.resized.cell, [7, 1, 3, 3]);
  assert.deepEqual(r.font.cell, [7, 1, 3, 3]);
  for (const k of ['start', 'moved', 'home', 'bottom', 'back', 'resized', 'font']) assert.equal(r[k].placed, true, k);
  assert.ok(r.shownMatches, 'the shown phase after a font change is the steady cursor');
  assert.equal(r.blinkOff, null);
});

test('a devicePixelRatio change redraws the layer at the new cell size', async () => {
  const r = await withPage('renderer.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    await page.evaluate(async (theme) => {
      window.p = new T.Pane(10, 3, { ...theme, cursorBlink: true });
      p.focus(true);
      await p.write('dpr\r\nab');
      window.before = (await p.layered()).region;
      window.changes = 0;
      p.term.onDimensionsChange(() => changes++);
    }, THEME);
    const cdp = await page.context().newCDPSession(page);
    await cdp.send('Emulation.setDeviceMetricsOverride', { width: 1000, height: 700, deviceScaleFactor: 2, mobile: false });
    await page.evaluate(() => window.dispatchEvent(new Event('resize')));
    await page.waitForFunction(() => changes > 0, null, { timeout: 5000 });
    return page.evaluate(async () => {
      const a = await p.layered();
      const steady = new T.Pane(10, 3, { theme: p.term.options.theme });
      steady.focus(true);
      await steady.write('dpr\r\nab');
      const shown = (await steady.layered()).canvas;
      const d = p.term.dimensions.device.cell;
      return { before, after: a.region, cell: [d.width, d.height], same: T.diff(a.shown, shown) };
    });
  });
  assert.deepEqual(r.after, [r.cell[0], 0, 6 * r.cell[0], 3 * r.cell[1]]);
  assert.ok(Math.abs(r.after[0] - 2 * r.before[0]) <= 2, `${r.before} → ${r.after}`);
  assert.equal(r.same, 0);
});

test('blur, hiding and dispose remove the layer; a translucent background keeps the old redraw', async () => {
  const r = await inPage(async (theme) => {
    const p = new T.Pane(10, 2, { ...theme, cursorBlink: true });
    await p.write('x');
    p.focus(true);
    await p.layered();
    const out = {};
    const first = p.debug.cursorLayer;
    out.inScreen = first.parentElement === p.debug.canvas.parentElement && first.previousElementSibling === p.debug.canvas;
    out.size = [first.width, first.height];
    p.focus(false);
    await p.layered();
    out.blurred = [p.debug.cursorLayer, first.isConnected];
    p.focus(true);
    await p.layered();
    const second = p.debug.cursorLayer;
    p.container.style.display = 'none';
    for (let n = 0; n < 50 && second.isConnected; n++) await T.sleep(20);
    out.hidden = second.isConnected;
    p.container.style.display = '';
    // The canvas has its pixels back once layout ran (a hidden pane has none), after this frame's callbacks
    await new Promise((resolve) => requestAnimationFrame(resolve));
    for (let n = 0; n < 50 && !p.debug.cursorLayer; n++) await p.layered();
    const third = p.debug.cursorLayer;
    out.shownAgain = !!third;
    p.dispose();
    out.disposed = third.isConnected;
    // Translucent background: no layer, the blink redraws the cursor row as before
    const t = new T.Pane(10, 2, { theme: { ...theme.theme, background: '#10101080' }, cursorBlink: true, allowTransparency: true });
    await t.write('y');
    t.focus(true);
    await t.layered();
    out.translucent = t.debug.cursorLayer;
    t.takeRequests();
    await T.sleep(1300);
    out.rowRequests = t.takeRequests().filter((x) => x !== 'all').length;
    t.dispose();
    return out;
  }, THEME);
  assert.equal(r.inScreen, true);
  assert.ok(r.size[0] > 0 && r.size[0] <= 6 * 20 && r.size[1] > 0 && r.size[1] <= 2 * 40, `${r.size}`);
  assert.deepEqual(r.blurred, [null, false]);
  assert.equal(r.hidden, false);
  assert.equal(r.shownAgain, true);
  assert.equal(r.disposed, false);
  assert.equal(r.translucent, null);
  assert.ok(r.rowRequests >= 1);
});
