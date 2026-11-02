// Mouse and wheel parity with the real xterm.js 6.1 at the DOM level: the same output and the same CDP mouse events
// on an wongtty/xterm Terminal and on xterm.js, compared byte for byte on onData/onBinary, and by view position for
// wheel scrolling.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { cdp, withPage } from './harness.mjs';

const ALT = 1, CTRL = 2, SHIFT = 8;
const PROTOCOLS = { X10: 9, VT200: 1000, DRAG: 1002, ANY: 1003 };
const ENCODINGS = { DEFAULT: [], SGR: [1006], SGR_PIXELS: [1016] };
// Every mouse mode off: resetting any one tracking mode turns tracking off, and resetting an SGR encoding returns
// to the default one, in both.
const MOUSE_OFF = '\x1b[?1000l\x1b[?1006l';
const BUTTONS = [['left', 1], ['middle', 4], ['right', 2]];
const MODIFIERS = [0, SHIFT, ALT, CTRL, ALT | CTRL | SHIFT];
// Cells around the single-byte limits of the default encoding: 95 + 32 = 127, 223 + 32 = 255
const CELLS = [[0, 0], [7, 3], [94, 40], [95, 95], [96, 99], [222, 50], [223, 60], [239, 98]];

/** Runs `fn` on the page with a CDP session; `fn` gets helpers that act on both terminals in turn. */
function withTerminals(fn) {
  return withPage('mouse-parity.mjs', async (page) => {
    await page.waitForFunction(() => window.ready || window.setupError, null, { timeout: 30000 });
    const setupError = await page.evaluate(() => window.setupError);
    if (setupError) throw new Error(setupError);
    const session = await cdp(page);
    const mouse = (type, [x, y], { button = 'none', buttons = 0, modifiers = 0, clickCount = 0, deltaX = 0, deltaY = 0 } = {}) =>
      session.send('Input.dispatchMouseEvent', { type, x, y, button, buttons, modifiers, clickCount, deltaX, deltaY });
    // The layout never changes, so each cell's points are asked for once.
    const cache = new Map();
    const points = async (col, row) => {
      if (!cache.has(`${col},${row}`)) cache.set(`${col},${row}`, await page.evaluate(([c, r]) => window.points(c, r), [col, row]));
      return cache.get(`${col},${row}`);
    };
    /** Calls `events(point…)` for wongtty, then for xterm.js, at the same cells of each. */
    const both = async (cells, events) => {
      const per = await Promise.all(cells.map(([col, row]) => points(col, row)));
      for (const k of [0, 1]) await events(...per.map((p) => p[k]));
    };
    let sent = 0;
    let pending = [];
    const handled = async () => {
      await Promise.all(pending.splice(0));
      await page.evaluate((n) => window.untilWheels(n), sent);
    };
    /**
     * Wheels at one point, waited for until the page has handled them all. Chrome merges a queued wheel into the
     * one before it when their modifiers match, so lists alternate modifiers (a merge would hang here); Ctrl wheels
     * take Chrome's zoom path and can overtake others, so each goes alone.
     */
    const wheels = async (at, list) => {
      for (const options of list) {
        const alone = options.modifiers & CTRL;
        if (alone) await handled();
        pending.push(mouse('mouseWheel', at, options));
        sent++;
        if (alone) await handled();
      }
      await handled();
    };
    return fn({
      page, mouse, wheel: (at, options) => wheels(at, [options]), wheels, both,
      write: (data) => page.evaluate((d) => window.write(d), data),
      take: () => page.evaluate(() => window.take()),
      viewportY: () => page.evaluate(() => window.viewportY()),
      setOption: (key, value) => page.evaluate(([k, v]) => window.setOption(k, v), [key, value]),
    });
  }, { viewport: { width: 2000, height: 820 } });
}

test('same cell size as xterm.js, so wheel pixels convert to the same lines', async () => {
  const [wongtty, xterm] = await withTerminals(({ page }) => page.evaluate(() => window.cells()));
  assert.ok(wongtty.width > 0 && wongtty.height > 0);
  assert.equal(wongtty.width, xterm);
});

test('button reports match xterm.js across protocol × encoding × button × modifier × cell', { timeout: 300000 }, async () => {
  const { mismatches, total, reports } = await withTerminals(async ({ mouse, both, write, take }) => {
    const mismatches = [];
    let total = 0, reports = 0;
    for (const [protocol, mode] of Object.entries(PROTOCOLS)) {
      for (const [encoding, extra] of Object.entries(ENCODINGS)) {
        await write(`${MOUSE_OFF}\x1b[?${[mode, ...extra].join(';')}h`);
        for (const [button, mask] of BUTTONS) {
          for (const modifiers of MODIFIERS) {
            for (const [col, row] of CELLS) {
              // Hover, press, drag one cell right and down, release
              // CDP handles the events in order, so they are sent without waiting for each.
              await both([[col, row], [Math.min(col + 1, 239), Math.min(row + 1, 99)]], (from, to) => Promise.all([
                mouse('mouseMoved', from, { modifiers }),
                mouse('mousePressed', from, { button, buttons: mask, modifiers, clickCount: 1 }),
                mouse('mouseMoved', to, { button, buttons: mask, modifiers }),
                mouse('mouseReleased', to, { button, modifiers, clickCount: 1 }),
              ]));
              const { wongtty, xterm } = await take();
              total++;
              reports += xterm.length;
              try {
                assert.deepEqual(wongtty, xterm);
              } catch {
                mismatches.push({ protocol, encoding, button, modifiers, col, row, wongtty, xterm });
              }
            }
          }
        }
      }
    }
    return { mismatches, total, reports };
  });
  assert.equal(total, 4 * Object.keys(ENCODINGS).length * 3 * 5 * CELLS.length);
  assert.ok(reports > total, `xterm.js reported ${reports} times`);
  assert.deepEqual(mismatches.slice(0, 5), [], `${mismatches.length} of ${total} sequences differ`);
});

test('wheel reports, arrows and refused wheels match xterm.js', { timeout: 300000 }, async () => {
  // A notch each way, a trackpad-like run that only adds up to a line, a fast flick
  const DELTAS = [-100, 100, -30, -30, 300];
  const CELLS3 = [[5, 5], [96, 99], [230, 40]];
  const { mismatches, total, sent } = await withTerminals(async ({ page, wheels, both, write, take, viewportY }) => {
    const mismatches = [];
    let total = 0, sent = 0;
    const compare = async (what, cell) => {
      const { wongtty, xterm } = await take();
      const [oy, xy] = await viewportY();
      total++;
      sent += xterm.length;
      if (JSON.stringify([wongtty, oy]) !== JSON.stringify([xterm, xy])) mismatches.push({ what, cell, wongtty, xterm, viewportY: [oy, xy] });
    };
    // Every delta with every modifier set, the modifiers changing from one wheel to the next
    const run = async (what, cells, modifiersList = [0, SHIFT, ALT, CTRL]) => {
      for (const cell of cells) {
        await both([cell], (at) => wheels(at, DELTAS.flatMap((deltaY) => modifiersList.map((modifiers) => ({ modifiers, deltaY })))));
        await compare(what, cell);
      }
    };
    // DRAG, ANY and X10 handle the wheel like VT200 (X10 ignores it), so one cell is enough for them
    for (const [protocol, mode] of Object.entries(PROTOCOLS)) {
      for (const [encoding, extra] of Object.entries(ENCODINGS)) {
        await write(`${MOUSE_OFF}\x1b[?${[mode, ...extra].join(';')}h`);
        await run(`${protocol} ${encoding}`, protocol === 'VT200' ? CELLS3 : [[96, 99]]);
      }
    }
    // Alternate screen without mouse reporting: arrow keys, cursor or application mode
    await write(`${MOUSE_OFF}\x1b[?1049h`);
    await run('alternate', CELLS3);
    await write('\x1b[?1h');
    await run('alternate DECCKM', CELLS3);
    // Line and page wheels (only scripts can make them)
    for (const deltaMode of [1, 2]) {
      await page.evaluate((m) => window.syntheticWheel(10, 10, { deltaY: -3, deltaMode: m }), deltaMode);
      await page.evaluate((m) => window.syntheticWheel(10, 10, { deltaY: 1, deltaMode: m, shiftKey: true }), deltaMode);
      await compare(`alternate deltaMode ${deltaMode}`, [10, 10]);
    }
    await write('\x1b[?1l\x1b[?1000;1006h');
    for (const deltaMode of [1, 2]) {
      await page.evaluate((m) => window.syntheticWheel(10, 10, { deltaY: 2, deltaMode: m }), deltaMode);
      await compare(`VT200 SGR deltaMode ${deltaMode}`, [10, 10]);
    }
    // attachCustomWheelEventHandler: refused wheels are neither reported nor sent as arrows
    await page.evaluate(() => window.refuseCtrlWheel());
    await run('refuse Ctrl, VT200 SGR', [[5, 5]], [0, CTRL]);
    await write('\x1b[?1000;1006l');
    await run('refuse Ctrl, alternate', [[5, 5]], [0, CTRL]);
    return { mismatches, total, sent };
  });
  assert.equal(total, (3 + 1 + 1 + 1) * Object.keys(ENCODINGS).length + 2 * 3 + 4 + 2);
  assert.ok(sent > 100, `xterm.js sent ${sent} times`);
  assert.deepEqual(mismatches.slice(0, 5), [], `${mismatches.length} of ${total} cases differ`);
});

test('wheel scrolls the view by the same rows as xterm.js', { timeout: 120000 }, async () => {
  const STEPS = [
    [-100, 0], [-100, 0], [-53, 0], [-10, 0], [-10, 0], [-10, 0], [-10, 0], [-10, 0], [-7, 0], [-100, ALT], [100, CTRL],
    [100, SHIFT], [-100, SHIFT], [120, 0], [-300, 0], [33, 0], [-1, 0], [5000, 0], [100, 0], [-20000, 0], [-100, 0], [250, ALT],
  ];
  const result = await withTerminals(async ({ page, wheel, both, write, viewportY, setOption }) => {
    await write(Array.from({ length: 300 }, (_, i) => `line ${i}`).join('\r\n'));
    const trace = async () => {
      const seen = [[], []];
      await both([[20, 30]], async (at) => {
        const k = seen[0].length === STEPS.length ? 1 : 0;
        for (const [deltaY, modifiers] of STEPS) {
          await wheel(at, { deltaY, modifiers });
          seen[k].push((await viewportY())[k]);
        }
      });
      return seen;
    };
    const plain = await trace();
    // A predominantly horizontal wheel does not scroll
    await both([[20, 30]], (at) => wheel(at, { deltaX: -150, deltaY: -100 }));
    const horizontal = await viewportY();
    // Output while scrolled up keeps the view; the next wheel starts from there
    await both([[20, 30]], (at) => wheel(at, { deltaY: -400 }));
    await write('\r\nmore\r\nmore');
    const afterOutput = await trace();
    await setOption('scrollSensitivity', 3);
    await setOption('fastScrollSensitivity', 2);
    const sensitive = await trace();
    await page.evaluate(() => window.syntheticWheel(20, 30, { deltaY: -4, deltaMode: 1 }));
    await page.evaluate(() => window.syntheticWheel(20, 30, { deltaY: 1, deltaMode: 2 }));
    return { plain, horizontal, afterOutput, sensitive, synthetic: await viewportY() };
  });
  assert.ok(new Set(result.plain[1]).size > 5, `xterm.js moved: ${result.plain[1]}`);
  assert.deepEqual(result.plain[0], result.plain[1], 'scrollSensitivity 1');
  assert.equal(result.horizontal[0], result.horizontal[1], 'horizontal wheel');
  assert.deepEqual(result.afterOutput[0], result.afterOutput[1], 'after output');
  assert.deepEqual(result.sensitive[0], result.sensitive[1], 'scrollSensitivity 3, fastScrollSensitivity 2');
  assert.equal(result.synthetic[0], result.synthetic[1], 'line and page wheels');
});

test('Alt+click moves the cursor like xterm.js (altClickMovesCursor)', async () => {
  const { cases, moved, mismatches } = await withTerminals(async ({ mouse, both, write, take, setOption }) => {
    const mismatches = [];
    let cases = 0, moved = 0;
    const click = async (what, cells, modifiers = ALT) => {
      for (const cell of cells) {
        await both([cell], (at) => Promise.all([
          mouse('mousePressed', at, { button: 'left', buttons: 1, modifiers, clickCount: 1 }),
          mouse('mouseReleased', at, { button: 'left', modifiers, clickCount: 1 }),
        ]));
        const { wongtty, xterm } = await take();
        cases++;
        if (xterm.some(([, bytes]) => /^(\x1b[[O][ABCD])+$/.test(bytes))) moved++;
        if (JSON.stringify(wongtty) !== JSON.stringify(xterm)) mismatches.push({ what, cell, wongtty, xterm });
      }
    };
    // A prompt that wraps onto a second row, cursor at its end
    await write(`$ ${'x'.repeat(250)}`);
    await click('normal buffer', [[3, 0], [100, 0], [5, 1], [200, 1], [0, 2], [50, 5]]);
    await click('no Alt', [[3, 0]], 0);
    await write('\x1b[?1h');
    await click('DECCKM', [[3, 0], [5, 1]]);
    await setOption('altClickMovesCursor', false);
    await click('altClickMovesCursor off', [[3, 0]]);
    await setOption('altClickMovesCursor', true);
    // The alternate screen walks rows, wrapped ones included
    await write('\x1b[?1l\x1b[?1049h\x1b[3;5H' + 'y'.repeat(300) + '\x1b[10;20H');
    await click('alternate', [[0, 0], [4, 2], [100, 3], [10, 9], [19, 9], [60, 20]]);
    // Mouse reporting on: the click is reported instead
    await write('\x1b[?1000;1006h');
    await click('reporting', [[4, 2]]);
    return { cases, moved, mismatches };
  });
  assert.equal(cases, 17);
  assert.ok(moved >= 10, `xterm.js moved the cursor ${moved} times`);
  assert.deepEqual(mismatches, []);
});

test('mouseEventsRequireAlt reports only Alt clicks, without the Alt bit, like xterm.js', async () => {
  const { mismatches, reports } = await withTerminals(async ({ mouse, wheel, both, write, take, setOption }) => {
    await setOption('mouseEventsRequireAlt', true);
    await write('\x1b[?1002;1006h');
    const mismatches = [];
    let reports = 0;
    for (const modifiers of [0, ALT, ALT | CTRL, SHIFT]) {
      await both([[10, 10], [12, 11]], async (from, to) => {
        await Promise.all([
          mouse('mousePressed', from, { button: 'left', buttons: 1, modifiers, clickCount: 1 }),
          mouse('mouseMoved', to, { button: 'left', buttons: 1, modifiers }),
          mouse('mouseReleased', to, { button: 'left', modifiers, clickCount: 1 }),
        ]);
        await wheel(to, { modifiers, deltaY: -100 });
      });
      const { wongtty, xterm } = await take();
      reports += xterm.length;
      if (JSON.stringify(wongtty) !== JSON.stringify(xterm)) mismatches.push({ modifiers, wongtty, xterm });
    }
    return { mismatches, reports };
  });
  assert.ok(reports > 0);
  assert.deepEqual(mismatches, []);
});
