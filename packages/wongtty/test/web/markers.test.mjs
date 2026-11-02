// Markers (registerMarker, markers, IMarker) of the wongtty/xterm Terminal next to @xterm/headless, and the built-in
// SearchAddon's API without a page: errors, events and odd arguments that must neither throw nor stop the page.
import { test, before } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import xtermHeadless from '@xterm/headless';
import * as m from '../../dist/xterm.js';
import { __trapForTest } from '../../dist/index.js';

before(async () => {
  await m.init(undefined, await readFile(new URL('../../dist/xterm_web_bg.wasm', import.meta.url)));
});

const write = (t, data) => new Promise((resolve) => t.write(data, resolve));

/** The same terminal in wongtty and in @xterm/headless; `step(data)` writes to both. */
const pair = (options) => {
  const t = new m.Terminal(options);
  const x = new xtermHeadless.Terminal({ ...options, allowProposedApi: true });
  return { both: [t, x], step: (data) => Promise.all([write(t, data), write(x, data)]), dispose: () => { t.dispose(); x.dispose(); } };
};

test('markers follow their line as output scrolls and are disposed when it leaves the scrollback, like xterm.js', async () => {
  const { both, step, dispose } = pair({ cols: 10, rows: 3, scrollback: 4 });
  await step('one\r\ntwo\r\n');
  const seen = [[], []];
  const marks = both.map((t, k) => [0, -1, -2].map((offset) => {
    const marker = t.registerMarker(offset);
    marker.onDispose(() => seen[k].push(offset));
    return marker;
  }));
  const state = () => marks.map((list) => list.map((marker) => [marker.line, marker.isDisposed]));
  assert.deepEqual(state()[0], state()[1]);
  assert.deepEqual(state()[0], [[2, false], [1, false], [0, false]]);
  for (let i = 0; i < 5; i++) {
    // The callback runs after onDispose: a write that drops a marker has fired it by then.
    await step(`line ${i}\r\n`);
    assert.deepEqual(state()[0], state()[1], `after line ${i}`);
    assert.deepEqual(seen[0], seen[1], `after line ${i}`);
  }
  assert.deepEqual(seen[0], [-2]);
  const lines = both.map((t) => t.markers.map((marker) => marker.line));
  assert.deepEqual(lines[0], lines[1]);
  dispose();
});

test('dispose, erase in display, reset and terminal dispose fire onDispose once, like xterm.js', async () => {
  const { both, step, dispose } = pair({ cols: 10, rows: 3 });
  await step('a\r\nb\r\nc');
  const seen = [[], []];
  const mark = (name) => both.map((t, k) => {
    const marker = t.registerMarker(0);
    marker.onDispose(() => seen[k].push(name));
    return marker;
  });
  const own = mark('own');
  own.forEach((marker) => { marker.dispose(); marker.dispose(); });
  assert.deepEqual(own.map((marker) => [marker.line, marker.isDisposed]), [[-1, true], [-1, true]]);
  assert.deepEqual(both.map((t) => t.markers.length), [0, 0]);
  mark('ED');
  await step('\x1b[2J');
  mark('RIS');
  await step('\x1bc');
  mark('reset');
  both.forEach((t) => t.reset());
  mark('dispose');
  dispose();
  assert.deepEqual(seen[0], seen[1]);
  assert.deepEqual(seen[0], ['own', 'ED', 'RIS', 'reset', 'dispose']);
});

test('the alternate buffer has its own markers, moved and disposed like xterm.js, all gone when it is left', async () => {
  const { both, step, dispose } = pair({ cols: 10, rows: 5, scrollback: 10 });
  await step('p0\r\np1\r\np2');
  const seen = [[], []];
  const mark = (name, offset) => both.map((t, k) => {
    const marker = t.registerMarker(offset);
    marker.onDispose(() => seen[k].push(name));
    return marker;
  });
  const marks = { primary: mark('primary', 0) };
  const state = () => [0, 1].map((k) => ({
    lines: Object.fromEntries(Object.entries(marks).map(([name, pair]) => [name, pair[k].line])),
    markers: both[k].markers.map((marker) => marker.line),
    seen: seen[k],
  }));
  const check = (label) => {
    const [ox, xt] = state();
    assert.deepEqual(ox, xt, label);
  };
  await step('\x1b[?1049h\x1b[Ha\r\nb\r\nc\r\nd');
  for (const [name, offset] of [['top', -3], ['b', -2], ['c', -1], ['d', 0]]) marks[name] = mark(name, offset);
  check('registered');
  await step('\x1b[2;1H\x1b[M');
  check('a deleted line');
  await step('\x1b[1;1H\x1b[L');
  check('an inserted line');
  await step('\x1b[5;1H\n\n');
  check('the screen scrolled');
  marks.e = mark('e', 0);
  both.forEach((t) => t.resize(10, 4));
  check('fewer rows');
  await step('\x1b[?1049l');
  check('back on the normal buffer');
  assert.deepEqual(state()[0].markers, [2]);
  marks.again = await step('\x1b[?1049h').then(() => mark('again', 0));
  await step('\x1b[2J');
  check('erase in display');
  await step('\x1b[?1049l');
  check('left again');
  dispose();
});

test('registerMarker checks its offset like xterm.js; listeners added after dispose never run', async () => {
  const t = new m.Terminal({ cols: 10, rows: 3, scrollback: 5 });
  await write(t, 'x\r\ny');
  for (const offset of [0.5, NaN, Infinity]) assert.throws(() => t.registerMarker(offset), /only accepts integers/);
  const marker = t.registerMarker();
  assert.deepEqual([marker.line, typeof marker.id, t.markers.length], [1, 'number', 1]);
  assert.notEqual(t.registerMarker(0).id, marker.id);
  // Outside the buffer xterm.js still hands out a marker (on a line that does not exist); wongtty returns undefined.
  assert.equal(t.registerMarker(-10), undefined);
  assert.equal(t.registerMarker(10), undefined);
  // The alternate buffer has markers of its own, which go when it is left.
  await write(t, '\x1b[?1049h');
  const alt = t.registerMarker(0);
  assert.deepEqual(t.markers.map((m) => m.id), [alt.id]);
  await write(t, '\x1b[?1049l');
  assert.equal(t.markers.length, 2);
  assert.equal(alt.isDisposed, true);
  marker.dispose();
  const late = [];
  marker.onDispose(() => late.push(1));
  marker.onDispose(42).dispose();
  t.dispose();
  assert.deepEqual([late, t.markers, t.registerMarker(0), marker.line], [[], [], undefined, -1]);
});

test('an engine crash disposes every marker, and markers made after it are not the old ones', async (t) => {
  t.mock.method(console, 'warn', () => {});
  const term = new m.Terminal({ cols: 10, rows: 3 });
  await write(term, 'a\r\nb');
  const old = term.registerMarker(0);
  const seen = [];
  old.onDispose(() => seen.push('old'));
  assert.throws(() => __trapForTest(), WebAssembly.RuntimeError);
  await write(term, '');
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.deepEqual([seen, old.isDisposed, old.line, term.markers.length], [['old'], true, -1, 0]);
  const fresh = term.registerMarker(0);
  assert.equal(fresh.id, old.id, 'the engine numbers markers again from 1');
  assert.deepEqual([old.isDisposed, fresh.isDisposed, term.markers.length], [true, false, 1]);
  old.dispose();
  assert.equal(fresh.isDisposed, false);
  term.dispose();
});

test('SearchAddon: errors, events and odd arguments', async () => {
  const addon = new m.SearchAddon({ highlightLimit: 2 });
  assert.throws(() => addon.findNext('x'), /Cannot use addon until it has been loaded/);
  const t = new m.Terminal({ cols: 10, rows: 3 });
  addon.activate(t);
  await write(t, 'ab ab\r\nab');
  const events = [];
  addon.onBeforeSearch(() => events.push('before'));
  addon.onAfterSearch(() => events.push('after'));
  addon.onDidChangeResults((e) => events.push(e));
  assert.equal(addon.findNext('ab', { decorations: {} }), true);
  // The limit caps the results; the active match past it has index -1.
  assert.equal(addon.findPrevious('ab', { decorations: {} }), true);
  assert.deepEqual(events, ['before', { resultIndex: 0, resultCount: 2 }, 'after', 'before', { resultIndex: -1, resultCount: 2 }, 'after']);
  assert.throws(() => addon.findNext('(', { regex: true }), SyntaxError);
  for (const term of [undefined, null, 42, '']) assert.equal(addon.findNext(term), false);
  for (const options of [null, 42, 'x', { decorations: 1 }]) assert.equal(typeof addon.findNext('ab', options), 'boolean');
  addon.clearActiveDecoration();
  addon.clearDecorations();
  addon.dispose();
  assert.equal(addon.findNext('ab'), true, 'searching still works after dispose, like the addon');
  t.dispose();
  assert.equal(addon.findNext('ab'), false);
  addon.free();
});
