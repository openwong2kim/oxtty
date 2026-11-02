// Input beside key encoding, each compared with a real xterm.js 6.1 on the same page: the textarea, paste, focus
// reports, the custom key handler, disableStdin, scrollOnUserInput, onUserInput and wongtty's kitty keyboard API.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

const setup = (page, options = {}) => page.evaluate((o) => window.api.setup(o), options);
const write = (page, data) => page.evaluate((d) => window.api.write(d), data);
const take = (page) => page.evaluate(() => window.api.take());
const both = async (page, fn) => {
  for (const name of ['ox', 'xt']) await fn(name);
  return take(page);
};
const press = (page, key) => async (name) => {
  await page.evaluate((n) => window.api.focus(n), name);
  await page.keyboard.press(key);
};

test('input matches xterm.js', async (t) => {
  await withPage('parity.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);

    await t.test('the textarea is built like xterm.js and takes focus', async () => {
      await setup(page);
      const shape = await page.evaluate(() => ['ox', 'xt'].map((name) => {
        const terminal = window.api.terminal(name);
        const textarea = terminal.textarea;
        const attributes = Object.fromEntries([...textarea.attributes].filter((a) => a.name !== 'style').map((a) => [a.name, a.value]));
        const helpers = textarea.parentElement;
        textarea.focus();
        const focused = document.activeElement === textarea && terminal.element.classList.contains('focus');
        terminal.blur();
        const blurred = document.activeElement !== textarea && !terminal.element.classList.contains('focus');
        terminal.focus();
        const refocused = document.activeElement === textarea;
        terminal.blur();
        return {
          attributes, className: textarea.className, helpers: helpers.className, inScreen: helpers.parentElement.classList.contains('xterm-screen'),
          compositionView: !!helpers.querySelector(':scope > .composition-view'), focused, blurred, refocused,
        };
      }));
      assert.deepEqual(shape[0], shape[1]);
      assert.equal(shape[0].className, 'xterm-helper-textarea');
      assert.ok(shape[0].compositionView && shape[0].focused && shape[0].blurred && shape[0].refocused);
    });

    await t.test('the textarea follows the cursor cell', async () => {
      await setup(page);
      await write(page, 'abc\r\n de');
      const placed = await page.evaluate(() => {
        const t = window.api.terminal('ox');
        const { width, height } = t.dimensions.css.cell;
        return { left: t.textarea.style.left, top: t.textarea.style.top, width, height };
      });
      assert.equal(placed.left, `${3 * placed.width}px`);
      assert.equal(placed.top, `${placed.height}px`);
    });

    await t.test('a mousedown on the terminal focuses the textarea like xterm.js', async () => {
      await setup(page);
      const focused = await page.evaluate(() => ['ox', 'xt'].map((name) => {
        const terminal = window.api.terminal(name);
        const screen = terminal.element.querySelector('.xterm-screen');
        terminal.blur();
        for (const type of ['mousedown', 'mouseup']) screen.dispatchEvent(new MouseEvent(type, { bubbles: true, cancelable: true, button: 0, detail: 1 }));
        const result = document.activeElement === terminal.textarea && terminal.element.classList.contains('focus');
        terminal.blur();
        return result;
      }));
      assert.deepEqual(focused, [true, true]);
    });

    await t.test('paste() and the paste event send what xterm.js sends', async () => {
      const texts = ['plain', 'a\r\nb\nc\rd', 'x\x1b[201~y'];
      for (const bracketed of [false, true]) {
        await setup(page);
        if (bracketed) await write(page, '\x1b[?2004h');
        for (const text of texts) {
          const api = await both(page, (name) => page.evaluate(([n, s]) => window.api.paste(n, s), [name, text]));
          const event = await both(page, (name) => page.evaluate(([n, s]) => {
            const clipboardData = new DataTransfer();
            clipboardData.setData('text/plain', s);
            window.api.terminal(n).textarea.dispatchEvent(new ClipboardEvent('paste', { clipboardData, bubbles: true, cancelable: true }));
          }, [name, text]));
          assert.deepEqual(api.ox, api.xt, `paste(${JSON.stringify(text)}), bracketed ${bracketed}`);
          assert.deepEqual(event.ox, event.xt, `paste event ${JSON.stringify(text)}, bracketed ${bracketed}`);
          assert.deepEqual(event.ox, api.ox);
        }
      }
      // The known bytes: newlines become CR; brackets wrap the paste and an ESC inside cannot end it.
      const shown = await both(page, (name) => page.evaluate((n) => window.api.paste(n, 'a\r\nb\x1b'), name));
      assert.deepEqual(shown.ox, ['\x1b[200~a\rb␛\x1b[201~']);
    });

    await t.test('focus reports follow the focus events mode', async () => {
      for (const mode of ['', '\x1b[?1004h']) {
        await setup(page);
        await write(page, mode);
        const sent = await both(page, async (name) => {
          await page.evaluate((n) => window.api.focus(n), name);
          await page.evaluate((n) => window.api.blur(n), name);
        });
        assert.deepEqual(sent.ox, sent.xt);
        // Turning the mode on reports the current focus (out) first.
        assert.deepEqual(sent.ox, mode ? ['\x1b[O', '\x1b[I', '\x1b[O'] : []);
      }
    });

    await t.test('replies fire one onData each, also several in one write, like xterm.js', async () => {
      const queries = ['\x1b[c', '\x1b[>c', '\x1b[6n', '\x1b[5n', '\x1b[4$p', '\x1b[?2004$p', '\x1b]10;?\x07', '\x1b]11;?\x1b\\', '\x1b]4;1;?\x07', '\x1b[?1004h'];
      await setup(page);
      for (const q of queries) await write(page, q);
      await write(page, `out${queries.join('')}\x1b]10;?;11;?\x07\r\n`);
      const sent = await take(page);
      assert.deepEqual(sent.ox, sent.xt);
      // Each query once alone, then all of them in one write, the double colour query answered twice.
      assert.equal(sent.ox.length, 2 * queries.length + 2);
    });

    await t.test('a custom key handler returning false stops the key', async () => {
      await setup(page);
      await page.evaluate(() => {
        window.seen = { ox: [], xt: [] };
        for (const name of ['ox', 'xt']) window.api.terminal(name).attachCustomKeyEventHandler((e) => {
          window.seen[name].push(`${e.type}:${e.key}`);
          return e.key !== 'a';
        });
      });
      const sent = await both(page, async (name) => {
        await press(page, 'KeyA')(name);
        await press(page, 'KeyB')(name);
      });
      assert.deepEqual(sent.ox, sent.xt);
      assert.deepEqual(sent.ox, ['b']);
      const seen = await page.evaluate(() => window.seen);
      assert.deepEqual(seen.ox, seen.xt);
      // Stopped, the keydown is not cancelled, so the browser still fires keypress for it; b is sent from keydown.
      assert.deepEqual(seen.ox, ['keydown:a', 'keypress:a', 'keyup:a', 'keydown:b', 'keyup:b']);
    });

    await t.test('disableStdin drops keys, pastes and replies', async () => {
      await setup(page, { disableStdin: true });
      const sent = await both(page, async (name) => {
        await press(page, 'KeyA')(name);
        await page.evaluate((n) => window.api.paste(n, 'p'), name);
      });
      await write(page, '\x1b[6n');
      const replies = await take(page);
      assert.deepEqual(sent, { ox: [], xt: [] });
      assert.deepEqual(replies, { ox: [], xt: [] });
      assert.equal(await page.evaluate(() => window.api.terminal('ox').textarea.readOnly), true);
      const enabled = await both(page, async (name) => {
        await page.evaluate((n) => window.api.setOption(n, 'disableStdin', false), name);
        await press(page, 'KeyA')(name);
      });
      assert.deepEqual(enabled, { ox: ['a'], xt: ['a'] });
      assert.equal(await page.evaluate(() => window.api.terminal('ox').textarea.readOnly), false);
    });

    await t.test('typing scrolls to the bottom unless scrollOnUserInput is false', async () => {
      for (const scrollOnUserInput of [true, false]) {
        await setup(page, { scrollOnUserInput });
        await write(page, '\r\n'.repeat(20));
        await page.evaluate(() => {
          for (const name of ['ox', 'xt']) window.api.terminal(name).scrollLines(-5);
        });
        await page.evaluate(() => window.api.frame());
        await both(page, press(page, 'KeyA'));
        const atBottom = await page.evaluate(() => ['ox', 'xt'].map((name) => {
          const { viewportY, baseY } = window.api.terminal(name).buffer.active;
          return viewportY === baseY;
        }));
        assert.deepEqual(atBottom, [scrollOnUserInput, scrollOnUserInput], `scrollOnUserInput ${scrollOnUserInput}`);
      }
    });

    await t.test('onUserInput fires before onData for input, never for replies', async () => {
      await setup(page);
      const order = await page.evaluate(async () => {
        const t = window.api.terminal('ox');
        const order = [];
        t.onUserInput(() => order.push('user'));
        t.onData((d) => order.push(d));
        await window.api.write('\x1b[6n');
        window.api.paste('ox', 'p');
        return order;
      });
      await press(page, 'KeyA')('ox');
      const typed = await page.evaluate(() => window.api.take().ox);
      assert.deepEqual(order, ['\x1b[1;1R', 'user', 'p']);
      assert.deepEqual(typed, ['\x1b[1;1R', 'p', 'a']);
    });

    await t.test('keys, keypad mode, pastes, mouse and focus reports right after the write that changed the modes use them, like xterm.js', async () => {
      await setup(page, { vtExtensions: { kittyKeyboard: true } });
      // Each step writes to a terminal and acts in the same task the write callback resolves in: no frame between.
      const sent = await page.evaluate(async () => {
        const key = (t, init) => t.textarea.dispatchEvent(new KeyboardEvent('keydown', { bubbles: true, cancelable: true, ...init }));
        const keypad = [];
        const steps = [
          ['\x1b[>1u', (t) => key(t, { key: ' ', code: 'Space', keyCode: 32, which: 32 })],
          ['', (t) => key(t, { key: 'Escape', code: 'Escape', keyCode: 27, which: 27 })],
          ['\x1b[<u\x1b[?1h', (t) => key(t, { key: 'ArrowUp', code: 'ArrowUp', keyCode: 38, which: 38 })],
          // DECKPAM: the mode reads set at once; like xterm.js it changes no key (NumpadEnter stays CR).
          ['\x1b[?1l\x1b=', (t) => {
            keypad.push(t.modes.applicationKeypadMode);
            key(t, { key: 'Enter', code: 'NumpadEnter', keyCode: 13, which: 13, location: 3 });
          }],
          ['\x1b>\x1b[?2004h', (t) => (keypad.push(t.modes.applicationKeypadMode), t.paste('p'))],
          ['\x1b[?2004l\x1b[?1000h\x1b[?1006h', (t) => {
            const screen = t.element.querySelector('.xterm-screen');
            const { left, top } = screen.getBoundingClientRect();
            screen.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, cancelable: true, button: 0, detail: 1, clientX: left + 2, clientY: top + 2 }));
          }],
          ['\x1b[?1000l\x1b[?1006l\x1b[?1004h', (t) => (t.focus(), t.blur())],
        ];
        const out = [];
        for (const [data, act] of steps) {
          for (const name of ['ox', 'xt']) {
            const t = window.api.terminal(name);
            await new Promise((resolve) => t.write(data, resolve));
            act(t);
          }
          out.push(window.api.take());
        }
        return { out, keypad };
      });
      assert.deepEqual(sent.keypad, [true, true, false, false]);
      for (const { ox, xt } of sent.out) assert.deepEqual(ox, xt);
      assert.deepEqual(sent.out.map(({ ox }) => ox), [[' '], ['\x1b[27u'], ['\x1bOA'], ['\r'], ['\x1b[200~p\x1b[201~'], ['\x1b[<0;1;1M'], ['\x1b[O', '\x1b[I', '\x1b[O']]);
    });

    await t.test('input() sends its data as if typed, or as is with wasUserInput false, like xterm.js', async () => {
      await setup(page, { scrollback: 50 });
      await write(page, 'abc\r\n'.repeat(20));
      const facts = await page.evaluate(() => ['ox', 'xt'].map((name) => {
        const t = window.api.terminal(name);
        const state = () => {
          const { viewportY, baseY } = t.buffer.active;
          return [viewportY === baseY, t.hasSelection()];
        };
        const run = (fn) => {
          t.scrollLines(-5);
          t.select(0, t.buffer.active.viewportY, 3);
          fn();
          return state();
        };
        const typed = run(() => t.input('\x1b[A'));
        const sent = run(() => t.input('raw', false));
        t.options.disableStdin = true;
        t.input('dropped');
        t.input('dropped', false);
        t.options.disableStdin = false;
        return { typed, sent };
      }));
      assert.deepEqual(facts[0], facts[1]);
      assert.deepEqual(facts[0], { typed: [true, false], sent: [false, true] });
      assert.deepEqual(await take(page), { ox: ['\x1b[A', 'raw'], xt: ['\x1b[A', 'raw'] });
      const order = await page.evaluate(() => {
        const t = window.api.terminal('ox');
        const seen = [];
        const subs = [t.onUserInput(() => seen.push('user')), t.onData((d) => seen.push(d))];
        t.input('a');
        t.input('b', false);
        subs.forEach((s) => s.dispose());
        return seen;
      });
      assert.deepEqual(order, ['user', 'a', 'b']);
      await take(page);
    });

    await t.test('kitty keyboard: vtExtensions, flags and reset (wongtty API)', async () => {
      const escape = async () => {
        await press(page, 'Escape')('ox');
        return (await take(page)).ox.join('');
      };
      await setup(page);
      await write(page, '\x1b[>1u');
      assert.equal(await page.evaluate(() => window.api.terminal('ox').kittyKeyboardFlags), 0, 'pushes are ignored with the extension off');
      assert.equal(await escape(), '\x1b');

      await setup(page, { vtExtensions: { kittyKeyboard: true } });
      await write(page, '\x1b[>1u');
      assert.equal(await page.evaluate(() => window.api.terminal('ox').kittyKeyboardFlags), 1);
      assert.equal(await escape(), '\x1b[27u');
      assert.equal(await page.evaluate(() => {
        const t = window.api.terminal('ox');
        t.resetKittyKeyboard();
        return t.kittyKeyboardFlags;
      }), 0, 'reset applies at once');
      assert.equal(await escape(), '\x1b');

      // Turning the extension off drops what the program pushed.
      await write(page, '\x1b[>1u');
      await page.evaluate(() => window.api.setOption('ox', 'vtExtensions', { kittyKeyboard: false }));
      await page.evaluate(() => window.api.frame());
      assert.equal(await page.evaluate(() => window.api.terminal('ox').kittyKeyboardFlags), 0);
      assert.equal(await escape(), '\x1b');
    });
  });
});
