// Link parity with the real xterm.js 6.1 and @xterm/addon-web-links at the DOM level: OSC 8 hyperlinks through
// options.linkHandler, URLs through the built-in WebLinksAddon, and registerLinkProvider providers (one that answers
// at once, one that answers on a later task), under the same CDP mouse events at the same cells of both terminals.
// Compared: every hover, leave and activate callback with its text, range and button, the providers' calls, the
// pointer cursor, and the selection a double click on a link makes.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { cdp, withPage } from './harness.mjs';

const osc = (uri, text, id = '') => `\x1b]8;${id ? `id=${id}` : ''};${uri}\x07${text}\x1b]8;;\x07`;
// 60 columns. Row 3 wraps a URL into row 4, row 6 an OSC 8 link into row 7.
const LINES = [
  'plain text https://example.com/path?q=1&x=(2) and more',
  `${osc('https://osc.example/one', 'OSC one')} then ${osc('https://osc.example/two', 'two', 'x')} ${osc('file:///etc/passwd', 'file link')}`,
  'TICKET-42 custom provider and https://web.example/also.',
  `${'x'.repeat(40)} https://wrap.example/${'a'.repeat(30)} end`,
  `pre ${osc('https://osc.example/wrapped', 'w'.repeat(70))} post`,
  `${osc('https://osc.example/wide', '漢字link')} and ${osc('https://osc.example/same', 'same')}${osc('https://osc.example/same', 'again')}`,
  'OVERLAP https://overlap.example/x tail',
  'ASYNC-7 answers later https://async.example/ok',
];
const CONTENT = LINES.join('\r\n');

function withTerminals(fn) {
  return withPage('links-parity.mjs', async (page) => {
    const setupError = await page.evaluate(() => window.start());
    if (setupError) throw new Error(setupError);
    const session = await cdp(page);
    const mouse = (type, [x, y], { button = 'none', buttons = 0, clickCount = 0 } = {}) =>
      session.send('Input.dispatchMouseEvent', { type, x, y, button, buttons, clickCount });
    /** Waits out the asynchronous provider and a frame. */
    const settle = () => page.evaluate(() => new Promise((resolve) => setTimeout(() => requestAnimationFrame(() => resolve()), 10)));
    // Each terminal's callbacks are read right after its own events: moving to the other terminal leaves it, which
    // shows up at the start of its next read, as for the other terminal.
    const states = [undefined, undefined];
    const both = async (cell, events) => {
      const points = await page.evaluate(([c, r]) => window.points(c, r), cell);
      for (const k of [0, 1]) {
        await events(points[k], k);
        await settle();
        states[k] = await page.evaluate((n) => window.state(n), k);
      }
    };
    const mismatches = [];
    let total = 0;
    const compare = async (what) => {
      const [wongtty, xterm] = states;
      total++;
      if (JSON.stringify(wongtty) !== JSON.stringify(xterm)) mismatches.push({ what, wongtty, xterm });
      return xterm;
    };
    return fn({
      page, mouse, both, compare, mismatches, count: () => total,
      move: (cell) => both(cell, (at) => mouse('mouseMoved', at)),
      settle,
      click: (cell, button = 'left') => both(cell, async (at) => {
        const buttons = { left: 1, right: 2, middle: 4 }[button];
        // The press comes once the providers answered the move (the asynchronous one on a later task)
        await mouse('mouseMoved', at);
        await settle();
        await mouse('mousePressed', at, { button, buttons, clickCount: 1 });
        await mouse('mouseReleased', at, { button, clickCount: 1 });
      }),
      /** Writes to both, then reads both (output can make a hovered link leave and hover again). */
      write: async (data) => {
        await page.evaluate((d) => window.write(d), data);
        await settle();
        for (const k of [0, 1]) states[k] = await page.evaluate((n) => window.state(n), k);
      },
      /** Runs `fn` on the page for both, then reads both. */
      run: async (fn) => {
        await page.evaluate(fn);
        await settle();
        for (const k of [0, 1]) states[k] = await page.evaluate((n) => window.state(n), k);
      },
    });
  }, { viewport: { width: 1400, height: 500 } });
}

test('hover, leave and activation of OSC 8 links, web links and provider links match xterm.js', { timeout: 300000 }, async () => {
  const result = await withTerminals(async (t) => {
    await t.write(CONTENT);
    // xterm.js looks a hovered link up again the first time a terminal gets focus (it draws the cursor then): focus
    // both before hovering, so the clicks below focus terminals that had it before
    await t.page.evaluate(() => window.call('focus', []));
    const cells = await t.page.evaluate(() => window.cells());
    assert.equal(cells[0], cells[1]);
    // Every link and the cells around them, then off the text and out of the terminal
    const HOVERS = [
      [0, 0], [11, 0], [25, 0], [44, 0], [45, 0], [50, 0],
      [0, 1], [6, 1], [7, 1], [13, 1], [15, 1], [18, 1], [22, 1],
      [0, 2], [8, 2], [9, 2], [35, 2], [52, 2], [53, 2],
      [41, 3], [59, 3], [5, 4], [12, 4], [13, 4],
      [3, 5], [4, 5], [30, 5], [59, 5], [0, 6], [13, 6], [14, 6],
      [0, 7], [1, 7], [6, 7], [7, 7], [12, 7], [16, 7],
      [0, 8], [7, 8], [10, 8], [30, 8],
      [2, 9], [22, 9], [30, 9], [20, 12],
    ];
    for (const cell of HOVERS) {
      await t.move(cell);
      await t.compare(`hover ${cell}`);
    }
    // Clicks: left activates; right and middle activate too (xterm.js activates on mouseup of any button)
    for (const [cell, button] of [[[20, 0], 'left'], [[2, 1], 'left'], [[16, 1], 'left'], [[3, 2], 'left'], [[40, 2], 'left'],
      [[50, 3], 'left'], [[5, 4], 'left'], [[30, 5], 'left'], [[1, 7], 'left'], [[14, 7], 'left'], [[3, 8], 'left'], [[20, 8], 'left'],
      [[3, 9], 'left'], [[30, 9], 'left'], [[20, 0], 'right'], [[2, 1], 'middle']]) {
      await t.click(cell, button);
      await t.compare(`${button} click ${cell}`);
    }
    // A press on a link released on another activates nothing
    const to = await t.page.evaluate(() => window.points(2, 1));
    await t.both([20, 0], async (at, k) => {
      await t.mouse('mouseMoved', at);
      await t.settle();
      await t.mouse('mousePressed', at, { button: 'left', buttons: 1, clickCount: 1 });
      await t.mouse('mouseMoved', to[k], { button: 'left', buttons: 1 });
      await t.mouse('mouseReleased', to[k], { button: 'left', clickCount: 1 });
    });
    await t.compare('press on a link, release on another');
    // Leaving the terminal leaves the link
    await t.move([20, 0]);
    await t.compare('back on a link');
    await t.both([20, 20], (at) => t.mouse('mouseMoved', at));
    await t.compare('out of the terminal');
    // Non-http OSC 8 links count once linkHandler allows them
    await t.run(() => window.allowNonHttpProtocols(true));
    await t.move([25, 1]);
    await t.move([26, 1]);
    await t.compare('file link allowed');
    await t.click([26, 1]);
    await t.compare('file link click');
    // Output under the hovered link looks it up again (each terminal gets it while the mouse is on it)
    await t.both([20, 0], async (at, k) => {
      await t.mouse('mouseMoved', at);
      await t.settle();
      await t.page.evaluate(([n, d]) => window.writeOne(n, d), [k, '\x1b7\x1b[1;1Hreplaced https://new.example/z    \x1b8']);
    });
    await t.compare('output under the hovered link');
    await t.move([22, 0]);
    await t.compare('hover the new link');
    // A resize lets go of the hovered link
    await t.run(() => window.call('resize', [62, 14]));
    await t.compare('resize');
    await t.move([12, 2]);
    await t.compare('hover after resize');
    // The program taking clicks changes nothing for links
    await t.write('\x1b[?1000h');
    await t.move([20, 0]);
    await t.click([20, 0]);
    await t.compare('click while the program takes clicks');
    await t.write('\x1b[?1000l');
    // Without the web links addon and the providers only OSC 8 links are left
    await t.run(() => { window.disposeWebLinks(); window.disposeProviders(); });
    for (const cell of [[0, 2], [12, 2], [40, 2], [3, 1], [3, 8]]) {
      await t.move(cell);
      await t.compare(`hover ${cell}, addon and providers disposed`);
    }
    return { mismatches: t.mismatches, total: t.count() };
  });
  assert.equal(result.total, 45 + 16 + 1 + 2 + 2 + 2 + 2 + 1 + 5);
  assert.deepEqual(result.mismatches, [], `${result.mismatches.length} of ${result.total} cases differ: ${JSON.stringify(result.mismatches.slice(0, 2))}`);
});

test('a double click on a hovered link selects the link like xterm.js, and the hovered link is underlined', { timeout: 300000 }, async () => {
  const result = await withTerminals(async (t) => {
    await t.write(CONTENT);
    await t.page.evaluate(() => window.call('focus', []));
    const selections = [];
    for (const cell of [[20, 0], [3, 1], [50, 3], [5, 4], [3, 2], [30, 7]]) {
      await t.both(cell, async (at) => {
        await t.mouse('mouseMoved', at);
        await t.settle();
        for (const clickCount of [1, 2]) {
          await t.mouse('mousePressed', at, { button: 'left', buttons: 1, clickCount });
          await t.mouse('mouseReleased', at, { button: 'left', clickCount });
        }
      });
      selections.push({ cell, positions: await t.page.evaluate(() => window.selection()) });
      await t.compare(`double click ${cell}`);
    }
    // The underline: lit pixels on the bottom row of the hovered link's cells, none once the mouse left
    const underline = async (cell) => {
      await t.mouse('mouseMoved', (await t.page.evaluate(([c, r]) => window.points(c, r), cell))[0]);
      await t.page.evaluate(() => new Promise((resolve) => setTimeout(() => requestAnimationFrame(() => requestAnimationFrame(resolve)), 10)));
      return t.page.evaluate(() => window.underlinePixels(0, 11, 45));
    };
    await t.page.evaluate(() => window.call('clearSelection', []));
    const hovered = await underline([20, 0]);
    const left = await underline([20, 20]);
    return { mismatches: t.mismatches, selections, hovered, left };
  });
  for (const { cell, positions } of result.selections) assert.deepEqual(positions[0], positions[1], `selection after double click ${cell}`);
  assert.ok(result.selections.every(({ positions }) => positions[1]), 'xterm.js selected every link');
  assert.deepEqual(result.mismatches, [], JSON.stringify(result.mismatches.slice(0, 2)));
  assert.ok(result.hovered > 100, `underline pixels while hovered: ${result.hovered}`);
  assert.ok(result.left < result.hovered / 10, `underline pixels after leaving: ${result.left}`);
});
