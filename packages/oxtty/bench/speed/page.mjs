// The page bench.mjs opens. `setup(kind, n, cols, rows)` builds `n` terminals of one kind, all in view: 'oxtty'
// (oxtty/xterm), 'xterm' (xterm.js with @xterm/addon-webgl and Unicode 11 widths) or 'ghostty' (ghostty-web).
// `run(workload, mode, seconds)` keeps every terminal printing scenario `workload` (../corpus.mjs) and reports what
// the page saw:
//
// - mode 'agent': every terminal gets AGENT_BYTES every AGENT_MS (staggered from terminal to terminal), as agents
//   streaming output;
// - mode 'flat': every terminal has FLAT_INFLIGHT chunks of FLAT_BYTES in flight and writes the next one as soon as
//   a write callback reports one parsed (the flow control xterm.js recommends for a PTY).
//
// Reported: bytes whose write callback fired, write→frame latency (from `write` to the first animation frame that
// starts after its callback), animation frames, and long tasks (PerformanceObserver 'longtask').
import { Terminal as XtermTerminal } from '@xterm/xterm';
import { WebglAddon } from '@xterm/addon-webgl';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { init, Terminal } from 'oxtty/xterm';
import { generate } from '../corpus.mjs';

const AGENT_BYTES = 2048;
const AGENT_MS = 50;
const FLAT_BYTES = 16384;
const FLAT_INFLIGHT = 8;
/** Each workload's output, repeated as needed; every terminal starts at a different place in it. */
const CORPUS_BYTES = 4 << 20;

let ghostty = null;
const corpora = new Map();
let terms = [];
let hosts = [];

window.setup = async (kind, n, cols, rows) => {
  for (const t of terms) t.dispose();
  for (const h of hosts) h.remove();
  terms = [];
  hosts = [];
  if (kind === 'ghostty' && !ghostty) {
    ghostty = await import('ghostty-web');
    await ghostty.init();
  }
  for (let i = 0; i < n; i++) {
    const host = document.createElement('div');
    host.style.cssText = 'display:inline-block;vertical-align:top';
    document.body.appendChild(host);
    const options = { cols, rows, scrollback: 10000, fontFamily: 'Menlo, monospace', fontSize: 12, allowProposedApi: true };
    let term;
    if (kind === 'xterm') {
      term = new XtermTerminal(options);
      term.open(host);
      term.loadAddon(new WebglAddon());
      term.loadAddon(new Unicode11Addon());
      term.unicode.activeVersion = '11';
    } else if (kind === 'ghostty') {
      term = new ghostty.Terminal(options);
      term.open(host);
    } else {
      term = new Terminal(options);
      term.open(host);
    }
    terms.push(term);
    hosts.push(host);
  }
  await frames(10);
  return { canvases: document.querySelectorAll('canvas').length };
};

const frames = (n) =>
  new Promise((resolve) => {
    const next = () => (n-- > 0 ? requestAnimationFrame(next) : resolve());
    next();
  });

window.run = (workload, mode, seconds) => {
  const cols = terms[0].cols;
  const rows = terms[0].rows;
  const key = `${workload} ${cols}x${rows}`;
  if (!corpora.has(key)) corpora.set(key, generate(workload, CORPUS_BYTES, cols, rows));
  const corpus = corpora.get(key);
  const size = mode === 'agent' ? AGENT_BYTES : FLAT_BYTES;
  const offsets = terms.map((_, i) => Math.floor((corpus.length / terms.length) * i));
  const next = (i) => {
    let at = offsets[i];
    if (at + size > corpus.length) at = 0;
    offsets[i] = at + size;
    return corpus.subarray(at, at + size);
  };

  let accepted = 0;
  let writes = 0;
  let running = true;
  const latencies = [];
  // Parsed writes waiting for the next frame: their write times.
  let parsed = [];
  let frameCount = 0;
  const onFrame = () => {
    if (!running) return;
    frameCount++;
    const now = performance.now();
    for (const t0 of parsed) latencies.push(now - t0);
    parsed = [];
    requestAnimationFrame(onFrame);
  };
  requestAnimationFrame(onFrame);
  const longTasks = [];
  const observer = new PerformanceObserver((list) => {
    for (const e of list.getEntries()) longTasks.push(e.duration);
  });
  observer.observe({ type: 'longtask' });

  const write = (i, after) => {
    const chunk = next(i);
    const t0 = performance.now();
    writes++;
    terms[i].write(chunk, () => {
      if (!running) return;
      accepted += chunk.length;
      parsed.push(t0);
      after?.();
    });
  };
  const timers = [];
  if (mode === 'agent') {
    terms.forEach((_, i) => {
      timers.push(setTimeout(() => {
        write(i);
        timers.push(setInterval(() => write(i), AGENT_MS));
      }, (AGENT_MS / terms.length) * i));
    });
  } else {
    const pump = (i) => write(i, () => pump(i));
    terms.forEach((_, i) => {
      for (let k = 0; k < FLAT_INFLIGHT; k++) pump(i);
    });
  }

  const start = performance.now();
  return new Promise((resolve) => {
    setTimeout(() => {
      running = false;
      const ms = performance.now() - start;
      for (const t of timers) {
        clearTimeout(t);
        clearInterval(t);
      }
      observer.disconnect();
      latencies.sort((a, b) => a - b);
      const q = (p) => (latencies.length ? latencies[Math.min(latencies.length - 1, Math.floor(latencies.length * p))] : null);
      resolve({
        ms,
        accepted,
        writes,
        frames: frameCount,
        latency: { n: latencies.length, p50: q(0.5), p95: q(0.95) },
        longTasks: { count: longTasks.length, ms: longTasks.reduce((a, b) => a + b, 0) },
      });
    }, seconds * 1000);
  });
};

/** Waits until every terminal parsed what it was given, so the next run starts from an idle page. */
window.settle = async () => {
  // ghostty-web cannot take an empty write; an SGR reset changes nothing on screen.
  await Promise.all(terms.map((t) => new Promise((resolve) => t.write('\x1b[0m', resolve))));
  await frames(5);
};

/** The visible rows of terminal `i`, to check the engines show the same screen. */
window.screen = (i) => {
  const b = terms[i].buffer.active;
  const out = [];
  for (let y = 0; y < terms[i].rows; y++) out.push(b.getLine(b.viewportY + y)?.translateToString(true) ?? '');
  return out;
};

await init();
window.ready = true;
