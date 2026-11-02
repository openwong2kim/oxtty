// Without WebGL2 `open()` still succeeds, as in xterm.js: the terminal does not draw, but input, the buffer and
// events work, `dimensions` stays undefined, and the page gets one warning.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

test('open() without WebGL2 works without a renderer and warns once', async () => {
  await withPage('no-webgl.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    assert.deepEqual(await page.evaluate(() => window.errors), []);
    const warnings = await page.evaluate(() => window.warnings);
    assert.equal(warnings.length, 1, warnings.join('\n'));
    assert.match(warnings[0], /no renderer/);
    const state = await page.evaluate(async () => {
      const term = window.terms[0];
      const data = [];
      term.onData((d) => data.push(d));
      await new Promise((resolve) => term.write('ab\x1b[6n', resolve));
      await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      term.focus();
      return {
        data,
        dimensions: term.dimensions,
        line: term.buffer.active.getLine(0).translateToString(true),
        dom: !!term.element.querySelector('.xterm-screen > .xterm-helpers > textarea'),
        canvases: term.element.querySelectorAll('canvas').length,
        focused: document.activeElement === term.textarea,
      };
    });
    assert.deepEqual(state, { data: ['\x1b[1;3R'], dimensions: undefined, line: 'ab', dom: true, canvases: 0, focused: true });
    await page.evaluate(() => {
      window.typed = [];
      window.terms[0].onData((d) => window.typed.push(d));
    });
    await page.keyboard.type('hi');
    await page.keyboard.press('Enter');
    assert.deepEqual(await page.evaluate(() => window.typed), ['h', 'i', '\r']);
  });
});
