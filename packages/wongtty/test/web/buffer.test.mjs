// The buffer API, modes, reset, scrolling and events of the wongtty/xterm Terminal beyond the parity cases in
// terminal.test.mjs: next to @xterm/headless where xterm.js has an answer, and odd arguments that must neither
// throw nor stop the page (the front end aborts on a panic).
import { test, before } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import xtermHeadless from '@xterm/headless';
import serializeAddon from '@xterm/addon-serialize';
import * as m from '../../dist/xterm.js';

before(async () => {
  await m.init(undefined, await readFile(new URL('../../dist/xterm_web_bg.wasm', import.meta.url)));
});

const write = (t, data) => new Promise((resolve) => t.write(data, resolve));

test('combining marks, styles and a wide character pushed to the next row read like xterm.js', async () => {
  const input = '\x1b[1;2mab\x1b[0m é\r\nabcd한x';
  const t = new m.Terminal({ cols: 5, rows: 3 });
  const x = new xtermHeadless.Terminal({ cols: 5, rows: 3, allowProposedApi: true });
  await write(t, input);
  await new Promise((resolve) => x.write(input, resolve));
  const [a, b] = [t.buffer.active, x.buffer.active];
  for (let y = 0; y < 3; y++) {
    const [la, lb] = [a.getLine(y), b.getLine(y)];
    assert.equal(la.isWrapped, lb.isWrapped, `line ${y} isWrapped`);
    for (const args of [[true], [false], [false, 2], [true, 1, 3]]) assert.equal(la.translateToString(...args), lb.translateToString(...args), `line ${y} ${args}`);
    for (let col = 0; col < 5; col++) {
      const [ca, cb] = [la.getCell(col), lb.getCell(col)];
      assert.deepEqual(
        [ca.getChars(), ca.getCode(), ca.getWidth(), !!ca.isBold(), !!ca.isDim(), !!ca.isItalic()],
        [cb.getChars(), cb.getCode(), cb.getWidth(), !!cb.isBold(), !!cb.isDim(), !!cb.isItalic()],
        `line ${y} cell ${col}`,
      );
    }
  }
  assert.equal(a.getLine(0).getCell(3).getCode(), b.getLine(0).getCell(3).getCode());
  x.dispose();
  t.dispose();
});

test('typed spaces are text and erased cells are empty, like xterm.js (trimRight keeps typed trailing spaces)', async () => {
  const input = 'a b   \r\nxy  \x1b[2D\x1b[K\r\n\x1b[5Cz  \x1b[1X\r\n  ';
  const t = new m.Terminal({ cols: 8, rows: 4 });
  const x = new xtermHeadless.Terminal({ cols: 8, rows: 4, allowProposedApi: true });
  await write(t, input);
  await new Promise((resolve) => x.write(input, resolve));
  for (let y = 0; y < 4; y++) {
    const [la, lb] = [t.buffer.active.getLine(y), x.buffer.active.getLine(y)];
    for (const args of [[true], [false], [true, 1, 7]]) assert.equal(la.translateToString(...args), lb.translateToString(...args), `line ${y} ${args}`);
    for (let col = 0; col < 8; col++) {
      assert.deepEqual([la.getCell(col).getChars(), la.getCell(col).getCode()], [lb.getCell(col).getChars(), lb.getCell(col).getCode()], `line ${y} cell ${col}`);
    }
  }
  assert.equal(t.buffer.active.getLine(0).translateToString(true), 'a b   ');
  x.dispose();
  t.dispose();
});

test('the inactive buffer reads empty and buffers keep their type across a screen switch', async () => {
  const t = new m.Terminal({ cols: 10, rows: 2 });
  await write(t, 'normal');
  const normal = t.buffer.active;
  assert.deepEqual([normal.type, t.buffer.alternate.length, t.buffer.alternate.getLine(0)], ['normal', 0, undefined]);
  await write(t, '\x1b[?1049h\x1b[Halt');
  assert.deepEqual([normal.type, t.buffer.active.type, t.buffer.normal.getLine(0)], ['normal', 'alternate', undefined]);
  assert.equal(t.buffer.alternate.getLine(0).translateToString(true), 'alt');
  await write(t, '\x1b[?1049l');
  assert.equal(normal.getLine(0).translateToString(true), 'normal');
  t.dispose();
});

test('odd arguments return undefined, clamp or read empty instead of throwing', async () => {
  const t = new m.Terminal({ cols: 4, rows: 2 });
  await write(t, 'abcd');
  const buffer = t.buffer.active;
  for (const y of [-1, 0.5, NaN, Infinity, 2, 1e300]) assert.equal(buffer.getLine(y), undefined, `getLine(${y})`);
  const line = buffer.getLine(0);
  for (const x of [-1, 0.5, NaN, 4]) assert.equal(line.getCell(x), undefined, `getCell(${x})`);
  assert.equal(line.getCell(0, buffer.getNullCell()).getChars(), 'a');
  assert.deepEqual(
    [[false, -5, 2], [false, 2, 1e9], [true, NaN], [false, 3, 1], [undefined, undefined, 2]].map((args) => line.translateToString(...args)),
    ['ab', 'cd', '', '', 'ab'],
  );
  assert.deepEqual([buffer.getNullCell().getChars(), buffer.getNullCell().getWidth(), buffer.getNullCell().getCode()], ['', 1, 0]);
  t.scrollToLine(-1e9);
  t.scrollToLine(1e9);
  t.scrollPages(1e9);
  t.dispose();
  assert.deepEqual([buffer.length, buffer.getLine(0), t.buffer.active.cursorX, t.modes.wraparoundMode], [0, undefined, 0, true]);
  t.reset();
  t.scrollToTop();
  assert.throws(() => t.ansiSnapshot(), /disposed/);
});

test('events read unbound deliver, their disposables unsubscribe, and nothing subscribes after dispose', async () => {
  const t = new m.Terminal({ cols: 10, rows: 2 });
  const { onData, onWriteParsed } = t;
  const data = [];
  let parsed = 0;
  onData((d) => data.push(d));
  const sub = onWriteParsed(() => parsed++);
  await write(t, '\x1b[6n');
  sub.dispose();
  await write(t, '\x1b[6n');
  assert.deepEqual([data, parsed], [['\x1b[1;1R', '\x1b[1;1R'], 1]);
  assert.equal(typeof onData(42).dispose, 'function');
  t.dispose();
  const late = [];
  onData((d) => late.push(d)).dispose();
  assert.deepEqual(late, []);
});

test('reset applies at once, also while a synchronized update is open', async () => {
  const t = new m.Terminal({ cols: 10, rows: 2 });
  await write(t, '\x1b[?2026hheld\x1b[?2004h');
  assert.equal(t.modes.synchronizedOutputMode, true);
  t.reset();
  assert.deepEqual(
    [t.buffer.active.getLine(0).translateToString(true), t.modes.synchronizedOutputMode, t.modes.bracketedPasteMode],
    ['', false, false],
  );
  await write(t, 'after');
  assert.equal(t.buffer.active.getLine(0).translateToString(true), 'after');
  t.dispose();
});

test('scrollPages scrolls by screens less one row and ansiSnapshot rebuilds the screen in xterm.js', async () => {
  const t = new m.Terminal({ cols: 10, rows: 3, scrollback: 50 });
  await write(t, Array.from({ length: 12 }, (_, i) => `line ${i}`).join('\r\n'));
  const scrolls = [];
  t.onScroll((y) => scrolls.push(y));
  t.scrollPages(-2);
  t.scrollPages(1);
  assert.deepEqual(scrolls, [5, 7]);
  const snapshot = t.ansiSnapshot();
  assert.equal(snapshot.ok, true);
  const x = new xtermHeadless.Terminal({ cols: 10, rows: 3, scrollback: 50, allowProposedApi: true });
  await new Promise((resolve) => x.write(snapshot.payload, resolve));
  assert.equal(x.buffer.active.getLine(11).translateToString(true).trimEnd(), 'line 11');
  x.dispose();
  t.dispose();
});

test('ansiSnapshot({ scrollback: 0, excludeModes: true }) replays like addon-serialize with the same options', async () => {
  const output = [
    Array.from({ length: 8 }, (_, i) => `old ${i}`).join('\r\n'),
    '\r\n\x1b[1;31mred\x1b[0m \x1b[48;5;22m한글\x1b[0m \x1b[4;38;2;1;2;3mrgb\x1b[0m',
    '\x1b[?1h\x1b[?2004h\x1b[?1000h\x1b[?25l\x1b[2;5Hmid\x1b[3;1H\x1b[7mlast\x1b[0m',
  ].join('');
  const options = { cols: 16, rows: 4, scrollback: 50, allowProposedApi: true };
  const t = new m.Terminal(options);
  const x = new xtermHeadless.Terminal(options);
  const serialize = new serializeAddon.SerializeAddon();
  x.loadAddon(serialize);
  await write(t, output);
  await new Promise((resolve) => x.write(output, resolve));
  const snapshot = t.ansiSnapshot({ scrollback: 0, excludeModes: true });
  assert.equal(snapshot.ok, true);
  /** What a fresh xterm.js shows after `data`: rows with their cells' colours and styles, cursor and modes. */
  const replay = async (data) => {
    const r = new xtermHeadless.Terminal(options);
    await new Promise((resolve) => r.write(data, resolve));
    const b = r.buffer.active;
    const rows = [];
    for (let y = 0; y < b.length; y++) {
      const line = b.getLine(y);
      const cells = [];
      for (let col = 0; col < line.length; col++) {
        const c = line.getCell(col);
        cells.push([c.getChars(), c.getWidth(), c.getFgColorMode(), c.getFgColor(), c.getBgColorMode(), c.getBgColor(), c.isBold(), c.isUnderline(), c.isInverse()]);
      }
      rows.push(cells);
    }
    const state = { length: b.length, cursor: [b.cursorX, b.cursorY], modes: r.modes, rows };
    r.dispose();
    return state;
  };
  const ox = await replay(snapshot.payload);
  const xt = await replay(serialize.serialize({ scrollback: 0, excludeModes: true }));
  assert.deepEqual(ox, xt);
  assert.equal(ox.length, 4, 'the screen alone');
  assert.equal(ox.modes.applicationCursorKeysMode, false, 'no modes');
  // Without options, everything comes back as before.
  assert.equal((await replay(t.ansiSnapshot().payload)).length, 9);
  x.dispose();
  t.dispose();
});
