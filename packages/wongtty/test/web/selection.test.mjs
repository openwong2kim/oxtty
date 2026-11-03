// Selection parity with the real xterm.js 6.1 at the DOM level: the same output, the same CDP mouse events at the
// same cells and the same API calls on an wongtty/xterm Terminal and on xterm.js, compared by getSelection() text,
// getSelectionPosition(), hasSelection() and the number of onSelectionChange events.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { cdp, withPage } from './harness.mjs';

// Set to `false` once the members land in crates/wongtty-web.
const TODO = false;

const ALT = 1, SHIFT = 8;
const COLS = 40;

// 16 rows at 40 columns: ASCII words and separators, Hangul, CJK and emoji, combining marks, a wrapped sentence,
// a URL and a path, typed trailing spaces, tabs, a wide character pushed to the next row, a wrapped run of wide
// characters and a word that wraps.
const LINES = [
  'hello world foo_bar baz-qux (paren) [x]',
  '한글 단어 테스트 가나다라 끝',
  '漢字かな 文字 😀🎉 end',
  'été café naïve x',
  'The quick brown fox jumps over the lazy dog and keeps running',
  'see https://example.com/a?q=1&x=(2) /usr/local/bin/tool.sh ok',
  'trailing   ',
  'a\tb\tcc\td',
  `${'x'.repeat(39)}가나 tail`,
  '가'.repeat(25),
  `abc ${'y'.repeat(36)}zzzz more`,
];
const CONTENT = LINES.join('\r\n');
const DOUBLE_CLICK_COLS = [0, 1, 2, 3, 5, 7, 10, 13, 17, 20, 24, 28, 33, 36, 38, 39];

/** Runs `fn` on a page with both terminals; `fn` gets helpers that act on both terminals in turn. */
function withTerminals(size, fn) {
  return withPage('selection-parity.mjs', async (page) => {
    const setupError = await page.evaluate((s) => window.start(s), size);
    if (setupError) throw new Error(setupError);
    const session = await cdp(page);
    const mouse = (type, [x, y], { button = 'none', buttons = 0, modifiers = 0, clickCount = 0 } = {}) =>
      session.send('Input.dispatchMouseEvent', { type, x, y, button, buttons, modifiers, clickCount });
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
    const press = (at, clickCount, modifiers) => [
      mouse('mousePressed', at, { button: 'left', buttons: 1, modifiers, clickCount }),
      mouse('mouseReleased', at, { button: 'left', modifiers, clickCount }),
    ];
    const gestures = {
      click: (cell, modifiers = 0) => both([cell], (at) => Promise.all(press(at, 1, modifiers))),
      doubleClick: (cell, modifiers = 0) => both([cell], (at) => Promise.all([...press(at, 1, modifiers), ...press(at, 2, modifiers)])),
      tripleClick: (cell, modifiers = 0) => both([cell], (at) => Promise.all([...press(at, 1, modifiers), ...press(at, 2, modifiers), ...press(at, 3, modifiers)])),
      /** Press at `from` (the `clickCount`-th click), move to `to` and release there. */
      drag: (from, to, modifiers = 0, clickCount = 1) => both([from, to], (a, b) => Promise.all([
        ...Array.from({ length: clickCount - 1 }, (_, i) => press(a, i + 1, modifiers)).flat(),
        mouse('mousePressed', a, { button: 'left', buttons: 1, modifiers, clickCount }),
        mouse('mouseMoved', b, { button: 'left', buttons: 1, modifiers }),
        mouse('mouseReleased', b, { button: 'left', modifiers, clickCount }),
      ])),
    };
    const state = () => page.evaluate(() => window.state());
    const mismatches = [];
    let total = 0;
    /** Compares the selections after `what` (text, typed trailing spaces included); returns xterm.js's. */
    const compare = async (what) => {
      const { wongtty, xterm } = await state();
      total++;
      if (JSON.stringify(wongtty) !== JSON.stringify(xterm)) mismatches.push({ what, wongtty, xterm });
      return xterm;
    };
    return fn({
      page, mouse, both, ...gestures, state, compare, mismatches, count: () => total,
      write: (data) => page.evaluate((d) => window.write(d), data),
      call: (name, ...args) => {
        // A resize moves the cells, and the second terminal with them
        if (name === 'resize') cache.clear();
        return page.evaluate(([n, a]) => window.call(n, a), [name, args]);
      },
      setOption: (key, value) => page.evaluate(([k, v]) => window.setOption(k, v), [key, value]),
      viewportY: () => page.evaluate(() => window.viewportY()),
      takeData: () => page.evaluate(() => window.takeData()),
    });
  }, { viewport: { width: 1400, height: 600 } });
}

test('same cell size as xterm.js, so the same points hit the same cells', { todo: TODO, timeout: 300000 }, async () => {
  const [wongtty, xterm] = await withTerminals({ cols: COLS, rows: 16, scrollback: 20 }, ({ page }) => page.evaluate(() => window.cells()));
  assert.ok(wongtty > 0);
  assert.equal(wongtty, xterm);
});

test('mouse selections match xterm.js: double and triple click, drags, Alt column drags, Shift+click extend', { todo: TODO, timeout: 300000 }, async () => {
  const result = await withTerminals({ cols: COLS, rows: 16, scrollback: 20 }, async (t) => {
    await t.write(CONTENT);
    const nonEmpty = { double: 0, triple: 0, drag: 0, column: 0 };
    for (let row = 0; row < 16; row++) {
      for (const col of DOUBLE_CLICK_COLS) {
        await t.doubleClick([col, row]);
        if ((await t.compare(`double click ${col},${row}`)).has) nonEmpty.double++;
      }
      await t.tripleClick([3, row]);
      if ((await t.compare(`triple click 3,${row}`)).has) nonEmpty.triple++;
    }
    const DRAGS = [
      [[2, 0], [9, 0]], [[9, 0], [2, 0]], [[0, 0], [39, 0]], [[5, 0], [5, 3]], [[30, 3], [4, 0]],
      // Across wide characters, starting and ending on their right halves
      [[1, 1], [8, 1]], [[3, 1], [12, 2]], [[2, 2], [15, 2]], [[13, 2], [11, 2]],
      // Across wrapped rows and past the end of the text
      [[10, 4], [10, 5]], [[30, 5], [39, 6]], [[0, 4], [25, 7]], [[38, 10], [3, 11]], [[39, 11], [39, 13]],
      [[5, 12], [9, 13]], [[20, 14], [3, 15]], [[0, 8], [39, 8]], [[3, 9], [39, 9]], [[12, 15], [0, 14]],
    ];
    for (const [from, to] of DRAGS) {
      await t.drag(from, to);
      if ((await t.compare(`drag ${from} → ${to}`)).has) nonEmpty.drag++;
      await t.drag(from, to, ALT);
      if ((await t.compare(`Alt drag ${from} → ${to}`)).has) nonEmpty.column++;
    }
    // Word and line drags: the second and third click held down
    for (const [from, to] of [[[2, 0], [14, 0]], [[14, 0], [2, 0]], [[6, 4], [30, 5]], [[30, 5], [6, 4]], [[3, 1], [20, 2]], [[10, 14], [5, 15]]]) {
      await t.drag(from, to, 0, 2);
      await t.compare(`word drag ${from} → ${to}`);
      await t.drag(from, to, 0, 3);
      await t.compare(`line drag ${from} → ${to}`);
    }
    // Shift+click moves the end of the selection
    for (const [from, to] of [[[2, 0], [20, 0]], [[20, 2], [3, 1]], [[10, 4], [5, 6]], [[30, 9], [2, 12]]]) {
      await t.click(from);
      await t.click(to, SHIFT);
      await t.compare(`click ${from}, Shift+click ${to}`);
      await t.doubleClick(from);
      await t.click(to, SHIFT);
      await t.compare(`double click ${from}, Shift+click ${to}`);
    }
    // A click clears
    await t.click([5, 5]);
    await t.compare('click');
    return { mismatches: t.mismatches, total: t.count(), nonEmpty };
  });
  assert.equal(result.total, 16 * (DOUBLE_CLICK_COLS.length + 1) + 19 * 2 + 6 * 2 + 4 * 2 + 1);
  assert.ok(result.nonEmpty.double > 200 && result.nonEmpty.triple === 16 && result.nonEmpty.drag === 19 && result.nonEmpty.column > 10, JSON.stringify(result.nonEmpty));
  assert.deepEqual(result.mismatches, [], `${result.mismatches.length} of ${result.total} cases differ`);
});

test('select, selectLines, selectAll and wordSeparator match xterm.js', { todo: TODO, timeout: 300000 }, async () => {
  const result = await withTerminals({ cols: COLS, rows: 16, scrollback: 20 }, async (t) => {
    await t.write(CONTENT);
    for (const [col, row, length] of [[0, 0, 5], [6, 0, 50], [0, 1, 3], [1, 1, 4], [30, 4, 20], [0, 9, 45], [39, 8, 1], [0, 6, 11], [3, 2, 9], [0, 15, 0]]) {
      await t.call('select', col, row, length);
      await t.compare(`select(${col}, ${row}, ${length})`);
    }
    for (const [start, end] of [[0, 0], [4, 5], [2, 9], [12, 13], [-3, 2], [14, 99], [5, 3]]) {
      await t.call('selectLines', start, end);
      await t.compare(`selectLines(${start}, ${end})`);
    }
    await t.call('selectAll');
    await t.compare('selectAll');
    await t.call('clearSelection');
    await t.compare('clearSelection');
    for (const separator of [' ', ' ()[]{}\',"`-/:.?&=', 'o', '']) {
      await t.setOption('wordSeparator', separator);
      for (const [col, row] of [[1, 0], [14, 0], [22, 0], [30, 0], [10, 5], [27, 5], [2, 6], [3, 1], [15, 2], [6, 15], [33, 14]]) {
        await t.doubleClick([col, row]);
        await t.compare(`wordSeparator ${JSON.stringify(separator)}: double click ${col},${row}`);
      }
    }
    return { mismatches: t.mismatches, total: t.count() };
  });
  assert.equal(result.total, 10 + 7 + 2 + 4 * 11);
  assert.deepEqual(result.mismatches, [], `${result.mismatches.length} of ${result.total} cases differ`);
});

test('forced selection while the program reports the mouse, and synthetic events, match xterm.js', { todo: TODO, timeout: 300000 }, async () => {
  const result = await withTerminals({ cols: COLS, rows: 16, scrollback: 20 }, async (t) => {
    await t.write(CONTENT);
    await t.drag([2, 0], [20, 1]);
    await t.compare('before reporting');
    // Turning reporting on disables (and clears) the selection
    await t.write('\x1b[?1002;1006h');
    await t.compare('reporting on');
    // Without a modifier the drag is reported; macOS forces with Option (macOptionClickForcesSelection), else Shift
    await t.drag([2, 0], [20, 1]);
    await t.compare('plain drag, reporting');
    await t.drag([2, 0], [20, 1], SHIFT);
    await t.compare('Shift drag, reporting');
    await t.drag([2, 0], [20, 1], ALT);
    await t.compare('Alt drag, reporting, macOptionClickForcesSelection off');
    await t.setOption('macOptionClickForcesSelection', true);
    await t.drag([2, 0], [20, 1], ALT);
    await t.compare('Alt drag, reporting, macOptionClickForcesSelection on');
    await t.doubleClick([14, 0], ALT);
    await t.compare('Alt double click, reporting, macOptionClickForcesSelection on');
    // wmux's plainDragSelect replays a held press as a synthetic mousedown with the force modifier, then moves
    const force = { altKey: true, shiftKey: true };
    await t.page.evaluate((f) => window.synthetic([
      ['mousedown', 3, 4, { ...f, button: 0, buttons: 1, detail: 1 }],
      ['mousemove', 12, 5, { ...f, button: 0, buttons: 1, detail: 1 }],
      ['mouseup', 12, 5, { button: 0, buttons: 0, detail: 1 }],
    ]), force);
    await t.compare('synthetic forced drag');
    await t.write('\x1b[?1002l');
    await t.page.evaluate(() => window.synthetic([
      ['mousedown', 6, 4, { button: 0, buttons: 1, detail: 2 }],
      ['mouseup', 6, 4, { button: 0, buttons: 0, detail: 2 }],
    ]));
    await t.compare('synthetic double click');
    const reports = await t.takeData();
    return { mismatches: t.mismatches, total: t.count(), reports };
  });
  assert.deepEqual(result.reports[0], result.reports[1], 'same reports to the program');
  assert.deepEqual(result.mismatches, [], `${result.mismatches.length} of ${result.total} cases differ`);
});

test('the selection follows its text as the scrollback trims, then clears, like xterm.js', { todo: TODO, timeout: 300000 }, async () => {
  const result = await withTerminals({ cols: COLS, rows: 6, scrollback: 10 }, async (t) => {
    // History full: 10 lines of scrollback and 6 on the screen
    await t.write(Array.from({ length: 16 }, (_, i) => `line ${i} ${'한'.repeat(i % 4)} end`).join('\r\n'));
    // select() keeps no end: once its start trims away it sticks to the top line (xterm.js never clears it)
    const texts = [];
    await t.call('select', 3, 2, 50);
    texts.push((await t.compare('select in the scrollback')).text);
    for (let i = 16; i < 22; i++) {
      await t.write(`\r\nline ${i}`);
      texts.push((await t.compare(`select, after line ${i}`)).text);
    }
    // A drag on screen rows 1-4 (buffer lines 11-14) loses its start to the trim, then its end, and clears
    const has = { drag: [], lines: [] };
    await t.drag([3, 1], [8, 4]);
    for (let i = 22; i < 38; i++) {
      await t.write(`\r\nmore ${i}`);
      has.drag.push((await t.compare(`drag, after more ${i}`)).has);
    }
    await t.call('selectLines', 4, 12);
    for (let i = 38; i < 52; i++) {
      await t.write(`\r\nmore ${i}`);
      has.lines.push((await t.compare(`selectLines, after more ${i}`)).has);
    }
    return { mismatches: t.mismatches, total: t.count(), texts, has };
  });
  assert.deepEqual(result.mismatches, [], `${result.mismatches.length} of ${result.total} cases differ`);
  assert.ok(result.texts[0] && result.texts[0] === result.texts[2], 'the text stays while the lines trim');
  assert.notEqual(result.texts[3], result.texts[0], 'the start trimmed away');
  assert.equal(result.has.drag.indexOf(false), 14, 'the drag cleared once its end fell off');
  assert.equal(result.has.lines.indexOf(false), 12, 'selectLines cleared once its end fell off');
});

test('alternate screen switch, a change of rows and full resets clear the selection; onSelectionChange fires like xterm.js', { todo: TODO, timeout: 300000 }, async () => {
  const result = await withTerminals({ cols: COLS, rows: 16, scrollback: 20 }, async (t) => {
    // The cursor leaves the wrapped last line. Widening leaves the cursor's line unreflowed in both, with a blank
    // tail on its first row that both trim as erased cells
    await t.write(`${CONTENT}\r\n`);
    await t.drag([2, 0], [20, 3]);
    await t.compare('drag');
    await t.write('\x1b[?1049h');
    await t.compare('alternate screen on');
    await t.write('alt text here');
    await t.doubleClick([5, 0]);
    await t.compare('double click on the alternate screen');
    await t.write('\x1b[?1049l');
    await t.compare('alternate screen off');
    await t.drag([2, 0], [20, 3]);
    await t.call('resize', COLS + 5, 16);
    await t.compare('resize columns only');
    await t.call('resize', COLS + 5, 18);
    await t.compare('resize rows');
    await t.call('selectAll');
    // Typing clears the selection (user input)
    for (const k of [0, 1]) {
      await t.page.evaluate((n) => window.focusTerminal(n), k);
      await t.page.keyboard.type('x');
    }
    await t.compare('typing');
    // A full reset from the program (RIS) and from the API, also on the alternate screen and twice in one write
    await t.write(CONTENT);
    await t.drag([2, 0], [20, 3]);
    await t.compare('drag before RIS');
    await t.write('\x1bc');
    await t.compare('RIS');
    await t.write(CONTENT);
    await t.call('selectLines', 1, 4);
    await t.call('reset');
    await t.compare('reset()');
    await t.write(`\x1b[?1049h${CONTENT}`);
    await t.doubleClick([2, 0]);
    await t.compare('double click on the alternate screen before RIS');
    await t.write('\x1bc');
    await t.compare('RIS on the alternate screen');
    await t.write(CONTENT);
    await t.call('selectAll');
    await t.write('\x1bcabc\x1bc');
    await t.compare('two RIS in one write');
    await t.call('reset');
    await t.compare('reset() without a selection');
    return { mismatches: t.mismatches, total: t.count() };
  });
  assert.deepEqual(result.mismatches, [], `${result.mismatches.length} of ${result.total} cases differ`);
});

test('a right click selects the word under it with rightClickSelectsWord and puts the selection in the textarea; copy copies it; like xterm.js', { timeout: 300000 }, async () => {
  const result = await withTerminals({ cols: COLS, rows: 16, scrollback: 20 }, async (t) => {
    await t.write(CONTENT);
    const textareas = [];
    // Each textarea is read right after its own click: the next terminal's right click focuses its textarea, and
    // the blur empties the other one (in both)
    const rightClick = async ([col, row]) => {
      const points = await t.page.evaluate(([c, r]) => window.points(c, r), [col, row]);
      const values = [];
      for (const k of [0, 1]) {
        await Promise.all([
          t.mouse('mousePressed', points[k], { button: 'right', buttons: 2, clickCount: 1 }),
          t.mouse('mouseReleased', points[k], { button: 'right', clickCount: 1 }),
        ]);
        values.push((await t.page.evaluate(() => window.textareas()))[k]);
      }
      textareas.push(values);
    };
    for (const on of [false, true]) {
      await t.setOption('rightClickSelectsWord', on);
      await t.call('clearSelection');
      await t.compare(`rightClickSelectsWord ${on}: clear`);
      // A word, the same word again, whitespace (nothing to select), inside a selection, a wide-character word
      for (const cell of [[14, 0], [14, 0], [11, 6], [2, 1], [22, 0]]) {
        await rightClick(cell);
        await t.compare(`rightClickSelectsWord ${on}: right click ${cell}`);
      }
      await t.drag([2, 4], [30, 4]);
      await rightClick([10, 4]);
      await t.compare(`rightClickSelectsWord ${on}: right click inside a drag selection`);
    }
    // Copy puts the selection on the clipboard; without one the event is left alone
    const copies = [await t.page.evaluate(() => window.copied())];
    await t.call('clearSelection');
    copies.push(await t.page.evaluate(() => window.copied()));
    return { mismatches: t.mismatches, total: t.count(), textareas, copies };
  });
  for (const [wongtty, xterm] of result.copies) assert.deepEqual(wongtty, xterm);
  assert.ok(result.copies[0][1][0].length > 0 && result.copies[0][1][1], 'xterm.js copied the selection');
  assert.deepEqual(result.mismatches, [], `${result.mismatches.length} of ${result.total} cases differ`);
  for (const [wongtty, xterm] of result.textareas) assert.equal(wongtty, xterm);
  assert.ok(result.textareas.some(([, xterm]) => xterm.length > 0), 'the textarea held a selection');
});

test('dragging past the top or bottom scrolls and extends the selection like xterm.js', { todo: TODO, timeout: 300000 }, async () => {
  const result = await withTerminals({ cols: COLS, rows: 8, scrollback: 30 }, async (t) => {
    await t.write(Array.from({ length: 30 }, (_, i) => `row ${i}`).join('\r\n'));
    await t.call('scrollLines', -10);
    const before = await t.viewportY();
    /** Holds the button down beyond the edge until both reached the end of the scrollback. */
    const dragOut = (to, modifiers = 0) => t.both([[4, 3], to], async (a, b) => {
      await t.mouse('mousePressed', a, { button: 'left', buttons: 1, modifiers, clickCount: 1 });
      await t.mouse('mouseMoved', b, { button: 'left', buttons: 1, modifiers });
      await new Promise((resolve) => setTimeout(resolve, 600));
      await t.mouse('mouseReleased', b, { button: 'left', modifiers, clickCount: 1 });
    });
    await dragOut([4, -3]);
    const up = await t.compare('drag above the top');
    const top = await t.viewportY();
    await dragOut([10, 11], ALT);
    const down = await t.compare('Alt drag below the bottom');
    const bottom = await t.viewportY();
    return { mismatches: t.mismatches, before, top, bottom, up, down };
  });
  assert.equal(result.before[0], 12);
  assert.equal(result.before[1], 12);
  assert.deepEqual(result.top, [0, 0], 'both scrolled to the top');
  assert.deepEqual(result.bottom, [22, 22], 'both scrolled to the bottom');
  assert.ok(result.up.has && result.down.has);
  assert.deepEqual(result.mismatches, []);
});

test('selectAll and selectLines over a long compressed scrollback give xterm.js\'s text', { todo: TODO, timeout: 300000 }, async () => {
  const result = await withTerminals({ cols: COLS, rows: 16, scrollback: 3000 }, async (t) => {
    // Every line kind above, 120 times, with colours: well past the rows the engine keeps uncompressed
    await t.write(Array.from({ length: 120 }, (_, i) => `\x1b[3${i % 8}m${LINES[i % LINES.length]}\x1b[0m ${i}`).join('\r\n'));
    await t.call('selectAll');
    const all = await t.compare('selectAll');
    for (const [start, end] of [[0, 1], [0, 2], [3, 150], [100, 101], [5, 170], [150, 160]]) {
      await t.call('selectLines', start, end);
      await t.compare(`selectLines(${start}, ${end})`);
    }
    return { mismatches: t.mismatches, total: t.count(), length: all.text.length };
  });
  assert.ok(result.length > 4000, `${result.length}`);
  assert.deepEqual(result.mismatches, [], `${result.mismatches.length} of ${result.total} cases differ`);
});
