// Size gate for both WebAssembly modules (PRD §7): raw bytes and gzip -9, the transfer size most hosts serve.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { gzipSync } from 'node:zlib';

const KiB = 1024;
// Raw limits are the PRD's; gzip limits keep the ratio both modules have today (about 0.36) with room to grow. The
// owner raised the engine's on 2026-11-03, from 340 KiB raw and 125 KiB gzipped, for its textRows and searchRows exports.
const LIMITS = {
  'wongtty.wasm': { bytes: 360 * KiB, gzip: 135 * KiB },
  'xterm_web_bg.wasm': { bytes: 800 * KiB, gzip: 300 * KiB },
};

for (const [file, limit] of Object.entries(LIMITS)) {
  test(`${file} stays within ${limit.bytes / KiB} KiB raw and ${limit.gzip / KiB} KiB gzipped`, async (t) => {
    const bytes = await readFile(new URL(`../../dist/${file}`, import.meta.url));
    const gzip = gzipSync(bytes, { level: 9 }).length;
    t.diagnostic(`${file}: ${bytes.length} bytes, ${gzip} gzipped`);
    assert.ok(bytes.length <= limit.bytes, `${bytes.length} bytes`);
    assert.ok(gzip <= limit.gzip, `${gzip} bytes gzipped`);
  });
}
