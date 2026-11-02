// The scrollbar next to a real xterm.js 6.1: the .xterm-scrollable-element DOM, its classes and sizes for the same
// scrollback, revealing and fading, hiding on the alternate screen, options.scrollbar and the theme's slider colours,
// and dragging the slider or pressing the track scrolling the same rows.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

const OPTIONS = { cols: 30, rows: 8, fontFamily: 'Menlo, monospace', fontSize: 14 };

test('the scrollbar next to xterm.js', { timeout: 120000 }, async (t) => {
  await withPage('modules.mjs', async (page) => {
    await page.waitForFunction(() => window.ready === true);
    /** Fresh terminals: [wongtty, xterm.js], each in a 400×200 parent at the same place, one shown at a time. */
    const setup = (options = OPTIONS) => page.evaluate((o) => {
      for (const term of window.made ?? []) term.dispose();
      document.body.replaceChildren();
      window.made = [window.ox.Terminal, window.xterm.Terminal].map((Terminal) => {
        const parent = document.body.appendChild(document.createElement('div'));
        parent.style.cssText = 'position: absolute; left: 20px; top: 20px; width: 400px; height: 200px';
        const term = new Terminal(o);
        term.open(parent);
        return term;
      });
      window.write = (data) => Promise.all(window.made.map((term) => new Promise((resolve) => term.write(data, resolve))));
      window.frames = (n = 3) => new Promise((resolve) => {
        const step = (left) => (left ? requestAnimationFrame(() => step(left - 1)) : resolve());
        step(n);
      });
      window.lines = (n) => window.write(Array.from({ length: n }, (_, i) => `line ${i}`).join('\r\n'));
      window.show = (index) => window.made.forEach((term, i) => { term.element.parentElement.style.visibility = i === index || index < 0 ? '' : 'hidden'; });
    }, options);
    const wait = (ms) => page.evaluate((m) => new Promise((resolve) => setTimeout(resolve, m)), ms);
    const bars = () => page.evaluate(() => window.made.map((term) => {
      const root = term.element.querySelector(':scope > .xterm-scrollable-element');
      const bar = root?.querySelector(':scope > .xterm-scrollbar.xterm-vertical');
      const slider = bar?.querySelector(':scope > .xterm-slider');
      return root && {
        root: [root.className, root.getAttribute('role'), root.style.position, root.firstElementChild.className],
        bar: [bar.className, bar.getAttribute('role'), bar.getAttribute('aria-hidden'), bar.style.position, bar.style.width, bar.style.height, bar.style.right, bar.style.top],
        slider: [slider.className, slider.style.position, slider.style.top, slider.style.left, slider.style.width, slider.style.height, slider.style.transform, slider.style.contain],
      };
    }));

    await t.test('DOM, classes and sizes match xterm.js for the same scrollback and view position', async () => {
      await setup();
      await page.evaluate(() => window.frames());
      await wait(700);
      let [ox, xt] = await bars();
      assert.deepEqual(ox, xt, 'no scrollback');
      for (const [lines, scroll] of [[30, 0], [300, 0], [300, -50], [300, -1000]]) {
        await page.evaluate(async ([n, s]) => {
          await window.lines(n);
          for (const term of window.made) term.scrollLines(s);
          await window.frames();
        }, [lines, scroll]);
        await wait(900);
        [ox, xt] = await bars();
        assert.deepEqual(ox, xt, `${lines} lines, scrolled ${scroll}`);
      }
      assert.match(ox.bar[0], /xterm-invisible/);
      assert.ok(Number.parseFloat(ox.slider[5]) >= 20, 'the slider is at least 20px');
    });

    await t.test('a fresh terminal shows the bar like xterm.js from the first frame, on the theme background', async () => {
      const state = () => page.evaluate(() => window.made.map((term) => {
        const root = term.element.querySelector(':scope > .xterm-scrollable-element');
        const bar = root.querySelector(':scope > .xterm-scrollbar.xterm-vertical');
        return [bar.className, bar.querySelector(':scope > .xterm-slider').style.height, getComputedStyle(term.element).backgroundColor, getComputedStyle(root).backgroundColor];
      }));
      for (const [lines, theme] of [[0, undefined], [100, undefined], [0, { background: '#203040' }], [100, { background: '#203040' }]]) {
        await setup({ ...OPTIONS, theme });
        const label = `${lines} lines, theme ${JSON.stringify(theme)}`;
        const seen = [await state()];
        await page.evaluate(async (n) => {
          if (n) await window.lines(n);
          await window.frames(2);
        }, lines);
        seen.push(await state());
        await wait(150);
        seen.push(await state());
        await wait(1000);
        seen.push(await state());
        for (const [k, [ox, xt]] of seen.entries()) assert.deepEqual(ox, xt, `${label}, step ${k}`);
        assert.match(seen[3][0][0], /xterm-invisible/, `${label}: hidden in the end`);
      }
      // The background follows a theme change, like xterm.js.
      await page.evaluate(() => { for (const term of window.made) term.options.theme = { background: '#405060' }; });
      const [ox, xt] = await state();
      assert.deepEqual(ox, xt);
      assert.equal(ox[2], 'rgb(64, 80, 96)');
    });

    await t.test('pointer over the terminal reveals the bar and leaving fades it; a scroll shows it for 500 ms', async () => {
      await setup();
      await page.evaluate(async () => {
        await window.lines(100);
        await window.frames();
      });
      await wait(900);
      const classes = async () => (await bars()).map((b) => b.bar[0]);
      const VISIBLE = 'xterm-visible xterm-scrollbar xterm-vertical';
      const FADED = 'xterm-invisible xterm-scrollbar xterm-vertical xterm-fade';
      for (const index of [0, 1]) {
        await page.evaluate((i) => window.show(i), index);
        await page.mouse.move(200, 100);
        await wait(50);
        const over = await classes();
        await page.mouse.move(700, 600);
        await wait(50);
        const left = await classes();
        assert.equal(over[index], VISIBLE, `over (${index})`);
        assert.equal(left[index], FADED, `left (${index})`);
      }
      await page.evaluate(async () => {
        window.show(-1);
        for (const term of window.made) term.scrollLines(-3);
        await window.frames();
      });
      await wait(100);
      assert.deepEqual(await classes(), [VISIBLE, VISIBLE], 'a scroll reveals the bar');
      await wait(600);
      assert.deepEqual(await classes(), [FADED, FADED], 'and it fades 500 ms later');
    });

    await t.test('the alternate screen has no scrollback: the bar is not needed, like xterm.js', async () => {
      await setup();
      await page.evaluate(async () => {
        await window.lines(100);
        await window.frames();
      });
      for (const index of [0, 1]) {
        // Shown when the switch comes: not needed hides it at once, without the fade.
        await page.evaluate((i) => window.show(i), index);
        await page.mouse.move(200, 100);
        await wait(50);
        await page.evaluate(async (i) => {
          await new Promise((resolve) => window.made[i].write('\x1b[?1049h', resolve));
          await window.frames();
        }, index);
        await page.mouse.move(700, 600);
      }
      await wait(50);
      const [ox, xt] = await bars();
      assert.deepEqual(ox, xt);
      assert.equal(ox.bar[0], 'xterm-invisible xterm-scrollbar xterm-vertical');
      await page.evaluate(async () => {
        window.show(-1);
        await window.write('\x1b[?1049l');
        await window.frames();
      });
      await wait(900);
      const [back, backXt] = await bars();
      assert.deepEqual(back, backXt);
    });

    await t.test('options.scrollbar: width resizes the bar, showScrollbar false hides it in its wrapper, FitAddon agrees', async () => {
      await setup();
      const r = await page.evaluate(async () => {
        await window.lines(100);
        await window.frames();
        const [ox, xt] = window.made;
        const fit = new window.ox.FitAddon();
        ox.loadAddon(fit);
        const out = { proposed: [fit.proposeDimensions().cols], cell: ox.dimensions.css.cell.width };
        for (const term of window.made) term.options.scrollbar = { width: 20 };
        await window.frames();
        const width = (term) => term.element.querySelector('.xterm-scrollbar.xterm-vertical')?.style.width;
        const sliderWidth = (term) => term.element.querySelector('.xterm-vertical > .xterm-slider')?.style.width;
        out.width = [[width(ox), sliderWidth(ox)], [width(xt), sliderWidth(xt)]];
        out.proposed.push(fit.proposeDimensions().cols);
        const slider = (term) => getComputedStyle(term.element.querySelector('.xterm-vertical > .xterm-slider')).backgroundColor;
        out.defaultColor = [slider(ox), slider(xt)];
        for (const term of window.made) term.options.theme = { foreground: '#ff8000' };
        out.themeColor = [slider(ox), slider(xt)];
        for (const term of window.made) term.options.theme = { scrollbarSliderBackground: '#00ff0080' };
        out.sliderColor = [slider(ox), slider(xt)];
        const canvas = ox.element.querySelector('canvas');
        for (const term of window.made) term.options.scrollbar = { showScrollbar: false };
        await window.frames();
        out.off = window.made.map((term) => ({
          parent: term.element.querySelector('.xterm-screen').parentElement.className.trim(),
          wrappers: term.element.querySelectorAll('.xterm-scrollable-element').length,
          bar: [...term.element.querySelector('.xterm-scrollbar.xterm-vertical').classList].sort(),
          width: [width(term), sliderWidth(term)],
        }));
        out.canvas = ox.element.querySelector('.xterm-screen canvas') === canvas;
        out.proposed.push(fit.proposeDimensions().cols);
        for (const term of window.made) term.options.scrollbar = { showScrollbar: true };
        await window.frames();
        out.on = { parent: ox.element.querySelector('.xterm-screen').parentElement.className, width: width(ox) };
        out.proposed.push(fit.proposeDimensions().cols);
        for (const term of window.made) term.dispose();
        out.disposed = document.querySelectorAll('.xterm-scrollable-element').length;
        window.made = [];
        return out;
      });
      assert.deepEqual(r.width[0], r.width[1]);
      assert.deepEqual(r.width[0], ['20px', '20px']);
      assert.equal(r.defaultColor[0], r.defaultColor[1]);
      assert.deepEqual(r.themeColor, ['rgba(255, 128, 0, 0.2)', 'rgba(255, 128, 0, 0.2)'], 'the slider is the foreground at 20%');
      assert.deepEqual(r.sliderColor, ['rgba(0, 255, 0, 0.5)', 'rgba(0, 255, 0, 0.5)'], 'theme.scrollbarSliderBackground');
      // xterm.js keeps the wrapper and a 0 px bar that never shows (it fades out if it was showing).
      assert.deepEqual(r.off[0], r.off[1]);
      assert.equal(r.off[0].wrappers, 1);
      assert.ok(r.off[0].bar.includes('xterm-invisible'));
      assert.ok(r.canvas, 'the canvas stays');
      assert.match(r.on.parent, /^xterm-scrollable-element /);
      assert.equal(r.on.width, '14px');
      const cols = (scrollbar) => Math.floor((400 - scrollbar) / r.cell);
      assert.deepEqual(r.proposed, [cols(14), cols(20), cols(0), cols(14)]);
      assert.equal(r.disposed, 0);
    });

    await t.test('opened with showScrollbar false, the DOM matches xterm.js and the bar stays hidden', async () => {
      await setup({ ...OPTIONS, scrollbar: { showScrollbar: false } });
      for (const lines of [0, 300]) {
        await page.evaluate(async (n) => {
          if (n) await window.lines(n);
          for (const term of window.made) term.scrollLines(-5);
          await window.frames();
        }, lines);
        await wait(100);
        const [ox, xt] = await bars();
        assert.deepEqual(ox, xt, `${lines} lines`);
        assert.match(ox.bar[0], /xterm-invisible/);
        assert.equal(ox.bar[4], '0px');
      }
    });

    await t.test('dragging the slider and pressing the track scroll the same rows as xterm.js', async () => {
      await setup();
      await page.evaluate(async () => {
        await window.lines(300);
        await window.frames();
      });
      const results = [];
      for (const index of [0, 1]) {
        await page.evaluate((i) => window.show(i), index);
        const rect = () => page.evaluate((i) => {
          const r = window.made[i].element.querySelector('.xterm-vertical > .xterm-slider').getBoundingClientRect();
          const b = window.made[i].element.querySelector('.xterm-scrollbar.xterm-vertical').getBoundingClientRect();
          return { x: r.x + r.width / 2, y: r.y + r.height / 2, barTop: b.y, barX: b.x + b.width / 2 };
        }, index);
        const viewportY = async () => {
          await page.evaluate(() => window.frames());
          return page.evaluate((i) => window.made[i].buffer.active.viewportY, index);
        };
        const out = [];
        let s = await rect();
        await page.mouse.move(s.x, s.y);
        await wait(50);
        await page.mouse.down();
        for (const dy of [-10, -40, -80]) {
          await page.mouse.move(s.x, s.y + dy);
          out.push(await viewportY());
        }
        await page.mouse.move(s.x, s.y + 25);
        out.push(await viewportY());
        await page.mouse.up();
        out.push(await page.evaluate((i) => window.made[i].element.querySelector('.xterm-vertical > .xterm-slider').classList.contains('xterm-active'), index));
        s = await rect();
        for (const offset of [30, 100, 5]) {
          await page.mouse.move(s.barX, s.barTop + offset);
          await wait(50);
          await page.mouse.down();
          await page.mouse.up();
          out.push(await viewportY());
        }
        results.push(out);
        await page.mouse.move(700, 600);
        await wait(700);
      }
      assert.deepEqual(results[0], results[1]);
      assert.notEqual(results[0][0], results[0][2], 'the drag scrolled');
      assert.equal(results[0][4], false, 'xterm-active is removed on release');
    });
  });
});
