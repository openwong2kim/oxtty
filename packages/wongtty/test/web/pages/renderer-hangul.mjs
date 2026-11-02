// Page for renderer-hangul.test.mjs: real xterm.js (WebGL addon) and wongtty/xterm side by side with the same font
// options and text (PRD Hangul fallback width risk), compared by cell metrics, pixels and where each glyph's ink lies.
import { Terminal as XtermTerminal } from '@xterm/xterm';
import { WebglAddon } from '@xterm/addon-webgl';
import { Unicode11Addon } from '/node_modules/@xterm/addon-unicode11/lib/addon-unicode11.mjs';
import { init, Terminal } from 'wongtty/xterm';
import { diff, read } from './kit.mjs';

// Headless Chrome with an emulated deviceScaleFactor reports the device-pixel-content-box in CSS pixels, so both
// renderers' device-pixel observers would shrink their canvas to half resolution at DPR 2. Without callbacks they
// keep the computed device size.
window.ResizeObserver = class {
  observe() {}
  unobserve() {}
  disconnect() {}
};

/** Leftmost and rightmost device-pixel columns with ink (brighter than the background) in each run of cells on row 0. */
function inkExtents(data, width, cellWidth, cellHeight, runs) {
  return runs.map(([from, to]) => {
    let left = -1;
    let right = -1;
    for (let x = from * cellWidth; x < to * cellWidth; x++) {
      for (let y = 0; y < cellHeight; y++) {
        const i = (y * width + x) * 4;
        if (data[i] + data[i + 1] + data[i + 2] > 3 * 0x60) {
          if (left < 0) left = x;
          right = x;
          break;
        }
      }
    }
    return [left - from * cellWidth, right - from * cellWidth];
  });
}

async function compare(text, options, runs) {
  const cols = 60;
  const rows = 2;
  const xhost = document.createElement('div');
  document.body.appendChild(xhost);
  const xterm = new XtermTerminal({ cols, rows, allowProposedApi: true, ...options });
  xterm.open(xhost);
  xterm.loadAddon(new Unicode11Addon());
  xterm.unicode.activeVersion = '11';
  xterm.loadAddon(new WebglAddon({ preserveDrawingBuffer: true }));
  await new Promise((resolve) => xterm.write(`\x1b[?25l${text}`, resolve));
  await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  const xdims = xterm._core._renderService.dimensions;
  // The WebGL canvas, not the link layer's
  const xcanvas = xhost.querySelector('.xterm-screen canvas:not(.xterm-link-layer)');
  const xdata = read(xcanvas);

  const ohost = document.createElement('div');
  document.body.appendChild(ohost);
  const term = new Terminal({ cols, rows, ...options });
  term.open(ohost);
  term.loadAddon(new Unicode11Addon());
  await new Promise((resolve) => term.write(`\x1b[?25l${text}`, resolve));
  const ocanvas = term._rendererDebug().canvas;
  term.refresh(0, -1);
  const odata = await new Promise((resolve) => requestAnimationFrame(() => resolve(read(ocanvas))));
  const odims = term.dimensions;
  const result = {
    xterm: { cell: xdims.device.cell, char: xdims.device.char, canvas: [xcanvas.width, xcanvas.height] },
    wongtty: { cell: odims.device.cell, char: odims.device.char, canvas: [ocanvas.width, ocanvas.height] },
    diff: diff(xdata, odata),
    xtermInk: inkExtents(xdata, xcanvas.width, xdims.device.cell.width, xdims.device.cell.height, runs),
    wongttyInk: inkExtents(odata, ocanvas.width, odims.device.cell.width, odims.device.cell.height, runs),
  };
  xterm.dispose();
  xhost.remove();
  term.dispose();
  ohost.remove();
  return result;
}

await init();
window.compare = compare;
window.ready = true;
