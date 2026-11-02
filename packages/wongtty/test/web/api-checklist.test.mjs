// api-checklist.json stays honest: every entry has a known status and a reason, every `supported` member is
// declared in the generated dist/xterm_web.d.ts (or dist/xterm.d.ts) and is exercised by some test, and the counts
// per status are printed.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readdir, readFile } from 'node:fs/promises';

const read = (path) => readFile(new URL(path, import.meta.url), 'utf8');
const { statuses, members } = JSON.parse(await read('./api-checklist.json'));

/** Class name → members declared in a wasm-bindgen .d.ts (methods, getters, setters, properties); '' → functions. */
const declared = (types) => {
  const classes = new Map([['', new Set()]]);
  for (const [, owner, body] of types.matchAll(/^export (?:declare )?(?:class|interface) (\w+)[^{]*\{([\s\S]*?)^\}/gm)) {
    const names = classes.get(owner) ?? classes.set(owner, new Set()).get(owner);
    for (const [, name] of body.matchAll(/^\s+(?:readonly |static |get |set )*(\w+)\??[(:]/gm)) names.add(name);
  }
  for (const [, name] of types.matchAll(/^export (?:declare )?(?:async )?function (\w+)/gm)) classes.get('').add(name);
  return classes;
};
const types = declared((await read('../../dist/xterm_web.d.ts')) + (await read('../../dist/xterm.d.ts')));

// `Owner.member` names whose last segment is a member of the generated API; `options.*` hang off the options proxy.
const API_MEMBER = /^(Terminal(?:\.unicode)?|buffer(?:\.active)?|IBufferLine|IBufferCell|FitAddon|WebglAddon)\.(\w+)$/;
const OPTION = /^options\.([\w.]+)$/;
// Owners that are classes of the same name in the generated API; the others (buffer, lines, cells) may be any class.
const CLASSES = new Set(['Terminal', 'FitAddon', 'WebglAddon']);

/** Whether the generated types declare checklist member `name` (an API member or `init`); undefined if not one. */
const isDeclared = (name) => {
  if (name === 'init') return types.get('').has('init');
  const match = API_MEMBER.exec(name);
  if (!match) return undefined;
  const [, owner, member] = match;
  if (CLASSES.has(owner)) return types.get(owner)?.has(member) ?? false;
  return [...types.entries()].some(([cls, names]) => cls && cls !== 'Terminal' && cls !== 'InitOutput' && names.has(member)) ||
    (owner === 'Terminal.unicode' && types.get('Terminal')?.has('unicode'));
};

/** The text a test must contain to exercise `name`: `exercisedBy` in the entry, else the member's own name. */
const probe = (name, entry) => {
  if (entry.exercisedBy) return new RegExp(entry.exercisedBy);
  if (name === 'init') return /\binit\(/;
  const member = API_MEMBER.exec(name)?.[2] ?? OPTION.exec(name)?.[1].split('.').at(-1) ?? /^DOM \.([\w-]+)$/.exec(name)?.[1];
  return member && (member === 'constructor' ? /new (?:\w+\.)*Terminal\(/ : new RegExp(`\\b${member}\\b`));
};

const testSources = async () => {
  const files = [new URL('../web.test.mjs', import.meta.url)];
  for (const dir of ['./', './pages/']) {
    for (const file of await readdir(new URL(dir, import.meta.url))) {
      if (/\.test\.mjs$|^(?!harness).*\.mjs$/.test(file) && file !== 'api-checklist.test.mjs') files.push(new URL(dir + file, import.meta.url));
    }
  }
  return (await Promise.all(files.map((file) => readFile(file, 'utf8')))).join('\n');
};

test('every checklist entry has a known status and a reason', (t) => {
  const counts = Object.fromEntries(Object.keys(statuses).map((status) => [status, 0]));
  for (const [name, { status, reason }] of Object.entries(members)) {
    assert.ok(status in statuses, `${name}: status "${status}"`);
    assert.ok(reason, `${name}: reason`);
    counts[status]++;
  }
  t.diagnostic(`api-checklist: ${Object.entries(counts).map(([status, n]) => `${status} ${n}`).join(', ')} (${Object.keys(members).length} members)`);
});

test('the type reader sees the generated API', () => {
  assert.equal(isDeclared('init'), true);
  assert.equal(isDeclared('Terminal.write'), true);
  assert.equal(isDeclared('Terminal.constructor'), true);
  assert.equal(isDeclared('Terminal.noSuchMember'), false);
  assert.equal(isDeclared('DOM .xterm'), undefined);
});

test('supported members are declared in the generated types and exercised by a test', async (t) => {
  const sources = await testSources();
  const supported = Object.entries(members).filter(([, entry]) => entry.status === 'supported');
  for (const [name, entry] of supported) {
    const found = isDeclared(name);
    if (found !== undefined) assert.ok(found, `${name} is not declared in dist/xterm_web.d.ts or dist/xterm.d.ts`);
    else if (OPTION.test(name)) assert.ok(types.get('Terminal')?.has('options'), `${name}: Terminal.options is not declared`);
    const pattern = probe(name, entry);
    assert.ok(pattern, `${name}: not an API member; give the entry an "exercisedBy" regular expression`);
    assert.match(sources, pattern, `${name}: no test under test/web or test/web.test.mjs exercises it (${pattern})`);
  }
  // Not a failure: members still `todo` that the generated API already declares, to flip once their tests pass.
  const ready = Object.entries(members)
    .filter(([name, { status }]) => status === 'todo' && isDeclared(name))
    .map(([name]) => name);
  t.diagnostic(`api-checklist: ${supported.length} supported members checked; todo but declared: ${ready.join(', ') || 'none'}`);
});
