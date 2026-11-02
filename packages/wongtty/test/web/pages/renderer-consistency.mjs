// Page for renderer-consistency.test.mjs (ported from the TypeScript front end at 0510a7d): a terminal is driven by
// seeded random output, resizes, view scrolls, selection, focus and cursor changes, drawing only what changed after
// every step. At each checkpoint the picture is compared with a full rebuild (a new atlas, model and glyph buffers).
import { init } from 'wongtty/xterm';
import { Pane, diff } from './kit.mjs';

// mulberry32
function random(seed) {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

// Distinct focused and unfocused selection colours, so focus changes must redraw the selection
const OPTIONS = { scrollback: 200, theme: { selectionBackground: '#806030', selectionInactiveBackground: '#305080' } };
const WORDS = ['build', 'PASS', '한글', '터미널', '😀', '👍🏽', 'é', 'ñ', '│', '█▓░', '⠿', 'abc', 'x', '  ', '\t'];
const SGR = ['0', '1', '2', '3', '4', '4:3', '7', '9', '31', '42', '38;5;208', '48;5;24', '38;2;10;200;90', '48;2;60;0;90', '58;2;255;0;0', '1;34', '22;39;49'];

async function step(p, rnd, state) {
  const pick = (a) => a[Math.floor(rnd() * a.length)];
  const { cols, rows } = p.term;
  const r = rnd();
  if (r < 0.55) {
    let s = '';
    for (let n = 1 + Math.floor(rnd() * 12); n > 0; n--) s += (rnd() < 0.3 ? `\x1b[${pick(SGR)}m` : '') + pick(WORDS) + (rnd() < 0.2 ? '\r\n' : ' ');
    await p.write(s);
  } else if (r < 0.65) {
    await p.write(`\x1b[${1 + Math.floor(rnd() * rows)};${1 + Math.floor(rnd() * cols)}H`);
  } else if (r < 0.7) {
    await p.write(`\x1b[${pick([2, 4, 6, 0])} q`);
  } else if (r < 0.74) {
    await p.write(pick(['\x1b[?25l', '\x1b[?25h', '\x1b[2J', '\x1b[K', '\x1b[3S', '\x1b[2T', '\x1b[?1049h', '\x1b[?1049l']));
  } else if (r < 0.8) {
    await p.write('\r\n'.repeat(1 + Math.floor(rnd() * rows)));
  } else if (r < 0.85) {
    p.term.scrollLines(3 - Math.floor(rnd() * 9));
  } else if (r < 0.9) {
    p.term.resize(10 + Math.floor(rnd() * 50), 3 + Math.floor(rnd() * 15));
  } else if (r < 0.96) {
    const a = [Math.floor(rnd() * cols), Math.floor(rnd() * rows)];
    const b = [Math.floor(rnd() * cols), Math.floor(rnd() * rows)];
    const [start, end] = a[1] < b[1] || (a[1] === b[1] && a[0] <= b[0]) ? [a, b] : [b, a];
    p.term._setRendererSelection(rnd() < 0.2 ? undefined : { start, end, columnSelect: rnd() < 0.3 });
  } else {
    state.focused = !state.focused;
    p.focus(state.focused);
  }
}

/** The picture after a full rebuild: two option changes each acquire a new atlas and clear the model. */
async function rebuilt(p) {
  p.term.options.drawBoldTextInBrightColors = false;
  p.term.options.drawBoldTextInBrightColors = true;
  return p.pixels();
}

async function run(seed, steps) {
  const rnd = random(seed);
  const p = new Pane(40, 12, OPTIONS);
  const state = { focused: false };
  const diffs = [];
  for (let n = 1; n <= steps; n++) {
    await step(p, rnd, state);
    const incremental = await p.pixels();
    if (n % 20 === 0 || n === steps) diffs.push(diff(incremental, await rebuilt(p)));
  }
  // Sensitivity: one changed cell must show up
  p.term.scrollToBottom();
  const before = await p.pixels();
  await p.write('\x1b7\x1b[1;1H\x1b[0;7mX\x1b8');
  const sentinel = diff(before, await p.pixels());
  p.dispose();
  return { checkpoints: diffs.length, diffs, sentinel };
}

await init();
window.run = run;
window.ready = true;
