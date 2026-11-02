// Page for image.test.mjs: an wongtty/xterm Terminal with an image addon and a real xterm.js 6.1 Terminal with
// @xterm/addon-webgl and @xterm/addon-image, side by side with the same options and written the same bytes.
import { init, Terminal, ImageAddon } from 'wongtty/xterm';
import { Terminal as XTerm } from '@xterm/xterm';
import { WebglAddon } from '@xterm/addon-webgl';
import { ImageAddon as XImageAddon } from '@xterm/addon-image';
import initFrontEnd from '/dist/xterm_web.js';

const frames = (n = 2) => new Promise((resolve) => {
  const step = () => (n-- > 0 ? requestAnimationFrame(step) : resolve());
  step();
});
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

/**
 * A sixel with raster attributes for a `width` × `height` image; `color(x, y)` gives `[r, g, b]` in percent, or
 * null for a pixel left unpainted.
 */
function sixel(width, height, color) {
  const registers = new Map();
  let out = `"1;1;${width};${height}`;
  const pixels = [];
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const c = color(x, y);
      if (!c) {
        pixels.push(-1);
        continue;
      }
      const key = c.join(';');
      if (!registers.has(key)) {
        registers.set(key, registers.size);
        out += `#${registers.get(key)};2;${key}`;
      }
      pixels.push(registers.get(key));
    }
  }
  for (let band = 0; band < height; band += 6) {
    for (const register of registers.values()) {
      out += `#${register}`;
      for (let x = 0; x < width; x++) {
        let bits = 0;
        for (let r = 0; r < 6 && band + r < height; r++) if (pixels[(band + r) * width + x] === register) bits |= 1 << r;
        out += String.fromCharCode(63 + bits);
      }
      out += '$';
    }
    out += '-';
  }
  return out;
}

/** OSC 1337 payload (after `1337;`) of a PNG drawn by `draw(ctx)` on a `width` × `height` canvas. */
async function png(width, height, draw, header = '') {
  const canvas = document.createElement('canvas');
  canvas.width = width;
  canvas.height = height;
  draw(canvas.getContext('2d'));
  const blob = await new Promise((resolve) => canvas.toBlob(resolve, 'image/png'));
  const bytes = new Uint8Array(await blob.arrayBuffer());
  let binary = '';
  for (const b of bytes) binary += String.fromCharCode(b);
  return `File=inline=1;size=${bytes.length}${header}:${btoa(binary)}`;
}

let pair;

window.api = {
  sixel,
  png,
  /** Fresh terminals; `addon` options go to both image addons, `kind` picks wongtty's (`'builtin'` or `'xterm'`). */
  async setup(options = {}, addon = {}, kind = 'builtin') {
    if (pair) {
      pair.ox.dispose();
      pair.xt.dispose();
    }
    document.body.replaceChildren();
    const make = () => document.body.appendChild(document.createElement('div'));
    const common = { cols: 30, rows: 8, fontFamily: 'Menlo, monospace', fontSize: 14, ...options };
    const ox = new Terminal(common);
    const xt = new XTerm({ ...common, allowProposedApi: true });
    ox.open(make());
    xt.open(make());
    xt.loadAddon(new WebglAddon());
    const oxAddon = kind === 'builtin' ? new ImageAddon(addon) : new XImageAddon(addon);
    const xtAddon = new XImageAddon(addon);
    ox.loadAddon(oxAddon);
    xt.loadAddon(xtAddon);
    const data = { ox: [], xt: [] };
    ox.onData((d) => data.ox.push(d));
    xt.onData((d) => data.xt.push(d));
    pair = { ox, xt, oxAddon, xtAddon, data };
    // The addon's sixel decoder is instantiated asynchronously
    await sleep(50);
    await frames();
  },
  async writeBoth(text) {
    const { ox, xt } = pair;
    await Promise.all([new Promise((r) => ox.write(text, r)), new Promise((r) => xt.write(text, r))]);
    await frames(3);
  },
  /** The image sequence: `kind` is `'sixel'` (`body` the DCS payload) or `'iip'` (`body` after `1337;`). */
  sequence(kind, body, params = '') {
    return kind === 'sixel' ? `\x1bP${params}q${body}\x1b\\` : `\x1b]1337;${body}\x07`;
  },
  /** `prefix`, the image sequence and `suffix` to both in one write (PNGs decode while the parsers wait). */
  async image(kind, body, { prefix = '', suffix = '', params = '' } = {}) {
    await this.writeBoth(prefix + this.sequence(kind, body, params) + suffix);
  },
  /** What both answer to `sequence`. */
  async csi(sequence) {
    pair.data.ox.length = 0;
    pair.data.xt.length = 0;
    await this.writeBoth(sequence);
    return { ox: pair.data.ox.join(''), xt: pair.data.xt.join('') };
  },
  /** Cursors, the image tiles by viewport cell (`row:col` → tile, images numbered by first appearance) and storage. */
  // `_imageDebug().tiles` is `[row, col, id, tile]` per covered cell, flat
  state() {
    const { ox, xt, oxAddon, xtAddon } = pair;
    const debug = ox._imageDebug();
    const order = new Map();
    const name = (id) => order.get(id) ?? order.set(id, order.size).get(id);
    const oxTiles = {};
    for (let k = 0; k < debug.tiles.length; k += 4) {
      const [row, col, id, tile] = debug.tiles.subarray(k, k + 4);
      oxTiles[`${row}:${col}`] = `${name(id)}/${tile}`;
    }
    order.clear();
    const xtTiles = {};
    const buffer = xt._core.buffer;
    for (let row = 0; row < xt.rows; row++) {
      const line = buffer.lines.get(buffer.ydisp + row);
      for (let col = 0; col < xt.cols; col++) {
        const e = line?.getExtended?.(col)?.payload;
        if (e && e.imageId !== undefined && e.imageId !== -1 && e.tileId !== -1) xtTiles[`${row}:${col}`] = `${name(e.imageId)}/${e.tileId}`;
      }
    }
    const cursor = (t) => [t.buffer.active.cursorX, t.buffer.active.cursorY, t.buffer.active.viewportY];
    return {
      cursor: { ox: cursor(ox), xt: cursor(xt) },
      tiles: { ox: oxTiles, xt: xtTiles },
      usage: { ox: oxAddon.storageUsage, xt: xtAddon.storageUsage },
      limit: { ox: oxAddon.storageLimit, xt: xtAddon.storageLimit },
      debug,
    };
  },
  /**
   * Pixels of the image layer: xterm.js's `.xterm-image-layer-top` canvas against wongtty's picture, at the opaque
   * pixels of xterm.js's layer. Returns how many were compared, the largest channel difference and how many differ
   * by more than `tolerance`; also the opaque pixel counts of xterm.js's layer.
   */
  async compare(tolerance = 2) {
    const { ox, xt } = pair;
    const layer = xt.element.querySelector('.xterm-image-layer-top');
    const canvas = ox._rendererDebug().canvas;
    // The addon redraws only the rows a render covers: render all of them
    xt.refresh(0, xt.rows - 1);
    ox.refresh(0, ox.rows - 1);
    await frames(2);
    const oxPixels = await new Promise((resolve) => requestAnimationFrame(() => {
      const copy = document.createElement('canvas');
      copy.width = canvas.width;
      copy.height = canvas.height;
      const ctx = copy.getContext('2d');
      ctx.drawImage(canvas, 0, 0);
      resolve(ctx.getImageData(0, 0, copy.width, copy.height).data);
    }));
    if (!layer) return { compared: 0, max: 0, off: 0, opaque: 0, size: [canvas.width, canvas.height] };
    const xtPixels = layer.getContext('2d').getImageData(0, 0, layer.width, layer.height).data;
    let compared = 0;
    let max = 0;
    let off = 0;
    const width = Math.min(layer.width, canvas.width);
    const height = Math.min(layer.height, canvas.height);
    for (let y = 0; y < height; y++) {
      for (let x = 0; x < width; x++) {
        const i = (y * layer.width + x) * 4;
        if (xtPixels[i + 3] !== 255) continue;
        const j = (y * canvas.width + x) * 4;
        let d = 0;
        for (let c = 0; c < 3; c++) d = Math.max(d, Math.abs(xtPixels[i + c] - oxPixels[j + c]));
        compared++;
        max = Math.max(max, d);
        if (d > tolerance) off++;
      }
    }
    return { compared, max, off, size: [canvas.width, canvas.height], layer: [layer.width, layer.height] };
  },
  get pair() {
    return pair;
  },
};

await init();
/** The front end's wasm exports (its memory). */
window.frontEnd = () => initFrontEnd();
window.ready = true;
