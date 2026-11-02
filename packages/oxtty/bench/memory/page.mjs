// The page bench.mjs opens: `setup(kind, n, cols, rows, lines, blink)` builds `n` terminals of one kind ('oxtty' or
// 'xterm' with @xterm/addon-webgl), all in view, feeds each the same log output and waits a few frames. `idle()`
// focuses the first terminal; `counts()` reports the WebGL draw calls and canvas `drawImage` calls since its last call.
// `breakdown()` reports the WebAssembly memories and canvas backing stores (for breakdown.mjs); `hide(hidden)` and
// `dispose()` take the terminals out of view and dispose of them; `startBusy(interval)` and `stopBusy()` keep every
// terminal printing (busy.mjs).
import { Terminal as XtermTerminal } from '@xterm/xterm';
import { WebglAddon } from '@xterm/addon-webgl';
import { init, Terminal } from 'oxtty/xterm';

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

// Counts every WebGL draw call, 2D `drawImage` and `transferFromImageBitmap`, whichever terminal or context makes it
const counts = { gl: 0, drawImage: 0, bitmap: 0 };
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
if (globalThis.ImageBitmapRenderingContext) {
  const proto = ImageBitmapRenderingContext.prototype;
  const original = proto.transferFromImageBitmap;
  proto.transferFromImageBitmap = function (...args) {
    counts.bitmap++;
    return original.apply(this, args);
  };
}
for (const proto of [CanvasRenderingContext2D.prototype, OffscreenCanvasRenderingContext2D.prototype]) {
  const original = proto.drawImage;
  proto.drawImage = function (...args) {
    counts.drawImage++;
    return original.apply(this, args);
  };
}

// Every WebAssembly memory the page instantiates, and every canvas it makes, to report their sizes
const memories = [];
function recordInstance(instance) {
  const memory = instance?.exports?.memory;
  if (memory instanceof WebAssembly.Memory && !memories.some((m) => m.memory === memory)) {
    memories.push({ name: 'oxtty_out_ptr' in instance.exports ? 'engine' : 'front end', memory });
  }
}
for (const key of ['instantiate', 'instantiateStreaming']) {
  const original = WebAssembly[key];
  WebAssembly[key] = async function (...args) {
    const result = await original.apply(this, args);
    recordInstance(result.instance ?? result);
    return result;
  };
}
WebAssembly.Instance = new Proxy(WebAssembly.Instance, {
  construct(target, args) {
    const instance = Reflect.construct(target, args);
    recordInstance(instance);
    return instance;
  },
});
const canvases = [];
const createElement = Document.prototype.createElement;
Document.prototype.createElement = function (name, ...rest) {
  const element = createElement.call(this, name, ...rest);
  if (String(name).toLowerCase() === 'canvas') canvases.push(new WeakRef(element));
  return element;
};
const Offscreen = globalThis.OffscreenCanvas;
if (Offscreen) {
  globalThis.OffscreenCanvas = class extends Offscreen {
    constructor(...args) {
      super(...args);
      canvases.push(new WeakRef(this));
    }
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
    // `mixed`: sizes differ from pane to pane, as in a split layout
    const size = window.mixed ? { cols: cols - (i % 5) * 7, rows: rows - (i % 3) * 5 } : { cols, rows };
    const options = { ...size, scrollback: 10000, fontFamily: 'Menlo, monospace', fontSize: 12, cursorBlink: blink };
    const term = kind === 'xterm' ? new XtermTerminal(options) : new Terminal(options);
    term.open(host);
    if (kind === 'xterm') term.loadAddon(new WebglAddon());
    for (let k = 0; k < lines / 1000; k++) await new Promise((resolve) => term.write(chunk(size.cols, 7 + k), resolve));
    terms.push(term);
  }
  await frames(20);
  window.terms = terms;
  const gl = document.createElement('canvas').getContext('webgl2');
  const info = gl?.getExtension('WEBGL_debug_renderer_info');
  return { gpuName: info ? gl.getParameter(info.UNMASKED_RENDERER_WEBGL) : '?', canvases: document.querySelectorAll('canvas').length };
};

window.idle = () => {
  window.terms[0].focus();
  return document.hasFocus();
};
window.counts = () => {
  const out = { ...counts };
  counts.gl = counts.drawImage = counts.bitmap = 0;
  return out;
};

window.breakdown = () => {
  const canvas = { attached: 0, attachedCount: 0, detached: 0, detachedCount: 0, offscreen: 0, offscreenCount: 0 };
  for (const ref of canvases) {
    const c = ref.deref();
    if (!c || c.width * c.height === 0) continue;
    const kind = Offscreen && c instanceof Offscreen ? 'offscreen' : c.isConnected ? 'attached' : 'detached';
    canvas[kind] += c.width * c.height * 4;
    canvas[`${kind}Count`]++;
  }
  return { wasm: memories.map((m) => ({ name: m.name, bytes: m.memory.buffer.byteLength })), canvas };
};
// Busy: every terminal gets a few lines of output every `interval` ms, as agents printing; frames are counted
let busyTimer = null;
let busyFrames = 0;
window.startBusy = (interval) => {
  let seed = 1;
  busyFrames = 0;
  const frame = () => {
    if (busyTimer === null) return;
    busyFrames++;
    requestAnimationFrame(frame);
  };
  busyTimer = setInterval(() => {
    for (const term of window.terms) term.write(chunk(term.cols, seed++).split('\r\n').slice(0, 3).join('\r\n') + '\r\n');
  }, interval);
  requestAnimationFrame(frame);
};
window.stopBusy = () => {
  clearInterval(busyTimer);
  busyTimer = null;
  return busyFrames;
};
window.hide = async (hidden) => {
  for (const term of window.terms) term.element.parentElement.style.display = hidden ? 'none' : 'inline-block';
  await frames(10);
};
window.dispose = async () => {
  for (const term of window.terms) term.dispose();
  window.terms = [];
  await frames(10);
};

await init();
window.ready = true;
