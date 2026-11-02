import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

test('init() loads both modules in the browser and open() builds the xterm.js DOM', async () => {
  await withPage('open.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    const dom = await page.evaluate(() => [...document.querySelectorAll('.terminal.xterm > .xterm-scrollable-element > .xterm-screen > .xterm-helpers')].length);
    assert.equal(dom, 1);
    const reply = await page.evaluate(() => new Promise((resolve) => {
      window.term.onData(resolve);
      window.term.write('ab\x1b[6n');
    }));
    assert.equal(reply, '\x1b[1;3R');
  });
});
