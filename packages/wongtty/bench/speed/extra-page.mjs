// Page for extra.mjs: loads only the engine named by ?kind= (wongtty, xterm, ghostty) so startup measures that engine
// alone. Exposes setup/fill/scroll/resize/type/frame-work probes on window.
import { generate } from '../corpus.mjs';

const kind = new URLSearchParams(location.search).get('kind');
// Experiment: drop canvas copies from an OffscreenCanvas (wongtty's present) to see what they cost.
if (new URLSearchParams(location.search).get('nodraw')) {
  const d = CanvasRenderingContext2D.prototype.drawImage;
  CanvasRenderingContext2D.prototype.drawImage = function (src, ...a) {
    if (src instanceof OffscreenCanvas) return;
    return d.call(this, src, ...a);
  };
}
// Experiment: present each pane with a zero-copy transferToImageBitmap into a bitmaprenderer overlay instead of
// drawImage (what the "transfer" idea would do); counts full and region presents too.
window.drawStats = { full: 0, region: 0, px: 0 };
{
  const d = CanvasRenderingContext2D.prototype.drawImage;
  const xfer = new URLSearchParams(location.search).get('xfer');
  const overlays = new WeakMap();
  CanvasRenderingContext2D.prototype.drawImage = function (src, ...a) {
    if (!(src instanceof OffscreenCanvas)) return d.call(this, src, ...a);
    if (a.length > 2) { window.drawStats.region++; window.drawStats.px += a[2] * a[3]; } else { window.drawStats.full++; window.drawStats.px += src.width * src.height; }
    if (!xfer) return d.call(this, src, ...a);
    let o = overlays.get(this);
    if (!o) {
      const c = document.createElement('canvas');
      c.width = this.canvas.width;
      c.height = this.canvas.height;
      c.style.cssText = `position:absolute;left:${this.canvas.offsetLeft}px;top:${this.canvas.offsetTop}px;width:${this.canvas.clientWidth}px;height:${this.canvas.clientHeight}px;pointer-events:none`;
      this.canvas.offsetParent.appendChild(c);
      this.canvas.style.visibility = 'hidden';
      o = c.getContext('bitmaprenderer');
      overlays.set(this, o);
    }
    o.transferFromImageBitmap(src.transferToImageBitmap());
  };
}
const t0 = performance.now();
let make;
if (kind === 'xterm') {
  const [{ Terminal }, { WebglAddon }, { Unicode11Addon }] = await Promise.all([
    import('@xterm/xterm'),
    import('@xterm/addon-webgl'),
    import('@xterm/addon-unicode11'),
  ]);
  make = (options, host) => {
    const t = new Terminal(options);
    t.open(host);
    t.loadAddon(new WebglAddon());
    t.loadAddon(new Unicode11Addon());
    t.unicode.activeVersion = '11';
    return t;
  };
} else if (kind === 'ghostty') {
  const g = await import('ghostty-web');
  await g.init();
  make = (options, host) => {
    // ghostty-web's scrollback is in bytes; 16 MiB keeps at least 10,000 lines (as bench/engines/node.mjs).
    const t = new g.Terminal({ ...options, scrollback: options.scrollback * 1678 });
    t.open(host);
    return t;
  };
} else {
  const { init, Terminal } = await import('wongtty/xterm');
  await init();
  make = (options, host) => {
    const t = new Terminal(options);
    t.open(host);
    return t;
  };
}
window.initMs = performance.now() - t0;
window.loadedAt = performance.now();

let terms = [];
const frame = () => new Promise((r) => requestAnimationFrame(r));
/** Resolves after the next frame's rAF callbacks and the main-thread rendering steps that follow them. */
const frameDone = () =>
  new Promise((resolve) =>
    requestAnimationFrame((ts) => {
      // The frame's start time, so the work of rAF callbacks that ran before this one counts.
      const start = ts;
      const ch = new MessageChannel();
      ch.port1.onmessage = () => resolve({ start, end: performance.now(), ts });
      ch.port2.postMessage(0);
    }),
  );
window.frameDone = frameDone;
const writeP = (t, data) => new Promise((r) => t.write(data, r));

window.setup = async (n, cols, rows, scrollback = 10000) => {
  const start = performance.now();
  for (let i = 0; i < n; i++) {
    const host = document.createElement('div');
    host.style.cssText = 'display:inline-block;vertical-align:top';
    document.body.appendChild(host);
    terms.push(make({ cols, rows, scrollback, fontFamily: 'Menlo, monospace', fontSize: 12, allowProposedApi: true }, host));
  }
  const built = performance.now();
  await Promise.all(terms.map((t) => writeP(t, 'user@host:~/project$ ')));
  const parsed = performance.now();
  const f = await frameDone();
  return { buildMs: built - start, parsedMs: parsed - start, firstFrameMs: f.end - start, sinceNavMs: f.end };
};

const corpora = new Map();
const corpus = (name, cols, rows, size) => {
  const key = `${name} ${cols} ${rows} ${size}`;
  if (!corpora.has(key)) corpora.set(key, generate(name, size, cols, rows));
  return corpora.get(key);
};

/** Writes `size` bytes of `name` to every terminal and waits for it to be parsed and shown. */
window.fill = async (name, size) => {
  const c = corpus(name, terms[0].cols, terms[0].rows, size);
  const start = performance.now();
  await Promise.all(terms.map((t) => {
    for (let i = 0; i < c.length; i += 16384) t.write(c.subarray(i, i + 16384));
    return writeP(t, '\x1b[0m');
  }));
  await frameDone();
  await frameDone();
  return { ms: performance.now() - start, baseY: terms[0].buffer.active.baseY, viewportY: terms[0].buffer.active.viewportY };
};

const q = (v, p) => {
  const s = [...v].sort((a, b) => a - b);
  return s.length ? s[Math.min(s.length - 1, Math.floor(s.length * p))] : null;
};
const stats = (v) => ({ n: v.length, p50: q(v, 0.5), p95: q(v, 0.95), max: q(v, 1), mean: v.reduce((a, b) => a + b, 0) / v.length });

/** Scrolls the viewport of `which` terminals (count from 0) by `step` lines a frame for `frames` frames, up then down. */
window.scroll = async (which, step, nframes, mode = 'lines') => {
  const ts = terms.slice(0, which);
  const work = [];
  const gaps = [];
  let last = null;
  const len = ts[0].buffer.active.baseY;
  for (let i = 0; i < nframes; i++) {
    await new Promise((resolve) =>
      requestAnimationFrame((stamp) => {
        const s = stamp;
        if (last !== null) gaps.push(stamp - last);
        last = stamp;
        for (const t of ts) {
          if (mode === 'jump') t.scrollToLine(Math.floor(((i * 7919) % 97) / 97 * len));
          else t.scrollLines(i < nframes / 2 ? -step : step);
        }
        const ch = new MessageChannel();
        ch.port1.onmessage = () => {
          work.push(performance.now() - s);
          resolve();
        };
        ch.port2.postMessage(0);
      }),
    );
  }
  // The frame after the last scroll is where the renderers draw the last position.
  const f = await frameDone();
  for (const t of terms) t.scrollToBottom();
  await frameDone();
  return { work: stats(work), gap: stats(gaps), viewportY: ts[0].buffer.active.viewportY, last: f.end - f.start };
};

/** Resizes every terminal between colsA and colsB `count` times; time of the resize calls and of the frame after. */
window.resize = async (colsA, colsB, rows, count) => {
  const calls = [];
  const frames = [];
  for (let i = 0; i < count; i++) {
    const cols = i % 2 ? colsA : colsB;
    const s = performance.now();
    for (const t of terms) t.resize(cols, rows);
    calls.push(performance.now() - s);
    const f = await frameDone();
    frames.push(f.end - s);
  }
  return { call: stats(calls), toFrame: stats(frames), cols: terms[0].cols, baseY: terms[0].buffer.active.baseY };
};

/** Echo: terminal 0 writes back what it gets from the keyboard. Stages: keydown → onData → write callback → frame. */
const keyTimes = [];
const echoLat = [];
const stage = { data: [], parsed: [], frame: [] };
window.armEcho = () => {
  terms[0].focus();
  window.addEventListener('keydown', (e) => keyTimes.push(e.timeStamp), true);
  terms[0].onData((d) => {
    const k = keyTimes.shift();
    // The echo comes back as a later task, as PTY output over IPC does.
    const ch = new MessageChannel();
    ch.port1.onmessage = () => echo(d, k);
    ch.port2.postMessage(0);
  });
  const echo = (d, k) => {
    const td = performance.now();
    terms[0].write(d, () => {
      const tp = performance.now();
      frameDone().then((f) => {
        echoLat.push(f.end - k);
        stage.data.push(td - k);
        stage.parsed.push(tp - td);
        stage.frame.push(f.end - tp);
      });
    });
  };
};
window.echoStats = () => {
  const out = { ...stats(echoLat), data: stats(stage.data).p50, parsed: stats(stage.parsed).p50, frame: stats(stage.frame).p50 };
  echoLat.length = 0;
  for (const k of Object.keys(stage)) stage[k].length = 0;
  return out;
};

/** Background load like page.mjs 'agent': `bytes` every `ms` to terminals 1..n-1 (or all with includeFirst). */
let timers = [];
window.agent = (name, bytes, ms, includeFirst = false) => {
  const c = corpus(name, terms[0].cols, terms[0].rows, 4 << 20);
  const offsets = terms.map((_, i) => Math.floor((c.length / terms.length) * i));
  terms.forEach((t, i) => {
    if (i === 0 && !includeFirst) return;
    timers.push(setTimeout(() => {
      const tick = () => {
        let at = offsets[i];
        if (at + bytes > c.length) at = 0;
        offsets[i] = at + bytes;
        t.write(c.subarray(at, at + bytes));
      };
      tick();
      timers.push(setInterval(tick, ms));
    }, (ms / terms.length) * i));
  });
};
window.stopAgent = async () => {
  for (const t of timers) { clearTimeout(t); clearInterval(t); }
  timers = [];
  await Promise.all(terms.map((t) => writeP(t, '\x1b[0m')));
};

/** Frame work and gaps for `nframes` frames, while whatever is running runs. */
window.frames = async (nframes) => {
  const work = [];
  const gaps = [];
  let last = null;
  for (let i = 0; i < nframes; i++) {
    const f = await frameDone();
    if (last !== null) gaps.push(f.ts - last);
    last = f.ts;
    work.push(f.end - f.start);
  }
  return { work: stats(work), gap: stats(gaps) };
};

window.ready = true;
window.terms = () => terms;
