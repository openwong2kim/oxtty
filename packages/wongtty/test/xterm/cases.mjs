// Differential harness inputs. Holds only the data and the wmux replay order.
// Origin: wmux (written by the owner, MIT) 3bfef3aac41bcb5c4a156abec0027fb3a4978b29
//   bench/rust-terminal-core.ts `fixtures` → TEXT(25), bench/rust-snapshot-parity.ts `cases` → ANSI(29),
//   the resize seed formula (24), partial-utf8 (1) and margins refusal inputs from the same file, core/harness/corpus/* → ./corpus/* (copied as is).
//   The TEXT and ANSI array lines were pasted verbatim.
//
// Mapping of the 91 wmux cases (corpus 6 is the same input in both benches, so it runs once → 85 unique inputs = 25 + 29 + 6 + 24 + 1)
// | wmux bench                                        | count                                 | id in this file                                 |
// | rust-terminal-core.ts (text)                      | 25 + corpus 6 = 31                    | text/<name>, corpus/<name>                      |
// | rust-snapshot-parity.ts (ANSI)                    | 29 + corpus 6 + seed 24 + utf8 1 = 60 | ansi/<i>, corpus/<name>, seed/<n>, partial-utf8 |
// | same file, uncounted margins refusal              | 1                                     | margins                                         |
// | wongtty: known difference repros and regressions  | —                                     | known/<name>                                    |
// The wmux text bench had no continuation step. Here every case continues with the same tail rule (wmux ANSI bench).

import { readFileSync, existsSync } from 'node:fs';

/**
 * @typedef {import('./screen.mjs').Event} Event
 * @typedef {{
 *   id: string,
 *   cols: number, rows: number, scrollback: number,   // starting geometry. The final geometry comes from the resizes in input
 *   input: Event[],
 *   bytes: number,        // number of bytes input must carry (counted separately at the source). Inconclusive if it differs from the count actually written
 *   tail: Uint8Array,     // continuation. The first printed character reveals pending wrap; the tail reveals resumption of split sequences
 *   missing?: string,     // the reason, if the input file is missing — the result is inconclusive (failure)
 * }} Case
 */

/** For checks: case count per group. Ties in with 91 = 31 + 60. */
export const WMUX_COUNTS = { text: 25, ansi: 29, corpus: 6, seed: 24, 'partial-utf8': 1 };

const enc = new TextEncoder();
const TAIL = enc.encode('\x1b[?2026lZ한\r\nnext');
const CSI_TAIL = enc.encode('44mTAIL\x1b[0m');
const UTF8_TAIL = Uint8Array.of(0x98, 0x80, 0x21);
// A split CSI resumes its parameters. Same even when a C0 (LF) inside the sequence is at the end.
const tailFor = (text) => (/\x1b\[31;\n?$/.test(text) ? CSI_TAIL : TAIL);

// wmux bench/rust-snapshot-parity.ts `cases`: [text, cols], rows 6, scrollback 100
const ANSI = [
  ['\x1b[5mblink\x1b[25m plain', 20], ['\x1b[6mrapid\x1b[0m reset', 20],
  ['\x1b[5mA\x1b[25mB\x1b[5mC', 20], ['\x1b[5mnormal\x1b[?1049h\x1b[6malt', 20],
  ['\x1b[38;5;53mcolor', 20], ['\x1b[48;2;53;55;6mcolor', 20], ['\x1b[38:2::53:55:5mcolor', 20],
  ['', 20], ['hello\r\nworld', 20], ['abcdefgh', 8], ['abcdefghijklmno', 8],
  ['abcdefg한글😀Z', 8], ['abc🫠XYZ🩷tail', 8], ['a e\u0301 ⚠️ 👨‍👩‍👧', 12],
  ['\x1b[1;2;3;4;7;9;31;44mstyled\x1b[0m plain', 30],
  ['\x1b[38;5;123;48;5;42mpalette\x1b[0m', 20],
  ['\x1b[38;2;11;22;33;48;2;44;55;66mrgb', 20],
  ['one\r\ntwo\x1b[1;2HZ', 20], ['abc\r\x1b[44m\x1b[2K', 20],
  ['normal\r\nscroll\x1b[?1049h\x1b[Halt', 20],
  ['normal\x1b[?1049hOTHER\x1b[?1049l', 20],
  ['\x1b[?1h\x1b[?2004h\x1b[?1002h\x1b[?1006hready', 20],
  ['\x1b[4habc', 20], ['\x1b[?7labcdefghijkl', 8],
  ['\x1b[?2026hhello\x1b[?2026l', 20],
  ['\x1b[?2026hhello', 20],
  ['normal\x1b[?1049h\x1b[31malt\x1b[0m', 20],
  ['hello\x1b[31;', 20],
  [Array.from({ length: 50 }, (_, i) => `\x1b[3${i % 8}mline${i}\x1b[0m`).join('\r\n'), 20],
];

// wmux bench/rust-terminal-core.ts `fixtures`: default 40x6, scrollback 100. chunks are written after the final resize
const TEXT = [
  { name: 'empty', text: '' },
  { name: 'colored-log', text: '\x1b[1;31mERROR\x1b[0m boom\r\nsecond line\r\n' },
  { name: 'soft-wrap', text: 'abcdefghijklmnopqrstuvwxy', cols: 8 },
  { name: 'exact-right-margin', text: 'abcdefgh', cols: 8 },
  { name: 'scrollback-cap', text: Array.from({ length: 40 }, (_, i) => `line ${i}`).join('\r\n'), rows: 4, scrollback: 5 },
  { name: 'cjk-wrap', text: 'abc한글세계xyz한글', cols: 8 },
  { name: 'wide-right-edge', text: 'abcdefg한z', cols: 8 },
  { name: 'combining', text: 'abc e\u0301 한\u0301 끝', cols: 8 },
  { name: 'emoji-zwj-vs16', text: '⚠️ ❤️ ✔️ 👨‍👩‍👧 👍🏽 1️⃣ end', cols: 12 },
  { name: 'unicode-new-emoji', text: 'abc🫠XYZ🩷tail', cols: 8 },
  { name: 'dim-wide', text: 'a\x1b[2m한 dim\x1b[22m shown' },
  { name: 'cursor-overwrite', text: 'first\r\nsecond\x1b[1;2HZ\x1b[2;3H!' },
  { name: 'erase-insert-delete', text: 'abcdefgh\r\x1b[3C\x1b[2P\x1b[2@XY\x1b[K' },
  { name: 'scroll-margins', text: 'one\r\ntwo\r\nthree\r\nfour\x1b[2;4r\x1b[4;1Hnew\r\nlast', rows: 5 },
  { name: 'alt-active', text: 'normal\x1b[?1049h\x1b[Halternate\r\nprompt', rows: 4 },
  { name: 'alt-return', text: 'normal\x1b[?1049hother\x1b[?1049l' },
  { name: 'sync-complete', text: '\x1b[?2026hfirst\r\nsecond\x1b[?2026l' },
  { name: 'sync-open', text: '\x1b[?2026hfirst\r\nsecond' },
  { name: 'split-utf8-escape', text: '', chunks: Array.from(Buffer.from('한글\x1b[31m😀\x1b[0m!'), byte => Buffer.from([byte])) },
  { name: 'incomplete-utf8', text: 'ready', chunks: [Buffer.from([0xf0, 0x9f])] },
  { name: 'incomplete-csi', text: 'ready\x1b[31;' },
  { name: 'queries-no-writeback', text: 'before\x1b[c\x1b[6n\x1b]52;c;?\x07after' },
  { name: 'resize-reflow', text: 'abcdefghijklmnop\r\nnext', cols: 12, rows: 4, geometry: { start: { cols: 8, rows: 4 }, changes: [] } },
  { name: 'resize-mid-stream', text: 'abcdefghijklmnopqrstuv', cols: 12, rows: 4, geometry: { start: { cols: 8, rows: 4 }, changes: [{ offset: 12, cols: 12, rows: 4 }] } },
  { name: 'resize-narrow', text: 'abc한글0123456789\r\nlast', cols: 8, rows: 4, geometry: { start: { cols: 16, rows: 4 }, changes: [] } },
];

// Minimal inputs (20x6) reproducing one known difference each. Paired with known-differences.json entries; once the engine is fixed they fail as stale.
// Also holds regression inputs pinning that there is no difference (osc8-underline, tab-pending-wrap, decscusr, deccolm,
// the B regressions from serializer fixes: tab-cell, origin-mode, c0-inside-csi, edits that cut a wide char: *-wide-*, and *-pending-wrap).
const KNOWN = {
  // Erasing wrapped lines: xterm keeps isWrapped as a line attribute and changes it by fixed rules on each erase.
  'el0-col0-wrapped': `${'X'.repeat(25)}\r\x1b[K`,
  'el0-wrapping-row': `${'X'.repeat(25)}\x1b[1;5H\x1b[K`,
  'el0-pending-wrap': `${'X'.repeat(20)}\x1b[K`,
  'el1-last-col': `${'X'.repeat(25)}\x1b[1;20H\x1b[1K`,
  'el2-wrapped': `${'X'.repeat(25)}\x1b[2K`,
  'el2-wrapping-row': `${'X'.repeat(25)}\x1b[1;1H\x1b[2K`,
  'ed0-col0-wrapped': `${'X'.repeat(25)}\r\x1b[J`,
  'ed1-top-from-history': `a\r\nb\r\nc\r\nd\r\ne\r\n${'X'.repeat(30)}\r\n1\r\n2\r\n3\r\n4\r\n5\x1b[3;1H\x1b[1J`,
  'ed2-top-from-history': `a\r\nb\r\nc\r\nd\r\ne\r\n${'X'.repeat(30)}\r\n1\r\n2\r\n3\r\n4\r\n5\x1b[2J`,
  'ech-last-col': `${'X'.repeat(25)}\x1b[1;18H\x1b[5X`,
  'dch-wrapping-row': `${'X'.repeat(25)}\x1b[1;5H\x1b[3P`,
  'ich-wrapping-row': `${'X'.repeat(25)}\x1b[1;5H\x1b[3@`,
  'il-above-continuation': `${'X'.repeat(25)}\x1b[2;1H\x1b[L`,
  'dl-parent-row': `a\r\n${'X'.repeat(25)}\x1b[2;1H\x1b[M`,
  'dl-top-history': `${'X'.repeat(25)}\x1b[1;1H\x1b[M`,
  'sgr-21': '\x1b[21mX',
  'sgr-53': '\x1b[53mO',
  'sgr-59': '\x1b[4;58;5;3mX\x1b[59mY',
  'ed2-history': 'a\r\nb\r\nc\x1b[2J',
  tab: 'a\tb',
  'mode-47': 'main\x1b[?47halt',
  'mode-1047': 'main\x1b[?1047halt',
  'mode-1048': '\x1b[3;3H\x1b[?1048h\x1b[H\x1b[?1048lX',
  'mode-9': '\x1b[?9h',
  'mode-45': '\x1b[?45h',
  'ris-cursor': '\x1b[?25l\x1bc',
  decstr: '\x1b[?25l\x1b[4h\x1b[!p',
  'combining-col0': '́a',
  'zwj-alone': '‍',
  lnm: '\x1b[20ha\nb',
  'incomplete-utf8-then-esc': Uint8Array.of(...enc.encode('ready'), 0xf0, 0x9f, ...enc.encode('\x1b[0mX')),
  'osc8-underline': '\x1b]8;;http://x\x1b\\L\x1b]8;;\x1b\\',
  'tab-pending-wrap': 'abcdefghijklmnopqrst\tZ',
  decscusr: '\x1b[5 q',
  deccolm: '\x1b[?3hX',
  'tab-cell': 'a\tb\tc',
  'origin-mode': '\x1b[?6h\x1b[3;5HX',
  'c0-inside-csi': 'ab\x1b[31;\n',
  'ech-wide-half': 'a가b\x1b[1;3H\x1b[X',
  'el1-wide-half': 'a가b\x1b[1;2H\x1b[1K',
  'dch-wide-half': 'a가b\x1b[1;2H\x1b[P',
  'ich-wide-to-edge': `${'x'.repeat(18)}가\x1b[1;1H\x1b[@`,
  'ich-wide-right-half': 'a가b\x1b[1;3H\x1b[@',
  'irm-wide-half': 'a가b\x1b[1;3H\x1b[4hZ',
  'invalid-utf8': Uint8Array.of(0x61, 0xac, 0x62),
  'decrc-pending-wrap': `${'X'.repeat(20)}\x1b7\x1b8`,
  'alt-return-pending-wrap': `${'X'.repeat(20)}\x1b[?1049h\x1b[?1049l`,
  'ech-pending-wrap': `${'X'.repeat(20)}\x1b[X`,
  'dch-pending-wrap': `${'X'.repeat(20)}\x1b[P`,
  'ich-pending-wrap': `${'X'.repeat(20)}\x1b[@`,
  'ed0-pending-wrap': `${'X'.repeat(20)}\x1b[J`,
  'ed0-last-col': `${'X'.repeat(20)}\x1b[1;20H\x1b[J`,
  'ed1-line-1': 'a\r\nb\x1b[1J',
  'ed1-line-2': 'a\r\nb\r\nc\x1b[1J',
  'esc-non-ascii': '\x1b가[',
};

/**
 * wmux feedAtRecordedSizes order: cut initial at the changes positions and insert resizes, match the final geometry, then append chunks.
 * @returns {Case}
 */
function replay(id, { initial, start, end, changes = [], chunks = [], scrollback, tail }) {
  const input = [];
  let from = 0;
  for (const c of changes) {
    input.push(initial.subarray(from, c.offset), { cols: c.cols, rows: c.rows });
    from = c.offset;
  }
  input.push(initial.subarray(from), end, ...chunks);
  const bytes = initial.length + chunks.reduce((n, c) => n + c.length, 0);
  return { id, cols: start.cols, rows: start.rows, scrollback, input, bytes, tail };
}

/** ./corpus/<name>/{recording.bin, events.jsonl}. If the files are missing, missing (inconclusive). @returns {Case} */
export function corpus(name, root = new URL('./corpus/', import.meta.url)) {
  const id = `corpus/${name}`;
  const dir = new URL(`${name}/`, root);
  for (const file of ['recording.bin', 'events.jsonl']) {
    if (!existsSync(new URL(file, dir))) return { id, missing: `${id}/${file} is missing` };
  }
  const events = readFileSync(new URL('events.jsonl', dir), 'utf8').trim().split('\n').map((l) => JSON.parse(l));
  const resizes = events.filter((e) => e.type === 'resize');
  const start = events[0].geometry;
  return replay(id, {
    initial: readFileSync(new URL('recording.bin', dir)), start, end: resizes.at(-1)?.geometry ?? start,
    changes: resizes.map((e) => ({ offset: e.byteOffset, ...e.geometry })),
    scrollback: 5000, tail: TAIL,
  });
}

function seed(n) {
  const text = Array.from({ length: n }, (_, i) => `\x1b[3${i % 8}m${'abc한'.repeat(1 + i % 5)}\x1b[0m`).join('\r\n');
  const initial = Buffer.from(text);
  const end = { cols: 8 + n % 13, rows: 6 };
  return replay(`seed/${n}`, {
    initial, start: { cols: 12, rows: 6 }, end, changes: [{ offset: initial.length, ...end }], scrollback: 100, tail: TAIL,
  });
}

const inline = (id, text, cols, rows = 6) => {
  const initial = typeof text === 'string' ? Buffer.from(text) : text;
  const g = { cols, rows };
  return replay(id, { initial, start: g, end: g, scrollback: 100, tail: typeof text === 'string' ? tailFor(text) : TAIL });
};

/** @type {Case[]} */
export const CASES = [
  ...TEXT.map((f) => {
    const end = { cols: f.cols ?? 40, rows: f.rows ?? 6 };
    return replay(`text/${f.name}`, {
      initial: Buffer.from(f.text), start: f.geometry?.start ?? end, end, changes: f.geometry?.changes,
      chunks: f.chunks, scrollback: f.scrollback ?? 100, tail: tailFor(f.text),
    });
  }),
  ...ANSI.map(([text, cols], i) => inline(`ansi/${i}`, text, cols)),
  ...['alt-screen', 'cjk-emoji', 'resize-reflow', 'resize-roundtrip', 'scroll-flood', 'sgr-spectrum'].map((n) => corpus(n)),
  ...Array.from({ length: 24 }, (_, i) => seed(i + 1)),
  replay('partial-utf8', {
    initial: Buffer.concat([Buffer.from('ready'), Buffer.from([0xf0, 0x9f])]), start: { cols: 20, rows: 6 },
    end: { cols: 20, rows: 6 }, scrollback: 5000, tail: UTF8_TAIL,
  }),
  inline('margins', '\x1b[2;4rhello', 20),
  ...Object.entries(KNOWN).map(([name, text]) => inline(`known/${name}`, text, 20)),
  // Widening with a wrap pending: xterm leaves the cursor past the old last column, which is now a real column.
  ...[['alt', '\x1b[?1049h'], ['primary', '']].map(([screen, enter]) => replay(`known/${screen}-widen-pending-wrap`, {
    initial: Buffer.from(`${enter}abcde`), start: { cols: 5, rows: 3 }, end: { cols: 7, rows: 3 }, scrollback: 100, tail: TAIL,
  })),
];
