// The Rust front end (wongtty/xterm) in Node: the write queue, events, options and the crate's re-entrancy and
// crash rules. Drawing and input run in the browser tests.
import { test, before } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { init, Terminal } from '../dist/xterm.js';
import { __trapForTest } from '../dist/index.js';

before(async () => {
  await init(undefined, readFileSync(new URL('../dist/xterm_web_bg.wasm', import.meta.url)));
});

/** Resolves after `term` has parsed everything written so far. */
const parsed = (term) => new Promise((resolve) => term.write('', resolve));

test('replies, title and bell reach their events after the write is parsed', async () => {
  const term = new Terminal({ cols: 20, rows: 5 });
  const seen = [];
  term.onData((data) => seen.push(['data', data]));
  term.onTitleChange((title) => seen.push(['title', title]));
  term.onBell(() => seen.push(['bell']));
  term.write('ab\x1b[6n\x1b]0;hi\x07\x07');
  assert.deepEqual(seen, [], 'nothing fires before the queue drains');
  await parsed(term);
  assert.deepEqual(seen, [['data', '\x1b[1;3R'], ['title', 'hi'], ['bell']]);
  term.dispose();
});

test('write and writeln callbacks run in order, after their own data, and a surrogate pair may be split across writes', async () => {
  const term = new Terminal({ cols: 20, rows: 5 });
  const order = [];
  term.write('\ud83d', () => order.push(1));
  term.write('\ude00\x1b[6n', () => order.push(2));
  term.writeln('', () => order.push(3));
  term.write('\x1b[6n');
  const replies = [];
  term.onData((data) => replies.push(data));
  await parsed(term);
  assert.deepEqual(order, [1, 2, 3]);
  assert.deepEqual(replies, ['\x1b[1;3R', '\x1b[2;1R'], 'the emoji is one wide character, and writeln ends the line');
  term.dispose();
});

test('listeners may write, resize and dispose the terminal from inside an event', async () => {
  const term = new Terminal({ cols: 20, rows: 5 });
  const sizes = [];
  term.onResize((size) => sizes.push(size));
  let replies = 0;
  term.onData(() => {
    replies++;
    if (replies === 1) {
      term.write('\x1b[6n');
      term.resize(30, 6);
    } else {
      term.dispose();
      term.write('ignored');
    }
  });
  term.write('\x1b[6n');
  await new Promise((resolve) => setTimeout(resolve, 50));
  assert.equal(replies, 2);
  assert.deepEqual(sizes, [{ cols: 30, rows: 6 }]);
  assert.doesNotThrow(() => term.dispose());
});

test('after an engine crash the terminal starts over and keeps working', async () => {
  const term = new Terminal({ cols: 20, rows: 5 });
  const replies = [];
  term.onData((data) => replies.push(data));
  term.write('before');
  await parsed(term);
  assert.throws(() => __trapForTest(), WebAssembly.RuntimeError);
  term.write('\x1b[Hab\x1b[6n');
  await parsed(term);
  assert.deepEqual(replies.at(-1), '\x1b[1;3R');
  term.dispose();
});

test('options: init-only keys throw when changed, others are stored, the clipboard handler answers OSC 52', async () => {
  const written = [];
  const term = new Terminal({ cols: 20, rows: 5, clipboard: { write: (text, selection) => written.push([selection, text]) } });
  assert.equal(term.options.cols, 20);
  assert.throws(() => { term.options.cols = 30; }, /can only be set in the constructor/);
  term.options.fontSize = 12;
  assert.equal(term.options.fontSize, 12);
  term.write('\x1b]52;c;aGk=\x07');
  await parsed(term);
  assert.deepEqual(written, [['c', 'hi']]);
  term.options = { clipboard: { read: () => Promise.resolve('pasted') } };
  const replies = [];
  term.onData((data) => replies.push(data));
  term.write('\x1b]52;c;?\x07');
  await parsed(term);
  await new Promise((resolve) => setTimeout(resolve, 10));
  assert.deepEqual(replies, ['\x1b]52;c;cGFzdGVk\x07']);
  term.dispose();
});

test('options.windowsPty: growing rows adds blank rows instead of pulling back scrollback, like xterm.js', async () => {
  const cursorRowAfterGrow = async (options) => {
    const term = new Terminal({ cols: 20, rows: 5, ...options });
    term.write(Array.from({ length: 10 }, (_, i) => `${i}`).join('\r\n'));
    await parsed(term);
    term.resize(20, 8);
    const reply = new Promise((resolve) => term.onData(resolve));
    term.write('\x1b[6n');
    const row = Number((await reply).match(/\[(\d+);/)[1]);
    term.dispose();
    return row;
  };
  assert.equal(await cursorRowAfterGrow({}), 8, 'scrollback comes back down');
  assert.equal(await cursorRowAfterGrow({ windowsPty: { backend: 'conpty', buildNumber: 19045 } }), 5);
});

test('onCursorMove and onScroll fire after output moves the cursor and scrolls the viewport', async () => {
  const term = new Terminal({ cols: 10, rows: 3 });
  const seen = [];
  term.onCursorMove(() => seen.push('cursor'));
  term.onScroll((top) => seen.push(`scroll ${top}`));
  term.write('a');
  await parsed(term);
  term.write('\r\n\r\n\r\n');
  await parsed(term);
  assert.deepEqual(seen, ['cursor', 'scroll 1', 'cursor']);
  term.dispose();
});
