import { test } from 'node:test';
import assert from 'node:assert/strict';
import { withPage } from './harness.mjs';

test('incremental rendering matches a full rebuild pixel for pixel', async () => {
  const results = await withPage('renderer-consistency.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    const out = [];
    for (const seed of [1, 2, 3]) out.push(await page.evaluate((s) => window.run(s, 200), seed));
    return out;
  });
  for (const [k, r] of results.entries()) {
    assert.equal(r.checkpoints, 10);
    assert.deepEqual(r.diffs, new Array(10).fill(0), `seed ${k + 1}`);
    assert.ok(r.sentinel > 0, 'a single changed cell shows up');
  }
});
