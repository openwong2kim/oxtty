// Run after scripts/build-node.sh:  node --test crates/wongtty-node/test.cjs
const { test } = require('node:test');
const assert = require('node:assert/strict');
const path = require('node:path');
const { Terminal } = require(path.join(__dirname, 'wongtty.node'));

test('native binding mirrors the wasm API', () => {
  const t = new Terminal(80, 24, 100);
  t.write('안녕 \x1b[31mred\x1b[0m\r\n');
  t.write(Buffer.from('\x1b[3;7H\x1b[6n\x1b]0;native\x07'));
  assert.equal(t.rowText(0), '안녕 red');
  assert.equal(t.cols, 80);
  assert.equal(t.rows, 24);
  assert.deepEqual(t.cursor(), { line: 2, col: 6, visible: true, shape: 'block', blink: false, styled: false });
  assert.equal(t.takeReplies().toString(), '\x1b[3;7R');
  assert.equal(t.takeTitle(), 'native');
  const cells = t.rowCells(0);
  assert.ok(cells instanceof Uint32Array);
  assert.equal(cells.length, 80 * 4);
  assert.equal(String.fromCodePoint(cells[0]), '안');
  assert.equal(t.rowCells(99), null);
  assert.equal(t.takeDamage().full, true);
  t.write('x');
  const d = t.takeDamage();
  assert.equal(d.full, false);
  assert.ok(d.lines.includes(2));
});

test('native sync flush and scrollback', () => {
  const t = new Terminal(20, 5);
  t.write('\x1b[?2026hHELD');
  assert.equal(t.syncPending(), true);
  assert.equal(t.rowText(0), 'HELD');
  t.flushSync();
  assert.equal(t.syncPending(), false);
  for (let i = 0; i < 10; i++) t.write(`\r\nline ${i}`);
  assert.ok(t.historyLength > 0);
  assert.equal(typeof t.trimmedLines, 'number');
  assert.equal(t.rowText(-t.historyLength), 'HELD');
});

test('native extras, cursor shape, view scroll', () => {
  const t = new Terminal(20, 5, 100);
  t.write('é\x1b[58;5;196mU\x1b[0m\x1b]8;;https://wongtty.dev\x1b\\L\x1b]8;;\x1b\\\x1b[3 q');
  const extras = t.rowExtras(0);
  assert.equal(extras[0].text, 'é');
  assert.equal(extras[1].underlineColor, (2 << 24) | 196);
  assert.equal(extras[2].hyperlink.uri, 'https://wongtty.dev');
  assert.deepEqual([t.cursor().shape, t.cursor().blink], ['underline', true]);
  for (let i = 0; i < 10; i++) t.write('\r\nline');
  t.scrollDisplay(2);
  assert.equal(t.displayOffset, 2);
  t.scrollToBottom();
  assert.equal(t.displayOffset, 0);
  t.takeDamage();
  t.write('\r\nx');
  assert.deepEqual(t.takeDamage(), { full: false, scroll: 1, lines: [3, 4] });
});

test('native ansi snapshot', () => {
  const t = new Terminal(20, 5);
  t.write('\x1b[44mblue\x1b[0m\x1b[3');
  const s = t.ansiSnapshot();
  assert.equal(s.ok, true);
  assert.ok(Buffer.isBuffer(s.payload));
  assert.ok(s.payload.toString().endsWith('\x1b[?25h\x1b[3'));
  const copy = new Terminal(20, 5);
  copy.write(s.payload);
  assert.deepEqual(copy.rowCells(0), t.rowCells(0));
  t.write('m\x1b[2;3r');
  assert.deepEqual(t.ansiSnapshot(), { ok: false, reason: 'margins' });
});

test('native host policy: clipboard and colour queries', () => {
  const t = new Terminal(80, 24);
  const colors = new Uint32Array(259);
  colors[256] = 0xcccccc;
  t.setColors(colors);
  t.setClipboardAccess(true, true);
  t.write('\x1b]10;?\x07\x1b]52;c;aGk=\x07\x1b]52;c;?\x07');
  assert.deepEqual(t.takeClipboardWrite(), { selection: 'c', text: 'hi' });
  assert.equal(t.takeClipboardRead(), 'c');
  assert.equal(t.replyClipboard('c', 'ok'), true);
  assert.equal(t.takeReplies().toString(), '\x1b]10;rgb:cccc/cccc/cccc\x1b\\\x1b]52;c;b2s=\x07');
  assert.throws(() => t.setColors(new Uint32Array(3)));
  t.setColors(null);
  t.write('\x1b]10;?\x07');
  assert.equal(t.takeReplies().length, 0);
});

test('native parser hooks', () => {
  const t = new Terminal(20, 5);
  t.setHook('csi', 0x6e, true);
  t.setHook('osc', 133, true);
  const bytes = Buffer.from('a\x1b]133;A\x07b\x1b[6nc');
  let fed = t.feedUntilHook(bytes);
  assert.deepEqual(t.pendingHook(), { kind: 'osc', id: 133, params: [], data: Buffer.from('A') });
  t.finishHook(true);
  fed += t.feedUntilHook(bytes.subarray(fed));
  assert.deepEqual(t.pendingHook(), { kind: 'csi', id: 0x6e, params: [[6]], data: Buffer.alloc(0) });
  t.finishHook(false);
  fed += t.feedUntilHook(bytes.subarray(fed));
  assert.equal(fed, bytes.length);
  assert.equal(t.pendingHook(), null);
  assert.equal(t.rowText(0), 'abc');
  assert.equal(t.takeReplies().length, 0);
});

test('native setFocus answers CSI ? 1004 h', () => {
  const t = new Terminal(20, 5);
  t.write('\x1b[?1004h');
  t.setFocus(true);
  t.write('\x1b[?1004h');
  t.setFocus(null);
  t.write('\x1b[?1004h');
  assert.equal(t.takeReplies().toString(), '\x1b[I');
  t.setFocus(false);
  t.write('\x1b[6n\x1b[?1004h');
  assert.deepEqual(t.takeReplyList().map(String), ['\x1b[1;1R', '\x1b[O']);
  assert.deepEqual(t.takeReplyList(), []);
});

test('native windowsPty', () => {
  const t = new Terminal(10, 3);
  t.setWindowsPty('conpty', 19045);
  t.write('0123456789\r\n');
  assert.equal(t.rowWrapped(0), true);
  assert.throws(() => t.setWindowsPty('pty', 1), /unknown backend/);
});

test('textRows: translateToString of each line in one call', () => {
  const t = new Terminal(10, 2, 50);
  t.write('a\r\n한 b  \r\nwrapped line');
  // Typed spaces stay, as in translateToString(true)
  assert.equal(t.textRows(-2, 1), 'a\n한 b  \nwrapped li\nne\n');
  assert.equal(t.textRows(1, 1, false), 'ne        \n');
  assert.equal(t.textRows(-9, -2), 'a\n');
});

test('native search mirrors the wasm one (without regex)', () => {
  const t = new Terminal(10, 3, 50);
  t.write('foo one\r\n0123456789abcdef\r\n한글foo');
  assert.deepEqual(t.searchNext('89ab', null, -2, 0), { startLine: 0, startCol: 8, endLine: 1, endCol: 2 });
  assert.deepEqual(t.searchPrevious('FOO', { caseSensitive: false }, 2, 10), { startLine: 2, startCol: 4, endLine: 2, endCol: 7 });
  assert.equal(t.searchNext('FOO', { caseSensitive: true }, -2, 0), null);
  assert.equal(t.searchRows('foo', {}, -2, 2).length, 2);
  const line = t.searchLine(2);
  assert.equal(line.text, '한글foo');
  assert.deepEqual(Array.from(line.cells.slice(0, 3)), [0x80000000, 0x80000002, 4]);
});

test('native markers', () => {
  const t = new Terminal(10, 2, 3);
  t.write('a\r\nb');
  const id = t.addMarker(1);
  assert.equal(t.addMarker(5), null);
  t.write('\r\nc\r\nd');
  assert.equal(t.markerLine(id), 1);
  t.write('\r\ne\r\nf\r\ng');
  assert.equal(t.markerLine(id), null);
  assert.deepEqual(t.takeDisposedMarkers(), [id]);
});

test('native ansi snapshot options', () => {
  const t = new Terminal(10, 2, 50);
  for (let i = 0; i < 6; i++) t.write(`line ${i}\r\n`);
  t.write('\x1b[?1h');
  const cold = t.ansiSnapshot({ scrollback: 0, excludeModes: true });
  const copy = new Terminal(10, 2, 50);
  copy.write(cold.payload);
  assert.equal(copy.historyLength, 0);
  assert.equal(copy.modes() & 2, 0);
  assert.ok(t.ansiSnapshot().payload.length > cold.payload.length);
});

test('native image tiles', () => {
  const t = new Terminal(10, 4);
  t.write('\x1b[31m');
  assert.deepEqual(t.placeImage(5, 2, 2, { cursor: 'iip' }), { tiles: 4, covered: [] });
  assert.deepEqual(Array.from(t.rowImages(1)), [0, 5, 2, 1, 5, 3]);
  assert.deepEqual(t.cursor().col, 2);
  assert.equal(t.pen()[0], (1 << 24) | 1);
});

test('native rowCellsInto and viewportCellsInto fill the caller buffer like rowCells', () => {
  const t = new Terminal(6, 3, 20);
  for (let i = 0; i < 8; i++) t.write(`\r\n\x1b[3${i % 8}m안${i}\x1b[0m`);
  t.scrollDisplay(4);
  const out = new Uint32Array(6 * 4 + 2).fill(7);
  assert.equal(t.rowCellsInto(-2, out), 6 * 4);
  assert.deepEqual(out.subarray(0, 24), t.rowCells(-2));
  assert.deepEqual(Array.from(out.subarray(24)), [7, 7]);
  assert.equal(t.rowCellsInto(-100, out), -1);
  assert.throws(() => t.rowCellsInto(0, new Uint32Array(23)), /holds 23 words, the cells need 24/);
  const view = new Uint32Array(3 * 6 * 4);
  assert.equal(t.viewportCellsInto(view), view.length);
  for (let y = 0; y < 3; y++) assert.deepEqual(view.subarray(y * 24, y * 24 + 24), t.rowCells(y - t.displayOffset));
  // A view into a larger buffer is written at its own offset
  const backing = new Uint32Array(view.length + 8).fill(9);
  assert.equal(t.viewportCellsInto(backing.subarray(8)), view.length);
  assert.deepEqual(backing.subarray(8), view);
  assert.deepEqual(Array.from(backing.subarray(0, 8)), [9, 9, 9, 9, 9, 9, 9, 9]);
  // A SharedArrayBuffer view is written the same way (the host owns any cross-thread synchronisation)
  const shared = new Uint32Array(new SharedArrayBuffer(view.length * 4));
  assert.equal(t.viewportCellsInto(shared), view.length);
  assert.deepEqual(Array.from(shared), Array.from(view));
  assert.equal(t.rowCellsInto(-2, shared), 6 * 4);
  assert.deepEqual(Array.from(shared.subarray(0, 24)), Array.from(t.rowCells(-2)));
  assert.throws(() => t.viewportCellsInto(new Uint32Array(view.length - 1)), /need 72/);
});

test('index.d.ts declares every method and getter the binding exports', () => {
  const dts = require('node:fs').readFileSync(path.join(__dirname, 'index.d.ts'), 'utf8');
  const declared = new Set([...dts.matchAll(/^ {2}(?:get |set |static )?(\w+)\(/gm)].map((m) => m[1]));
  const exported = Object.getOwnPropertyNames(Terminal.prototype).filter((n) => n !== 'constructor');
  exported.push(...Object.getOwnPropertyNames(Terminal).filter((n) => typeof Terminal[n] === 'function'));
  assert.deepEqual(exported.filter((n) => !declared.has(n)), []);
});

test('textRows and searchRows of the wasm build give what the native ones give on the same output', async () => {
  const wasm = await import('../../packages/wongtty/dist/index.js');
  await wasm.init();
  let seed = 11;
  const next = (n) => {
    seed = (seed * 1103515245 + 12345) >>> 0;
    return (seed >>> 8) % n;
  };
  // KELVIN SIGN and İ fold to ASCII; wide and combining characters, colours, typed and erased blanks, wraps
  const tokens = ['ab', 'Ab', 'K', 'K', 'İ', 'i', ' ', 'x_y', '漢', '\x1b[32mb\x1b[0m', 'é', '\x1b[3X', '-', '\r\n', '\r\n'];
  for (const cols of [9, 24]) {
    const native = new Terminal(cols, 5, 1500);
    const web = new wasm.Terminal({ cols, rows: 5, scrollback: 1500 });
    let text = '';
    for (let i = 0; i < 5000; i++) text += tokens[next(tokens.length)];
    native.write(text);
    web.write(text);
    const [top, bottom] = [-native.historyLength, native.rows - 1];
    assert.equal(-web.historyLength, top);
    for (const trim of [true, false]) {
      assert.equal(web.textRows(top, bottom, trim), native.textRows(top, bottom, trim));
      assert.equal(web.textRows(top + 7, top + 30, trim), native.textRows(top + 7, top + 30, trim));
    }
    for (const q of ['ab', 'k', 'i', 'ik', 'b a', 'x_y', 'B', 'zz']) {
      for (const o of [{}, { caseSensitive: true }, { wholeWord: true }]) {
        assert.deepEqual(web.searchRows(q, o, top, bottom), native.searchRows(q, o, top, bottom), `${cols} ${q} ${JSON.stringify(o)}`);
        assert.deepEqual(web.searchRows(q, o, top + 50, top + 80), native.searchRows(q, o, top + 50, top + 80));
      }
    }
    web.dispose();
  }
});
