// Generates the parity fixtures of the Rust input encoders (crates/wongtty/src/input) from the real xterm.js 6.1 in
// headless Chrome: `node test/web/gen-input-fixtures.mjs` from packages/wongtty writes crates/wongtty/tests/fixtures/
// input-*.json, which crates/wongtty/tests/input.rs replays case by case. Not a test itself; rerun it when the
// xterm.js devDependency changes and commit the output.
//
// - input-keys-dom.json: the key matrix of the old DOM parity test (keys × modifiers × modes, CDP key presses on the
//   textarea), each press with the DOM events xterm.js handled, its evaluateKeyDown/evaluateKeyUp results and the
//   bytes it sent on onData.
// - input-keys-eval.json: evaluateKeyDown/evaluateKeyUp on script-built events, for what CDP cannot press: other
//   platforms, repeats and releases under every kitty flag, Meta, dead keys, non-ASCII and non-BMP keys.
// - input-mouse.json: the mouse matrix of the old DOM parity test (protocol × encoding × button × modifier × cell)
//   and its wheel cases, each event as MouseService got it, with the modes and the bytes it sent.
// - input-paste-focus.json: paste with and without bracketed paste mode, and focus reports.
import { writeFile } from 'node:fs/promises';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';
import { withPage } from './harness.mjs';

const require = createRequire(import.meta.url);
const XTERM = require('@xterm/xterm/package.json').version;
const OUT = join(dirname(fileURLToPath(import.meta.url)), '..', '..', '..', '..', 'crates', 'wongtty', 'tests', 'fixtures');
const SOURCE = 'packages/wongtty/test/web/gen-input-fixtures.mjs';

/** One JSON value per case line, so fixture diffs stay readable. */
async function save(name, meta, cases) {
  const head = JSON.stringify({ source: SOURCE, xterm: XTERM, ...meta });
  const body = `${head.slice(0, -1)},"cases":[\n${cases.map(c => JSON.stringify(c)).join(',\n')}\n]}\n`;
  await writeFile(join(OUT, name), body);
  console.log(`${name}: ${cases.length} cases, ${(body.length / 1024).toFixed(0)} KiB`);
}

const ready = page => page.waitForFunction(() => window.ready);
const api = (page, name, ...args) => page.evaluate(([n, a]) => window.api[n](...a), [name, args]);

// --- Keys over the DOM: the matrix of the old input-keys parity test (packages/wongtty/xterm/test at 0510a7d).
// Physical key codes, so Shift picks the shifted key and text as on a US layout.
const KEYS = [
  ...[...'ABCDEFGHIJKLMNOPQRSTUVWXYZ'].map(c => `Key${c}`), ...[...'0123456789'].map(d => `Digit${d}`),
  'Minus', 'Equal', 'BracketLeft', 'BracketRight', 'Backslash', 'Semicolon', 'Quote', 'Comma', 'Period', 'Slash', 'Backquote', 'Space',
  ...Array.from({ length: 12 }, (_, i) => `F${i + 1}`),
  'ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Home', 'End', 'PageUp', 'PageDown', 'Insert', 'Delete',
  'Enter', 'Tab', 'Backspace', 'Escape',
  ...Array.from({ length: 10 }, (_, i) => `Numpad${i}`),
  'NumpadAdd', 'NumpadSubtract', 'NumpadMultiply', 'NumpadDivide', 'NumpadDecimal', 'NumpadEnter',
];
const MODIFIERS = ['', 'Shift', 'Control', 'Alt', 'Meta', 'Shift+Control', 'Shift+Alt', 'Control+Alt'];
const KEY_MODES = [
  { name: 'normal', write: '' },
  { name: 'macOptionIsMeta', write: '', options: { macOptionIsMeta: true } },
  { name: 'APP_CURSOR', write: '\x1b[?1h' },
  { name: 'APP_KEYPAD', write: '\x1b=' },
  { name: 'kitty 1', write: '\x1b[>1u', options: { vtExtensions: { kittyKeyboard: true } } },
  { name: 'kitty 31', write: '\x1b[>31u', options: { vtExtensions: { kittyKeyboard: true } } },
];

async function keysDom() {
  return withPage('input-fixtures.mjs', async page => {
    await ready(page);
    const platform = await page.evaluate(() => navigator.platform);
    const modes = [];
    const cases = [];
    for (const [m, mode] of KEY_MODES.entries()) {
      await api(page, 'setup', mode.options ?? {});
      await api(page, 'write', mode.write);
      await api(page, 'focus');
      modes.push({ name: mode.name, options: mode.options ?? {}, state: await api(page, 'state') });
      for (const key of KEYS) {
        for (const modifier of MODIFIERS) {
          const press = modifier ? `${modifier}+${key}` : key;
          await page.keyboard.press(press);
          const { events, out } = await api(page, 'take');
          if (out.some(([kind]) => kind !== 'data')) throw new Error(`${press}: binary output`);
          cases.push([m, press, events, out.map(([, d]) => d).join('')]);
        }
      }
    }
    return { platform, modes, cases };
  });
}

// --- Keys through evaluateKeyDown/evaluateKeyUp on script-built events.
// [key, code, keyCode, shifted key, location]; the shifted key replaces `key` while Shift is held.
const LETTERS = [...'abcdefghijklmnopqrstuvwxyz'].map((c, i) => [c, `Key${c.toUpperCase()}`, 65 + i, c.toUpperCase()]);
const DIGITS = [...'0123456789'].map((d, i) => [d, `Digit${d}`, 48 + i, ')!@#$%^&*('[i]]);
const EVAL_KEYS = [
  ...LETTERS, ...DIGITS,
  [';', 'Semicolon', 186, ':'], ['=', 'Equal', 187, '+'], [',', 'Comma', 188, '<'], ['-', 'Minus', 189, '_'],
  ['.', 'Period', 190, '>'], ['/', 'Slash', 191, '?'], ['`', 'Backquote', 192, '~'], ['[', 'BracketLeft', 219, '{'],
  ['\\', 'Backslash', 220, '|'], [']', 'BracketRight', 221, '}'], ["'", 'Quote', 222, '"'], [' ', 'Space', 32, ' '],
  ['Enter', 'Enter', 13], ['Tab', 'Tab', 9], ['Backspace', 'Backspace', 8], ['Escape', 'Escape', 27],
  ['ArrowUp', 'ArrowUp', 38], ['ArrowDown', 'ArrowDown', 40], ['ArrowLeft', 'ArrowLeft', 37], ['ArrowRight', 'ArrowRight', 39],
  ['Home', 'Home', 36], ['End', 'End', 35], ['PageUp', 'PageUp', 33], ['PageDown', 'PageDown', 34],
  ['Insert', 'Insert', 45], ['Delete', 'Delete', 46],
  ...Array.from({ length: 25 }, (_, i) => [`F${i + 1}`, `F${i + 1}`, 112 + i]),
  ...Array.from({ length: 10 }, (_, i) => [`${i}`, `Numpad${i}`, 96 + i, `${i}`, 3]),
  ['+', 'NumpadAdd', 107, '+', 3], ['-', 'NumpadSubtract', 109, '-', 3], ['*', 'NumpadMultiply', 106, '*', 3],
  ['/', 'NumpadDivide', 111, '/', 3], ['.', 'NumpadDecimal', 110, '.', 3], ['Enter', 'NumpadEnter', 13, 'Enter', 3],
  ['=', 'NumpadEqual', 187, '=', 3], ['ArrowUp', 'Numpad8', 38, 'ArrowUp', 3], ['Home', 'Numpad7', 36, 'Home', 3],
  ['Clear', 'Numpad5', 12, 'Clear', 3], ['Delete', 'NumpadDecimal', 46, 'Delete', 3],
  ['Shift', 'ShiftLeft', 16, 'Shift', 1], ['Shift', 'ShiftRight', 16, 'Shift', 2], ['Control', 'ControlLeft', 17, 'Control', 1],
  ['Control', 'ControlRight', 17, 'Control', 2], ['Alt', 'AltLeft', 18, 'Alt', 1], ['Alt', 'AltRight', 18, 'Alt', 2],
  ['Meta', 'MetaLeft', 91, 'Meta', 1], ['Meta', 'MetaRight', 93, 'Meta', 2], ['AltGraph', 'AltRight', 225, 'AltGraph', 2],
  ['CapsLock', 'CapsLock', 20], ['NumLock', 'NumLock', 144], ['ScrollLock', 'ScrollLock', 145],
  ['PrintScreen', 'PrintScreen', 44], ['Pause', 'Pause', 19], ['ContextMenu', 'ContextMenu', 93],
  ['MediaPlayPause', 'MediaPlayPause', 179], ['MediaStop', 'MediaStop', 178], ['MediaTrackNext', 'MediaTrackNext', 176],
  ['MediaTrackPrevious', 'MediaTrackPrevious', 177], ['AudioVolumeDown', 'AudioVolumeDown', 174],
  ['AudioVolumeUp', 'AudioVolumeUp', 175], ['AudioVolumeMute', 'AudioVolumeMute', 173],
  // Dead keys (Alt+E/N/U/I on a US Mac layout, a dead backquote elsewhere)
  ['Dead', 'KeyE', 69], ['Dead', 'KeyN', 78], ['Dead', 'KeyU', 85], ['Dead', 'KeyI', 73], ['Dead', 'Backquote', 192],
  // iOS hardware keyboards
  ['UIKeyInputUpArrow', '', 0], ['UIKeyInputDownArrow', '', 0], ['UIKeyInputLeftArrow', '', 0], ['UIKeyInputRightArrow', '', 0],
  // Other layouts, macOS Option characters, IME and emoji keyboards
  ['é', 'Digit2', 50, '2'], ['å', 'KeyA', 65, 'Å'], ['ß', 'KeyS', 83, '§'], ['€', 'KeyE', 69, '€'], ['ä', 'Quote', 222, 'Ä'],
  ['ㅁ', 'KeyA', 65, 'ㅁ'], ['한', 'KeyG', 229, '한'], ['😀', '', 0, '😀'], ['∑', 'KeyW', 87, '„'],
  // Odd events: CapsLock on, no code, no key, IME processing
  ['A', 'KeyA', 65, 'a'], ['a', '', 65, 'A'], ['Unidentified', '', 0], ['', '', 0], ['Process', 'KeyA', 229],
];
const EVAL_FLAGS = [1, 2, 3, 8, 10, 16, 21, 31];

/** Every key × modifier set (bit 0 Shift, 1 Alt, 2 Ctrl, 3 Meta) × type, as evaluate() events. */
function evalEvents(types) {
  const list = [];
  for (const [key, code, keyCode, shifted, location = 0] of EVAL_KEYS) {
    for (let mods = 0; mods < 16; mods++) {
      for (const type of types) {
        list.push([type, {
          type: type === 'release' ? 'keyup' : 'keydown', key: mods & 1 && shifted !== undefined ? shifted : key, code, keyCode,
          location, shiftKey: !!(mods & 1), altKey: !!(mods & 2), ctrlKey: !!(mods & 4), metaKey: !!(mods & 8), repeat: type === 'repeat',
        }]);
      }
    }
  }
  return list;
}

async function keysEval() {
  const configs = [];
  const LEGACY = [{ appCursor: false }, { appCursor: true }];
  const platforms = [
    ['MacIntel', [...LEGACY.flatMap(c => [{ ...c, macOptionIsMeta: false }, { ...c, macOptionIsMeta: true }]),
      ...EVAL_FLAGS.flatMap(kittyFlags => [{ kittyFlags, macOptionIsMeta: false }, { kittyFlags, macOptionIsMeta: true }])]],
    ['Win32', LEGACY],
    ['Linux x86_64', LEGACY],
  ];
  const table = new Map();
  const index = r => {
    const k = JSON.stringify(r);
    if (!table.has(k)) table.set(k, table.size);
    return table.get(k);
  };
  for (const [platform, list] of platforms) {
    await withPage(`input-fixtures.mjs?platform=${encodeURIComponent(platform)}`, async page => {
      await ready(page);
      for (const config of list) {
        const kitty = config.kittyFlags ?? 0;
        // Legacy: releases send nothing and repeats equal presses, so presses only.
        const types = kitty ? ['press', 'repeat', 'release'] : ['press'];
        await api(page, 'setup', { macOptionIsMeta: !!config.macOptionIsMeta, vtExtensions: { kittyKeyboard: true } });
        await api(page, 'write', `${config.appCursor ? '\x1b[?1h' : ''}${kitty ? `\x1b[>${kitty}u` : ''}`);
        const state = await api(page, 'state');
        if (state.kittyFlags !== kitty || state.appCursor !== !!config.appCursor) throw new Error(`${platform}: modes not set`);
        const results = await api(page, 'evaluate', evalEvents(types));
        configs.push({ platform, macOptionIsMeta: !!config.macOptionIsMeta, state, types, results: results.map(index) });
      }
    });
  }
  return { keys: EVAL_KEYS, results: [...table.keys()].map(k => JSON.parse(k)), configs };
}

// --- Mouse: the matrix of the old mouse parity test.
const ALT = 1, CTRL = 2, SHIFT = 8;
const PROTOCOLS = { X10: 9, VT200: 1000, DRAG: 1002, ANY: 1003 };
const ENCODINGS = { DEFAULT: [], SGR: [1006], SGR_PIXELS: [1016] };
const MOUSE_OFF = '\x1b[?1000l\x1b[?1006l';
const BUTTONS = [['left', 1], ['middle', 4], ['right', 2]];
const MOUSE_MODIFIERS = [0, SHIFT, ALT, CTRL, ALT | CTRL | SHIFT];
// Cells around the single-byte limits of the default encoding: 95 + 32 = 127, 223 + 32 = 255
const CELLS = [[0, 0], [7, 3], [94, 40], [95, 95], [96, 99], [222, 50], [223, 60], [239, 98]];
const DELTAS = [-100, 100, -30, -30, 300];

async function mouse() {
  return withPage('input-fixtures.mjs', async page => {
    await ready(page);
    await api(page, 'setup', { cols: 240, rows: 100, fontSize: 6, scrollback: 1000 });
    await page.evaluate(() => {
      window.wheels = 0;
      window.addEventListener('wheel', () => setTimeout(() => window.wheels++), { capture: true, passive: true });
    });
    const cdp = await page.context().newCDPSession(page);
    const send = (type, [x, y], { button = 'none', buttons = 0, modifiers = 0, clickCount = 0, deltaX = 0, deltaY = 0 } = {}) =>
      cdp.send('Input.dispatchMouseEvent', { type, x, y, button, buttons, modifiers, clickCount, deltaX, deltaY });
    const cache = new Map();
    const point = async ([col, row]) => {
      const key = `${col},${row}`;
      if (!cache.has(key)) cache.set(key, await api(page, 'point', col, row));
      return cache.get(key);
    };
    let wheels = 0;
    /** One CDP wheel at a time, each waited for, so Chrome never merges them. */
    const wheel = async (cell, options) => {
      await send('mouseWheel', await point(cell), options);
      wheels++;
      await page.waitForFunction(n => window.wheels >= n, wheels);
    };
    const cases = [];
    const record = async label => {
      const { events, out } = await api(page, 'take');
      // Every byte sent belongs to a recorded event.
      if (JSON.stringify(events.flatMap(e => e[7])) !== JSON.stringify(out)) throw new Error(`${label}: unrecorded output`);
      cases.push([label, events]);
    };
    for (const [protocol, mode] of Object.entries(PROTOCOLS)) {
      for (const [encoding, extra] of Object.entries(ENCODINGS)) {
        await api(page, 'write', `${MOUSE_OFF}\x1b[?${[mode, ...extra].join(';')}h`);
        for (const [button, mask] of BUTTONS) {
          for (const modifiers of MOUSE_MODIFIERS) {
            for (const cell of CELLS) {
              // Hover, press, drag one cell right and down, release
              const from = await point(cell);
              const to = await point([Math.min(cell[0] + 1, 239), Math.min(cell[1] + 1, 99)]);
              await send('mouseMoved', from, { modifiers });
              await send('mousePressed', from, { button, buttons: mask, modifiers, clickCount: 1 });
              await send('mouseMoved', to, { button, buttons: mask, modifiers });
              await send('mouseReleased', to, { button, modifiers, clickCount: 1 });
              await record(`buttons ${protocol} ${encoding} ${button} ${modifiers} ${cell}`);
            }
          }
        }
      }
    }
    const buttonCases = cases.length;
    // Wheels: every delta with every modifier set, the modifiers changing from one wheel to the next
    const run = async (label, cells, modifiersList = [0, SHIFT, ALT, CTRL]) => {
      for (const cell of cells) {
        for (const deltaY of DELTAS) for (const modifiers of modifiersList) await wheel(cell, { deltaY, modifiers });
        await record(`wheel ${label} ${cell}`);
      }
    };
    for (const [protocol, mode] of Object.entries(PROTOCOLS)) {
      for (const [encoding, extra] of Object.entries(ENCODINGS)) {
        await api(page, 'write', `${MOUSE_OFF}\x1b[?${[mode, ...extra].join(';')}h`);
        await run(`${protocol} ${encoding}`, protocol === 'VT200' ? [[5, 5], [96, 99], [230, 40]] : [[96, 99]]);
      }
    }
    await api(page, 'write', MOUSE_OFF);
    await run('normal screen', [[5, 5]]);
    await api(page, 'write', '\x1b[?1049h');
    await run('alternate', [[5, 5], [96, 99], [230, 40]]);
    await api(page, 'write', '\x1b[?1h');
    await run('alternate DECCKM', [[5, 5], [96, 99]]);
    // Line and page wheels (only scripts can make them)
    for (const deltaMode of [1, 2]) {
      await api(page, 'syntheticWheel', 10, 10, { deltaY: -3, deltaMode });
      await api(page, 'syntheticWheel', 10, 10, { deltaY: 0.4, deltaMode });
      await api(page, 'syntheticWheel', 10, 10, { deltaY: 1, deltaMode, shiftKey: true });
      await record(`wheel alternate deltaMode ${deltaMode}`);
    }
    await api(page, 'write', '\x1b[?1l\x1b[?1000;1006h');
    for (const deltaMode of [1, 2]) {
      await api(page, 'syntheticWheel', 10, 10, { deltaY: 2, deltaMode });
      await api(page, 'syntheticWheel', 10, 10, { deltaY: -1, deltaMode, altKey: true });
      await record(`wheel VT200 SGR deltaMode ${deltaMode}`);
    }
    // Other sensitivities
    await api(page, 'setOption', 'scrollSensitivity', 3);
    await api(page, 'setOption', 'fastScrollSensitivity', 2);
    await run('sensitivity VT200 SGR', [[5, 5]]);
    await api(page, 'write', '\x1b[?1000;1006l');
    await run('sensitivity alternate', [[5, 5]]);
    await api(page, 'setOption', 'scrollSensitivity', 1);
    await api(page, 'setOption', 'fastScrollSensitivity', 5);
    // mouseEventsRequireAlt: only Alt clicks are reported, without the Alt bit; wheels as usual
    await api(page, 'setOption', 'mouseEventsRequireAlt', true);
    await api(page, 'write', '\x1b[?1002;1006h');
    for (const modifiers of [0, ALT, ALT | CTRL, SHIFT]) {
      const from = await point([10, 10]);
      const to = await point([12, 11]);
      await send('mousePressed', from, { button: 'left', buttons: 1, modifiers, clickCount: 1 });
      await send('mouseMoved', to, { button: 'left', buttons: 1, modifiers });
      await send('mouseReleased', to, { button: 'left', modifiers, clickCount: 1 });
      await wheel([12, 11], { modifiers, deltaY: -100 });
      await record(`requireAlt ${modifiers}`);
    }
    return { options: { cols: 240, rows: 100, scrollback: 1000 }, buttonCases, cases };
  }, { viewport: { width: 2000, height: 820 } });
}

// --- Paste and focus
const PASTES = ['', 'abc', 'a\nb', 'a\r\nb', 'a\rb', '\n\n', '\r\n\r\n', '\r\r\n\n\r', 'x\x1by', '\x1b[201~evil\x1b[200~',
  '한글\n😀', 'tab\there', '\x03\x04', 'end\n', '␛'];

async function pasteFocus() {
  return withPage('input-fixtures.mjs', async page => {
    await ready(page);
    const cases = [];
    for (const [label, options, write] of [['plain', {}, ''], ['bracketed', {}, '\x1b[?2004h'],
      ['bracketed, ignoreBracketedPasteMode', { ignoreBracketedPasteMode: true }, '\x1b[?2004h']]) {
      await api(page, 'setup', options);
      await api(page, 'write', write);
      const state = await api(page, 'state');
      for (const text of PASTES) {
        await api(page, 'paste', text);
        const { out } = await api(page, 'take');
        cases.push(['paste', label, options, state, text, out.map(([, d]) => d).join('')]);
      }
    }
    await api(page, 'setup');
    const step = async (label, action) => {
      await action();
      const { out } = await api(page, 'take');
      cases.push(['focus', label, await api(page, 'state'), out.map(([, d]) => d).join('')]);
    };
    await step('focus, mode off', () => api(page, 'focus'));
    await step('blur, mode off', () => api(page, 'blur'));
    await step('mode on while blurred', () => api(page, 'write', '\x1b[?1004h'));
    await step('focus', () => api(page, 'focus'));
    await step('blur', () => api(page, 'blur'));
    await step('focus again', () => api(page, 'focus'));
    await step('mode on again while focused', () => api(page, 'write', '\x1b[?1004l\x1b[?1004h'));
    await step('mode off', () => api(page, 'write', '\x1b[?1004l'));
    await step('blur, mode off again', () => api(page, 'blur'));
    return cases;
  });
}

const dom = await keysDom();
await save('input-keys-dom.json', { platform: dom.platform, modes: dom.modes }, dom.cases);
const evaluated = await keysEval();
await save('input-keys-eval.json', { keys: evaluated.keys, results: evaluated.results }, evaluated.configs);
const mice = await mouse();
await save('input-mouse.json', { options: mice.options, buttonCases: mice.buttonCases }, mice.cases);
await save('input-paste-focus.json', {}, await pasteFocus());
