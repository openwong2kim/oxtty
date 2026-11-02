// `terminal.parser` hooks of the Rust front end, compared with @xterm/headless 6.1 doing the same: handler
// arguments, which handler wins, skipped and kept default actions, async handlers, writes from inside a handler,
// dispose and engine crashes while a handler holds parsing; then the ways wmux uses them (OSC 133 prompt marks,
// reply overrides, DECRQSS, colour queries). A terminal without handlers never feeds through the hook API.
import { test, before } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import xtermHeadless from '@xterm/headless';
import * as m from '../../dist/xterm.js';
import { Terminal as Engine, __trapForTest } from '../../dist/index.js';

before(async () => {
  await m.init(undefined, await readFile(new URL('../../dist/xterm_web_bg.wasm', import.meta.url)));
});

const write = (t, data) => new Promise((resolve) => t.write(data, resolve));
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const text = (t) => Array.from({ length: t.rows }, (_, y) => t.buffer.active.getLine(t.buffer.active.baseY + y)?.translateToString(true) ?? '');

/** Runs `scenario(terminal, log)` on an oxtty Terminal and on @xterm/headless; returns both results. */
async function compare(options, scenario) {
  const results = [];
  for (const make of [(o) => new m.Terminal(o), (o) => new xtermHeadless.Terminal({ allowProposedApi: true, ...o })]) {
    const t = make(options);
    const log = [];
    t.onData((d) => log.push(['data', d]));
    t.onTitleChange((d) => log.push(['title', d]));
    await scenario(t, log);
    results.push({ log, text: text(t), cursor: [t.buffer.active.cursorX, t.buffer.active.cursorY] });
    t.dispose();
  }
  return results;
}

/** Errors thrown on later tasks while `fn` runs, instead of failing the test run. */
async function uncaught(fn) {
  const saved = process.listeners('uncaughtException');
  const errors = [];
  process.removeAllListeners('uncaughtException');
  process.on('uncaughtException', (err) => errors.push(err.message));
  try {
    await fn();
    await sleep(10);
  } finally {
    process.removeAllListeners('uncaughtException');
    for (const listener of saved) process.on('uncaughtException', listener);
  }
  return errors;
}

test('CSI, OSC and DCS handlers see what xterm.js handlers see and decide the default action alike', async () => {
  const [ox, xt] = await compare({ cols: 20, rows: 4 }, async (t, log) => {
    const p = t.parser;
    p.registerCsiHandler({ final: 'n' }, (params) => { log.push(['dsr', params]); return params[0] === 6; });
    p.registerCsiHandler({ prefix: '?', final: 'l' }, (params) => { log.push(['decrst', params]); return params.includes(25); });
    p.registerCsiHandler({ final: 'm' }, (params) => { log.push(['sgr', params]); return false; });
    p.registerCsiHandler({ intermediates: ' ', final: 'q' }, (params) => { log.push(['decscusr', params]); return true; });
    p.registerCsiHandler({ final: 'c' }, (params) => { log.push(['da1', params]); return false; });
    p.registerOscHandler(133, (data) => { log.push(['osc133', data]); return true; });
    p.registerOscHandler(2, (data) => { log.push(['osc2', data]); return data === 'hidden'; });
    p.registerOscHandler(7, (data) => { log.push(['osc7', data]); return true; });
    p.registerDcsHandler({ intermediates: '$', final: 'q' }, (data, params) => { log.push(['decrqss', data, params]); return true; });
    await write(t, 'a\x1b[6nb\x1b[5n\x1b[?25;7lc\x1b[1;38:2::10:20:30;4m\x1b[0m\x1b[c\x1b[;5n');
    await write(t, '\x1b]133;A;cl=m\x07\x1b]2;hidden\x1b\\\x1b]2;shown\x07\x1b]7\x07\x1b]133;D;0\x07');
    await write(t, '\x1bP1$qm\x1b\\\x1b[2 q\x1b[3;5HZ\x1b[6n');
  });
  // An empty sub-parameter is -1, an empty parameter 0.
  assert.deepEqual(xt.log.find(([k]) => k === 'sgr'), ['sgr', [1, 38, [2, -1, 10, 20, 30], 4]]);
  assert.deepEqual(ox.log, xt.log);
  assert.deepEqual(ox.text, xt.text);
  assert.deepEqual(ox.cursor, xt.cursor);
});

test('the newest handler goes first, and a disposed one no longer runs', async () => {
  const [ox, xt] = await compare({ cols: 10, rows: 2 }, async (t, log) => {
    const older = t.parser.registerCsiHandler({ final: 'n' }, () => { log.push('older'); return false; });
    const newer = t.parser.registerCsiHandler({ final: 'n' }, () => { log.push('newer'); return true; });
    await write(t, '\x1b[6n');
    newer.dispose();
    newer.dispose();
    await write(t, '\x1b[6n');
    older.dispose();
    await write(t, '\x1b[6n');
  });
  assert.deepEqual(ox.log, xt.log);
  assert.deepEqual(ox.log, ['newer', 'older', ['data', '\x1b[1;1R'], ['data', '\x1b[1;1R']]);
});

test('an async handler pauses parsing until it settles', async () => {
  const [ox, xt] = await compare({ cols: 20, rows: 2 }, async (t, log) => {
    let release;
    t.parser.registerOscHandler(1337, (data) => new Promise((resolve) => {
      log.push(['handler', data, text(t)[0]]);
      release = () => resolve(data === 'skip');
    }));
    const parsed = [];
    t.onWriteParsed(() => parsed.push('parsed'));
    t.write('ab\x1b]1337;keep\x07cd', () => parsed.push(1));
    t.write('ef', () => parsed.push(2));
    await sleep(20);
    log.push(['paused', text(t)[0], parsed.length]);
    release();
    await write(t, '');
    log.push(['resumed', text(t)[0], parsed.filter((p) => p !== 'parsed')]);
  });
  assert.deepEqual(ox.log, xt.log);
  assert.deepEqual(ox.log, [['handler', 'keep', 'ab'], ['paused', 'ab', 0], ['resumed', 'abcdef', [1, 2]]]);
});

test('async handlers that settle false fall through to older ones, sync or async, then the default', async () => {
  const [ox, xt] = await compare({ cols: 20, rows: 2 }, async (t, log) => {
    t.parser.registerCsiHandler({ final: 'n' }, () => { log.push('oldest'); return false; });
    t.parser.registerCsiHandler({ final: 'n' }, () => { log.push('older'); return Promise.resolve(false); });
    t.parser.registerCsiHandler({ final: 'n' }, () => { log.push('newest'); return new Promise((resolve) => setTimeout(() => resolve(false), 5)); });
    await write(t, 'x\x1b[6ny');
    log.push(text(t)[0]);
  });
  assert.deepEqual(ox.log, xt.log);
  assert.deepEqual(ox.log, ['newest', 'older', 'oldest', ['data', '\x1b[1;2R'], 'xy']);
});

// oxtty only: @xterm/headless leaves the rejection unhandled and lets the throw escape the write.
test('a rejected or throwing handler falls through to older handlers and the default, its error rethrown', async () => {
  const term = new m.Terminal({ cols: 10, rows: 2 });
  const log = [];
  term.onData((d) => log.push(d));
  term.parser.registerCsiHandler({ final: 'n' }, () => { log.push('older'); return false; });
  term.parser.registerCsiHandler({ final: 'n' }, () => { throw new Error('sync'); });
  term.parser.registerCsiHandler({ final: 'n' }, () => Promise.reject(new Error('async')));
  const errors = await uncaught(() => write(term, '\x1b[6n'));
  assert.deepEqual(log, ['older', '\x1b[1;1R']);
  assert.deepEqual(errors, ['async', 'sync']);
  term.dispose();
});

test('write() from inside a handler is parsed after the data being parsed', async () => {
  const [ox, xt] = await compare({ cols: 20, rows: 2 }, async (t, log) => {
    t.parser.registerOscHandler(1337, (data) => {
      t.write(`<${data}>`, () => log.push(`inner ${data}`));
      return true;
    });
    t.write('a\x1b]1337;1\x07b', () => log.push('outer 1'));
    await write(t, 'c\x1b]1337;2\x07d');
    log.push(text(t)[0]);
  });
  assert.deepEqual(ox.log, xt.log);
  // The inner writes queue behind the outer write already waiting.
  assert.deepEqual(ox.log, ['outer 1', 'inner 1', 'inner 2', 'abcd<1><2>']);
});

test('replies and titles before a hooked sequence reach the host before its handler runs', async () => {
  const [ox, xt] = await compare({ cols: 20, rows: 2 }, async (t, log) => {
    t.parser.registerOscHandler(1337, (data) => { log.push(['handler', data]); return true; });
    await write(t, '\x1b[6n\x1b]2;title\x07\x1b]1337;x\x07\x1b[5n');
  });
  assert.deepEqual(ox.log, xt.log);
  assert.deepEqual(ox.log, [['data', '\x1b[1;1R'], ['title', 'title'], ['handler', 'x'], ['data', '\x1b[0n']]);
});

test('hooks inside a synchronized update run where they are, as in xterm.js', async () => {
  const [ox, xt] = await compare({ cols: 20, rows: 2 }, async (t, log) => {
    t.parser.registerCsiHandler({ final: 'n' }, () => { log.push(['dsr', text(t)[0], t.modes.synchronizedOutputMode]); return true; });
    await write(t, '\x1b[?2026hA\x1b[6nB');
    log.push(['parsed', text(t)[0]]);
    await write(t, '\x1b[?2026lC\x1b[?2026h\x1b[6nD\x1b[?2026l');
    log.push(['parsed', text(t)[0], t.modes.synchronizedOutputMode]);
  });
  assert.deepEqual(ox, xt);
  assert.deepEqual(ox.log, [['dsr', 'A', true], ['parsed', 'AB'], ['dsr', 'ABC', true], ['parsed', 'ABCD', false]]);
});

test('handlers inside a synchronized update fire in stream order between replies and text, as in xterm.js', async () => {
  const [ox, xt] = await compare({ cols: 20, rows: 2 }, async (t, log) => {
    t.parser.registerOscHandler(133, (data) => { log.push(['osc133', data, text(t)[0]]); return true; });
    t.parser.registerCsiHandler({ final: 'n' }, (params) => { log.push(['dsr', params, text(t)[0]]); return false; });
    await write(t, '\x1b[?2026hA\x1b]133;A\x07B\x1b[6nC\x1b]133;B\x07D\x1b[5n');
    log.push(['parsed', text(t)[0], t.modes.synchronizedOutputMode]);
  });
  assert.deepEqual(ox, xt);
  assert.deepEqual(ox.log, [
    ['osc133', 'A', 'A'],
    ['dsr', [6], 'AB'],
    ['data', '\x1b[1;3R'],
    ['osc133', 'B', 'ABC'],
    ['dsr', [5], 'ABCD'],
    ['data', '\x1b[0n'],
    ['parsed', 'ABCD', true],
  ]);
});

test('an async handler inside a synchronized update holds later writes until it settles; the update times out', async () => {
  const t = new m.Terminal({ cols: 20, rows: 2 });
  let release;
  t.parser.registerOscHandler(1337, () => new Promise((resolve) => { release = resolve; }));
  const parsed = [];
  t.write('\x1b[?2026hA\x1b]1337;x\x07B', () => parsed.push('AB'));
  t.write('C', () => parsed.push('C'));
  await sleep(1300);
  assert.deepEqual([text(t)[0], parsed, t.modes.synchronizedOutputMode], ['A', [], false]);
  release(true);
  await write(t, '');
  assert.deepEqual([text(t)[0], parsed], ['ABC', ['AB', 'C']]);
  t.dispose();
});

test('invalid identifiers throw as in xterm.js', async () => {
  const t = new m.Terminal();
  const x = new xtermHeadless.Terminal({ allowProposedApi: true });
  for (const id of [{ final: 'nn' }, { final: '\x10' }, { prefix: 'a', final: 'n' }, { prefix: '??', final: 'n' }, { intermediates: '$$$', final: 'p' }, { intermediates: 'a', final: 'p' }]) {
    let expected;
    try { x.parser.registerCsiHandler(id, () => true); } catch (err) { expected = err.message; }
    assert.ok(expected, JSON.stringify(id));
    assert.throws(() => t.parser.registerCsiHandler(id, () => true), { message: expected }, JSON.stringify(id));
    assert.throws(() => t.parser.registerDcsHandler(id, () => true), { message: expected }, JSON.stringify(id));
  }
  assert.throws(() => t.parser.registerOscHandler(-1, () => true));
  assert.throws(() => t.parser.registerOscHandler(1.5, () => true));
  t.dispose();
  x.dispose();
});

test('reset() while an async handler holds parsing applies at once, the rest of that write after it', async () => {
  const [ox, xt] = await compare({ cols: 20, rows: 2 }, async (t, log) => {
    let release;
    t.parser.registerOscHandler(1337, () => new Promise((resolve) => { release = resolve; }));
    const done = write(t, 'ab\x1b]1337;x\x07cd');
    const later = write(t, 'ef');
    await sleep(10);
    t.reset();
    await sleep(10);
    log.push(text(t)[0]);
    release(false);
    await done;
    await later;
    log.push(text(t)[0]);
  });
  assert.deepEqual(ox.log, xt.log);
  assert.deepEqual(ox.log, ['', 'cdef']);
});

// oxtty only: @xterm/headless has no focus and sends no report.
test('CSI ? 1004 h reports the focus unless a handler takes the sequence', async () => {
  const t = new m.Terminal({ cols: 10, rows: 2 });
  const data = [];
  t.onData((d) => data.push(d));
  // Not opened, so not focused: xterm.js reports focus out, in order with the other replies.
  await write(t, '\x1b[6n\x1b[?1004h\x1b[5n');
  const taken = t.parser.registerCsiHandler({ prefix: '?', final: 'h' }, (params) => params.includes(1004));
  await write(t, '\x1b[?1004h\x1b[?25h');
  taken.dispose();
  await write(t, '\x1b[?25;1004h');
  assert.equal(data.join(''), '\x1b[1;1R\x1b[O\x1b[0n\x1b[O');
  t.dispose();
});

test('dispose() while an async handler holds parsing drops the held write and runs nothing more', async () => {
  const [ox, xt] = await compare({ cols: 20, rows: 2 }, async (t, log) => {
    let release;
    t.parser.registerOscHandler(1337, () => new Promise((resolve) => { release = resolve; }));
    t.parser.registerCsiHandler({ final: 'n' }, () => { log.push('dsr'); return false; });
    t.write('ab\x1b]1337;x\x07cd\x1b[6n', () => log.push('callback'));
    t.onWriteParsed(() => log.push('parsed'));
    await sleep(10);
    t.dispose();
    release(false);
    await sleep(10);
    t.write('more', () => log.push('late'));
    await sleep(10);
  });
  assert.deepEqual(ox.log, xt.log);
  assert.deepEqual(ox.log, []);
});

test('a handler may dispose the terminal it runs in', async () => {
  const t = new m.Terminal({ cols: 20, rows: 2 });
  const log = [];
  t.parser.registerCsiHandler({ final: 'n' }, () => { t.dispose(); return true; });
  t.onData((d) => log.push(d));
  t.write('a\x1b[6nb\x1b[5n', () => log.push('callback'));
  await sleep(20);
  assert.deepEqual(log, []);
  assert.equal(t.parser.registerOscHandler(1, () => true).dispose(), undefined);
});

test('an engine crash while an async handler holds parsing starts the terminal over and parses on', async (t) => {
  t.mock.method(console, 'warn', () => {});
  const term = new m.Terminal({ cols: 20, rows: 2 });
  const data = [];
  term.onData((d) => data.push(d));
  let release;
  const seen = [];
  term.parser.registerOscHandler(1337, () => new Promise((resolve) => { release = resolve; }));
  term.parser.registerCsiHandler({ final: 'n' }, (params) => { seen.push(params[0]); return false; });
  const settled = [];
  term.write('ab\x1b]1337;x\x07cd', () => settled.push('held'));
  term.write('ef', () => settled.push('next'));
  await sleep(10);
  assert.throws(() => __trapForTest(), WebAssembly.RuntimeError);
  release(true);
  await write(term, '\x1b[6n');
  assert.deepEqual(settled, ['held', 'next']);
  // The held bytes went with the crashed instance; later writes and the hooks run on the new one.
  assert.equal(text(term)[0], 'ef');
  assert.deepEqual(seen, [6]);
  assert.deepEqual(data, ['\x1b[1;3R']);
  term.dispose();
});

test('a terminal without handlers feeds with plain write and sets no hook', async (t) => {
  const until = t.mock.method(Engine.prototype, 'writeUntilHook');
  const setHook = t.mock.method(Engine.prototype, 'setHook');
  const term = new m.Terminal({ cols: 20, rows: 2 });
  await write(term, 'plain\x1b[6n\x1b]133;A\x07');
  assert.equal(until.mock.callCount(), 0);
  assert.equal(setHook.mock.callCount(), 0);
  // One hook per sequence while it has handlers; plain write again once the last one is gone.
  const a = term.parser.registerOscHandler(133, () => false);
  const b = term.parser.registerOscHandler(133, () => false);
  assert.deepEqual(setHook.mock.calls.map((c) => c.arguments), [['osc', 133, true]]);
  await write(term, 'x');
  assert.equal(until.mock.callCount(), 1);
  a.dispose();
  b.dispose();
  assert.deepEqual(setHook.mock.calls.map((c) => c.arguments), [['osc', 133, true], ['osc', 133, false]]);
  await write(term, 'y');
  assert.equal(until.mock.callCount(), 1);
  term.dispose();
});

// The ways wmux registers handlers (src/shared/terminal, src/renderer/web/viewerParser.ts, kittyKeyboard.ts).

test('wmux: OSC 133 prompt marks run in stream order, read the modes and write resets after the data', async () => {
  const [ox, xt] = await compare({ cols: 30, rows: 3 }, async (t, log) => {
    // installKittyPromptReset: never takes the mark.
    t.parser.registerOscHandler(133, (data) => { log.push(['kitty', data]); return false; });
    // shellPromptModeReset: reads the modes at the mark and queues DECRSTs behind the output.
    t.parser.registerOscHandler(133, (data) => {
      log.push(['prompt', data, t.modes.mouseTrackingMode, text(t)[0]]);
      if (data.startsWith('A') && t.modes.mouseTrackingMode !== 'none') t.write('\x1b[?1000l\x1b[?1006l');
      return false;
    });
    t.parser.registerCsiHandler({ prefix: '?', final: 'h' }, (params) => { log.push(['decset', params]); return false; });
    await write(t, '\x1b[?1000;1006hrun\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07');
    log.push(['after', t.modes.mouseTrackingMode, text(t)[0]]);
  });
  assert.deepEqual(ox.log, xt.log);
  assert.deepEqual(ox.log.at(-1), ['after', 'none', 'run$ ']);
});

test('wmux: reply overrides absorb DA, DSR, XTVERSION, DECRQM and DECRQSS, colour queries but not colour sets', async () => {
  const [ox, xt] = await compare({ cols: 20, rows: 2 }, async (t, log) => {
    const p = t.parser;
    const absorb = () => true;
    // holdNewXtermReplies (replyParity) and installViewerParser.
    p.registerCsiHandler({ prefix: '>', final: 'q' }, absorb);
    for (const id of [{ final: 'c' }, { prefix: '>', final: 'c' }, { prefix: '=', final: 'c' }, { final: 'n' }, { prefix: '?', final: 'n' }, { intermediates: '$', final: 'p' }, { prefix: '?', intermediates: '$', final: 'p' }]) {
      p.registerCsiHandler(id, absorb);
    }
    p.registerDcsHandler({ intermediates: '$', final: 'q' }, absorb);
    const colours = [4, 10, 11, 12].map((ident) => p.registerOscHandler(ident, (data) => data.includes('?')));
    await write(t, '\x1b[c\x1b[>c\x1b[=c\x1b[6n\x1b[?6n\x1b[4$p\x1b[?25$p\x1b[>q\x1bP$qm\x1b\\\x1b]10;?\x07\x1b]4;1;?\x07');
    log.push('queries done');
    // A colour set still applies: the query answers with it once nothing absorbs it.
    await write(t, '\x1b]10;rgb:12/34/56\x07');
    for (const d of colours) d.dispose();
    await write(t, '\x1b]10;?\x07');
  });
  // @xterm/headless answers no colour query (xterm.js does in a page, with the theme).
  assert.deepEqual(xt.log, ['queries done']);
  assert.deepEqual(ox.log, ['queries done', ['data', '\x1b]10;rgb:1212/3434/5656\x1b\\']]);
});

test('wmux: a DCS handler gets the whole payload, also split across writes', async () => {
  const [ox, xt] = await compare({ cols: 20, rows: 2 }, async (t, log) => {
    t.parser.registerDcsHandler({ intermediates: '$', final: 'q' }, (data, params) => { log.push(['decrqss', data, params]); return data === 'r'; });
    for (const chunk of ['\x1bP1$q', 'r\x1b', '\\Z\x1bP$q', 'm\x1b\\']) await write(t, chunk);
  });
  // The engine does not answer DECRQSS (xterm.js sends `DCS 1 $ r 0 m ST` for the one not taken).
  const handled = ({ log }) => log.filter(([k]) => k === 'decrqss');
  assert.deepEqual(handled(ox), handled(xt));
  assert.deepEqual(ox.text, xt.text);
  assert.deepEqual(ox.log.slice(0, 2), [['decrqss', 'r', [1]], ['decrqss', 'm', [0]]]);
});
