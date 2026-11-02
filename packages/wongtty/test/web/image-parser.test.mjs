// The image addon on `terminal.parser`: its handlers exist only while an ImageAddon is loaded (a terminal without
// one keeps feeding with plain write), they sit among the host's handlers newest first, and its window option
// answers replace the engine's.
import { test, before } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import * as m from '../../dist/xterm.js';
import { Terminal as Engine } from '../../dist/index.js';

before(async () => {
  await m.init(undefined, await readFile(new URL('../../dist/xterm_web_bg.wasm', import.meta.url)));
});

const write = (t, data) => new Promise((resolve) => t.write(data, resolve));
/** A 6 × 6 red sixel. */
const sixel = '\x1bPq"1;1;6;6#0;2;100;0;0#0~~~~~~-\x1b\\';
const id = (s) => [...s].reduce((n, c) => n * 256 + c.charCodeAt(0), 0);

test('the image handlers are set only while an ImageAddon is loaded; other terminals feed with plain write', async (t) => {
  const until = t.mock.method(Engine.prototype, 'writeUntilHook');
  const setHook = t.mock.method(Engine.prototype, 'setHook');
  const plain = new m.Terminal({ cols: 20, rows: 4 });
  await write(plain, `a${sixel}\x1b[c\x1b[?1049h`);
  assert.equal(until.mock.callCount(), 0);
  assert.equal(setHook.mock.callCount(), 0);

  const term = new m.Terminal({ cols: 20, rows: 4 });
  const addon = new m.ImageAddon();
  term.loadAddon(addon);
  assert.deepEqual(setHook.mock.calls.map((c) => c.arguments), [
    ['csi', id('?h'), true], ['csi', id('?l'), true], ['csi', id('c'), true], ['csi', id('?S'), true],
    ['csi', id('!p'), true], ['csi', id('t'), true], ['dcs', id('q'), true], ['osc', 1337, true],
  ]);
  await write(term, sixel);
  assert.ok(until.mock.callCount() > 0);
  assert.equal(term._imageDebug().pixels, 36);

  addon.dispose();
  assert.ok(setHook.mock.calls.slice(8).every((c) => c.arguments[2] === false));
  assert.equal(setHook.mock.callCount(), 16);
  const calls = until.mock.callCount();
  await write(term, sixel);
  assert.equal(until.mock.callCount(), calls);
  assert.equal(term._imageDebug().loaded, false);
  plain.dispose();
  term.dispose();
});

test('a host DCS q handler registered after the addon runs first: false lets the addon place the sixel, true skips it', async () => {
  const term = new m.Terminal({ cols: 20, rows: 4 });
  term.loadAddon(new m.ImageAddon());
  const seen = [];
  const host = term.parser.registerDcsHandler({ final: 'q' }, (data) => {
    seen.push(data.length);
    return seen.length > 1;
  });
  await write(term, sixel);
  const first = term._imageDebug().pixels;
  await write(term, sixel);
  assert.equal(first, 36);
  assert.equal(term._imageDebug().pixels, 36);
  assert.equal(seen.length, 2);
  host.dispose();
  // Elsewhere, so it does not cover the first
  await write(term, `\x1b[3;5H${sixel}`);
  assert.equal(term._imageDebug().pixels, 72);
  term.dispose();
});

test('CSI 18 t answers as windowOptions allows on a terminal with the addon, not at all when it does not', async () => {
  const answer = async (addon) => {
    const term = new m.Terminal({ cols: 20, rows: 4 });
    term.loadAddon(addon);
    const data = [];
    term.onData((d) => data.push(d));
    await write(term, '\x1b[18t\x1b[22;0t');
    term.dispose();
    return data.join('');
  };
  assert.equal(await answer(new m.ImageAddon()), '\x1b[8;4;20t');
  assert.equal(await answer(new m.ImageAddon({ enableSizeReports: false })), '');
});
