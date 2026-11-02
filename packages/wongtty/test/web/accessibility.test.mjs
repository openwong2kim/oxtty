// options.screenReaderMode next to a real xterm.js 6.1: the accessibility tree mirrors the rows, the live region
// announces new output, focus past the last row scrolls, and turning it off or disposing leaves nothing behind.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { cdp, withPage } from './harness.mjs';

const OPTIONS = { cols: 30, rows: 6, fontFamily: 'Menlo, monospace', fontSize: 14, screenReaderMode: true };

test('screenReaderMode next to xterm.js', { timeout: 120000 }, async (t) => {
  await withPage('modules.mjs', async (page) => {
    await page.waitForFunction(() => window.ready === true);
    /** Fresh terminals: [wongtty, xterm.js], each in a 400×200 parent. */
    const setup = (options = OPTIONS) => page.evaluate((o) => {
      for (const term of window.made ?? []) term.dispose();
      document.body.replaceChildren();
      window.made = [window.ox.Terminal, window.xterm.Terminal].map((Terminal) => {
        const parent = document.body.appendChild(document.createElement('div'));
        parent.style.cssText = 'width: 400px; height: 200px';
        const term = new Terminal(o);
        term.open(parent);
        return term;
      });
      window.write = (data) => Promise.all(window.made.map((term) => new Promise((resolve) => term.write(data, resolve))));
      window.frames = (n = 3) => new Promise((resolve) => {
        const step = (left) => (left ? requestAnimationFrame(() => step(left - 1)) : resolve());
        step(n);
      });
    }, options);
    /** Waits out the one-second refresh limit both share. */
    const settle = () => page.evaluate(() => new Promise((resolve) => setTimeout(resolve, 1100)));
    const tree = () => page.evaluate(() => window.made.map((term) => ({
      rows: [...term.element.querySelectorAll('.xterm-accessibility-tree > [role="listitem"]')].map((row) => [
        row.textContent,
        row.getAttribute('aria-posinset'),
        row.getAttribute('aria-setsize'),
      ]),
      live: term.element.querySelector('.live-region[aria-live="assertive"]')?.textContent,
      first: term.element.firstElementChild.className,
    })));

    await t.test('the accessibility tree mirrors the rows (text, aria-posinset, aria-setsize)', async () => {
      await setup();
      await settle();
      await page.evaluate(async () => {
        let text = '';
        for (let i = 0; i < 12; i++) text += `line ${i}\r\n`;
        await window.write(text + 'tab\there 한글 é ' + 'x'.repeat(30) + 'wrapped');
        await window.frames();
      });
      await settle();
      const [ox, xt] = await tree();
      assert.equal(ox.first, 'xterm-accessibility', 'the tree is the first child of the element');
      assert.deepEqual(ox.rows, xt.rows);
      assert.equal(ox.rows.length, 6);
      await page.evaluate(async () => {
        for (const term of window.made) term.scrollLines(-4);
        await window.frames();
      });
      await settle();
      const [scrolledOx, scrolledXt] = await tree();
      assert.deepEqual(scrolledOx.rows, scrolledXt.rows);
      assert.equal(scrolledOx.rows[0][1], '5', 'aria-posinset follows the view');
    });

    await t.test('the live region announces output written across frames, up to 20 lines, like xterm.js', async () => {
      await setup();
      await settle();
      await page.evaluate(async () => {
        for (const chunk of ['hello\r\n', 'world\t!\r\n', '$ ', 'ls']) {
          await window.write(chunk);
          await window.frames();
        }
      });
      await settle();
      const [ox, xt] = await tree();
      assert.equal(ox.live, 'hello\nworld   !\n$ ls');
      assert.equal(ox.live, xt.live);
      // The limit, on one burst the viewport holds whole: both read it in one refresh. (Spread over frames, where
      // each implementation cuts a long stream depends on when its once-a-second refresh lands: a refresh that finds
      // "too much output" starts the count again.)
      await setup({ ...OPTIONS, rows: 30 });
      await settle();
      await page.evaluate(async () => {
        await window.write(Array.from({ length: 25 }, (_, i) => `out ${i}\r\n`).join(''));
        await window.frames();
      });
      await settle();
      const [manyOx, manyXt] = await tree();
      assert.equal(manyOx.live, manyXt.live);
      assert.equal(manyOx.live, Array.from({ length: 21 }, (_, i) => `out ${i}\n`).join(''), 'up to the 21st line');
    });

    await t.test('more lines than the viewport in one frame cannot be read: too much output', async () => {
      await setup();
      await settle();
      await page.evaluate(async () => {
        let text = '';
        for (let i = 0; i < 30; i++) text += `burst ${i}\r\n`;
        await window.write(text);
        await window.frames();
      });
      await settle();
      const [ox] = await tree();
      assert.equal(ox.live, 'Too much output to announce, navigate to rows manually to read');
    });

    await t.test('focus moving past the last or first row scrolls one row and keeps focus next to it', async () => {
      await setup();
      await page.evaluate(async () => {
        let text = '';
        for (let i = 0; i < 40; i++) text += `row ${i}\r\n`;
        await window.write(text);
        for (const term of window.made) term.scrollLines(-10);
        await window.frames();
      });
      await settle();
      const step = (index, from) => page.evaluate(async ([i, f]) => {
        const out = [];
        for (const term of window.made) {
          const rows = () => [...term.element.querySelectorAll('.xterm-accessibility-tree > [role="listitem"]')];
          rows()[f].focus();
          rows()[i].focus();
          await window.frames();
          out.push({ viewportY: term.buffer.active.viewportY, focused: rows().indexOf(document.activeElement), count: rows().length });
        }
        return out;
      }, [index, from]);
      const down = await step(5, 4);
      assert.deepEqual(down[0], down[1]);
      assert.equal(down[0].focused, 4);
      const up = await step(0, 1);
      assert.deepEqual(up[0], up[1]);
      assert.equal(up[0].focused, 1);
      const before = await page.evaluate(() => window.made.map((term) => term.buffer.active.viewportY));
      assert.deepEqual(before, [down[0].viewportY - 1, down[1].viewportY - 1]);
    });

    await t.test('turning it off removes the DOM and its listeners, on rebuilds it, dispose cleans up', async () => {
      await setup({ ...OPTIONS, screenReaderMode: false });
      const session = await cdp(page);
      const listeners = async () => {
        const { result } = await session.send('Runtime.evaluate', { expression: 'window.made[0].element' });
        const { listeners: list } = await session.send('DOMDebugger.getEventListeners', { objectId: result.objectId });
        return list.length;
      };
      const state = () => page.evaluate(() => window.made.map((term) => ({
        tree: term.element.querySelectorAll('.xterm-accessibility').length,
        rows: term.element.querySelectorAll('.xterm-accessibility-tree > div').length,
      })));
      const off = await listeners();
      assert.deepEqual(await state(), [{ tree: 0, rows: 0 }, { tree: 0, rows: 0 }]);
      await page.evaluate(() => window.made.forEach((term) => { term.options.screenReaderMode = true; }));
      assert.deepEqual(await state(), [{ tree: 1, rows: 6 }, { tree: 1, rows: 6 }]);
      assert.ok(await listeners() > off);
      await page.evaluate(() => window.made.forEach((term) => { term.options.screenReaderMode = false; }));
      assert.deepEqual(await state(), [{ tree: 0, rows: 0 }, { tree: 0, rows: 0 }]);
      assert.equal(await listeners(), off);
      await page.evaluate(async () => {
        window.made.forEach((term) => { term.options.screenReaderMode = true; });
        await window.write('after\r\n');
        window.made.forEach((term) => term.dispose());
        await window.frames();
        await new Promise((resolve) => setTimeout(resolve, 1100));
      });
      assert.equal(await page.evaluate(() => document.querySelectorAll('.xterm-accessibility').length), 0);
    });
  });
});
