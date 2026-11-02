// Helpers for the renderer pages: canvas reads and a terminal in a container of its own, driven through the public
// API and the test-only `_rendererDebug`.
import { Terminal } from 'oxtty/xterm';

export const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/** The pixels of a canvas, copied through a 2D canvas (call in the frame that drew it); none while it has no size. */
export function read(canvas) {
  if (!canvas.width || !canvas.height) return new Uint8ClampedArray(0);
  const copy = document.createElement('canvas');
  copy.width = canvas.width;
  copy.height = canvas.height;
  const ctx = copy.getContext('2d');
  ctx.drawImage(canvas, 0, 0);
  return ctx.getImageData(0, 0, copy.width, copy.height).data;
}

/** Number of differing bytes. */
export function diff(a, b) {
  if (a.length !== b.length) return Math.max(a.length, b.length);
  let n = 0;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) n++;
  return n;
}

/** A terminal in a container of its own, with the font the tests measure against. */
export class Pane {
  constructor(cols, rows, options = {}) {
    this.container = document.createElement('div');
    document.body.appendChild(this.container);
    this.term = new Terminal({ cols, rows, fontFamily: 'Menlo, monospace', fontSize: 14, ...options });
    this.term.open(this.container);
    this.log = [];
  }

  /** Renderer state. */
  get debug() {
    const debug = this.term._rendererDebug();
    this.log.push(...debug.requests);
    return debug;
  }

  /** What the renderer asked the terminal for since the last call (`'all'` or rows to redraw). */
  takeRequests() {
    void this.debug;
    const log = this.log;
    this.log = [];
    return log;
  }

  get canvas() {
    return this.debug.canvas;
  }

  write(data) {
    return new Promise((resolve) => this.term.write(data, resolve));
  }

  /** Draws a frame (only what changed) and reads the canvas in that frame, before its buffer is cleared. */
  pixels() {
    const canvas = this.debug.canvas;
    this.term.refresh(0, -1);
    return new Promise((resolve) => requestAnimationFrame(() => resolve(read(canvas))));
  }

  /**
   * Draws a frame and reads, in that frame, the canvas alone (`canvas`) and with the cursor layer drawn over it
   * (`shown`, what the page shows in the blink's shown phase; the canvas alone without a layer), and whether the
   * layer is shown now.
   */
  layered() {
    this.term.refresh(0, -1);
    return new Promise((resolve) =>
      requestAnimationFrame(() => {
        const { canvas, cursorLayer: layer, cursorLayerRegion: region } = this.debug;
        const copy = document.createElement('canvas');
        copy.width = canvas.width;
        copy.height = canvas.height;
        const ctx = copy.getContext('2d');
        ctx.drawImage(canvas, 0, 0);
        const alone = ctx.getImageData(0, 0, copy.width, copy.height).data;
        if (layer) ctx.drawImage(layer, region[0], region[1]);
        const shown = ctx.getImageData(0, 0, copy.width, copy.height).data;
        resolve({ canvas: alone, shown, layer: layer ? layer.style.opacity === '1' : null, region });
      }),
    );
  }

  /** `[r, g, b]` at the device pixel at (`dx`, `dy`) of cell (col, row). */
  async cellColor(col, row, dx = 0.5, dy = 0.5) {
    const d = this.term.dimensions.device;
    const x = Math.floor((col + dx) * d.cell.width);
    const y = Math.floor((row + dy) * d.cell.height);
    const data = await this.pixels();
    const i = (y * this.canvas.width + x) * 4;
    return [data[i], data[i + 1], data[i + 2]];
  }

  focus(focused) {
    this.term.element.dispatchEvent(new FocusEvent(focused ? 'focusin' : 'focusout'));
  }

  dispose() {
    this.term.dispose();
    this.container.remove();
  }
}

