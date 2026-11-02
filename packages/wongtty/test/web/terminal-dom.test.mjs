// The opened Terminal in headless Chrome next to a real xterm.js 6.1 (WebGL addon): the DOM it builds, focus, frames
// (onRender), cell size and fit, drawing, and what dispose leaves behind.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

const OPTIONS = { cols: 20, rows: 4, fontFamily: 'Menlo, monospace', fontSize: 15, theme: { background: '#101010' } };

test('the opened terminal next to xterm.js', async (t) => {
  await withPage('modules.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    assert.equal(await page.evaluate(() => window.ready), true);
    /** Fresh terminals: wongtty in #ox, xterm.js (WebGL) in #xt, each in a 300×200 parent; returns nothing. */
    const setup = (options = OPTIONS) => page.evaluate((o) => {
      for (const t of window.made ?? []) t.dispose();
      document.body.replaceChildren();
      const parent = (id) => Object.assign(document.body.appendChild(document.createElement('div')), { id, style: 'width: 300px; height: 200px' });
      const ox = new window.ox.Terminal(o);
      const xt = new window.xterm.Terminal(o);
      ox.open(parent('ox'));
      xt.open(parent('xt'));
      xt.loadAddon(new window.WebglAddon());
      window.made = [ox, xt];
    }, options);
    const frames = (n = 2) => page.evaluate((count) => new Promise((resolve) => {
      const step = (left) => (left ? requestAnimationFrame(() => step(left - 1)) : resolve());
      step(count);
    }), n);

    await t.test('open() builds the xterm.js element tree inside the parent', async () => {
      await setup();
      const shape = await page.evaluate(() => window.made.map((term) => {
        const screen = term.element.querySelector('.xterm-screen');
        return {
          classes: [...term.element.classList].filter((c) => ['terminal', 'xterm'].includes(c)),
          inParent: term.element.parentElement === document.getElementById(term === window.made[0] ? 'ox' : 'xt'),
          helpers: !!screen?.querySelector(':scope > .xterm-helpers'),
        };
      }));
      assert.deepEqual(shape[0], shape[1]);
      assert.deepEqual(shape[0], { classes: ['terminal', 'xterm'], inParent: true, helpers: true });
    });

    await t.test('open() on another parent moves the element and keeps it (park and re-append)', async () => {
      await setup();
      const moved = await page.evaluate(() => {
        const [ox] = window.made;
        const before = ox.element;
        const park = document.body.appendChild(document.createElement('div'));
        ox.open(park);
        return before === ox.element && ox.element.parentElement === park && !document.getElementById('ox').firstChild;
      });
      assert.equal(moved, true);
    });

    await t.test('the screen has xterm.js\'s size for the same font, and dimensions report the cell', async () => {
      await setup();
      await frames();
      const sizes = await page.evaluate(() => window.made.map((term) => {
        const screen = term.element.querySelector('.xterm-screen');
        return [screen.offsetWidth, screen.offsetHeight];
      }));
      assert.ok(sizes[1][0] > 0 && sizes[1][1] > 0, `xterm.js screen ${sizes[1]}`);
      assert.deepEqual(sizes[0], sizes[1]);
      const cell = await page.evaluate(() => window.made[0].dimensions.css.cell);
      assert.deepEqual([cell.width * OPTIONS.cols, cell.height * OPTIONS.rows], sizes[0]);
    });

    await t.test('focus inside the element adds the focus class like xterm.js', async () => {
      await setup();
      const states = await page.evaluate(() => window.made.map((term) => {
        const seen = [term.element.classList.contains('focus')];
        term.focus();
        seen.push(term.element.classList.contains('focus'));
        term.blur();
        seen.push(term.element.classList.contains('focus'));
        return seen;
      }));
      assert.deepEqual(states[0], states[1]);
      assert.deepEqual(states[0], [false, true, false]);
    });

    await t.test('onRender reports the rows a write changed, like xterm.js', async () => {
      await setup();
      await frames();
      const rendered = await page.evaluate(async () => {
        const seen = window.made.map((term) => {
          const list = [];
          term.onRender((e) => list.push(e));
          return list;
        });
        await Promise.all(window.made.map((term) => new Promise((resolve) => term.write('hello\r\nworld', resolve))));
        await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
        return seen;
      });
      assert.ok(rendered[1].length > 0, 'xterm.js rendered');
      assert.deepEqual(rendered[0], rendered[1]);
      // Writes that change no cell still refresh the cursor row; frames without writes report nothing.
      const quiet = await page.evaluate(async () => {
        const out = [];
        for (const data of ['\x1b[4;5H', '\x1b7\x1b[1;1Hx\x1b8', '\x1b[?25l', '\x1b[?25h', '', null]) {
          const seen = window.made.map((term) => {
            const list = [];
            return [list, term.onRender((e) => list.push([e.start, e.end]))];
          });
          if (data !== null) await Promise.all(window.made.map((term) => new Promise((resolve) => term.write(data, resolve))));
          await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
          out.push(seen.map(([list, listener]) => (listener.dispose(), list)));
        }
        return out;
      });
      for (const [ox, xt] of quiet) assert.deepEqual(ox, xt);
    });

    await t.test('a synchronized update applies its output at once and holds the frames until it ends, like xterm.js', async () => {
      await setup();
      await frames();
      const facts = await page.evaluate(async () => {
        const all = (data) => Promise.all(window.made.map((term) => new Promise((resolve) => term.write(data, resolve))));
        const raf = () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
        // Each render with whether the update was still open when it was drawn.
        const seen = window.made.map((term) => {
          const list = [];
          term.onRender((e) => list.push([e.start, e.end, term.modes.synchronizedOutputMode]));
          return list;
        });
        const now = () => window.made.map((term) => [term.buffer.active.getLine(0).translateToString(true), term.modes.synchronizedOutputMode]);
        // The renders since the last call: none drawn while the update was open, and whether one redrew every row.
        const renders = () => seen.map((list) => {
          const taken = list.splice(0);
          return [taken.some(([, , open]) => open), taken.some(([start, end]) => start === 0 && end === 3)];
        });
        await all('\x1b[?2026hhello');
        const inside = now();
        await raf();
        const drawnOpen = seen.map((list) => list.some(([, , open]) => open));
        await all(' world\x1b[?2026l');
        await raf();
        const ended = [now(), renders()];
        // An update the program never ends is ended by the timeout (1000 ms, as in xterm.js).
        const [ox] = window.made;
        await new Promise((resolve) => ox.write('\x1b[?2026h!', resolve));
        const held = now()[0];
        await new Promise((resolve) => setTimeout(resolve, 1300));
        await raf();
        return { inside, drawnOpen, ended, held, timedOut: [now()[0], renders()[0]] };
      });
      assert.deepEqual(facts.inside, [['hello', true], ['hello', true]]);
      // A slow run may hit the timeout before the program ends the update: then that redraws every row.
      assert.deepEqual(facts.drawnOpen, [false, false]);
      assert.deepEqual(facts.ended, [[['hello world', false], ['hello world', false]], [[false, true], [false, true]]]);
      assert.deepEqual(facts.held, ['hello world!', true]);
      assert.deepEqual(facts.timedOut, [['hello world!', false], [false, true]]);
    });

    await t.test('a font size change fires onDimensionsChange with the new cell and resizes the screen', async () => {
      await setup();
      await frames();
      const facts = await page.evaluate(() => {
        const [ox] = window.made;
        const widths = [];
        ox.onDimensionsChange((d) => widths.push(d.css.cell.width));
        const before = ox.dimensions.css.cell.width;
        ox.options.fontSize = 20;
        const screen = ox.element.querySelector('.xterm-screen');
        return { before, widths, after: ox.dimensions.css.cell, screen: [screen.offsetWidth, screen.offsetHeight] };
      });
      assert.equal(facts.widths.length, 1);
      assert.ok(facts.widths[0] > facts.before);
      assert.deepEqual(facts.screen, [facts.after.width * 20, facts.after.height * 4]);
    });

    await t.test('FitAddon proposes the cells that fit the parent, minus the 14px scrollbar xterm.js keeps', async () => {
      await setup();
      await frames();
      const facts = await page.evaluate(() => {
        const [ox] = window.made;
        const fit = new window.ox.FitAddon();
        ox.loadAddon(fit);
        const proposed = fit.proposeDimensions();
        fit.fit();
        const { width, height } = ox.dimensions.css.cell;
        return { proposed, size: [ox.cols, ox.rows], expected: { cols: Math.floor((300 - 14) / width), rows: Math.floor(200 / height) } };
      });
      assert.deepEqual(facts.proposed, facts.expected);
      assert.deepEqual(facts.size, [facts.expected.cols, facts.expected.rows]);
    });

    await t.test('coloured Hangul is drawn on the canvas and a cursor position request is answered', async () => {
      await setup({ ...OPTIONS, theme: { background: '#000000' } });
      const facts = await page.evaluate(async () => {
        const [ox] = window.made;
        const data = [];
        ox.onData((d) => data.push(d));
        const drawn = new Promise((resolve) => {
          ox.onRender(() => {
            const canvas = [...ox.element.querySelectorAll('canvas')].find((c) => c.style.display !== 'none');
            const copy = document.createElement('canvas');
            [copy.width, copy.height] = [canvas.width, canvas.height];
            const ctx = copy.getContext('2d');
            ctx.drawImage(canvas, 0, 0);
            const pixels = ctx.getImageData(0, 0, copy.width, copy.height).data;
            let lit = 0;
            let red = 0;
            for (let i = 0; i < pixels.length; i += 4) {
              if (pixels[i] || pixels[i + 1] || pixels[i + 2]) lit++;
              if (pixels[i] > 150 && pixels[i + 1] < 100 && pixels[i + 2] < 100) red++;
            }
            if (red) resolve({ lit, red });
          });
        });
        await new Promise((resolve) => ox.write('\x1b[31m한글\x1b[0m ok\x1b[6n', resolve));
        const timeout = new Promise((resolve) => setTimeout(() => resolve({ lit: 0, red: 0 }), 2000));
        return { data, ...(await Promise.race([drawn, timeout])) };
      });
      assert.deepEqual(facts.data, ['\x1b[1;8R']);
      assert.ok(facts.red > 20, `red glyph pixels: ${facts.red}`);
      assert.ok(facts.lit > facts.red, `lit pixels: ${facts.lit}`);
    });

    await t.test('dispose removes the element and draws nothing more, even with a write pending', async () => {
      await setup();
      await frames();
      const facts = await page.evaluate(async () => {
        const [ox] = window.made;
        let renders = 0;
        ox.onRender(() => renders++);
        const parent = ox.element.parentElement;
        ox.write('late');
        ox.dispose();
        ox.dispose();
        await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
        await new Promise((resolve) => setTimeout(resolve, 20));
        return { removed: !parent.querySelector('.xterm'), element: ox.element ?? null, renders };
      });
      assert.deepEqual(facts, { removed: true, element: null, renders: 0 });
    });
  });
});
