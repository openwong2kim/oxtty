// wongtty ⇄ xterm.js screen equivalence. Runs under `npm ci && npm test` (packages/wongtty).
//   Proof A: same events → wongtty record == xterm record (exactly the data the wmux renderer will read after the engine swap)
//   Proof B: same events → wongtty ansiSnapshot() → screen written to a new xterm == the original xterm (the screen wmux sees when restoring a pane)
// Compares at each step: initial → continued (case.tail) → returned (?1049lX if xterm is on the alternate screen).
// Any result other than equal must be in known-differences.json with a reason, and that entry must actually occur.
// Shows that the comparator can fail before running the cases: every compared field must have an input (lever) that changes it.
// With WONGTTY_SWAP_GATE=1, fails if any entry blocks the engine swap (blocksSwap) (PRD G3).

import { test, before } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { init, Terminal, Attr } from '../dist/index.js';
import { xterm, play, xtermScreen, wongttyScreen, diffScreens, fieldsOf, oracleMismatch, LEVERS, WONGTTY_CONSTANT } from './xterm/screen.mjs';
import { CASES, WMUX_COUNTS, corpus } from './xterm/cases.mjs';

const KNOWN = JSON.parse(readFileSync(new URL('./xterm/known-differences.json', import.meta.url), 'utf8'));
const ORACLE = oracleMismatch();

before(async () => { await init(); });

/** The two records from applying events to an empty 20x6 screen. */
async function screens(events) {
  const x = xterm(20, 6, 100);
  const o = new Terminal({ cols: 20, rows: 6, scrollback: 100 });
  try {
    await play(events, x, o);
    return { x: xtermScreen(x), o: wongttyScreen(o) };
  } finally {
    o.dispose();
    x.dispose();
  }
}

const fields = (diffs) => new Set(diffs.map((d) => d.field));

test('reference xterm is the version package.json pins', () => {
  assert.equal(oracleMismatch(), null);
  assert.match(oracleMismatch({ '@xterm/headless': '0.0.0' }), /0\.0\.0/);
});

test('every compared field has a lever', async () => {
  const blank = await screens([]);
  const levers = Object.keys(LEVERS).sort();
  assert.deepEqual([...fieldsOf(blank.x)].sort(), levers);
  assert.deepEqual([...fieldsOf(blank.o)].sort(), levers);
});

test('each lever moves its field on xterm and on wongtty (except WONGTTY_CONSTANT)', async () => {
  const blank = await screens([]);
  for (const [field, events] of Object.entries(LEVERS)) {
    const moved = await screens(events);
    assert.ok(fields(diffScreens(blank.x, moved.x)).has(field), `xterm: lever for ${field} did not move it`);
    const wongttyMoved = fields(diffScreens(blank.o, moved.o)).has(field);
    if (WONGTTY_CONSTANT.has(field)) assert.ok(!wongttyMoved, `wongtty: ${field} is listed constant but moved`);
    else assert.ok(wongttyMoved, `wongtty: lever for ${field} did not move it`);
  }
});

test('on every lever input xterm and wongtty agree except WONGTTY_CONSTANT', async () => {
  for (const [field, events] of Object.entries(LEVERS)) {
    const { x, o } = await screens(events);
    const left = diffScreens(x, o).filter((d) => !WONGTTY_CONSTANT.has(d.field));
    assert.deepEqual(left, [], `lever for ${field}`);
  }
});

test('B lever: one byte added to or dropped from the payload is a difference', async () => {
  const c = CASES.find((k) => k.id === 'ansi/8');
  assert.deepEqual(await proveB(c), { kind: 'compared', diffs: [] });
  const added = await proveB(c, (p) => Uint8Array.of(...p, 0x58));
  assert.ok(added.diffs.some((d) => d.field === 'lines.cells.chars'), 'added byte');
  const dropped = await proveB(c, (p) => p.filter((_, i) => i !== p.indexOf(0x77)));
  assert.ok(dropped.diffs.some((d) => d.field === 'lines.cells.chars'), 'dropped byte');
});

// ── Verdict ──────────────────────────────────────────────────────────────────────

/**
 * @typedef {(import('./xterm/screen.mjs').Diff & { step: string })} StepDiff
 * @typedef {{ kind: 'compared', diffs: StepDiff[] } | { kind: 'refused', reason: string }
 *   | { kind: 'inconclusive', why: string }} Outcome
 */

/** Turns recorded records into an Outcome. If the written byte count differs from the case or there are 0 lines, nothing was compared. @returns {Outcome} */
function compare(c, written, records) {
  if (written !== c.bytes) return { kind: 'inconclusive', why: `wrote ${written} bytes, case declares ${c.bytes}` };
  const empty = records.find(([, x, o]) => !Object.keys(x.lines).length || !Object.keys(o.lines).length);
  if (empty) return { kind: 'inconclusive', why: `empty screen record at ${empty[0]}` };
  return { kind: 'compared', diffs: records.flatMap(([step, x, o]) => diffScreens(x, o).map((d) => ({ ...d, step }))) };
}

/** @returns {Promise<Outcome>} */
async function proveA(c) {
  const x = xterm(c.cols, c.rows, c.scrollback);
  const o = new Terminal({ cols: c.cols, rows: c.rows, scrollback: c.scrollback });
  try {
    const written = await play(c.input, x, o);
    const records = [['initial', xtermScreen(x), wongttyScreen(o)]];
    await play([c.tail], x, o);
    records.push(['continued', xtermScreen(x), wongttyScreen(o)]);
    if (x.buffer.active.type === 'alternate') {
      await play(['\x1b[?1049lX'], x, o);
      records.push(['returned', xtermScreen(x), wongttyScreen(o)]);
    }
    return compare(c, written, records);
  } finally {
    o.dispose();
    x.dispose();
  }
}

/**
 * The restored xterm is created at the original's final size. edit is used only when a B lever changes the payload.
 * @returns {Promise<Outcome>}
 */
async function proveB(c, edit = (payload) => payload) {
  const x = xterm(c.cols, c.rows, c.scrollback);
  const o = new Terminal({ cols: c.cols, rows: c.rows, scrollback: c.scrollback });
  let restored;
  try {
    const written = await play(c.input, x, o);
    const s = o.ansiSnapshot();
    if (!s.ok) return written === c.bytes ? { kind: 'refused', reason: s.reason } : compare(c, written, []);
    restored = xterm(x.cols, x.rows, c.scrollback);
    await play([edit(s.payload)], restored);
    const records = [['initial', xtermScreen(x), xtermScreen(restored)]];
    await play([c.tail], x, restored);
    records.push(['continued', xtermScreen(x), xtermScreen(restored)]);
    if (x.buffer.active.type === 'alternate') {
      await play(['\x1b[?1049lX'], x, restored);
      records.push(['returned', xtermScreen(x), xtermScreen(restored)]);
    }
    return compare(c, written, records);
  } finally {
    restored?.dispose();
    o.dispose();
    x.dispose();
  }
}

const show = (v) => (JSON.stringify(v) ?? 'undefined').slice(0, 60);

/**
 * Pure verdict. A diff equal to or under an entry's field ('lines' covers 'lines.cells.fg') is documented.
 * Each entry attached to this case and proof must match at least once (prevents staleness).
 * @returns {{ pass: true } | { pass: false, message: string }}
 */
function judge(caseId, proof, outcome, known) {
  if (outcome.kind === 'inconclusive') return { pass: false, message: `inconclusive: ${outcome.why}` };
  const entries = known.filter((e) => e.proofs.includes(proof) && e.cases.includes(caseId));
  if (outcome.kind === 'refused') {
    return entries.some((e) => e.refusal === outcome.reason)
      ? { pass: true }
      : { pass: false, message: `unexpected refusal: ${outcome.reason}` };
  }
  const covers = (e, d) => e.fields?.some((f) => d.field === f || d.field.startsWith(`${f}.`)) ?? false;
  const left = outcome.diffs.filter((d) => !entries.some((e) => covers(e, d)));
  if (left.length) {
    const listed = left.slice(0, 8).map((d) => `${d.step} ${d.path} xterm=${show(d.xterm)} wongtty=${show(d.subject)}`);
    return { pass: false, message: `${left.length} unexpected differences: ${listed.join('; ')}` };
  }
  const stale = entries.filter((e) => !outcome.diffs.some((d) => covers(e, d)));
  if (stale.length) {
    return { pass: false, message: `stale known difference ${stale.map((e) => `"${e.id}"`).join(', ')}: it did not appear — delete or narrow it` };
  }
  return { pass: true };
}

test('judge: undocumented diff, stale entry, unexpected refusal and inconclusive all fail', () => {
  const d = (field) => ({ path: field, field, xterm: 1, subject: 2, step: 'initial' });
  const known = [
    { id: 'k', proofs: ['A'], cases: ['c'], fields: ['lines'], reason: 'r', blocksSwap: false },
    { id: 'r', proofs: ['B'], cases: ['c'], refusal: 'margins', reason: 'r', blocksSwap: false },
  ];
  assert.equal(judge('c', 'A', { kind: 'compared', diffs: [d('lines.cells.fg')] }, known).pass, true);
  assert.match(judge('c', 'A', { kind: 'compared', diffs: [d('lines.cells.fg'), d('cursor.x')] }, known).message, /unexpected differences.*cursor\.x/);
  assert.match(judge('c', 'A', { kind: 'compared', diffs: [] }, known).message, /stale.*"k"/);
  assert.match(judge('other', 'A', { kind: 'compared', diffs: [d('lines')] }, known).message, /unexpected/);
  assert.equal(judge('c', 'B', { kind: 'refused', reason: 'margins' }, known).pass, true);
  assert.match(judge('c', 'B', { kind: 'compared', diffs: [] }, known).message, /stale.*"r"/);
  assert.match(judge('c', 'A', { kind: 'refused', reason: 'margins' }, known).message, /unexpected refusal/);
  assert.equal(judge('other', 'A', { kind: 'compared', diffs: [] }, known).pass, true);
  assert.match(judge('c', 'A', { kind: 'inconclusive', why: 'x' }, known).message, /inconclusive/);
});

test('inconclusive sources: version mismatch, missing corpus, byte count, empty record', async () => {
  assert.match(oracleMismatch({ '@xterm/headless': '6.1.0-beta.303', '@xterm/addon-unicode11': '0.0.0' }), /addon-unicode11/);
  assert.match(corpus('no-such-recording').missing, /recording\.bin is missing/);
  const c = CASES.find((k) => k.id === 'ansi/8');
  assert.equal((await proveA(c)).kind, 'compared');
  assert.match((await proveA({ ...c, bytes: c.bytes + 1 })).why, /wrote \d+ bytes/);
  const blank = { lines: {} };
  assert.match(compare(c, c.bytes, [['initial', blank, blank]]).why, /empty screen/);
});

test('cases: wmux 91 = text 31 + ANSI 60 map onto 85 distinct inputs, ids unique', () => {
  const groups = {};
  for (const c of CASES) {
    const g = c.id.includes('/') ? c.id.slice(0, c.id.indexOf('/')) : c.id;
    groups[g] = (groups[g] ?? 0) + 1;
  }
  for (const [g, n] of Object.entries(WMUX_COUNTS)) assert.equal(groups[g], n, `group ${g}`);
  const { text, ansi, corpus: shared, seed } = WMUX_COUNTS;
  assert.equal(text + shared, 31);
  assert.equal(ansi + shared + seed + WMUX_COUNTS['partial-utf8'], 60);
  assert.equal(new Set(CASES.map((c) => c.id)).size, CASES.length);
});

test('known-differences: every entry has a reason, blocksSwap, real cases and real fields', () => {
  const ids = new Set(CASES.map((c) => c.id));
  const levers = Object.keys(LEVERS);
  for (const e of KNOWN) {
    assert.ok(typeof e.reason === 'string' && e.reason.length > 0, `${e.id}: reason`);
    assert.equal(typeof e.blocksSwap, 'boolean', `${e.id}: blocksSwap`);
    assert.ok(e.proofs.length && e.proofs.every((p) => p === 'A' || p === 'B'), `${e.id}: proofs`);
    assert.ok(e.cases.length && e.cases.every((c) => ids.has(c)), `${e.id}: cases ${e.cases}`);
    assert.ok(!e.fields !== !e.refusal, `${e.id}: exactly one of fields / refusal`);
    for (const f of e.fields ?? []) assert.ok(levers.some((l) => l === f || l.startsWith(`${f}.`)), `${e.id}: field ${f}`);
  }
  assert.equal(new Set(KNOWN.map((e) => e.id)).size, KNOWN.length);
});

test('wmux ANSI 60 = 59 same + seed/14 refused: no other B entry touches them', () => {
  const ansi60 = CASES.filter((c) => /^(ansi|corpus|seed)\/|^partial-utf8$/.test(c.id)).map((c) => c.id);
  assert.equal(ansi60.length, 60);
  // Per case, a B test passing without entries means equal. So here we pin that the only B entry is the seed/14 refusal.
  const touching = KNOWN.filter((e) => e.proofs.includes('B') && e.cases.some((id) => ansi60.includes(id)))
    .map((e) => [e.id, e.refusal, e.cases.filter((id) => ansi60.includes(id))]);
  assert.deepEqual(touching, [['resize-seed-14', 'unsupported-wide-edge', ['seed/14']]]);
});

test('swap gate: WONGTTY_SWAP_GATE=1 fails while a known difference blocks the engine swap', () => {
  if (process.env.WONGTTY_SWAP_GATE !== '1') return;
  const blockers = KNOWN.filter((e) => e.blocksSwap).map((e) => `${e.id} (${e.cases.join(', ')}): ${e.reason}`);
  assert.deepEqual(blockers, [], `${blockers.length} known differences block the swap:\n  ${blockers.join('\n  ')}`);
});

// ── Cases × proofs ───────────────────────────────────────────────────────────────
for (const c of CASES) {
  test(`A ${c.id}`, async () => {
    const why = ORACLE ?? c.missing;
    const outcome = why ? { kind: 'inconclusive', why } : await proveA(c);
    const verdict = judge(c.id, 'A', outcome, KNOWN);
    assert.ok(verdict.pass, verdict.message);
  });
  test(`B ${c.id}`, async () => {
    const why = ORACLE ?? c.missing;
    const outcome = why ? { kind: 'inconclusive', why } : await proveB(c);
    const verdict = judge(c.id, 'B', outcome, KNOWN);
    assert.ok(verdict.pass, verdict.message);
  });
}

// ── State the snapshot carries ───────────────────────────────────────────────────
// Charsets, the saved cursor and tab stops are not in the screen record, so each tail shows them on the screen.
// Proof A first pins that xterm.js and wongtty agree on the input itself.
const CARRIED = [
  ['G0 line drawing', '\x1b(0', 'qx'],
  ['G1 shifted in', '\x1b)0\x0e', 'q\x0fq\x0eq'],
  ['saved cursor and pen', '\x1b[3;5H\x1b[31m\x1b7\x1b[H\x1b[0mhome', '\x1b8X'],
  ['saved charset', '\x1b(0\x1b[2;2H\x1b7\x1b(B\x1b[H', '\x1b8q'],
  ['tab stops', '\x1b[3g\x1b[5G\x1bH\x1b[12G\x1bH\r', '\tA\tB\tC'],
];

test('B: charsets, the saved cursor and tab stops carry into xterm.js', async () => {
  if (ORACLE) return assert.fail(ORACLE);
  for (const [name, input, tail] of CARRIED) {
    const c = { cols: 20, rows: 6, scrollback: 100, input: [input], tail, bytes: new TextEncoder().encode(input).length };
    assert.deepEqual(await proveA(c), { kind: 'compared', diffs: [] }, `A ${name}`);
    assert.deepEqual(await proveB(c), { kind: 'compared', diffs: [] }, `B ${name}`);
  }
});

// ── Empty cells ──────────────────────────────────────────────────────────────────
// xterm.js keeps a typed space apart from an erased or never written cell (empty: getChars() ''); wongtty marks typed
// spaces (Attr.TYPED_SPACE). The records above compare blanks as ' ', so this compares which cells are empty, after
// every case's input and tail, skipping the cases whose characters or lines already differ (known entries).
test('A: the same cells are empty as in xterm.js, in every case', async () => {
  if (ORACLE) return assert.fail(ORACLE);
  const differ = ['lines.cells.chars', 'lines', 'baseY'];
  const charsDiffer = new Set(KNOWN.filter((e) => e.proofs.includes('A') && e.fields?.some((f) => differ.includes(f))).flatMap((e) => e.cases));
  const empties = (x, o) => {
    const out = [];
    const b = x.buffer.active;
    for (let y = 0; y < b.length; y++) {
      const line = b.getLine(y);
      const cells = o.rowCells(y - o.historyLength);
      const extras = new Set(o.rowExtras(y - o.historyLength).map((e) => e.col));
      for (let col = 0; col < x.cols; col++) {
        if (line.getCell(col).getWidth() === 0) continue;
        const xterm = line.getCell(col).getChars() === '';
        const [ch, , , attrs] = cells.subarray(col * 4, col * 4 + 4);
        const wongtty = ch === 32 && !extras.has(col) && !(attrs & Attr.TYPED_SPACE);
        if (xterm !== wongtty) out.push(`line ${y} col ${col}: xterm ${xterm ? 'empty' : 'text'}`);
      }
    }
    return out;
  };
  let compared = 0;
  for (const c of CASES.filter((c) => !c.missing && !charsDiffer.has(c.id))) {
    const x = xterm(c.cols, c.rows, c.scrollback);
    const o = new Terminal({ cols: c.cols, rows: c.rows, scrollback: c.scrollback });
    for (const events of [c.input, [c.tail]]) {
      await play(events, x, o);
      assert.deepEqual(empties(x, o), [], c.id);
    }
    compared++;
    o.dispose();
    x.dispose();
  }
  assert.ok(compared > 80, `${compared} cases`);
});
