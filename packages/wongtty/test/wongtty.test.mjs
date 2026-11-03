import { test, before } from 'node:test';
import assert from 'node:assert/strict';
import { init, Terminal, Attr, Mode, decodeColor, COLOR_SLOTS } from '../dist/index.js';

before(async () => {
  await init();
  await init(); // idempotent
});

const make = (o) => new Terminal({ cols: 80, rows: 24, ...o });

test('korean and ascii in one row, wide cells flagged', () => {
  const t = make();
  t.write('안녕 hello\r\n');
  assert.equal(t.rowText(0), '안녕 hello');
  const cells = t.rowCells(0);
  assert.equal(String.fromCodePoint(cells[0]), '안');
  assert.ok(cells[3] & Attr.WIDE);
  assert.ok(cells[4 + 3] & Attr.WIDE_SPACER);
  assert.deepEqual(t.cursor(), { line: 1, col: 0, visible: true, shape: 'block', blink: false, styled: false });
  t.dispose();
});

test('bytes split anywhere still decode', () => {
  const t = make();
  const bytes = new TextEncoder().encode('한\x1b[31mX');
  for (const b of bytes) t.write(Uint8Array.of(b));
  assert.equal(t.rowText(0), '한X');
  assert.deepEqual(decodeColor(t.rowCells(0)[2 * 4 + 1]), { kind: 'named', value: 1 });
});

test('256 and truecolor', () => {
  const t = make();
  t.write('\x1b[38;5;208mO\x1b[48;2;10;20;30mT');
  const c = t.rowCells(0);
  assert.deepEqual(decodeColor(c[1]), { kind: 'indexed', value: 208 });
  assert.deepEqual(decodeColor(c[4 + 2]), { kind: 'rgb', value: 0x0a141e });
});

test('alternate screen and modes', () => {
  const t = make();
  t.write('main\x1b[?1049h\x1b[H\x1b[2Jalt\x1b[?2004h');
  assert.ok(t.modes() & Mode.ALT_SCREEN);
  assert.ok(t.modes() & Mode.BRACKETED_PASTE);
  assert.equal(t.rowText(0), 'alt');
  t.write('\x1b[?1049l');
  assert.equal(t.rowText(0), 'main');
});

test('scrollback with negative lines', () => {
  const t = new Terminal({ cols: 20, rows: 5, scrollback: 100 });
  for (let i = 0; i < 12; i++) t.write(`line ${i}\r\n`);
  assert.equal(t.historyLength, 8);
  assert.equal(t.rowText(-8), 'line 0');
  assert.equal(t.rowText(0), 'line 8');
  assert.equal(t.rowText(-9), null);
});

test('textRows is translateToString of each line, across blocks of rows and the compressed history', () => {
  const t = make({ cols: 10, rows: 3, scrollback: 3000 });
  for (let i = 0; i < 2500; i++) t.write(`${i} 한\x1b[31mé\x1b[0m é \r\n`);
  t.write('wrapped line of text');
  const lines = [];
  for (let l = -t.historyLength; l < t.rows; l++) lines.push(t.rowText(l));
  // rowText trims every trailing blank, translateToString(true) only the empty cells.
  assert.equal(t.textRows().split('\n').map((l) => l.trimEnd()).join('\n'), lines.join('\n') + '\n');
  // Row by row is the same text as in one call (which reads blocks of 1024 rows).
  let rows = '';
  for (let l = -t.historyLength; l < t.rows; l++) rows += t.textRows(l, l);
  assert.equal(t.textRows(), rows);
  // The typed space that did not fit wrapped onto row 0, where translateToString(true) keeps it.
  assert.equal(t.textRows(-1, 0), '2499 한é é\n \n');
  t.write('\x1b[K\r\n');
  assert.equal(t.textRows(1, 2, false), 'ne of text\n          \n');
  assert.equal(t.textRows(5, 9), '');
  assert.equal(t.textRows(-9999, -t.historyLength), t.textRows(-t.historyLength, -t.historyLength));
});

test('damage lists changed rows', () => {
  const t = make();
  assert.deepEqual(t.takeDamage(), { full: true, scroll: 0, lines: [] });
  t.write('\x1b[3;1Hhello');
  const d = t.takeDamage();
  assert.equal(d.full, false);
  assert.ok(d.lines.includes(2));
  t.resize(100, 30);
  assert.equal(t.takeDamage().full, true);
  assert.equal(t.cols, 100);
});

test('rowCellsView matches rowCells without copying', () => {
  const t = make();
  t.write('\x1b[31m한x\r\nsecond');
  for (const y of [0, 1, t.rows - 1]) assert.deepEqual(Array.from(t.rowCellsView(y)), Array.from(t.rowCells(y)));
  assert.equal(t.rowCellsView(t.rows), null);
  const view = t.rowCellsView(0);
  t.rowCells(1); // the next call overwrites the same memory
  assert.equal(view[0], 's'.codePointAt(0));
});

test('cols and rows follow the engine size after resize', () => {
  const t = make();
  for (const [cols, rows] of [[100, 30], [1, 1], [0, 0]]) {
    t.resize(cols, rows);
    assert.equal(t.rowCells(0).length / 4, t.cols);
    assert.notEqual(t.rowCells(t.rows - 1), null);
    assert.equal(t.rowCells(t.rows), null);
  }
  t.dispose();
  assert.throws(() => t.cols, /disposed/);
});

test('replies, title, bell', () => {
  const t = make();
  t.write('\x1b[3;7H\x1b[6n\x1b]2;build\x07\x07');
  assert.equal(new TextDecoder().decode(t.takeReplies()), '\x1b[3;7R');
  assert.equal(t.takeReplies().length, 0);
  assert.equal(t.takeTitle(), 'build');
  assert.equal(t.takeTitle(), null);
  assert.equal(t.takeBells(), 1);
  t.write('out\x1b[c\x1b[6n\x1b[4$p\r\n');
  assert.deepEqual(t.takeReplyList().map((r) => new TextDecoder().decode(r)), ['\x1b[?1;2c', '\x1b[3;10R', '\x1b[4;2$y']);
  assert.deepEqual(t.takeReplyList(), []);
});

test('synchronized output: applied at once, open until the program ends it', () => {
  const t = make();
  t.write('\x1b[?2026hSYNC');
  assert.equal(t.syncPending(), true);
  assert.equal(t.rowText(0), 'SYNC');
  t.write('\x1b[?2026l');
  assert.equal(t.syncPending(), false);
  t.dispose();
});

test('synchronized output: timeout flushes and notifies', async () => {
  let flushed = 0;
  const t = make({ syncTimeoutMs: 20, onSyncFlush: () => flushed++ });
  t.write('\x1b[?2026hLATE');
  assert.equal(t.rowText(0), 'LATE');
  await new Promise((r) => setTimeout(r, 60));
  assert.equal(flushed, 1);
  assert.equal(t.syncPending(), false);
  t.dispose();
});

test('terminals are independent and disposable', () => {
  const a = make();
  const b = make();
  a.write('A');
  b.write('B');
  assert.equal(a.rowText(0), 'A');
  assert.equal(b.rowText(0), 'B');
  a.dispose();
  a.dispose();
  assert.throws(() => a.rowText(0), /disposed/);
  assert.equal(b.rowText(0), 'B');
  const c = make(); // reuses a's slot
  assert.equal(c.rowText(0), '');
});

test('large write grows the input buffer', () => {
  const t = new Terminal({ cols: 80, rows: 24, scrollback: 10000 });
  const chunk = 'x'.repeat(79) + '\r\n';
  t.write(chunk.repeat(5000));
  assert.equal(t.historyLength, 5000 - 23);
  assert.equal(t.rowText(0), 'x'.repeat(79));
});

test('row extras: combining, underline colour, shared hyperlink', () => {
  const t = make();
  t.write('é\x1b[4:3;58;2;255;0;0mX\x1b[0m\x1b]8;id=a;https://wongtty.dev\x1b\\LN\x1b]8;;\x1b\\Z');
  const cells = t.rowCells(0);
  assert.ok(cells[1 * 4 + 3] & Attr.UNDERLINE_COLOR);
  assert.ok(cells[2 * 4 + 3] & Attr.HYPERLINK);
  const extras = t.rowExtras(0);
  assert.deepEqual(extras.map((e) => e.col), [0, 1, 2, 3]);
  assert.equal(extras[0].text, 'é');
  assert.deepEqual(decodeColor(extras[1].underlineColor), { kind: 'rgb', value: 0xff0000 });
  assert.deepEqual(extras[2].hyperlink, { id: 'a', uri: 'https://wongtty.dev' });
  assert.equal(extras[2].hyperlink, extras[3].hyperlink);
  assert.deepEqual(t.rowExtras(1), []);
  assert.equal(t.rowExtras(99), null);
});

test('cursor shape, view scrolling, scroll damage', () => {
  const t = make({ cols: 20, rows: 5 });
  t.write('\x1b[5 q');
  assert.deepEqual([t.cursor().shape, t.cursor().blink], ['bar', true]);
  t.write('a\r\nb\r\nc\r\nd\r\ne');
  t.takeDamage();
  t.write('\r\nf');
  assert.deepEqual(t.takeDamage(), { full: false, scroll: 1, lines: [3, 4] });
  t.scrollDisplay(1);
  assert.equal(t.displayOffset, 1);
  assert.equal(t.takeDamage().full, true);
  t.scrollToBottom();
  assert.equal(t.displayOffset, 0);
});

test('ansi snapshot: payload rebuilds the screen, refusal names the reason', () => {
  const t = make({ cols: 20, rows: 5 });
  t.write('\x1b[?2026h\x1b[31mred\x1b[3');
  const s = t.ansiSnapshot();
  assert.equal(s.ok, true);
  assert.ok(s.payload instanceof Uint8Array);
  assert.deepEqual(t.ansiSnapshot(), s);
  assert.equal(t.syncPending(), true);
  const copy = make({ cols: 20, rows: 5 });
  copy.write(s.payload);
  copy.write('4mX\x1b[?2026l');
  t.write('4mX\x1b[?2026l');
  assert.equal(copy.rowText(0), 'redX');
  assert.deepEqual(copy.rowCells(0), t.rowCells(0));
  t.write('\x1b[2;3r');
  assert.deepEqual(t.ansiSnapshot(), { ok: false, reason: 'margins' });
});

test('host policy: colour queries and OSC 52 only with the host, in xterm.js reply format', () => {
  const t = make();
  t.write('\x1b]11;?\x07\x1b]52;c;aGk=\x07');
  assert.equal(t.takeReplies().length, 0);
  assert.equal(t.takeClipboardWrite(), null);
  const colors = new Array(COLOR_SLOTS).fill(0);
  colors[257] = 0x1e1e2e;
  t.setColors(colors);
  t.setClipboardAccess({ read: true, write: true });
  t.write('\x1b]11;?\x07\x1b]52;c;aGk=\x07\x1b]52;c;?\x1b\\');
  assert.deepEqual(t.takeClipboardWrite(), { selection: 'c', text: 'hi' });
  assert.equal(t.takeClipboardRead(), 'c');
  assert.equal(t.replyClipboard('c', '한글'), true);
  assert.equal(new TextDecoder().decode(t.takeReplies()), '\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b]52;c;7ZWc6riA\x07');
  t.setColors(null);
  t.write('\x1b]11;?\x07');
  assert.equal(t.takeReplies().length, 0);
  assert.throws(() => t.setColors([1, 2, 3]), RangeError);
  t.dispose();
});

test('a trap rebuilds the instance and every terminal starts over at its size with its settings', async () => {
  const { __trapForTest } = await import('../dist/index.js');
  const crashes = [];
  const a = make({ cols: 30, rows: 5, onCrash: (e) => crashes.push(['a', e.message]) });
  const b = make({ cols: 20, rows: 4, onCrash: (e) => crashes.push(['b', e.message]) });
  a.write('before');
  const colors = new Array(COLOR_SLOTS).fill(0);
  colors[257] = 0x102030;
  a.setColors(colors);
  a.setClipboardAccess({ write: true });
  assert.throws(() => __trapForTest(), WebAssembly.RuntimeError);
  assert.equal(a.rowText(0), '');
  assert.deepEqual([a.cols, a.rows, b.cols, b.rows], [30, 5, 20, 4]);
  // Each terminal starts over on its own next call (here, reading its size).
  assert.deepEqual(crashes.map((c) => c[0]), ['a', 'b']);
  a.write('\x1b]11;?\x07\x1b]52;c;aGk=\x07after');
  assert.equal(a.rowText(0), 'after');
  assert.equal(new TextDecoder().decode(a.takeReplies()), '\x1b]11;rgb:1010/2020/3030\x1b\\');
  assert.deepEqual(a.takeClipboardWrite(), { selection: 'c', text: 'hi' });
  b.write('b ok');
  assert.equal(b.rowText(0), 'b ok');
  assert.equal(crashes.length, 2);
  a.dispose();
  b.dispose();
});

test('kitty keyboard flags and X10 mouse mode reach JS, and kitty stays enabled across a crash', async () => {
  const { __trapForTest } = await import('../dist/index.js');
  const t = make({ onCrash: () => {} });
  t.write('\x1b[>1u\x1b[?9h');
  assert.equal(t.kittyFlags(), 0);
  assert.ok(t.modes() & Mode.MOUSE_X10);
  t.setKittyKeyboard(true);
  t.write('\x1b[>5u');
  assert.equal(t.kittyFlags(), 5);
  t.resetKittyKeyboard();
  assert.equal(t.kittyFlags(), 0);
  assert.throws(() => __trapForTest(), WebAssembly.RuntimeError);
  t.write('\x1b[>1u');
  assert.equal(t.kittyFlags(), 1, 'the restarted engine still has the protocol enabled');
  t.dispose();
});

test('setFocus answers CSI ? 1004 h in order with other replies, and is kept across a crash', async () => {
  const { __trapForTest } = await import('../dist/index.js');
  const t = make({ onCrash: () => {} });
  t.write('\x1b[?1004h');
  assert.equal(new TextDecoder().decode(t.takeReplies()), '', 'no report without the host focus');
  t.setFocus(false);
  t.write('\x1b[5n\x1b[?1004h');
  t.setFocus(true);
  t.write('\x1b[?1004h');
  assert.equal(new TextDecoder().decode(t.takeReplies()), '\x1b[0n\x1b[O\x1b[I');
  assert.throws(() => __trapForTest(), WebAssembly.RuntimeError);
  t.write('\x1b[?1004h');
  assert.equal(new TextDecoder().decode(t.takeReplies()), '\x1b[I', 'the restarted engine still has the focus');
  t.setFocus(null);
  t.write('\x1b[?1004h');
  assert.equal(new TextDecoder().decode(t.takeReplies()), '');
  t.dispose();
});

test('trimmedLines counts lines that left the top of the scrollback', () => {
  const t = make({ cols: 10, rows: 2, scrollback: 3 });
  t.write('0\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8\r\n9');
  assert.deepEqual([t.historyLength, t.trimmedLines], [3, 5]);
  t.dispose();
});

test('parser hooks: writeUntilHook stops at a hooked sequence, finishHook decides its default, resume goes on', async () => {
  const t = make({ cols: 20, rows: 4 });
  t.setHook('csi', 0x6e, true); // CSI n
  t.setHook('osc', 133, true);
  t.setHook('dcs', 0x2471, true); // DCS $ q
  assert.equal(t.writeUntilHook('a\x1b]133;A;x\x07b\x1b[6nc\x1bP1$qm\x1b\\d'), false);
  assert.equal(t.rowText(0), 'a');
  assert.deepEqual({ ...t.pendingHook(), data: new TextDecoder().decode(t.pendingHook().data) }, { kind: 'osc', id: 133, params: [], data: 'A;x' });
  assert.throws(() => t.writeUntilHook('x'), /hook is pending/);
  t.finishHook(true);
  assert.equal(t.resume(), false);
  assert.deepEqual(t.pendingHook().params, [[6]]);
  t.finishHook(false);
  assert.equal(t.resume(), false);
  assert.deepEqual(t.pendingHook(), { kind: 'dcs', id: 0x2471, params: [[1]], data: new TextEncoder().encode('m') });
  t.finishHook(true);
  assert.equal(t.resume(), true);
  assert.equal(t.pendingHook(), null);
  assert.equal(t.rowText(0), 'abcd');
  // The DECRQSS default answered; the skipped DSR did not.
  assert.equal(new TextDecoder().decode(t.takeReplies()), '\x1bP1$r0m\x1b\\');
  // `write` runs defaults itself; a hook dropped with setHook(…, false) no longer stops.
  t.write('\x1b[6n');
  assert.deepEqual(Array.from(t.takeReplies()), Array.from(new TextEncoder().encode('\x1b[1;5R')));
  t.setHook('csi', 0x6e, false);
  assert.equal(t.writeUntilHook('\x1b[6n'), true);
  // Hooks inside a synchronized update stop where they are; flushSync only ends the update.
  t.setHook('csi', 0x6e, true);
  assert.equal(t.writeUntilHook('\x1b[?2026hE\x1b[6nF'), false);
  assert.equal(t.flushSync(), false);
  assert.equal(t.syncPending(), false);
  t.finishHook(false);
  assert.equal(t.resume(), true);
  assert.equal(t.rowText(0), 'abcdEF');
  t.dispose();
});

test('windowsPty: the conpty wrapping heuristic, and no setting is no heuristic', () => {
  const t = make({ cols: 10, rows: 3 });
  t.write('0123456789\r\n');
  assert.equal(t.rowWrapped(0), false);
  t.setWindowsPty({ backend: 'conpty', buildNumber: 19045 });
  t.write('0123456789\r\n');
  assert.equal(t.rowWrapped(1), true);
  t.setWindowsPty(undefined);
  t.write('0123456789\r\n');
  assert.equal(t.rowWrapped(2), false);
  t.dispose();
});

test('search: plain and regex over scrollback, wrapped rows and wide characters', () => {
  const t = new Terminal({ cols: 10, rows: 3, scrollback: 50 });
  // Rows -4 'foo one', -3/-2 '0123456789' 'abcdef' (wrapped), -1 '한글foo', 0/1 'Foo_x (foo' ')' (wrapped), 2 'last'
  t.write('foo one\r\n0123456789abcdef\r\n한글foo\r\nFoo_x (foo)\r\nlast');
  assert.equal(t.historyLength, 4);
  const at = (m) => m && [m.startLine, m.startCol, m.endLine, m.endCol];
  assert.deepEqual(at(t.searchNext('foo')), [-4, 0, -4, 3]);
  assert.deepEqual(at(t.searchNext('foo', {}, -4, 3)), [-1, 4, -1, 7]);
  assert.deepEqual(at(t.searchNext('89ab')), [-3, 8, -2, 2], 'a match across a soft wrap');
  assert.deepEqual(at(t.searchNext('foo', { caseSensitive: true }, 0, 0)), [0, 7, 0, 10]);
  assert.deepEqual(at(t.searchNext('foo', { wholeWord: true }, 0, 0)), [0, 7, 0, 10], '_ is a word character');
  assert.deepEqual(at(t.searchPrevious('foo')), [0, 7, 0, 10]);
  assert.deepEqual(at(t.searchPrevious('foo', {}, -1, 4)), [-4, 0, -4, 3]);
  assert.equal(t.searchNext(''), null);
  // Regex runs in JS over the same text, with the same cells
  assert.deepEqual(at(t.searchNext('글f.o', { regex: true })), [-1, 2, -1, 7]);
  assert.deepEqual(at(t.searchNext('[0-9]+[a-f]+', { regex: true })), [-3, 0, -2, 6]);
  assert.deepEqual(at(t.searchPrevious('FOO', { regex: true }, 0, 0)), [-1, 4, -1, 7]);
  assert.deepEqual(t.searchRows('foo', { regex: true }, 0, 1).map(at), [[0, 0, 0, 3], [0, 7, 0, 10]]);
  assert.deepEqual(t.searchRows('foo', { regex: true }, 1, 1), [], 'only matches on the rows asked for');
  assert.deepEqual(t.searchRows('foo', {}, -4, 2).map(at), t.searchRows('foo', { regex: true }, -4, 2).map(at));
  // A plain ASCII query is matched in the engine, a plain one with other characters here: both as the RegExp finds
  for (const [q, o] of [['foo', { wholeWord: true }], ['FOO', { caseSensitive: true }], ['89ab', {}], ['o', {}], ['글f', {}], [' ', {}]]) {
    const re = q.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
    assert.deepEqual(t.searchRows(q, o, -4, 2).map(at), t.searchRows(re, { ...o, regex: true }, -4, 2).map(at), q);
  }
  const line = t.searchLine(-1);
  assert.deepEqual([line.first, line.rows, line.text], [-1, 1, '한글foo']);
  assert.deepEqual(Array.from(line.cells.slice(0, 3)), [0x80000000, 0x80000002, 4]);
  assert.equal(t.searchLine(3), null);
  t.dispose();
});

test('searchRows of a plain ASCII query, matched in the engine, finds what the per-line RegExp path finds', () => {
  let seed = 7;
  const next = (n) => {
    seed = (seed * 1103515245 + 12345) >>> 0;
    return (seed >>> 8) % n;
  };
  // Wide and combining characters, colours, words joined by `_`, case variants, typed and erased blanks, wraps
  const tokens = ['ab', 'Ab', 'aB', ' ', 'a b', 'x', '\x1b[31mb\x1b[0ma', '字', '\x1b[1mA\x1b[22mb', '-', '_', 'é', '\x1b[2Xz', '\t', '\r\n', '\r\n'];
  const queries = ['ab', 'AB', 'a b', 'b-', 'b_a', 'ba', 'z', 'xx', 'b A', 'qq', 'a'];
  const at = (m) => [m.startLine, m.startCol, m.endLine, m.endCol];
  let matches = 0;
  for (const cols of [7, 13, 31]) {
    const t = new Terminal({ cols, rows: 4, scrollback: 2000 });
    let text = '';
    for (let i = 0; i < 6000; i++) text += tokens[next(tokens.length)];
    t.write(text);
    const [top, bottom] = [-t.historyLength, t.rows - 1];
    for (const q of queries) {
      for (const o of [{}, { caseSensitive: true }, { wholeWord: true }]) {
        const re = q.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
        const want = t.searchRows(re, { ...o, regex: true }, top, bottom).map(at);
        assert.deepEqual(t.searchRows(q, o, top, bottom).map(at), want, `${cols} ${q} ${JSON.stringify(o)}`);
        // Part of the rows: those matches that touch them
        const [first, last] = [top + 100, top + 400];
        assert.deepEqual(t.searchRows(q, o, first, last).map(at), t.searchRows(re, { ...o, regex: true }, first, last).map(at));
        const view = t.searchRowsView(q, o, top, bottom, 5);
        assert.deepEqual(Array.from(view), want.slice(0, 5).flat(), 'the first `limit` of them');
        matches += want.length;
      }
    }
    t.dispose();
  }
  assert.ok(matches > 5000, `${matches}`);
});

test('searchRows folds case as the N-API binding does; searchRowsView refuses lines that fold longer on request', () => {
  const t = new Terminal({ cols: 20, rows: 3, scrollback: 10 });
  t.write('K x\r\nx İx\r\nKX');
  const at = (m) => [m.startLine, m.startCol, m.endLine, m.endCol];
  // KELVIN SIGN lowercases to k, İ to i and U+0307 (the RegExp `i` flag folds neither to ASCII)
  assert.deepEqual(t.searchRows('k', {}, 0, 2).map(at), [[0, 0, 0, 1], [2, 0, 2, 1]]);
  assert.deepEqual(t.searchRows('i', {}, 0, 2).map(at), [[1, 2, 1, 3]]);
  assert.deepEqual(t.searchRows('ix', {}, 0, 2), [], 'U+0307 comes between');
  assert.deepEqual(t.searchRows('k', { regex: true }, 0, 2).map(at), [[2, 0, 2, 1]]);
  assert.deepEqual(Array.from(t.searchRowsView('x', { keepLength: true }, 0, 0)), [0, 2, 0, 3]);
  assert.equal(t.searchRowsView('x', { keepLength: true }, 0, 2), null, 'row 1 holds İ and a match');
  assert.equal(t.searchRowsView('x', { keepLength: true }, 0, 2, 1).length, 4, 'the limit stops before row 1');
  assert.equal(t.searchRowsView('x', { keepLength: true, caseSensitive: true }, 0, 2).length, 12);
  assert.equal(t.searchRowsView('é', {}, 0, 2), null, 'not ASCII');
  t.dispose();
});

test('markers follow their line into the scrollback and are reported when dropped', () => {
  const t = new Terminal({ cols: 10, rows: 2, scrollback: 3 });
  t.write('a\r\nb');
  const id = t.addMarker(1);
  assert.ok(id > 0);
  assert.equal(t.addMarker(5), 0, 'outside the buffer');
  t.write('\r\nc\r\nd');
  assert.equal(t.markerLine(id), 1);
  assert.equal(t.rowText(t.markerLine(id) - t.historyLength), 'b');
  assert.deepEqual(t.takeDisposedMarkers(), []);
  t.write('\r\ne\r\nf\r\ng');
  assert.equal(t.markerLine(id), -1);
  assert.deepEqual(t.takeDisposedMarkers(), [id]);
  const other = t.addMarker(0);
  t.removeMarker(other);
  t.write('\x1bc');
  assert.deepEqual(t.takeDisposedMarkers(), [], 'removed by the host, not reported');
  t.dispose();
});

test('ansi snapshot options: the newest history lines only, and the screen without modes', () => {
  const t = new Terminal({ cols: 10, rows: 2, scrollback: 50 });
  for (let i = 0; i < 6; i++) t.write(`line ${i}\r\n`);
  t.write('\x1b[?1h\x1b[?1000h\x1b[?25l\x1b[31');
  const replay = (s) => {
    assert.equal(s.ok, true);
    const copy = new Terminal({ cols: 10, rows: 2, scrollback: 50 });
    copy.write(s.payload);
    return copy;
  };
  const two = replay(t.ansiSnapshot({ scrollback: 2 }));
  assert.deepEqual([two.historyLength, two.rowText(-2), two.modes()], [2, 'line 3', t.modes()]);
  const cold = replay(t.ansiSnapshot({ scrollback: 0, excludeModes: true }));
  assert.deepEqual([cold.historyLength, cold.rowText(0), cold.rowText(1)], [0, t.rowText(0), t.rowText(1)]);
  assert.equal(cold.modes(), new Terminal({ cols: 10, rows: 2 }).modes());
  assert.equal(new TextDecoder().decode(t.ansiSnapshot({ excludeModes: true }).payload).endsWith('\x1b[31'), false);
  assert.equal(new TextDecoder().decode(t.ansiSnapshot().payload).endsWith('\x1b[31'), true);
  t.dispose();
});

test('image tiles: placed from the cursor, flagged in cells, covered by later images and text', () => {
  const t = make({ cols: 10, rows: 4 });
  t.write('ab\r\n\x1b[2;3H\x1b[41m');
  assert.deepEqual(t.placeImage(7, 3, 2), { tiles: 6, covered: [] });
  assert.deepEqual(Array.from(t.rowImages(1)), [2, 7, 0, 3, 7, 1, 4, 7, 2]);
  assert.ok(t.rowCells(2)[2 * 4 + 3] & Attr.IMAGE);
  assert.deepEqual([t.cursor().line, t.cursor().col], [2, 2]);
  assert.deepEqual(t.placeImage(8, 1, 1, { cursor: 'iip' }), { tiles: 1, covered: [7] });
  assert.equal(t.cursor().col, 3);
  t.write('XY');
  assert.deepEqual(Array.from(t.rowImages(2)), [2, 8, 0]);
  assert.equal(decodeColor(t.pen()[1]).value, 1);
  t.placeImage(9, 2, 1, { scrolling: false });
  assert.deepEqual(Array.from(t.rowImages(0)), [0, 9, 0, 1, 9, 1]);
  assert.equal(t.rowImages(99), null);
});
