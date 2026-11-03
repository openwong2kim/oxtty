// SearchAddon parity with the real xterm.js 6.1 and @xterm/addon-search: the same output and the same findNext /
// findPrevious sequences on an wongtty/xterm Terminal with its built-in SearchAddon and on xterm.js, compared by the
// value each call returns, the selection it leaves (getSelectionPosition) and the viewport top it scrolls to; with
// decorations, also by the onDidChangeResults events.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

const COLS = 30;
const ROWS = 6;

// More lines than the screen, so matches above and below the viewport scroll; a line wrapped over three rows, a
// match across a soft wrap, Hangul and CJK (wide), combining marks, case variants, words inside words and joined by
// `_` (whole word), and lines without a match.
const LINES = [
  'foo bar Foo baz',
  'nothing here',
  'xfoo foo_x (foo) FOO',
  '한글 foo 漢字 foo 가나',
  'été café foo naïve',
  `${'a'.repeat(26)} fo${'o'.repeat(2)} bar ${'b'.repeat(20)} foo end`,
  'line 7 has 42 and 7',
  'tail foo',
  'last line bar',
];

const STEPS = [
  ...Array(6).fill(['findNext', 'foo']),
  ...Array(4).fill(['findPrevious', 'foo']),
  ['findNext', 'foo', { caseSensitive: true }],
  ['findNext', 'FOO', { caseSensitive: true }],
  ['findPrevious', 'Foo', { caseSensitive: true }],
  ...Array(3).fill(['findNext', 'foo', { wholeWord: true }]),
  ...Array(3).fill(['findPrevious', 'foo', { wholeWord: true }]),
  ['findNext', 'f'],
  ['findNext', 'fo'],
  ['findNext', 'foo'],
  ['findPrevious', 'fo'],
  ['findNext', '\\d+', { regex: true }],
  ['findNext', '\\d+', { regex: true }],
  ['findPrevious', '\\d+', { regex: true }],
  ['findPrevious', 'b+ ?f', { regex: true }],
  ['findNext', 'a{20,}', { regex: true }],
  ['findNext', '^\\w+', { regex: true, caseSensitive: true }],
  ['findNext', 'x*', { regex: true }],
  ['findNext', '(', { regex: true }],
  ['findNext', '한글'],
  ['findNext', '漢字 foo'],
  ['findNext', 'café'],
  ['findPrevious', 'naïve'],
  ['findNext', 'oo bar'],
  ['findNext', 'absent'],
  ['findNext', ''],
  ['scrollLines', -3],
  ['findPrevious', 'bar'],
  ['findPrevious', 'bar'],
  ['clearDecorations'],
  ['findNext', 'bar'],
];

/** Runs `fn(page)` on the parity page with fresh terminals of `size` holding `text`. */
const withTerminals = (size, text, fn) => withPage('search-parity.mjs', async (page) => {
  await page.evaluate((s) => window.start(s), size);
  await page.evaluate((t) => window.write(t), text);
  return fn(page);
});

/** Steps where the two differ, as readable lines. */
const differences = ([wongtty, xterm]) => wongtty.flatMap((a, i) => {
  const b = xterm[i];
  return JSON.stringify(a) === JSON.stringify(b) ? [] : [`step ${i} ${a.method}(${a.args}): wongtty ${JSON.stringify(a)} xterm ${JSON.stringify(b)}`];
});

test('findNext and findPrevious select and scroll to the same matches as @xterm/addon-search', { timeout: 120000 }, async () => {
  await withTerminals({ cols: COLS, rows: ROWS, scrollback: 100 }, LINES.join('\r\n'), async (page) => {
    const runs = await page.evaluate((steps) => window.run(steps), STEPS);
    assert.deepEqual(differences(runs), []);
    assert.ok(runs[1].some((r) => r.value === true) && runs[1].some((r) => r.value === false), 'the sequence finds and misses');
  });
});

test('with decorations, the result count and active index match, also after output and as the scrollback trims', { timeout: 120000 }, async () => {
  const decorations = { matchBackground: '#444444', activeMatchBackground: '#ffff00', matchOverviewRuler: '#888888', activeMatchColorOverviewRuler: '#ffff00' };
  await withTerminals({ cols: COLS, rows: ROWS, scrollback: 8 }, LINES.join('\r\n'), async (page) => {
    const steps = [
      ...Array(4).fill(['findNext', 'foo', { decorations }]),
      ...Array(2).fill(['findPrevious', 'foo', { decorations }]),
      ['findNext', 'bar', { decorations }],
      ['findNext', 'foo', { decorations, wholeWord: true }],
      ['clearActiveDecoration'],
      ['findNext', 'foo', { decorations, wholeWord: true }],
    ];
    const first = await page.evaluate((s) => window.run(s), steps);
    assert.deepEqual(differences(first), []);
    const events = await page.evaluate(() => window.results());
    assert.deepEqual(events[0], events[1]);
    assert.equal(events[1].length, steps.length - 1);
    // New output trims the scrollback: the selection moves up with its text, and the next search goes on from it.
    await page.evaluate(() => window.write('\r\nfoo again\r\nmore\r\nfoo'));
    const after = await page.evaluate((s) => window.run(s), [['findNext', 'foo', { decorations }], ['findPrevious', 'foo', { decorations }], ['findPrevious', 'foo', { decorations }]]);
    assert.deepEqual(differences(after), []);
    const later = await page.evaluate(() => window.results());
    assert.deepEqual(later[0], later[1]);
  });
});

test('a whole-word search takes only the first match of a line, like @xterm/addon-search', { timeout: 120000 }, async () => {
  await withTerminals({ cols: 40, rows: 4, scrollback: 10 }, 'foo_bar foo\r\nbar foo\r\nfoo-x', async (page) => {
    const runs = await page.evaluate((s) => window.run(s), [
      ...Array(3).fill(['findNext', 'foo', { wholeWord: true }]),
      ...Array(3).fill(['findPrevious', 'foo', { wholeWord: true }]),
    ]);
    assert.deepEqual(differences(runs), []);
    assert.deepEqual(runs[0].slice(0, 2).map((r) => r.position?.start), [{ x: 4, y: 1 }, { x: 0, y: 2 }]);
  });
});

// Long enough output for the compressed scrollback: wrapped lines with matches on their later rows, a term that
// overlaps itself, case variants, KELVIN SIGN (lowercases to `k`) and İ (to `i` and U+0307, a unit longer, which moves
// the addon's matches after it on its line; those lines are searched in JS), wide and combining characters. The İ
// line goes on past its last match: the built-in addon drops a match that ends a line after an İ, which
// @xterm/addon-search keeps (as before the engine matched terms).
const LONG = Array.from({ length: 400 }, (_, i) => [
  `row ${i} foo`,
  `${'-'.repeat(25)} aaaa wrapped foo ${'x'.repeat(i % 40)} FOO end`,
  i % 50 === 7 ? '\u212a kelvin and k' : `plain ${i}`,
  i % 97 === 3 ? 'İstanbul foo k i end' : `한글 é foo${i % 10}`,
  i % 9 ? '' : 'no match here at all',
].join('\r\n')).join('\r\n');

test('plain ASCII terms, matched in the engine, find and count what @xterm/addon-search does', { timeout: 300000 }, async () => {
  const decorations = { matchBackground: '#444444', activeMatchBackground: '#ffff00', matchOverviewRuler: '#888888', activeMatchColorOverviewRuler: '#ffff00' };
  await withPage('search-parity.mjs', async (page) => {
    await page.evaluate((s) => window.start(s), { cols: COLS, rows: ROWS, scrollback: 3000, highlightLimit: 700 });
    await page.evaluate((t) => window.write(t), LONG);
    const steps = [];
    for (const term of ['foo', 'FOO', 'aa', 'k', 'i', 'wrapped foo', 'absent', 'x end', 'row 3']) {
      for (const options of [{}, { caseSensitive: true }, { decorations }, { decorations, caseSensitive: true }]) {
        // A new term is highlighted (and counted) again
        steps.push(['clearDecorations']);
        steps.push(...Array(3).fill(['findNext', term, options]), ...Array(4).fill(['findPrevious', term, options]));
        steps.push(['findNext', term, options]);
      }
    }
    // Starts inside wrapped lines (on their first and later rows) and at the top and bottom, with another term
    for (const [col, row] of [[0, 0], [3, 40], [5, 41], [29, 77], [0, 900], [10, 1200], [0, 1599], [20, 1601], [0, 1605]]) {
      for (const [method, term] of [['findNext', 'foo'], ['findPrevious', 'foo'], ['findPrevious', 'aa'], ['findNext', 'k'], ['findPrevious', 'i']]) {
        steps.push(['select', col, row, 2], [method, term, {}], [method, term, {}]);
      }
    }
    const runs = await page.evaluate((s) => window.run(s), steps);
    assert.deepEqual(differences(runs), []);
    assert.ok(runs[1].filter((r) => r.value === true).length > 300, 'mostly finds');
    const events = await page.evaluate(() => window.results());
    assert.deepEqual(events[0], events[1]);
    assert.ok(events[1].some((e) => e.resultCount === 700), 'the highlight limit is reached');
    assert.ok(events[1].some((e) => e.resultCount > 0 && e.resultCount < 700));
  });
});
