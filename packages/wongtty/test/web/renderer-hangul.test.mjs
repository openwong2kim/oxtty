// Hangul fallback widths: wongtty/xterm draws exactly what real xterm.js with the WebGL addon draws (PRD risk "uneven
// widths in the Hangul fallback font"), at DPR 1 and 2.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

// The spike line where "안녕하세요" looked spread apart and "입니다" stuck together
const LINE = '한글: 안녕하세요, 터미널 엔진 wongtty입니다.';
// Two-cell runs of 안 녕 하 세 요 and 입 니 다
const RUNS = [[6, 8], [8, 10], [10, 12], [12, 14], [14, 16], [37, 39], [39, 41], [41, 43]];
const OPTIONS = [
  { fontFamily: 'Menlo, monospace', fontSize: 14 },
  { fontFamily: 'monospace', fontSize: 15 },
  { fontFamily: 'Menlo, "Apple SD Gothic Neo", monospace', fontSize: 14, lineHeight: 1.2, letterSpacing: 1 },
];

for (const deviceScaleFactor of [1, 2]) {
  test(`Hangul draws exactly as xterm.js with the WebGL addon (DPR ${deviceScaleFactor})`, async () => {
    const results = await withPage('renderer-hangul.mjs', async (page) => {
      await page.waitForFunction(() => window.ready);
      return page.evaluate(async ([line, runs, options]) => {
        const out = [];
        for (const o of options) out.push(await window.compare(line, o, runs));
        return out;
      }, [LINE, RUNS, OPTIONS]);
    }, { deviceScaleFactor });
    for (const [k, r] of results.entries()) {
      const what = JSON.stringify(OPTIONS[k]);
      assert.deepEqual(r.wongtty, r.xterm, `cell metrics, ${what}`);
      assert.equal(r.diff, 0, `pixels, ${what}`);
      assert.deepEqual(r.wongttyInk, r.xtermInk, `glyph ink, ${what}`);
      // The fallback font's Hangul is narrower than two cells, so every syllable leaves a gap on its right in both
      const slot = 2 * r.wongtty.cell.width;
      assert.ok(r.wongttyInk.every(([left, right]) => left >= 0 && right < slot - 2), `${what}: ${JSON.stringify(r.wongttyInk)} in ${slot}`);
    }
  });
}

// SGR attributes, colours, underline styles and colours, custom glyphs, emoji and combining marks. Left out: the
// curly underline and the shades ░▒▓, which the xterm.js 6.1 beta compared here draws differently from 6.0.0 (the
// version ported: a zigzag polygon, and its rewritten custom glyph rasterizer).
const ATTRIBUTES = [
  '\x1b[1mbold\x1b[22m \x1b[2mdim\x1b[22m \x1b[3mital\x1b[23m \x1b[7minv\x1b[27m \x1b[9mstrike\x1b[29m \x1b[8mhide\x1b[28m',
  '\x1b[31;42mred\x1b[0m \x1b[1;34mbrt\x1b[0m \x1b[38;5;208m208\x1b[0m \x1b[48;2;60;0;90;38;2;10;200;90mrgb\x1b[0m \x1b[7;33mrev\x1b[0m',
  '\x1b[4msingle\x1b[0m \x1b[4:2mdouble\x1b[0m \x1b[4:4mdotted\x1b[0m \x1b[4:5mdashed\x1b[0m',
  '\x1b[4;58;2;255;0;0mredline\x1b[0m \x1b[4;58;5;4mblueline\x1b[0m ┌─┬─┐│█▀▄▌  😀👍 é ñ',
].join('\r\n');

test('attributes, colours, underlines, custom glyphs and emoji draw exactly as xterm.js', async () => {
  const results = await withPage('renderer-hangul.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    return page.evaluate(async (text) => [
      await window.compare(text, { fontFamily: 'Menlo, monospace', fontSize: 14, rows: 4 }, []),
      await window.compare(text, { fontFamily: 'Menlo, monospace', fontSize: 14, rows: 4, minimumContrastRatio: 4.5, drawBoldTextInBrightColors: false }, []),
    ], ATTRIBUTES);
  });
  for (const r of results) {
    assert.deepEqual(r.wongtty, r.xterm, 'cell metrics');
    assert.equal(r.diff, 0, 'pixels');
  }
});
