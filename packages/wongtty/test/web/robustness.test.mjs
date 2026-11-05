// What the M2 eng review asked for beyond parity, in the browser with opened terminals: listeners that write,
// resize and dispose from inside events; an engine crash in one pane while others run; and no leaks over 100
// terminals created and disposed (DOM listeners, elements, WebAssembly memory).
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { cdp, withPage } from './harness.mjs';

/** Opens `page` on modules.mjs and runs `fn(page)` once init() has finished. */
const withModules = (fn) => withPage('modules.mjs', async (page) => {
  await page.waitForFunction(() => window.ready);
  assert.equal(await page.evaluate(() => window.ready), true);
  return fn(page);
});

test('re-entrancy: listeners write, resize and dispose an opened terminal from inside events', async (t) => {
  await withModules(async (page) => {
    await t.test('inside onData', async () => {
      const facts = await page.evaluate(async () => {
        const term = new window.ox.Terminal({ cols: 20, rows: 5 });
        term.open(document.body.appendChild(document.createElement('div')));
        const sizes = [];
        term.onResize((size) => sizes.push(size));
        let replies = 0;
        term.onData(() => {
          replies++;
          if (replies === 1) {
            term.write('\x1b[6n');
            term.resize(30, 6);
          } else {
            term.dispose();
            term.write('ignored');
          }
        });
        term.write('\x1b[6n');
        await new Promise((resolve) => setTimeout(resolve, 100));
        term.dispose();
        return { replies, sizes, element: term.element ?? null };
      });
      assert.deepEqual(facts, { replies: 2, sizes: [{ cols: 30, rows: 6 }], element: null });
    });

    await t.test('inside onTitleChange', async () => {
      const facts = await page.evaluate(async () => {
        const term = new window.ox.Terminal({ cols: 20, rows: 5 });
        term.open(document.body.appendChild(document.createElement('div')));
        const titles = [];
        const replies = [];
        term.onData((d) => replies.push(d));
        term.onTitleChange((title) => {
          titles.push(title);
          if (title === 'one') {
            term.resize(25, 5);
            term.write('\x1b]0;two\x07\x1b[6n');
          } else {
            term.dispose();
          }
        });
        term.write('\x1b]0;one\x07');
        await new Promise((resolve) => setTimeout(resolve, 100));
        return { titles, replies, element: term.element ?? null };
      });
      assert.deepEqual(facts.titles, ['one', 'two']);
      assert.deepEqual(facts.element, null);
      assert.ok(facts.replies.length <= 1, 'nothing after dispose but the reply parsed with the title');
    });

    await t.test('inside a write callback and onWriteParsed', async () => {
      const facts = await page.evaluate(async () => {
        const term = new window.ox.Terminal({ cols: 20, rows: 5 });
        term.open(document.body.appendChild(document.createElement('div')));
        const order = [];
        let parsed = 0;
        term.onWriteParsed(() => {
          parsed++;
          if (parsed === 1) term.write('b', () => order.push('b'));
        });
        term.write('a', () => {
          order.push('a');
          term.resize(21, 5);
        });
        await new Promise((resolve) => setTimeout(resolve, 50));
        term.write('c', () => {
          order.push('c');
          term.dispose();
        });
        term.write('d', () => order.push('d'));
        await new Promise((resolve) => setTimeout(resolve, 50));
        return order;
      });
      assert.deepEqual(facts, ['a', 'b', 'c']);
    });

    await t.test('inside onRender', async () => {
      const facts = await page.evaluate(async () => {
        const term = new window.ox.Terminal({ cols: 20, rows: 5 });
        term.open(document.body.appendChild(document.createElement('div')));
        let renders = 0;
        term.onRender(() => {
          renders++;
          if (renders === 1) {
            term.write('more');
            term.resize(22, 5);
          } else if (renders === 2) {
            term.dispose();
          }
        });
        term.write('first');
        await new Promise((resolve) => setTimeout(resolve, 300));
        return { renders, element: term.element ?? null };
      });
      assert.deepEqual(facts, { renders: 2, element: null });
    });
  });
});

test('crash isolation: an engine crash in one pane\'s listener leaves every pane working', async () => {
  await withModules(async (page) => {
    const facts = await page.evaluate(async () => {
      const panes = ['a', 'b', 'c'].map(() => {
        const term = new window.ox.Terminal({ cols: 20, rows: 5 });
        term.open(document.body.appendChild(document.createElement('div')));
        const replies = [];
        term.onData((d) => replies.push(d));
        return { term, replies };
      });
      const written = (term, data) => new Promise((resolve) => term.write(data, resolve));
      await Promise.all(panes.map(({ term }, i) => written(term, `pane ${i}`)));
      // Pane a's listener crashes the engine while it handles a reply.
      let trapped;
      const listener = panes[0].term.onData(() => {
        listener.dispose();
        try {
          window.engine.__trapForTest();
        } catch (err) {
          trapped = err.constructor.name;
        }
      });
      await written(panes[0].term, '\x1b[6n');
      await new Promise((resolve) => setTimeout(resolve, 20));
      for (const { replies } of panes) replies.length = 0;
      // Every pane parses, answers and keeps its element; frames keep coming.
      await Promise.all(panes.map(({ term }, i) => written(term, `\x1b[H\x1b[2J${'x'.repeat(i + 1)}\x1b[6n`)));
      await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
      return {
        trapped,
        replies: panes.map(({ replies }) => replies),
        attached: panes.map(({ term }) => document.body.contains(term.element)),
      };
    });
    assert.equal(facts.trapped, 'RuntimeError');
    assert.deepEqual(facts.replies, [['\x1b[1;2R'], ['\x1b[1;3R'], ['\x1b[1;4R']]);
    assert.deepEqual(facts.attached, [true, true, true]);
  });
});

/** Runs `fn(page, session, cycle, measure)`: `cycle(n)` creates, opens, writes and disposes n terminals. */
const withCycles = (fn) => withModules(async (page) => {
  const session = await cdp(page);
  await session.send('DOM.enable');
  const cycle = (n) => page.evaluate(async (count) => {
    const parent = document.getElementById('terminal');
    for (let i = 0; i < count; i++) {
      const term = new window.ox.Terminal({ cols: 40, rows: 10, scrollback: 100 });
      term.open(parent);
      term.onData(() => {});
      await new Promise((resolve) => term.write(`terminal ${i}\r\n`.repeat(20) + '\x1b[6n', resolve));
      term.dispose();
    }
  }, n);
  const listeners = async (expression) => {
    const { result } = await session.send('Runtime.evaluate', { expression });
    const { listeners: list } = await session.send('DOMDebugger.getEventListeners', { objectId: result.objectId, depth: -1 });
    await session.send('Runtime.releaseObject', { objectId: result.objectId });
    return list.length;
  };
  const measure = async () => {
    await session.send('HeapProfiler.collectGarbage');
    await page.evaluate(() => new Promise((resolve) => setTimeout(resolve, 50)));
    await session.send('HeapProfiler.collectGarbage');
    return {
      listeners: { window: await listeners('window'), document: await listeners('document'), parent: await listeners('document.getElementById("terminal")') },
      ...(await page.evaluate(() => ({
        elements: document.querySelectorAll('.xterm').length,
        styles: document.querySelectorAll('style#wongtty-xterm-style').length,
        memories: window.memories.map((m) => m.buffer.byteLength),
      }))),
    };
  };
  // Warm up: allocator pools and caches grow to their working size first.
  await cycle(20);
  return fn(cycle, measure);
});

test('no leaks over 100 terminals: DOM listeners and elements stay flat', { timeout: 120000 }, async () => {
  await withCycles(async (cycle, measure) => {
    const before = await measure();
    await cycle(100);
    const after = await measure();
    assert.deepEqual(after.listeners, before.listeners);
    assert.deepEqual([after.elements, after.styles], [0, 1]);
  });
});

test('the page-wide window and font listeners go with the last terminal and come back with the next one', async () => {
  await withModules(async (page) => {
    const session = await cdp(page);
    const count = async (expression) => {
      const { result } = await session.send('Runtime.evaluate', { expression });
      const { listeners } = await session.send('DOMDebugger.getEventListeners', { objectId: result.objectId });
      await session.send('Runtime.releaseObject', { objectId: result.objectId });
      return listeners.filter((l) => ['resize', 'loadingdone'].includes(l.type)).length;
    };
    const counts = async () => [await count('window'), await count('document.fonts')];
    const open = () => page.evaluate(() => {
      window.term = new window.ox.Terminal({ cols: 40, rows: 10 });
      window.term.open(document.getElementById('terminal'));
    });
    const before = await counts();
    await open();
    const opened = await counts();
    await page.evaluate(() => window.term.dispose());
    const disposed = await counts();
    await open();
    const reopened = await counts();
    assert.deepEqual({ opened, disposed, reopened }, { opened: [before[0] + 1, before[1] + 1], disposed: before, reopened: opened });
  });
});

test('no leaks over 100 terminals: both WebAssembly memories stay flat', { timeout: 180000 }, async () => {
  await withCycles(async (cycle, measure) => {
    // Twenty terminals do not always reach the allocator's working size: on CI the second memory sometimes grew
    // its last page in the measured run (21 -> 22 pages), where 600 more terminals stay at 22
    await cycle(100);
    const before = await measure();
    await cycle(100);
    const after = await measure();
    assert.equal(before.memories.length, 2, 'both WebAssembly memories are watched');
    assert.deepEqual(after.memories, before.memories);
  });
});
