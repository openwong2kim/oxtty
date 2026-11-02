// Page for viewport.test.mjs: an oxtty/xterm terminal in viewport-only mode next to an ordinary one holding the whole
// history, both drawn. The history provider is a second engine fed the same output (as wmux's daemon); its answers
// can be held to see the placeholder rows.
import { init, SearchAddon } from 'oxtty/xterm';
import { Terminal as Engine } from 'oxtty';
import { Pane, diff } from './kit.mjs';

export const COLS = 40;
export const ROWS = 8;
export const MARGIN = 30;

/** Deterministic output: numbered lines, some wrapped, Hangul, combining marks, colours and rare words. */
export function corpus(lines, seed = 1) {
  const words = ['build', 'PASS', '한글', 'café', '\x1b[31merror:\x1b[0m', '│', 'src/a.rs', '\x1b[44mok\x1b[0m'];
  let s = '';
  for (let i = 0; i < lines; i++) {
    seed = (seed * 1103515245 + 12345) & 0x7fffffff;
    let line = `${i}:`;
    for (let n = seed % 7; n >= 0; n--) line += ` ${words[(seed >> (n + 3)) % words.length]}`;
    if (i % 97 === 13) line += ` needle${i}`;
    if (seed % 11 === 0) line += ' '.repeat(COLS) + 'wrapped tail';
    s += line + '\r\n';
  }
  return s;
}

const frames = (n) => new Promise((resolve) => {
  const step = () => (n-- > 0 ? requestAnimationFrame(step) : resolve());
  step();
});

let full;
let view;
let engine;
let held = false;
let pending = [];
let fetches = 0;
let results = [];
let addons;

function answer(start, count) {
  const rows = [];
  for (let y = start; y < start + count; y++) {
    const line = y - engine.historyLength;
    rows.push(line < -engine.historyLength || line >= 0
      ? null
      : { cells: engine.rowCells(line), wrapped: engine.rowWrapped(line), extras: engine.rowExtras(line) });
  }
  return rows;
}

const provider = {
  get length() { return engine.historyLength; },
  fetch(start, count) {
    fetches++;
    if (!held) return Promise.resolve(answer(start, count));
    return new Promise((resolve) => pending.push(() => resolve(answer(start, count))));
  },
};

window.setup = async (lines) => {
  full = new Pane(COLS, ROWS, { scrollback: 100000 });
  view = new Pane(COLS, ROWS, { scrollback: MARGIN });
  engine = new Engine({ cols: COLS, rows: ROWS, scrollback: 100000 });
  view.term.setHistoryProvider(provider);
  const data = corpus(lines);
  engine.write(data);
  await Promise.all([full.write(data), view.write(data)]);
  addons = [new SearchAddon(), new SearchAddon()];
  full.term.loadAddon(addons[0]);
  view.term.loadAddon(addons[1]);
  addons[1].onDidChangeResults((e) => results.push(e));
  await frames(3);
  return { baseY: full.term.buffer.active.baseY, viewBaseY: view.term.buffer.active.baseY };
};

/** Pixels of both terminals after scrolling both to `line`, with the answers held, then after they come. */
window.render = async (line) => {
  held = true;
  for (const p of [full, view]) p.term.scrollToLine(line);
  await frames(2);
  const before = { full: await full.pixels(), view: await view.pixels() };
  const asked = pending.length;
  held = false;
  for (const resolve of pending.splice(0)) resolve();
  await frames(3);
  const after = { full: await full.pixels(), view: await view.pixels() };
  // A blank screen of the same size: what placeholders look like.
  const blank = new Pane(COLS, ROWS);
  await blank.write('\x1b[?25l');
  const empty = await blank.pixels();
  blank.dispose();
  return {
    asked,
    viewportY: [full.term.buffer.active.viewportY, view.term.buffer.active.viewportY],
    placeholder: diff(before.view, empty),
    beforeDiff: diff(before.full, before.view),
    afterDiff: diff(after.full, after.view),
    ink: diff(after.full, empty),
  };
};

/** `method(term, options)` on both terminals; waits for the viewport-only one to finish a history search. */
window.search = async (method, term, options) => {
  const expected = addons[0][method](term, options);
  const count = results.length;
  const sync = addons[1][method](term, options);
  if (!sync) {
    for (let i = 0; i < 500 && results.length === count; i++) await new Promise((r) => setTimeout(r, 0));
  }
  await frames(2);
  const state = (t) => ({ position: t.getSelectionPosition() ?? null, text: t.getSelection(), viewportY: t.buffer.active.viewportY });
  return { expected, sync, finished: results.length > count, full: state(full.term), view: state(view.term) };
};

/** Selects the same range in both (after fetching the host rows it covers) and copies it. */
window.select = async (column, row, length) => {
  const last = Math.min(view.term.buffer.active.length, row + Math.ceil((column + length) / COLS) + 1);
  for (let y = Math.max(0, row - 1); y < last; y++) view.term.buffer.active.getLine(y);
  await frames(3);
  for (const p of [full, view]) p.term.select(column, row, length);
  return [full.term.getSelection(), view.term.getSelection()];
};

window.fetches = () => fetches;
window.cacheSize = () => view.term._historyCacheSize();

await init();
window.ready = true;
