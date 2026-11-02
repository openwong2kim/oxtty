// The built-in FitAddon and loadAddon.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

async function inPage(fn, arg) {
  return withPage('renderer.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    return page.evaluate(fn, arg);
  });
}

test('FitAddon proposes and applies the size that fills the parent, like @xterm/addon-fit', async () => {
  const r = await inPage(async () => {
    const parent = document.createElement('div');
    parent.style.cssText = 'width: 400px; height: 200px';
    document.body.appendChild(parent);
    const term = new T.Terminal({ fontFamily: 'Menlo, monospace', fontSize: 14 });
    const fit = new T.FitAddon();
    term.loadAddon(fit);
    const beforeOpen = fit.proposeDimensions();
    term.open(parent);
    const cell = term.dimensions.css.cell;
    const proposed = fit.proposeDimensions();
    fit.fit();
    const fitted = [term.cols, term.rows];
    const noScrollback = new T.Terminal({ fontFamily: 'Menlo, monospace', fontSize: 14, scrollback: 0 });
    const fit2 = new T.FitAddon();
    noScrollback.loadAddon(fit2);
    const parent2 = document.createElement('div');
    parent2.style.cssText = 'width: 400px; height: 200px';
    document.body.appendChild(parent2);
    noScrollback.open(parent2);
    const wide = fit2.proposeDimensions();
    term.options.scrollbar = { width: 20 };
    const scrollbarWidth = fit.proposeDimensions();
    fit.dispose();
    const disposed = fit.proposeDimensions();
    term.dispose();
    noScrollback.dispose();
    return { beforeOpen, cell, proposed, fitted, wide, scrollbarWidth, disposed };
  });
  assert.equal(r.beforeOpen, undefined);
  const expected = { cols: Math.floor((400 - 14) / r.cell.width), rows: Math.floor(200 / r.cell.height) };
  assert.deepEqual(r.proposed, expected);
  assert.deepEqual(r.fitted, [expected.cols, expected.rows]);
  assert.deepEqual(r.wide, { cols: Math.floor(400 / r.cell.width), rows: expected.rows });
  assert.deepEqual(r.scrollbarWidth, { cols: Math.floor((400 - 20) / r.cell.width), rows: expected.rows }, 'options.scrollbar.width');
  assert.equal(r.disposed, undefined);
});

test('a loaded FitAddon refits by itself when fonts finish loading', async () => {
  const r = await inPage(async () => {
    const parent = document.createElement('div');
    parent.style.cssText = 'width: 600px; height: 100px';
    document.body.appendChild(parent);
    const term = new T.Terminal({ fontFamily: 'WongttyFitFont, monospace', fontSize: 14 });
    const fit = new T.FitAddon();
    term.loadAddon(fit);
    term.open(parent);
    fit.fit();
    const before = term.cols;
    const resizes = [];
    term.onResize((size) => resizes.push(size.cols));
    const face = new FontFace('WongttyFitFont', 'local("Arial"), local("Helvetica"), local("DejaVu Sans"), local("Liberation Sans")');
    document.fonts.add(face);
    await face.load();
    for (let n = 0; n < 50 && !resizes.length; n++) await T.sleep(20);
    const out = { before, after: term.cols, resizes, proposed: fit.proposeDimensions().cols };
    term.dispose();
    return out;
  });
  assert.ok(r.after < r.before, `${r.before} → ${r.after}`);
  assert.deepEqual(r.resizes, [r.after]);
  assert.equal(r.proposed, r.after);
});

test('a FitAddon subclass is activated, super works, and a refit runs its own fit (wmux FixedGeometryFitAddon)', async () => {
  const r = await inPage(async () => {
    class FixedGeometryFitAddon extends T.FitAddon {
      activate(terminal) {
        super.activate(terminal);
        this.term = terminal;
      }
      proposeDimensions() {
        return this.term?.element?.parentElement ? { cols: 30, rows: 5 } : undefined;
      }
      fit() {
        this.fits = (this.fits ?? 0) + 1;
        const d = this.proposeDimensions();
        if (d) this.term.resize(d.cols, d.rows);
      }
      inherited() {
        return super.proposeDimensions();
      }
    }
    const parent = document.createElement('div');
    parent.style.cssText = 'width: 600px; height: 100px';
    document.body.appendChild(parent);
    const term = new T.Terminal({ fontFamily: 'WongttySubclassFont, monospace', fontSize: 14 });
    const fit = new FixedGeometryFitAddon();
    term.loadAddon(fit);
    term.open(parent);
    fit.fit();
    const fitted = [term.cols, term.rows];
    const inherited = fit.inherited();
    const face = new FontFace('WongttySubclassFont', 'local("Arial"), local("Helvetica"), local("DejaVu Sans"), local("Liberation Sans")');
    document.fonts.add(face);
    await face.load();
    for (let n = 0; n < 50 && fit.fits < 2; n++) await T.sleep(20);
    const out = { activated: fit.term?.cols === term.cols, fitted, inherited, fits: fit.fits, size: [term.cols, term.rows] };
    term.dispose();
    return out;
  });
  assert.equal(r.activated, true);
  assert.deepEqual(r.fitted, [30, 5]);
  assert.equal(typeof r.inherited?.cols, 'number');
  assert.equal(r.fits, 2, 'the refit after the font loaded called the subclass fit');
  assert.deepEqual(r.size, [30, 5]);
});

test('loadAddon absorbs @xterm/addon-unicode11 and @xterm/addon-webgl and throws for any other addon', async () => {
  const r = await inPage(async () => {
    const { Unicode11Addon } = await import('/node_modules/@xterm/addon-unicode11/lib/addon-unicode11.mjs');
    const { WebglAddon } = await import('@xterm/addon-webgl');
    const term = new T.Terminal();
    const error = (addon) => {
      try {
        term.loadAddon(addon);
        return null;
      } catch (e) {
        return String(e.message);
      }
    };
    class SearchAddon {
      activate(t) {
        this.t = t;
      }
      dispose() {}
    }
    const webgl = new WebglAddon();
    const out = { unicode11: error(new Unicode11Addon()), search: error(new SearchAddon()), webgl: error(webgl), builtInWebgl: error(new T.WebglAddon()), plain: error({}) };
    term.open(document.body.appendChild(document.createElement('div')));
    await T.sleep(50);
    const renders = [];
    term.onRender((e) => renders.push(e));
    webgl.onContextLoss(() => {}).dispose();
    webgl.clearTextureAtlas();
    for (let n = 0; n < 50 && renders.length < 1; n++) await T.sleep(20);
    term.refresh(1, 2);
    for (let n = 0; n < 50 && renders.length < 2; n++) await T.sleep(20);
    webgl.dispose();
    out.renders = renders;
    term.dispose();
    return out;
  });
  assert.equal(r.unicode11, null);
  assert.match(r.search, /unsupported addon SearchAddon; only the built-in FitAddon, SearchAddon, WebLinksAddon, WebglAddon and ImageAddon, @xterm\/addon-webgl, @xterm\/addon-image and @xterm\/addon-unicode11/);
  assert.equal(r.webgl, null);
  assert.equal(r.builtInWebgl, null);
  assert.deepEqual(r.renders[0], { start: 0, end: 23 }, 'clearTextureAtlas redraws every row');
  assert.ok(r.renders[1]?.start <= 1 && r.renders[1]?.end >= 2, `refresh(1, 2) redraws those rows: ${JSON.stringify(r.renders[1])}`);
  assert.match(r.plain, /unsupported addon/);
});
