// The wongtty/xterm Terminal without a page, next to @xterm/headless where xterm.js has an answer: writes, engine
// events, buffer, options, addons, crash and dispose. No `open`, so no frames are drawn: replies must still flow (a
// hidden page answers programs too).
import { test, before } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import xtermHeadless from '@xterm/headless';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { WebglAddon } from '@xterm/addon-webgl';
import * as m from '../../dist/xterm.js';
import { __trapForTest } from '../../dist/index.js';

before(async () => {
  await m.init(undefined, await readFile(new URL('../../dist/xterm_web_bg.wasm', import.meta.url)));
});

const write = (t, data) => new Promise((resolve) => t.write(data, resolve));
/** Every value `target[event]` fires from now on. */
const collect = (target, event) => {
  const seen = [];
  target[event]((e) => seen.push(e));
  return seen;
};
const headlessReply = async (data) => {
  const x = new xtermHeadless.Terminal({ allowProposedApi: true });
  const seen = collect(x, 'onData');
  await new Promise((resolve) => x.write(data, resolve));
  x.dispose();
  return seen.join('');
};

test('status reports reach onData before the write callback, without any frame', async () => {
  const t = new m.Terminal({ cols: 10, rows: 3 });
  const data = collect(t, 'onData');
  await new Promise((resolve) => t.write('\x1b[2;3H\x1b[6n', () => {
    assert.deepEqual(data, ['\x1b[2;3R']);
    resolve();
  }));
  t.dispose();
});

test('events are IEvent properties that also work unbound, like xterm.js', async () => {
  const t = new m.Terminal();
  const x = new xtermHeadless.Terminal({ allowProposedApi: true });
  const { onData } = t;
  const seen = [];
  onData((d) => seen.push(d)).dispose();
  assert.equal(typeof x.onData, typeof t.onData);
  x.dispose();
  t.dispose();
});

test('several replies in one write fire one onData each, like @xterm/headless', async () => {
  const data = 'out\x1b[c\x1b[>c\x1b[6n\x1b[5n\x1b[4$p\x1b[?2004$p\r\n';
  const t = new m.Terminal({ cols: 40, rows: 6 });
  const x = new xtermHeadless.Terminal({ cols: 40, rows: 6, allowProposedApi: true });
  const [ox, xt] = [collect(t, 'onData'), collect(x, 'onData')];
  await Promise.all([write(t, data), write(x, data)]);
  assert.deepEqual(ox, xt);
  assert.equal(ox.length, 6);
  t.dispose();
  x.dispose();
});

test('DA1 and DA2 replies are the bytes xterm.js sends', async () => {
  for (const [query, reply] of [['\x1b[c', '\x1b[?1;2c'], ['\x1b[>c', '\x1b[>0;276;0c']]) {
    const t = new m.Terminal();
    const data = collect(t, 'onData');
    await write(t, query);
    assert.equal(data.join(''), await headlessReply(query));
    assert.equal(data.join(''), reply);
    t.dispose();
  }
});

test('titles and bells become events', async () => {
  const t = new m.Terminal();
  const titles = collect(t, 'onTitleChange');
  const bells = collect(t, 'onBell');
  await write(t, '\x1b]0;hi\x07\x07');
  await write(t, '\x1b]2;there\x1b\\');
  assert.deepEqual(titles, ['hi', 'there']);
  assert.equal(bells.length, 1);
  t.dispose();
});

test('colour queries answer with the theme, also after it changes at runtime', async () => {
  const t = new m.Terminal({ theme: { foreground: '#123456' } });
  const data = collect(t, 'onData');
  await write(t, '\x1b]10;?\x07\x1b]11;?\x07');
  assert.deepEqual(data.splice(0), ['\x1b]10;rgb:1212/3434/5656\x1b\\', '\x1b]11;rgb:0000/0000/0000\x1b\\']);
  t.options.theme = { background: '#abcdef', red: 'rgb(1, 2, 3)' };
  await write(t, '\x1b]11;?\x07\x1b]4;1;?\x07');
  assert.deepEqual(data, ['\x1b]11;rgb:abab/cdcd/efef\x1b\\', '\x1b]4;1;rgb:0101/0202/0303\x1b\\']);
  t.dispose();
});

test('OSC 52 goes through the clipboard option, and nowhere without it', async () => {
  const copied = [];
  const t = new m.Terminal({ clipboard: { write: (text, selection) => copied.push([selection, text]), read: async () => 'pasted' } });
  const data = collect(t, 'onData');
  await write(t, '\x1b]52;c;aGk=\x07\x1b]52;c;?\x07');
  assert.deepEqual(copied, [['c', 'hi']]);
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.deepEqual(data.splice(0), ['\x1b]52;c;cGFzdGVk\x07']);
  t.options.clipboard = undefined;
  await write(t, '\x1b]52;c;aGk=\x07\x1b]52;c;?\x07');
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.deepEqual([copied.length, data.length], [1, 0]);
  t.dispose();
});

test('a string write split inside a surrogate pair keeps the high half for the next write', async () => {
  const t = new m.Terminal({ cols: 10, rows: 2 });
  t.write('a\ud83d');
  await write(t, '\ude00b');
  assert.equal(t.buffer.active.getLine(0).translateToString(true), 'a😀b');
  t.dispose();
});

test('the buffer reads like xterm.js: lines, wrapping, wide cells, cursor and scroll position', async () => {
  const input = 'one\r\n' + 'x'.repeat(12) + '\r\n한글 b\r\nfour\r\nfive';
  const t = new m.Terminal({ cols: 10, rows: 3, scrollback: 50 });
  const x = new xtermHeadless.Terminal({ cols: 10, rows: 3, scrollback: 50, allowProposedApi: true });
  await write(t, input);
  await new Promise((resolve) => x.write(input, resolve));
  const [a, b] = [t.buffer.active, x.buffer.active];
  for (const key of ['type', 'length', 'baseY', 'viewportY', 'cursorX', 'cursorY']) assert.equal(a[key], b[key], key);
  for (let y = 0; y < b.length; y++) {
    const [la, lb] = [a.getLine(y), b.getLine(y)];
    assert.equal(la.isWrapped, lb.isWrapped, `line ${y} isWrapped`);
    assert.equal(la.length, lb.length);
    for (const args of [[true], [false], [false, 1, 4]]) assert.equal(la.translateToString(...args), lb.translateToString(...args), `line ${y} ${args}`);
  }
  const wide = a.getLine(3);
  assert.deepEqual([0, 1].map((i) => [wide.getCell(i).getChars(), wide.getCell(i).getWidth()]), [['한', 2], ['', 0]]);
  assert.equal(a.getLine(b.length), undefined);
  assert.equal(a.getNullCell().getChars(), b.getNullCell().getChars());
  x.dispose();

  const scrolls = collect(t, 'onScroll');
  t.scrollToTop();
  t.scrollLines(1);
  t.scrollToLine(2);
  t.scrollToBottom();
  assert.deepEqual(scrolls, [0, 1, 2, 3]);
  t.dispose();
});

test('the alternate screen changes the active buffer type and fires onBufferChange', async () => {
  const t = new m.Terminal({ cols: 10, rows: 3 });
  const types = [];
  t.buffer.onBufferChange((buffer) => types.push(buffer.type));
  await write(t, '\x1b[?1049h');
  assert.equal(t.buffer.active.type, 'alternate');
  await write(t, '\x1b[?1049l');
  assert.deepEqual(types, ['alternate', 'normal']);
  t.dispose();
});

test('modes follow the program like xterm.js', async () => {
  const t = new m.Terminal();
  const x = new xtermHeadless.Terminal({ allowProposedApi: true });
  assert.equal(t.modes.mouseTrackingMode, 'none');
  const input = '\x1b[?2004h\x1b[?1002h\x1b[?1h';
  await write(t, input);
  await new Promise((resolve) => x.write(input, resolve));
  assert.deepEqual({ ...t.modes }, { ...x.modes });
  const modes = t.modes;
  assert.deepEqual([modes.bracketedPasteMode, modes.mouseTrackingMode, modes.applicationCursorKeysMode, modes.wraparoundMode], [true, 'drag', true, true]);
  x.dispose();
  t.dispose();
});

test('options: init-only keys throw at runtime, resize updates size and fires onResize once per change', () => {
  const t = new m.Terminal({ cols: 20, rows: 5 });
  assert.throws(() => { t.options.scrollback = 10; }, /constructor/);
  assert.throws(() => { t.options.cols = 30; }, /constructor/);
  const resized = collect(t, 'onResize');
  t.resize(30, 6);
  t.resize(30, 6);
  assert.deepEqual(resized, [{ cols: 30, rows: 6 }]);
  assert.deepEqual([t.cols, t.rows, t.options.cols, t.options.rows], [30, 6, 30, 6]);
  t.dispose();
});

test('options read back the xterm.js defaults', () => {
  const t = new m.Terminal();
  const x = new xtermHeadless.Terminal({ allowProposedApi: true });
  for (const key of ['fontSize', 'cursorStyle', 'cursorBlink', 'scrollback', 'scrollOnUserInput', 'disableStdin', 'altClickMovesCursor', 'minimumContrastRatio']) {
    assert.deepEqual(t.options[key], x.options[key], key);
  }
  x.dispose();
  t.dispose();
});

test('loadAddon absorbs unicode11 and webgl addons and rejects the rest', () => {
  const t = new m.Terminal();
  t.loadAddon(new Unicode11Addon());
  t.unicode.activeVersion = '11';
  assert.throws(() => { t.unicode.activeVersion = '6'; }, /Unicode 11/);
  const webgl = new WebglAddon();
  t.loadAddon(webgl);
  webgl.onContextLoss(() => {}).dispose();
  webgl.dispose();
  const fit = new m.FitAddon();
  t.loadAddon(fit);
  assert.equal(fit.proposeDimensions(), undefined, 'not opened');
  assert.throws(() => t.loadAddon({ activate() {}, dispose() {}, findNext() {} }), /addon-search/);
  assert.throws(() => t.loadAddon({ activate() {}, dispose() {}, serialize() {} }), /ansiSnapshot/);
  assert.throws(() => t.loadAddon({ activate(term) { term.registerLinkProvider({}); }, dispose() {} }), /unsupported addon/);
  assert.throws(() => t.loadAddon({ activate() {}, dispose() {} }), /unsupported addon/);
  t.dispose();
});

test('kittyKeyboardFlags and modes are current right after a write, without any frame', async () => {
  const t = new m.Terminal({ vtExtensions: { kittyKeyboard: true } });
  await write(t, '\x1b[>5u\x1b[?2004h');
  assert.equal(t.kittyKeyboardFlags, 5);
  assert.equal(t.modes.bracketedPasteMode, true);
  await write(t, '\x1b[<u');
  assert.equal(t.kittyKeyboardFlags, 0);
  t.dispose();
  assert.equal(t.kittyKeyboardFlags, 0);
});

test('reset clears the screen at once', async () => {
  const t = new m.Terminal({ cols: 10, rows: 2 });
  await write(t, 'text');
  t.reset();
  assert.equal(t.buffer.active.getLine(0).translateToString(true), '');
  t.dispose();
});

test('an engine crash inside a listener settles the queued callbacks in order and later writes work', async (t) => {
  t.mock.method(console, 'warn', () => {});
  const term = new m.Terminal({ cols: 10, rows: 2 });
  const replies = [];
  let crashed = false;
  term.onData((data) => {
    replies.push(data);
    if (!crashed) {
      crashed = true;
      assert.throws(() => __trapForTest(), WebAssembly.RuntimeError);
    }
  });
  const settled = [];
  term.write('boom\x1b[6n', () => settled.push('boom'));
  term.write('x', () => settled.push('x'));
  await write(term, '\x1b[H\x1b[2Kafter\x1b[6n');
  settled.push('after');
  assert.deepEqual(settled, ['boom', 'x', 'after']);
  assert.deepEqual(replies.at(-1), '\x1b[1;6R');
  assert.equal(console.warn.mock.callCount(), 1);
  term.dispose();
});

test('size and options still read after dispose, like xterm.js', () => {
  const t = new m.Terminal({ cols: 12, rows: 3 });
  const x = new xtermHeadless.Terminal({ cols: 12, rows: 3, allowProposedApi: true });
  t.dispose();
  x.dispose();
  assert.deepEqual([t.cols, t.rows, t.options.cols], [x.cols, x.rows, x.options.cols]);
});

test('dispose drops a pending drain and later writes', async () => {
  const t = new m.Terminal();
  let parsed = 0;
  t.onWriteParsed(() => parsed++);
  let done = false;
  t.write('x'.repeat(16 * 1024 * 1024), () => { done = true; });
  await new Promise((resolve) => { const d = t.onWriteParsed(() => { d.dispose(); resolve(); }); });
  t.dispose();
  t.write('more');
  await new Promise((resolve) => setTimeout(resolve, 50));
  assert.equal(done, false);
  assert.equal(parsed, 1);
});
