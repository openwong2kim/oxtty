// Key parity at the DOM level: the same CDP key events (Playwright's keyboard sends Input.dispatchKeyEvent) go to
// the focused textarea of an wongtty/xterm Terminal and of a real xterm.js 6.1 Terminal on one page, and both must
// send the same bytes on onData, over keys × modifiers × modes. Both enter a mode from the same escape sequence.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

// Set to `false` once the member lands in crates/wongtty-web.
const TODO = false;

// Physical key codes, so Shift picks the shifted key and text as on a US layout.
const KEYS = [
  ...[...'ABCDEFGHIJKLMNOPQRSTUVWXYZ'].map((c) => `Key${c}`), ...[...'0123456789'].map((d) => `Digit${d}`),
  'Minus', 'Equal', 'BracketLeft', 'BracketRight', 'Backslash', 'Semicolon', 'Quote', 'Comma', 'Period', 'Slash', 'Backquote', 'Space',
  ...Array.from({ length: 12 }, (_, i) => `F${i + 1}`),
  'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Home', 'End', 'PageUp', 'PageDown', 'Insert', 'Delete',
  'Enter', 'Tab', 'Backspace', 'Escape',
  ...Array.from({ length: 10 }, (_, i) => `Numpad${i}`),
  'NumpadAdd', 'NumpadSubtract', 'NumpadMultiply', 'NumpadDivide', 'NumpadDecimal', 'NumpadEnter',
];
const MODIFIERS = ['', 'Shift', 'Control', 'Alt', 'Meta', 'Shift+Control', 'Shift+Alt', 'Control+Alt'];
// macOptionIsMeta matters on macOS only; without it Option is a third-level shift, and the synthetic Alt presses
// carry no Option character, so they send nothing in either terminal.
const MODES = [
  { name: 'normal', write: '' },
  { name: 'macOptionIsMeta', write: '', options: { macOptionIsMeta: true } },
  { name: 'APP_CURSOR', write: '\x1b[?1h' },
  { name: 'APP_KEYPAD', write: '\x1b=' },
  { name: 'kitty 1', write: '\x1b[>1u', options: { vtExtensions: { kittyKeyboard: true } } },
  { name: 'kitty 31', write: '\x1b[>31u', options: { vtExtensions: { kittyKeyboard: true } } },
];

const show = (chunks) => JSON.stringify(chunks.join(''));

test('key presses send the bytes xterm.js sends, per key, modifier and mode', { todo: TODO, timeout: 300000 }, async () => {
  const result = await withPage('parity.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    const mismatches = [];
    const sent = {};
    let presses = 0;
    for (const mode of MODES) {
      await page.evaluate(([options, write]) => window.api.setup(options).then(() => window.api.write(write)), [mode.options ?? {}, mode.write]);
      for (const key of KEYS) {
        for (const modifier of MODIFIERS) {
          const press = modifier ? `${modifier}+${key}` : key;
          await page.evaluate(() => window.api.focus('ox'));
          await page.keyboard.press(press);
          await page.evaluate(() => window.api.focus('xt'));
          await page.keyboard.press(press);
          const { ox, xt } = await page.evaluate(() => window.api.take());
          presses++;
          sent[`${mode.name} ${press}`] = xt.join('');
          if (ox.join('') !== xt.join('')) mismatches.push(`${mode.name} ${press}: wongtty ${show(ox)} xterm.js ${show(xt)}`);
        }
      }
    }
    return { mismatches, presses, sent };
  });
  assert.equal(result.presses, KEYS.length * MODIFIERS.length * MODES.length);
  assert.deepEqual(result.mismatches, []);
  // The comparison is not vacuous: most presses send something, and known keys send their known bytes.
  const empty = Object.values(result.sent).filter((d) => d === '').length;
  assert.ok(empty < result.presses / 2, `${empty} of ${result.presses} presses sent nothing`);
  assert.equal(result.sent['normal KeyA'], 'a');
  assert.equal(result.sent['normal Shift+KeyA'], 'A');
  assert.equal(result.sent['normal Control+KeyC'], '\x03');
  assert.equal(result.sent['normal ArrowUp'], '\x1b[A');
  assert.equal(result.sent['APP_CURSOR ArrowUp'], '\x1bOA');
  assert.equal(result.sent['normal Shift+F5'], '\x1b[15;2~');
  assert.equal(result.sent['macOptionIsMeta Alt+KeyB'], '\x1bb');
  assert.equal(result.sent['kitty 1 Escape'], '\x1b[27u');
  assert.equal(result.sent['kitty 1 Shift+Enter'], '\x1b[13;2u');
  assert.equal(result.sent['kitty 31 KeyA'], '\x1b[97;;97u\x1b[97;1:3u');
});
