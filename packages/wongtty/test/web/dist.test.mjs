// The built package as a Node ESM consumer sees it: `import 'wongtty/xterm'` through the `exports` map, `init()`, the
// files `npm pack` ships and the types. Runs in its own process, so the first test sees a module before `init()`.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { readFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const pkgDir = fileURLToPath(new URL('../../', import.meta.url));
const pkg = JSON.parse(await readFile(new URL('../../package.json', import.meta.url), 'utf8'));
const require = createRequire(import.meta.url);
const xterm = await import('wongtty/xterm');

test('constructing before init() throws an error that says to await init()', () => {
  assert.throws(() => new xterm.Terminal(), /await init\(\)/);
});

test('init() without arguments loads both modules in Node', async () => {
  await xterm.init();
  const t = new xterm.Terminal({ cols: 10, rows: 2 });
  t.dispose();
});

test('init() with both modules as bytes, then a terminal answers', async () => {
  const bytes = (file) => readFile(new URL(`../../dist/${file}`, import.meta.url));
  await xterm.init(await bytes('wongtty.wasm'), await bytes('xterm_web_bg.wasm'));
  await xterm.init();
  const t = new xterm.Terminal({ cols: 10, rows: 2 });
  const data = [];
  t.onData((d) => data.push(d));
  await new Promise((resolve) => t.write('hi\x1b[6n', resolve));
  assert.deepEqual(data, ['\x1b[1;3R']);
  t.dispose();
});

test('the exports map points at built files and wongtty/xterm is dist/xterm.js', () => {
  for (const [subpath, target] of Object.entries(pkg.exports)) {
    for (const file of typeof target === 'string' ? [target] : Object.values(target)) {
      assert.ok(existsSync(new URL(file, new URL('../../', import.meta.url))), `${subpath} → ${file}`);
    }
  }
  assert.equal(require.resolve('wongtty/xterm'), `${pkgDir}dist/xterm.js`);
  assert.equal(require.resolve('wongtty/wongtty.wasm'), `${pkgDir}dist/wongtty.wasm`);
});

test('both WebAssembly modules can be resolved through the exports map, for init() with bytes', () => {
  assert.equal(require.resolve('wongtty/xterm_web_bg.wasm'), `${pkgDir}dist/xterm_web_bg.wasm`);
});

test('npm pack ships the entry points, both WebAssembly modules, the types and the notices', () => {
  const [{ files }] = JSON.parse(execFileSync('npm', ['pack', '--dry-run', '--json'], { cwd: pkgDir, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }));
  const paths = files.map((f) => f.path);
  for (const file of ['dist/index.js', 'dist/index.d.ts', 'dist/wongtty.wasm', 'dist/xterm.js', 'dist/xterm.d.ts', 'dist/xterm_web.js', 'dist/xterm_web.d.ts', 'dist/xterm_web_bg.wasm', 'LICENSE-APACHE', 'THIRD_PARTY_NOTICES.md']) {
    assert.ok(paths.includes(file), file);
  }
  assert.deepEqual(paths.filter((p) => !p.startsWith('dist/') && !['package.json', 'README.md', 'LICENSE-APACHE', 'THIRD_PARTY_NOTICES.md'].includes(p)), []);
});

test('typesVersions maps wongtty/xterm to shipped types for moduleResolution "node", which ignores exports', () => {
  assert.deepEqual(pkg.typesVersions, { '*': { xterm: ['dist/xterm.d.ts'] } });
  assert.equal(pkg.exports['./xterm'].types, `./${pkg.typesVersions['*'].xterm[0]}`);
  const [{ files }] = JSON.parse(execFileSync('npm', ['pack', '--dry-run', '--json'], { cwd: pkgDir, encoding: 'utf8', stdio: ['ignore', 'pipe', 'ignore'] }));
  assert.ok(files.some((f) => f.path === pkg.typesVersions['*'].xterm[0]));
});

test('the shipped types are the source types and do not depend on @xterm/xterm', async () => {
  const types = await readFile(new URL('../../dist/xterm.d.ts', import.meta.url), 'utf8');
  assert.equal(types, await readFile(new URL('../../src/xterm.d.ts', import.meta.url), 'utf8'));
  assert.doesNotMatch(types + await readFile(new URL('../../dist/xterm_web.d.ts', import.meta.url), 'utf8'), /@xterm\/xterm/);
});
