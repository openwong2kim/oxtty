// Screen record: the definition of "the same screen". The xterm side and the wongtty side each build this shape, compared by a single diffScreens.
//
// Normalization rules (they live only here)
// - Line keys are bottom (screen) based: screen "0".."rows-1", history "-1" (newest)…. Screen lines are compared with each other even when history lengths differ.
// - wrapped has the xterm meaning ("the previous line continues into this line"). The oldest line has no previous line, so both sides use false.
// - Characters: blank ' ', trailing cell of a wide character ''. Combining characters are compared as whole strings.
// - Cursor x is clamped to cols-1 (xterm reports cols in pending wrap). Pending wrap is revealed by the first printed character of the case tail.
//   Cursor shape and blink are left out: they are not in the xterm public API (for wongtty, the engine test cursor_shape_follows_decscusr pins them).
// - Colors: 'default' or (mode | value). xterm CM_P16/P256/RGB bits == wongtty NAMED/INDEXED/RGB bits.
// - Underline uses getUnderlineStyle() (isUnderline() is true on OSC 8 links). Underline color only when underlined, 'default' without SGR 58.
// - Fields wongtty does not model are constants (WONGTTY_CONSTANT). If such a field turns on in xterm, it shows up as a difference.

import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import xtermHeadless from '@xterm/headless';
import unicode11 from '@xterm/addon-unicode11';
import { Attr, Mode } from '../../dist/index.js';

const { Terminal: XTerminal } = xtermHeadless;
const { Unicode11Addon } = unicode11;
const encoder = new TextEncoder();

/**
 * @typedef {'default' | number} Color
 * @typedef {{ chars: string, width: number, fg: Color, bg: Color, bold: boolean, dim: boolean, italic: boolean,
 *   underline: number, underlineColor: Color | null, blink: boolean, inverse: boolean,
 *   invisible: boolean, strikethrough: boolean, overline: boolean }} Cell
 * @typedef {{ wrapped: boolean, cells: Cell[] }} Line
 * @typedef {{
 *   cols: number, rows: number, buffer: string, baseY: number,
 *   cursor: { x: number, y: number, visible: boolean },
 *   modes: { applicationCursorKeys: boolean, applicationKeypad: boolean, bracketedPaste: boolean, insert: boolean,
 *     mouseTracking: string, origin: boolean, sendFocus: boolean, wraparound: boolean,
 *     reverseWraparound: boolean, synchronizedOutput: boolean },
 *   lines: Record<string, Line>,
 * }} Screen
 *   win32InputMode is left out: in this xterm configuration ?9001h does not turn it on, so no lever can be built.
 * @typedef {{ path: string, field: string, xterm: unknown, subject: unknown }} Diff
 *   path e.g. "lines[-3].cells[4].fg"; field is path without the [..] parts, "lines.cells.fg"
 * @typedef {Uint8Array | { cols: number, rows: number }} Event   write or resize
 */

/** Same settings as wmux snapshotTestTerminal. reflowCursorLine stays at its default (false). */
export function xterm(cols, rows, scrollback) {
  const t = new XTerminal({
    cols, rows, scrollback, allowProposedApi: true, logLevel: 'off',
    vtExtensions: { colorSchemeQuery: false, kittySgrBoldFaintControl: false },
  });
  t.loadAddon(new Unicode11Addon());
  t.unicode.activeVersion = '11';
  return t;
}

/** The reason if the installed reference xterm differs from the version pinned in package.json, null if it matches. */
export function oracleMismatch(
  pinned = JSON.parse(readFileSync(new URL('../../package.json', import.meta.url), 'utf8')).devDependencies,
) {
  const require = createRequire(import.meta.url);
  for (const name of ['@xterm/headless', '@xterm/addon-unicode11']) {
    const installed = require(`${name}/package.json`).version;
    if (installed !== pinned[name]) return `${name} ${installed} installed, package.json pins ${pinned[name]}`;
  }
  return null;
}

/**
 * Applies the events in order to every terminal and returns the number of bytes written.
 * Always writes bytes to xterm: writing successive strings loses the incomplete UTF-8 state.
 * @param {(Event | string)[]} events
 */
export async function play(events, ...terms) {
  let written = 0;
  for (const e of events) {
    if (typeof e === 'string' || e instanceof Uint8Array) {
      const bytes = typeof e === 'string' ? encoder.encode(e) : e;
      for (const t of terms) {
        if (t instanceof XTerminal) await new Promise((resolve) => t.write(bytes, resolve));
        else t.write(bytes);
      }
      written += bytes.length;
    } else {
      for (const t of terms) t.resize(e.cols, e.rows);
    }
  }
  return written;
}

const xcolor = (mode, value) => (mode === 0 ? 'default' : (mode | value) >>> 0);
const DEFAULT_FG = (1 << 24) | 256;
const DEFAULT_BG = (1 << 24) | 257;
const ocolor = (packed, def) => (packed === def ? 'default' : packed >>> 0);

/** Assigns bottom-based line keys. oldest is the key of the oldest line. */
function keyed(count, rows, line) {
  const lines = {};
  for (let i = 0; i < count; i++) lines[i - (count - rows)] = line(i - (count - rows), i === 0);
  return lines;
}

/** @returns {Screen} */
export function xtermScreen(t) {
  const b = t.buffer.active;
  const m = t.modes;
  return {
    cols: t.cols, rows: t.rows, buffer: b.type, baseY: b.baseY,
    cursor: { x: Math.min(b.cursorX, t.cols - 1), y: b.cursorY, visible: m.showCursor },
    modes: {
      applicationCursorKeys: m.applicationCursorKeysMode, applicationKeypad: m.applicationKeypadMode,
      bracketedPaste: m.bracketedPasteMode, insert: m.insertMode, mouseTracking: m.mouseTrackingMode,
      origin: m.originMode, sendFocus: m.sendFocusMode, wraparound: m.wraparoundMode,
      reverseWraparound: m.reverseWraparoundMode, synchronizedOutput: m.synchronizedOutputMode,
    },
    lines: keyed(b.length, t.rows, (y, oldest) => {
      const l = b.getLine(b.baseY + y);
      const cells = [];
      for (let x = 0; x < t.cols; x++) {
        const c = l.getCell(x);
        const width = c.getWidth();
        const underline = c.getUnderlineStyle();
        cells.push({
          chars: width === 0 ? '' : c.getChars() || ' ', width,
          fg: xcolor(c.getFgColorMode(), c.getFgColor()), bg: xcolor(c.getBgColorMode(), c.getBgColor()),
          bold: !!c.isBold(), dim: !!c.isDim(), italic: !!c.isItalic(), underline,
          underlineColor: underline ? xcolor(c.getUnderlineColorMode(), c.getUnderlineColor()) : null,
          blink: !!c.isBlink(), inverse: !!c.isInverse(), invisible: !!c.isInvisible(),
          strikethrough: !!c.isStrikethrough(), overline: !!c.isOverline(),
        });
      }
      return { wrapped: !oldest && l.isWrapped, cells };
    }),
  };
}

const UNDERLINES = [
  [Attr.UNDERLINE, 1], [Attr.DOUBLE_UNDERLINE, 2], [Attr.CURLY_UNDERLINE, 3],
  [Attr.DOTTED_UNDERLINE, 4], [Attr.DASHED_UNDERLINE, 5],
];
const mouse = (m) => (m & Mode.MOUSE_MOTION ? 'any' : m & Mode.MOUSE_DRAG ? 'drag' : m & Mode.MOUSE_CLICK ? 'vt200' : 'none');

/** @returns {Screen} Uses only the wongtty public API. */
export function wongttyScreen(t) {
  const synchronizedOutput = t.syncPending();
  const history = t.historyLength;
  const c = t.cursor();
  const m = t.modes();
  return {
    cols: t.cols, rows: t.rows, buffer: m & Mode.ALT_SCREEN ? 'alternate' : 'normal', baseY: history,
    cursor: { x: c.col, y: c.line, visible: c.visible },
    modes: {
      applicationCursorKeys: !!(m & Mode.APP_CURSOR), applicationKeypad: !!(m & Mode.APP_KEYPAD),
      bracketedPaste: !!(m & Mode.BRACKETED_PASTE), insert: !!(m & Mode.INSERT), mouseTracking: mouse(m),
      origin: !!(m & Mode.ORIGIN), sendFocus: !!(m & Mode.FOCUS_EVENTS), wraparound: !!(m & Mode.LINE_WRAP),
      reverseWraparound: false, synchronizedOutput,
    },
    lines: keyed(history + t.rows, t.rows, (y, oldest) => {
      const words = t.rowCells(y);
      const extras = new Map(t.rowExtras(y).map((e) => [e.col, e]));
      const cells = [];
      for (let x = 0; x < t.cols; x++) {
        const [ch, fg, bg, a] = words.subarray(x * 4, x * 4 + 4);
        const e = extras.get(x);
        const width = a & Attr.WIDE_SPACER ? 0 : a & Attr.WIDE ? 2 : 1;
        const underline = UNDERLINES.find(([bit]) => a & bit)?.[1] ?? 0;
        cells.push({
          chars: width === 0 ? '' : e?.text ?? String.fromCodePoint(ch), width,
          fg: ocolor(fg, DEFAULT_FG), bg: ocolor(bg, DEFAULT_BG),
          bold: !!(a & Attr.BOLD), dim: !!(a & Attr.DIM), italic: !!(a & Attr.ITALIC), underline,
          underlineColor: underline ? e?.underlineColor ?? 'default' : null,
          blink: !!(a & Attr.BLINK), inverse: !!(a & Attr.INVERSE), invisible: !!(a & Attr.HIDDEN),
          strikethrough: !!(a & Attr.STRIKEOUT), overline: false,
        });
      }
      return { wrapped: !oldest && t.rowWrapped(y - 1), cells };
    }),
  };
}

/** Fields wongtty reports as constants. The lever test enforces that "only these are constant". */
export const WONGTTY_CONSTANT = new Set([
  'lines.cells.overline', // SGR 53: no bit in the wongtty cell
  'modes.reverseWraparound', // ?45 unsupported
]);

/** Every compared field → an input that changes that field on an empty 20x6 screen. When fields are added, levers must be added too for the test to pass. */
export const LEVERS = {
  cols: [{ cols: 21, rows: 6 }], rows: [{ cols: 20, rows: 7 }],
  buffer: ['\x1b[?1049h'], baseY: ['\r\n'.repeat(6)],
  lines: ['\r\n'.repeat(6)], 'lines.cells': [{ cols: 21, rows: 6 }],
  'cursor.x': ['X'], 'cursor.y': ['\n'], 'cursor.visible': ['\x1b[?25l'],
  'lines.wrapped': ['X'.repeat(21)],
  'lines.cells.chars': ['X'], 'lines.cells.width': ['한'],
  'lines.cells.fg': ['\x1b[31mX'], 'lines.cells.bg': ['\x1b[41mX'],
  'lines.cells.bold': ['\x1b[1mX'], 'lines.cells.dim': ['\x1b[2mX'], 'lines.cells.italic': ['\x1b[3mX'],
  // The underline lever also sets fg: this pins that the underline color without SGR 58 is 'default' (not substituted with fg).
  'lines.cells.underline': ['\x1b[31;4:3mX'], 'lines.cells.underlineColor': ['\x1b[4;58;5;1mX'],
  'lines.cells.blink': ['\x1b[5mX'], 'lines.cells.inverse': ['\x1b[7mX'], 'lines.cells.invisible': ['\x1b[8mX'],
  'lines.cells.strikethrough': ['\x1b[9mX'], 'lines.cells.overline': ['\x1b[53mX'],
  'modes.applicationCursorKeys': ['\x1b[?1h'], 'modes.applicationKeypad': ['\x1b='],
  'modes.bracketedPaste': ['\x1b[?2004h'], 'modes.insert': ['\x1b[4h'], 'modes.mouseTracking': ['\x1b[?1000h'],
  'modes.origin': ['\x1b[?6h'], 'modes.sendFocus': ['\x1b[?1004h'], 'modes.wraparound': ['\x1b[?7l'],
  'modes.reverseWraparound': ['\x1b[?45h'], 'modes.synchronizedOutput': ['\x1b[?2026h'],
};

// Numeric keys (line numbers, cell indices) are data-defined, so they are written as [k] in path and dropped from field.
const isIndex = (k) => /^-?\d+$/.test(k);
const step = (path, k) => (isIndex(k) ? `${path}[${k}]` : path ? `${path}.${k}` : k);
const fieldOf = (path) => path.replace(/\[-?\d+\]/g, '');

/** Deep comparison. A key present on only one side is reported at that key with the missing side as undefined. @returns {Diff[]} */
export function diffScreens(xtermSide, subjectSide, path = '', out = []) {
  if (typeof xtermSide === 'object' && xtermSide && typeof subjectSide === 'object' && subjectSide) {
    for (const k of new Set([...Object.keys(xtermSide), ...Object.keys(subjectSide)])) {
      const p = step(path, k);
      if (!(k in xtermSide) || !(k in subjectSide)) {
        out.push({ path: p, field: fieldOf(p), xterm: xtermSide[k], subject: subjectSide[k] });
      } else {
        diffScreens(xtermSide[k], subjectSide[k], p, out);
      }
    }
  } else if (xtermSide !== subjectSide) {
    out.push({ path, field: fieldOf(path), xterm: xtermSide, subject: subjectSide });
  }
  return out;
}

/** Every field diffScreens can produce: value fields, and containers whose keys are data (lines, lines.cells). */
export function fieldsOf(screen, path = '', out = new Set()) {
  if (typeof screen !== 'object' || screen === null) return out.add(path);
  const keys = Object.keys(screen);
  if (keys.length && keys.every(isIndex)) {
    out.add(path);
    for (const k of keys) fieldsOf(screen[k], path, out);
  } else {
    for (const k of keys) fieldsOf(screen[k], path ? `${path}.${k}` : k, out);
  }
  return out;
}
