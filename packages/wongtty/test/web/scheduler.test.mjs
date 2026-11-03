// The page-wide write scheduler: terminals with queued writes take turns in one task between frames. Node runs it
// without frames; the browser tests run it against real frames.
import { test, before } from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import * as m from '../../dist/xterm.js';
import { withPage } from './harness.mjs';

before(async () => {
  await m.init(undefined, await readFile(new URL('../../dist/xterm_web_bg.wasm', import.meta.url)));
});

/** Writes `chunks` chunks of `size` bytes to each terminal; resolves with the order the callbacks fired in. */
const flood = (terms, chunks, size) =>
  new Promise((resolve) => {
    const order = [];
    let left = terms.length * chunks;
    terms.forEach((t, i) => {
      for (let k = 0; k < chunks; k++) {
        t.write(`${String.fromCharCode(65 + i)}${k}`.padEnd(size, '.'), () => {
          order.push([i, k]);
          if (--left === 0) resolve(order);
        });
      }
    });
  });

test('terminals take turns: each parses its writes in order, every callback fires once', async () => {
  const terms = [0, 1, 2].map(() => new m.Terminal({ cols: 80, rows: 24 }));
  const order = await flood(terms, 64, 64 * 1024);
  for (let i = 0; i < terms.length; i++) {
    assert.deepEqual(order.filter(([t]) => t === i).map(([, k]) => k), [...Array(64).keys()]);
  }
  // The first terminal had not finished when the last one started: they shared the runs.
  const firstDone = order.findLastIndex(([t]) => t === 0);
  const lastStarted = order.findIndex(([t]) => t === 2);
  assert.ok(lastStarted < firstDone, `terminal 2 started at ${lastStarted}, terminal 0 finished at ${firstDone}`);
  for (const t of terms) t.dispose();
});

test('a terminal disposed from a write callback leaves the queue and the others finish', async () => {
  const terms = [0, 1].map(() => new m.Terminal({ cols: 80, rows: 24 }));
  let afterDispose = 0;
  terms[0].write('x'.repeat(1 << 20), () => terms[0].dispose());
  terms[0].write('y'.repeat(1 << 20), () => afterDispose++);
  await new Promise((resolve) => terms[1].write('z'.repeat(4 << 20), resolve));
  await new Promise((resolve) => setTimeout(resolve, 50));
  assert.equal(afterDispose, 0);
  terms[1].dispose();
});

test('with frames: callbacks fire once and in order, the first frame shows a write made right after open', async () => {
  await withPage('scheduler.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    const first = await page.evaluate(
      () =>
        new Promise((resolve) => {
          const t = new window.Terminal({ cols: 40, rows: 8 });
          t.open(document.getElementById('terminal'));
          t.write('user@host:~$ ');
          const d = t.onRender(() => {
            d.dispose();
            resolve(t.buffer.active.getLine(0).translateToString(true));
          });
        }),
    );
    assert.equal(first, 'user@host:~$ ');
    const ok = await page.evaluate(async () => {
      const terms = [];
      for (let i = 0; i < 6; i++) {
        const host = document.createElement('div');
        document.body.appendChild(host);
        const t = new window.Terminal({ cols: 80, rows: 24 });
        t.open(host);
        terms.push(t);
      }
      const seen = terms.map(() => []);
      await Promise.all(
        terms.map((t, i) =>
          Promise.all(
            [...Array(40).keys()].map((k) => new Promise((resolve) => t.write('\x1b[31m'.padEnd(32 * 1024, 'x'), () => (seen[i].push(k), resolve())))),
          ),
        ),
      );
      return seen.every((s) => s.join() === [...Array(40).keys()].join());
    });
    assert.ok(ok);
  });
});

test('writes still finish when frames never come (a hidden page)', async () => {
  await withPage('scheduler.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    const ms = await page.evaluate(async () => {
      window.requestAnimationFrame = () => 1;
      window.cancelAnimationFrame = () => {};
      const terms = [0, 1, 2].map(() => {
        const t = new window.Terminal({ cols: 80, rows: 24 });
        t.open(document.getElementById('terminal'));
        return t;
      });
      const start = performance.now();
      await Promise.all(terms.map((t) => new Promise((resolve) => t.write('x'.repeat(4 << 20), resolve))));
      return performance.now() - start;
    });
    assert.ok(ms < 10000, `${ms} ms`);
  });
});

test('a second resize before the frame reflows once, at the frame; writes after it parse at the new size', async () => {
  await withPage('scheduler.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    const r = await page.evaluate(async () => {
      const frame = () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      const t = new window.Terminal({ cols: 20, rows: 6 });
      t.open(document.getElementById('terminal'));
      await frame();
      const resizes = [];
      t.onResize((e) => resizes.push(`${e.cols}x${e.rows}`));
      t.resize(30, 6);
      t.resize(31, 6);
      t.resize(32, 7);
      const atOnce = { cols: t.cols, rows: t.rows, resizes: resizes.join() };
      await frame();
      const after = { resizes: resizes.join(), width: t.buffer.active.getLine(0).length };
      // Right after a frame that reflowed, the next resize waits too; a write before that frame applies it first.
      t.resize(40, 7);
      const line = await new Promise((resolve) => t.write('y'.repeat(35), () => resolve(t.buffer.active.getLine(0).translateToString(true))));
      return { atOnce, after, line, cols: t.cols, resizes: resizes.join() };
    });
    assert.deepEqual(r.atOnce, { cols: 32, rows: 7, resizes: '30x6' });
    assert.deepEqual(r.after, { resizes: '30x6,32x7', width: 32 });
    assert.equal(r.line, 'y'.repeat(35));
    assert.equal(r.resizes, '30x6,32x7,40x7');
  });
});
