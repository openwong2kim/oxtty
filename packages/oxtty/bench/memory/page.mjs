// The page bench.mjs opens: `setup(kind, n, cols, rows, lines, blink)` builds `n` terminals of one kind ('oxtty',
// 'viewport' or 'xterm' with @xterm/addon-webgl), all in view, feeds each the same log output and waits a few frames.
// 'viewport' is oxtty in viewport-only mode: a scrollback of VIEWPORT_MARGIN lines and a history provider standing for
// wmux's daemon, which keeps the history in another process (here it makes rows up on demand and holds nothing).
// `idle()` focuses the first terminal; `counts()` reports the WebGL draw calls and canvas `drawImage` calls since its
// last call.
import { Terminal as XtermTerminal } from '@xterm/xterm';
import { WebglAddon } from '@xterm/addon-webgl';
import { init, Terminal } from 'oxtty/xterm';
import initFrontEnd from '/dist/xterm_web.js';
import { Terminal as Engine } from 'oxtty';

/** Local scrollback of a viewport-only terminal: the margin wmux would use. */
const VIEWPORT_MARGIN = 200;

/** 1000 lines of coloured words (Hangul, box drawing, paths), deterministic for a seed. */
function chunk(cols, seed0) {
  const words = ['build', 'PASS', '한글', '터미널', 'error:', '│', '─┼─', 'src/render/webgl.rs', '0x1f600', 'ok'];
  let s = '';
  let seed = seed0;
  for (let line = 0; line < 1000; line++) {
    let width = 0;
    while (width < cols - 12) {
      seed = (seed * 1103515245 + 12345) & 0x7fffffff;
      const word = words[seed % words.length];
      s += `\x1b[${31 + (seed % 7)}m${word}\x1b[0m `;
      width += [...word].reduce((w, c) => w + (c.charCodeAt(0) > 0x1100 ? 2 : 1), 0) + 1;
    }
    s += '\r\n';
  }
  return s;
}

// Counts every WebGL draw call and 2D `drawImage`, whichever terminal or context makes it
const counts = { gl: 0, drawImage: 0 };
for (const proto of [WebGLRenderingContext.prototype, WebGL2RenderingContext.prototype]) {
  for (const name of ['drawArrays', 'drawElements', 'drawArraysInstanced', 'drawElementsInstanced', 'drawRangeElements']) {
    const original = proto[name];
    if (!original) continue;
    proto[name] = function (...args) {
      counts.gl++;
      return original.apply(this, args);
    };
  }
}
for (const proto of [CanvasRenderingContext2D.prototype, OffscreenCanvasRenderingContext2D.prototype]) {
  const original = proto.drawImage;
  proto.drawImage = function (...args) {
    counts.drawImage++;
    return original.apply(this, args);
  };
}

const frames = (n) =>
  new Promise((resolve) => {
    const next = () => (n-- > 0 ? requestAnimationFrame(next) : resolve());
    next();
  });

window.setup = async (kind, n, cols, rows, lines, blink) => {
  const terms = [];
  for (let i = 0; i < n; i++) {
    const host = document.createElement('div');
    host.style.cssText = 'display:inline-block;vertical-align:top';
    document.body.appendChild(host);
    const scrollback = kind === 'viewport' ? VIEWPORT_MARGIN : 10000;
    const options = { cols, rows, scrollback, fontFamily: 'Menlo, monospace', fontSize: 12, cursorBlink: blink };
    const term = kind === 'xterm' ? new XtermTerminal(options) : new Terminal(options);
    term.open(host);
    if (kind === 'xterm') term.loadAddon(new WebglAddon());
    if (kind === 'viewport') {
      term.setHistoryProvider({ length: 0, limit: 10000, fetch: async (start, count) => new Array(count).fill(`line ${start}`) });
    }
    for (let k = 0; k < lines / 1000; k++) await new Promise((resolve) => term.write(chunk(cols, 7 + k), resolve));
    terms.push(term);
  }
  await frames(20);
  window.terms = terms;
  const gl = document.createElement('canvas').getContext('webgl2');
  const info = gl?.getExtension('WEBGL_debug_renderer_info');
  // WebAssembly memory: the engine instance all terminals share (read through a probe's row view) and the front end.
  let wasm = null;
  if (kind !== 'xterm') {
    const probe = new Engine({ cols: 2, rows: 1, scrollback: 0 });
    wasm = { engine: probe.rowCellsView(0).buffer.byteLength, frontEnd: (await initFrontEnd()).memory.buffer.byteLength };
    probe.dispose();
  }
  return { gpuName: info ? gl.getParameter(info.UNMASKED_RENDERER_WEBGL) : '?', canvases: document.querySelectorAll('canvas').length, wasm };
};

window.idle = () => {
  window.terms[0].focus();
  return document.hasFocus();
};
window.counts = () => {
  const out = { ...counts };
  counts.gl = counts.drawImage = 0;
  return out;
};

await init();
window.ready = true;
