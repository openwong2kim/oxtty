// The output the benchmarks feed: vtebench-style scenarios generated deterministically from a fixed seed, shared by
// throughput.mjs (Node) and speed/page.mjs (the browser).

export function rng(seed) {
  let s = seed >>> 0;
  return () => ((s = (s * 1664525 + 1013904223) >>> 0) / 2 ** 32);
}

const ESC = '\x1b';

/** One piece of output per call, for a terminal of `cols` × `rows`. */
export function cases(cols, rows) {
  return {
    // Lines like a build log: mostly plain text, occasionally colored.
    'light-cells': (r) => `${r() < 0.2 ? `${ESC}[3${1 + Math.floor(r() * 6)}m` : ''}[${(r() * 1e6).toFixed(0)}] compiling module_${Math.floor(r() * 9999)}.rs ok${ESC}[0m\r\n`,
    // A different truecolor foreground and background per cell.
    'dense-cells': (r) => {
      let line = '';
      for (let i = 0; i < cols; i++) {
        const c = () => Math.floor(r() * 256);
        line += `${ESC}[38;2;${c()};${c()};${c()};48;2;${c()};${c()};${c()}m${String.fromCharCode(33 + Math.floor(r() * 90))}`;
      }
      return `${line}${ESC}[0m\r\n`;
    },
    // Lines of Hangul, Han characters and emoji.
    unicode: (r) => {
      const pool = ['가', '나', '한', '글', '漢', '字', '😀', '🚀', 'é', 'a', ' '];
      let line = '';
      for (let i = 0; i < 50; i++) line += pool[Math.floor(r() * pool.length)];
      return `${line}\r\n`;
    },
    // TUI redraw: move to random positions and write short text (no scrolling).
    'cursor-motion': (r) => `${ESC}[${1 + Math.floor(r() * rows)};${1 + Math.floor(r() * (cols - 10))}H${ESC}[3${Math.floor(r() * 8)}m${'#'.repeat(1 + Math.floor(r() * 8))}`,
    // Scroll only the lower region (DECSTBM).
    'scrolling-region': (r) => `${ESC}[20;40r${ESC}[40;1Hline ${Math.floor(r() * 1e6)}\n`,
    // Keep streaming short plain lines to fill the scrollback.
    'scroll-flood': (r) => `${Math.floor(r() * 1e9)}\r\n`,
  };
}

/** At least `size` bytes of scenario `name` as UTF-8, from the fixed seed. */
export function generate(name, size, cols, rows) {
  const r = rng(0x5eed);
  const make = cases(cols, rows)[name];
  const encoder = new TextEncoder();
  const parts = [];
  let bytes = 0;
  while (bytes < size) {
    const s = make(r);
    parts.push(s);
    bytes += encoder.encode(s).length;
  }
  return encoder.encode(parts.join(''));
}
