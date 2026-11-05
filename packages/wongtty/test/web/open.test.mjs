import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

test('init() loads both modules in the browser and open() builds the xterm.js DOM', async () => {
  await withPage('open.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    const dom = await page.evaluate(() => [...document.querySelectorAll('.terminal.xterm > .xterm-scrollable-element > .xterm-screen > .xterm-helpers')].length);
    assert.equal(dom, 1);
    assert.equal(await page.evaluate(() => window.term.element.dataset.engine), 'wongtty');
    const reply = await page.evaluate(() => new Promise((resolve) => {
      window.term.onData(resolve);
      window.term.write('ab\x1b[6n');
    }));
    assert.equal(reply, '\x1b[1;3R');
  });
});

test('the input helpers stay above the canvas only while the terminal has focus', async () => {
  await withPage('open.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    const zIndex = () => page.evaluate(() => getComputedStyle(document.querySelector('.xterm-helpers')).zIndex);
    assert.equal(await zIndex(), '0');
    await page.evaluate(() => window.term.focus());
    assert.equal(await zIndex(), '5');
    await page.evaluate(() => window.term.blur());
    assert.equal(await zIndex(), '0');
  });
});
