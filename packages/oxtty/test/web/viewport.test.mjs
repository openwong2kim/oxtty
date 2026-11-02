// Viewport-only mode (PRD M4): a terminal keeping a short scrollback reads older history from a host provider. The
// provider here is a second, full-scrollback engine fed the same output, as wmux's daemon would be; every check
// compares the viewport-only terminal with an ordinary oxtty/xterm terminal holding everything. The buffer, scroll
// and memory checks run in Node; drawing, search and selection need a page.
import { test, before } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import * as m from '../../dist/xterm.js';
import { Terminal as Engine } from '../../dist/index.js';
import { withPage } from './harness.mjs';

before(async () => {
  await m.init(undefined, await readFile(new URL('../../dist/xterm_web_bg.wasm', import.meta.url)));
});

const write = (t, data) => new Promise((resolve) => t.write(data, resolve));
const COLS = 40;
const ROWS = 6;
const MARGIN = 30;

/** Deterministic output: numbered lines, some wrapped, Hangul, combining marks and colours. */
function corpus(lines, seed = 1) {
  const words = ['build', 'PASS', '한글', 'café', '\x1b[31merror:\x1b[0m', '│', 'src/a.rs', 'ok'];
  let s = '';
  for (let i = 0; i < lines; i++) {
    seed = (seed * 1103515245 + 12345) & 0x7fffffff;
    let line = `${i}:`;
    for (let n = seed % 7; n >= 0; n--) line += ` ${words[(seed >> (n + 3)) % words.length]}`;
    if (seed % 11 === 0) line += ' '.repeat(COLS) + 'wrapped tail';
    s += line + '\r\n';
  }
  return s;
}

/** The bench's coloured log lines, `cols` wide. */
function log(cols, lines, seed = 7) {
  const words = ['build', 'PASS', '한글', '터미널', 'error:', '│', '─┼─', 'src/render/webgl.rs', '0x1f600', 'ok'];
  let s = '';
  for (let line = 0; line < lines; line++) {
    let width = 0;
    while (width < cols - 12) {
      seed = (seed * 1103515245 + 12345) & 0x7fffffff;
      const word = words[seed % words.length];
      s += `\x1b[${31 + (seed % 7)}m${word}\x1b[0m `;
      width += [...word].reduce((w, c) => w + (c.charCodeAt(0) > 0x1100 ? 2 : 1), 0) + 1;
    }
    s += '\r\n';
  }
  return s;
}

/** A host engine holding the whole history and a provider over it; `fetches` counts the requests. */
function host(scrollback = 100000) {
  const engine = new Engine({ cols: COLS, rows: ROWS, scrollback });
  const counter = { fetches: 0 };
  const provider = {
    get length() { return engine.historyLength; },
    async fetch(start, count) {
      counter.fetches++;
      const rows = [];
      for (let y = start; y < start + count; y++) {
        const line = y - engine.historyLength;
        rows.push(line < -engine.historyLength || line >= 0
          ? null
          : { cells: engine.rowCells(line), wrapped: engine.rowWrapped(line), extras: engine.rowExtras(line) });
      }
      return rows;
    },
  };
  return { engine, provider, counter };
}

/** Lets fetch answers (promises) settle. */
const settle = () => new Promise((resolve) => setTimeout(resolve, 0));

/** A full terminal and a viewport-only one with its host, all fed `data`. */
async function setup(data, { scrollback = 100000, limit } = {}) {
  const full = new m.Terminal({ cols: COLS, rows: ROWS, scrollback });
  const view = new m.Terminal({ cols: COLS, rows: ROWS, scrollback: MARGIN });
  const h = host(scrollback);
  view.setHistoryProvider({ length: 0, limit, fetch: (s, c) => h.provider.fetch(s, c) });
  h.engine.write(data);
  await Promise.all([write(full, data), write(view, data)]);
  const step = async (more) => {
    h.engine.write(more);
    await Promise.all([write(full, more), write(view, more)]);
  };
  const dispose = () => [full, view, h.engine].forEach((t) => t.dispose());
  return { full, view, h, step, dispose };
}

const text = (t, y) => {
  const line = t.buffer.active.getLine(y);
  return line && JSON.stringify([line.translateToString(true), line.isWrapped]);
};

/** Lines of the viewport-only terminal that differ from the full one, reading them a block at a time. */
async function mismatches(full, view) {
  const out = [];
  const length = full.buffer.active.length;
  for (let y = 0; y < length; y += 128) {
    const end = Math.min(length, y + 128);
    for (let k = y; k < end; k++) view.buffer.active.getLine(k);
    await settle();
    for (let k = y; k < end; k++) if (text(view, k) !== text(full, k)) out.push(k);
  }
  return out;
}

test('the engine keeps only the margin: its memory stays flat while the host holds 12,000 lines', async () => {
  const probe = new Engine({ cols: 2, rows: 1, scrollback: 0 });
  const memory = () => probe.rowCellsView(0).buffer.byteLength;
  const data = log(120, 12000);
  const view = new m.Terminal({ cols: 120, rows: 40, scrollback: 200 });
  view.setHistoryProvider({ length: 0, cacheLines: 64, fetch: async (s, c) => new Array(c).fill('row') });
  const start = memory();
  await write(view, data);
  const viewGrowth = memory() - start;
  const full = new m.Terminal({ cols: 120, rows: 40, scrollback: 100000 });
  await write(full, data);
  const fullGrowth = memory() - start - viewGrowth;
  assert.equal(view.buffer.active.baseY, full.buffer.active.baseY);
  assert.ok(view.buffer.active.baseY > 11900);
  // Host rows are read through a cache that stays within its limit.
  for (let y = 0; y < 5000; y += 40) view.buffer.active.getLine(y);
  await settle();
  assert.ok(view._historyCacheSize() > 0 && view._historyCacheSize() <= 64, `${view._historyCacheSize()} rows cached`);
  view.buffer.active.getLine(4000);
  await settle();
  assert.equal(view.buffer.active.getLine(4000).translateToString(true), 'row');
  assert.equal(viewGrowth, 0, `viewport-only engine grew by ${viewGrowth} bytes`);
  assert.ok(fullGrowth >= 2 * 1024 * 1024, `full engine grew by ${fullGrowth} bytes`);
  view.dispose();
  full.dispose();
  probe.dispose();
});

test('buffer lines, length, baseY and viewportY match a full terminal once fetched', async () => {
  const { full, view, h, dispose } = await setup(corpus(3000));
  const [a, b] = [full.buffer.active, view.buffer.active];
  assert.ok(a.baseY > 3000, `baseY ${a.baseY}`);
  assert.deepEqual([b.length, b.baseY, b.viewportY], [a.length, a.baseY, a.viewportY]);
  // Before the answer, a host line is a blank line of cols cells; reading it asks for it.
  const first = b.getLine(5);
  assert.equal(first.length, COLS);
  assert.equal(first.translateToString(true), '');
  assert.equal(b.getLine(a.length), undefined);
  await settle();
  assert.equal(text(view, 5), text(full, 5));
  // Every line, wrapped ones across the boundary to the terminal's own lines included.
  assert.deepEqual(await mismatches(full, view), []);
  assert.ok(h.counter.fetches < a.length / 20, `${h.counter.fetches} fetches`);
  assert.ok(view._historyCacheSize() <= 256);
  dispose();
});

test('ANSI rows from the host are parsed at the terminal\'s width', async () => {
  const view = new m.Terminal({ cols: 20, rows: 4, scrollback: 5 });
  view.setHistoryProvider({ length: 3, fetch: async (start, count) => ['\x1b[1mbold\x1b[0m 한글', { ansi: 'x'.repeat(30), wrapped: true }, null].slice(start, start + count) });
  const b = view.buffer.active;
  assert.equal(b.baseY, 3);
  b.getLine(0);
  await settle();
  assert.equal(b.getLine(0).translateToString(true), 'bold 한글');
  assert.equal(b.getLine(0).getCell(0).isBold(), 1);
  assert.equal(b.getLine(0).getCell(5).getWidth(), 2);
  assert.equal(b.getLine(1).translateToString(), 'x'.repeat(20));
  assert.equal(b.getLine(2).isWrapped, true);
  assert.equal(b.getLine(2).translateToString(true), '');
  view.dispose();
});

test('scrolling over host lines follows xterm.js numbering and keeps a scrolled-back view on its text', async () => {
  const { full, view, h, step, dispose } = await setup(corpus(500));
  const both = [full, view];
  const scrolled = [[], []];
  both.forEach((t, k) => t.onScroll((y) => scrolled[k].push(y)));
  const same = (what) => assert.equal(view.buffer.active.viewportY, full.buffer.active.viewportY, what);
  both.forEach((t) => t.scrollToLine(100));
  same('scrollToLine');
  both.forEach((t) => t.scrollLines(-40));
  same('scrollLines');
  both.forEach((t) => t.scrollPages(1));
  same('scrollPages');
  // Output while scrolled back stays on the same line, in the host's lines and in the terminal's own.
  await step(corpus(80, 9));
  same('output in host lines');
  both.forEach((t) => t.scrollToTop());
  assert.equal(view.buffer.active.viewportY, 0);
  both.forEach((t) => t.scrollToLine(full.buffer.active.baseY - 3));
  await step(corpus(80, 9));
  same('output near the bottom');
  both.forEach((t) => t.scrollToBottom());
  assert.equal(view.buffer.active.viewportY, view.buffer.active.baseY);
  assert.deepEqual(scrolled[1], scrolled[0]);
  // ED3 clears the host's lines as well, and so does RIS.
  await step('x\r\n'.repeat(50) + '\x1b[3J');
  assert.deepEqual([view.buffer.active.baseY, view.buffer.active.length], [full.buffer.active.baseY, full.buffer.active.length]);
  await step(corpus(100));
  assert.equal(view.buffer.active.baseY, full.buffer.active.baseY);
  assert.deepEqual(await mismatches(full, view), []);
  both.forEach((t) => t.reset());
  h.engine.write('\x1bc');
  assert.equal(view.buffer.active.baseY, 0);
  // The alternate screen has no history.
  await step(corpus(100) + '\x1b[?1049h');
  assert.deepEqual([view.buffer.active.type, view.buffer.active.baseY], ['alternate', 0]);
  await step('\x1b[?1049l');
  assert.equal(view.buffer.active.baseY, full.buffer.active.baseY);
  assert.deepEqual(await mismatches(full, view), []);
  dispose();
});

test('a host at its limit drops its oldest lines, and the numbering follows like a full terminal\'s', async () => {
  const limit = 400;
  const { full, view, step, dispose } = await setup(corpus(300), { scrollback: limit, limit });
  await step(corpus(500, 3));
  assert.equal(view.buffer.active.baseY, limit);
  assert.equal(full.buffer.active.baseY, limit);
  assert.deepEqual(await mismatches(full, view), []);
  dispose();
});

test('a resize drops fetched rows until the host sets the provider again', async () => {
  const { full, view, h, step, dispose } = await setup(corpus(400));
  view.buffer.active.getLine(10);
  await settle();
  assert.ok(view._historyCacheSize() > 0);
  for (const t of [full, view, h.engine]) t.resize(30, ROWS);
  assert.equal(view._historyCacheSize(), 0);
  view.setHistoryProvider({ length: h.engine.historyLength, fetch: (s, c) => h.provider.fetch(s, c) });
  await step(corpus(20, 4));
  assert.deepEqual(await mismatches(full, view), []);
  view.setHistoryProvider(null);
  assert.ok(view.buffer.active.baseY <= MARGIN);
  assert.throws(() => view.setHistoryProvider({ length: 1 }), TypeError);
  dispose();
});

test('markers count the host\'s lines, on the normal screen and while the alternate one is up', async () => {
  const { full, view, step, dispose } = await setup(corpus(300));
  const marks = [full, view].map((t) => [0, -3].map((offset) => t.registerMarker(offset)));
  const lines = () => marks.map((list) => list.map((marker) => marker.line));
  assert.ok(lines()[0][0] > 300);
  assert.deepEqual(lines()[1], lines()[0]);
  await step(corpus(10, 2));
  assert.deepEqual(lines()[1], lines()[0]);
  await step('\x1b[?1049h');
  const alt = [full, view].map((t) => t.registerMarker(0));
  assert.deepEqual(lines()[1], lines()[0]);
  assert.equal(alt[1].line, alt[0].line);
  await step('\x1b[?1049l');
  assert.deepEqual(lines()[1], lines()[0]);
  dispose();
});

// One page for drawing, search and selection: a browser per test would load the machine the parallel tests share.
test('in a page: host rows are drawn once they arrive, search and selection read them like a full terminal', async () => {
  const r = await withPage('viewport.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    const { baseY } = await page.evaluate(() => window.setup(3000));
    const render = [];
    for (const line of [40, 1500, 0]) render.push(await page.evaluate((l) => window.render(l), line));
    const search = [];
    for (const [term, options] of [
      ['needle13', {}],
      ['needle1759', { caseSensitive: true }],
      ['NEEDLE2729', {}],
      ['needle2[0-9]{3}', { regex: true }],
      ['wrapped tail', { wholeWord: true }],
      ['no such text anywhere', {}],
    ]) {
      for (const method of ['findNext', 'findPrevious', 'findNext', 'findPrevious']) {
        search.push({ term, method, ...(await page.evaluate(([m, t, o]) => window.search(m, t, o), [method, term, options])) });
      }
    }
    const selection = await page.evaluate((top) => window.select(3, top, 40 * 100), baseY - 30 - 60);
    // Dragging each slider to the top of its track: the viewport-only one goes into the host's lines too.
    const drag = [];
    for (const which of ['full', 'view']) {
      const s = await page.evaluate((w) => window.slider(w), which);
      await page.mouse.move(s.x, s.y);
      await new Promise((resolve) => setTimeout(resolve, 50));
      await page.mouse.down();
      await page.mouse.move(s.x, s.y - 40);
      drag.push(await page.evaluate(() => window.viewportY()));
      await page.mouse.move(s.x, s.top - 50);
      drag.push(await page.evaluate(() => window.viewportY()));
      await page.mouse.up();
    }
    const input = [];
    for (const how of ['key', 'composition', 'input']) input.push(await page.evaluate((h) => window.userInput(h), how));
    return { render, search, selection, drag, input, baseY };
  });
  const { baseY } = r;
  // Drawing: blank placeholder rows until the answer, then the same pixels as the full terminal.
  for (const [k, s] of r.render.entries()) {
    assert.equal(s.viewportY[0], s.viewportY[1], `case ${k}`);
    assert.ok(s.asked > 0, `case ${k}: rows were asked for`);
    assert.equal(s.placeholder, 0, `case ${k}: placeholders are blank rows`);
    assert.ok(s.beforeDiff > 0, `case ${k}`);
    assert.ok(s.ink > 0, `case ${k}`);
    assert.equal(s.afterDiff, 0, `case ${k}: drawn like the full terminal`);
  }
  // Search: the same selection and scroll position, some of them after reading host lines.
  let async = 0;
  for (const s of r.search) {
    const what = `${s.method}(${s.term})`;
    const exists = s.term !== 'no such text anywhere';
    assert.equal(s.expected, exists, what);
    // false either went on reading host lines (and then reports) or found nothing in what the terminal holds.
    if (exists && !s.sync) {
      async++;
      assert.ok(s.finished, `${what} finished`);
    }
    assert.deepEqual(s.view, s.full, what);
  }
  assert.ok(async > 0, 'some searches read host lines');
  // Selection from host rows across the terminal's own scrollback into the screen.
  const [full, view] = r.selection;
  assert.ok(full.length > 1000, `${full.length} characters`);
  assert.equal(view, full);
  // The scrollbar: a drag of the full terminal's slider, then the same drag of the viewport-only one.
  const [[fullDragged], [fullTop], [, viewDragged], [, viewTop]] = r.drag;
  assert.ok(fullDragged < baseY && fullTop === 0, `full terminal dragged to ${fullDragged}, then ${fullTop}`);
  assert.equal(viewTop, 0, 'the viewport-only slider reaches the oldest host line');
  assert.ok(viewDragged < baseY - 30 - 8, `a short drag reaches host lines (${viewDragged})`);
  // User input from host lines scrolls to the bottom (scrollOnUserInput), even with no scrollback of its own.
  for (const [k, i] of r.input.entries()) {
    assert.equal(i.top, 0, `case ${k}`);
    assert.ok(i.baseY > 90, `case ${k}`);
    assert.equal(i.viewportY, i.baseY, ['key', 'composition', 'input'][k]);
  }
});
