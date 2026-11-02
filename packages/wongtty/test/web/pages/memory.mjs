// Page for memory.bench.mjs: `setup(kind, n, cols, rows)` opens n terminals of wongtty/xterm or of real xterm.js
// with @xterm/addon-webgl, writes the same output to each and waits until every one has drawn it.
import { Terminal as XtermTerminal } from '@xterm/xterm';
import { WebglAddon } from '@xterm/addon-webgl';
import initFrontEnd from '/dist/xterm_web.js';
import { init, Terminal } from 'wongtty/xterm';

/** 200 lines of coloured ASCII, Hangul and box drawing (deterministic). */
function output(cols) {
  const words = ['build', 'PASS', '한글', '터미널', 'error:', '│', '─┼─', 'src/render/webgl.rs', '0x1f600', 'ok'];
  let s = '';
  let seed = 7;
  for (let line = 0; line < 200; line++) {
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

const frames = (n) => new Promise((resolve) => {
  const step = () => (n-- > 0 ? requestAnimationFrame(step) : resolve());
  step();
});

async function setup(kind, n, cols, rows) {
  const text = output(cols);
  const terms = [];
  for (let i = 0; i < n; i++) {
    const host = document.createElement('div');
    host.style.cssText = 'display: inline-block; vertical-align: top';
    document.body.appendChild(host);
    const options = { cols, rows, fontFamily: 'Menlo, monospace', fontSize: 12 };
    let term;
    if (kind === 'xterm') {
      term = new XtermTerminal(options);
      term.open(host);
      term.loadAddon(new WebglAddon());
    } else {
      term = new Terminal(options);
      term.open(host);
    }
    await new Promise((resolve) => term.write(text, resolve));
    terms.push(term);
  }
  await frames(10);
  window.terms = terms;
  const frontEnd = kind === 'wongtty' ? (await initFrontEnd()).memory.buffer.byteLength : 0;
  return { canvases: document.querySelectorAll('canvas').length, frontEndWasm: frontEnd };
}

await init();
window.setup = setup;
window.ready = true;
