// Page for selection.test.mjs: an wongtty/xterm Terminal next to the real xterm.js 6.1 (WebGL and Unicode 11 addons,
// so both have the same cells and character widths). The test writes the same output to both, dispatches the same
// CDP mouse events at the same cells of each, makes the same API calls and compares the selections.
import { init, Terminal } from 'wongtty/xterm';
import { Terminal as XtermTerminal } from '@xterm/xterm';
import { WebglAddon } from '@xterm/addon-webgl';
import { Unicode11Addon } from '@xterm/addon-unicode11';

const frame = () => new Promise((resolve) => requestAnimationFrame(() => resolve()));

async function setup({ cols: COLS, rows: ROWS, scrollback }) {
  const OPTIONS = { cols: COLS, rows: ROWS, fontFamily: 'Menlo, monospace', fontSize: 14, scrollback };
  await init();
  document.body.style.cssText = 'margin: 0; display: flex; gap: 40px; padding: 60px 10px';
  const [a, b] = [document.createElement('div'), document.createElement('div')];
  document.body.append(a, b);

  const wongtty = new Terminal(OPTIONS);
  wongtty.open(a);
  const xterm = new XtermTerminal({ ...OPTIONS, allowProposedApi: true });
  xterm.open(b);
  xterm.loadAddon(new Unicode11Addon());
  xterm.unicode.activeVersion = '11';
  xterm.loadAddon(new WebglAddon());
  const both = [wongtty, xterm];

  const changes = [0, 0];
  both.forEach((t, k) => t.onSelectionChange(() => changes[k]++));
  const data = [[], []];
  both.forEach((t, k) => t.onData((d) => data[k].push(d)));

  const screens = both.map((t) => t.element.querySelector('.xterm-screen'));

  /** Client coordinates of cell (col, row) of each terminal; `dx` moves within the cell (0 left edge, 1 right edge). */
  const points = (col, row, dx = 0.5) => screens.map((screen, k) => {
    const rect = screen.getBoundingClientRect();
    return [rect.left + (col + dx) * rect.width / both[k].cols, rect.top + (row + 0.5) * rect.height / both[k].rows];
  });

  Object.assign(window, {
    /** Writes to both, waits for both to parse and for wongtty's frame (which syncs its mouse modes). */
    write: async (text) => {
      await Promise.all(both.map((t) => new Promise((resolve) => t.write(text, resolve))));
      await frame();
      await frame();
    },
    points,
    /** Each terminal's selection; `changes` counts onSelectionChange since the last call. */
    state: () => {
      const result = both.map((t, k) => ({ text: t.getSelection(), has: t.hasSelection(), position: t.getSelectionPosition() ?? null, changes: changes[k] }));
      changes.fill(0);
      return { wongtty: result[0], xterm: result[1] };
    },
    call: (name, args) => both.forEach((t) => t[name](...args)),
    setOption: (key, value) => both.forEach((t) => { t.options[key] = value; }),
    focusTerminal: (k) => both[k].focus(),
    /** A `copy` event on each element: the text it put on the clipboard and whether it took the event. */
    copied: () => both.map((t) => {
      const event = new ClipboardEvent('copy', { clipboardData: new DataTransfer(), bubbles: true, cancelable: true });
      t.element.dispatchEvent(event);
      return [event.clipboardData.getData('text/plain'), event.defaultPrevented];
    }),
    /** What each textarea holds (the context menu copies from it). */
    textareas: () => both.map((t) => t.textarea.value),
    viewportY: () => both.map((t) => t.buffer.active.viewportY),
    takeData: () => data.map((d) => d.splice(0)),
    cells: () => screens.map((screen) => screen.getBoundingClientRect().width / COLS),
    /**
     * Mouse events built by script at cells of each terminal in turn, like wmux replays them (plainDragSelect.ts):
     * mousedown on the screen, the rest on the document.
     */
    synthetic: (events) => {
      for (const k of [0, 1]) {
        for (const [type, col, row, init] of events) {
          const [clientX, clientY] = points(col, row)[k];
          const target = type === 'mousedown' ? screens[k] : document;
          target.dispatchEvent(new MouseEvent(type, { ...init, clientX, clientY, bubbles: true, cancelable: true, composed: true }));
        }
      }
    },
  });
}

/** Builds both terminals with these sizes; resolves with an error message, or undefined. */
window.start = (size) => setup(size).then(() => undefined, (err) => String(err?.stack ?? err));
