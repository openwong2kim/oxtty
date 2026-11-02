// IME input against a real xterm.js 6.1 on the same page: Chrome's own IME path (CDP Input.imeSetComposition and
// Input.insertText), and replayed event traces in the orders the Korean IMEs of macOS and Windows produce, ending
// with Enter, Space or a digit right after the last syllable. Only committed text may reach onData, each syllable
// exactly once.
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { cdp, withPage } from './harness.mjs';

// Set to `false` once the member lands in crates/wongtty-web.
const TODO = false;

// Korean 2-set layout: the jamo, the physical key and its key code.
const JAMO = { 'ㄱ': ['KeyR', 82], 'ㄴ': ['KeyS', 83], 'ㄷ': ['KeyE', 69], 'ㄹ': ['KeyF', 70], 'ㅁ': ['KeyA', 65], 'ㅎ': ['KeyG', 71], 'ㅏ': ['KeyK', 75], 'ㅐ': ['KeyO', 79], 'ㅜ': ['KeyN', 78], 'ㅡ': ['KeyM', 77], 'ㅣ': ['KeyL', 76] };
const tick = { type: 'tick' };
const key = (type, k, code, keyCode, isComposing) => ({ type, init: { key: k, code, keyCode, which: keyCode, isComposing } });

/**
 * Typing `syllables`, each a list of [jamo, composition so far]. A syllable after the first starts as its first
 * jamo is typed: the IME ends the previous composition and starts the next in that keydown. Every keydown is 229
 * (the IME takes it); macOS names the jamo, Windows says 'Process'.
 */
function compose(syllables, os) {
  const steps = [];
  syllables.forEach((stages, s) => {
    stages.forEach(([jamo, text], i) => {
      const [code, keyCode] = JAMO[jamo];
      steps.push(key('keydown', os === 'mac' ? jamo : 'Process', code, 229, s > 0 || i > 0));
      if (i === 0 && s > 0) steps.push({ type: 'end', data: syllables[s - 1].at(-1)[1] });
      if (i === 0) steps.push({ type: 'start' });
      steps.push({ type: 'update', data: text }, tick, key('keyup', jamo, code, keyCode, true), tick);
    });
  });
  return steps;
}

const WORDS = {
  '한글': [[['ㅎ', 'ㅎ'], ['ㅏ', '하'], ['ㄴ', '한']], [['ㄱ', 'ㄱ'], ['ㅡ', '그'], ['ㄹ', '글']]],
  '대한민국': [[['ㄷ', 'ㄷ'], ['ㅐ', '대']], [['ㅎ', 'ㅎ'], ['ㅏ', '하'], ['ㄴ', '한']], [['ㅁ', 'ㅁ'], ['ㅣ', '미'], ['ㄴ', '민']], [['ㄱ', 'ㄱ'], ['ㅜ', '구'], ['ㄱ', '국']]],
};

const ENDINGS = {
  // macOS Chrome: Enter's keydown comes twice, 229 while composing, then 13 after compositionend.
  'macOS Enter': (last) => [key('keydown', 'Enter', 'Enter', 229, true), { type: 'end', data: last }, key('keydown', 'Enter', 'Enter', 13, false), key('keyup', 'Enter', 'Enter', 13, false), tick],
  // macOS: Space commits, then the IME inserts the space as text.
  'macOS Space': (last) => [key('keydown', ' ', 'Space', 229, true), { type: 'end', data: last }, { type: 'insertText', data: ' ' }, tick, key('keyup', ' ', 'Space', 32, false), tick],
  // Windows Chrome: the IME ends the composition, then the Enter keydown arrives, before the commit's timer.
  'Windows Enter': (last) => [{ type: 'end', data: last }, key('keydown', 'Enter', 'Enter', 13, false), tick, key('keyup', 'Enter', 'Enter', 13, false), tick],
  // The same with the Enter keydown in a later task, after the commit's timer.
  'Windows Enter, late keydown': (last) => [{ type: 'end', data: last }, tick, tick, key('keydown', 'Enter', 'Enter', 13, false), tick, key('keyup', 'Enter', 'Enter', 13, false), tick],
  // Windows: Space is taken by the IME (229), which commits and then sends the Space key.
  'Windows Space': (last) => [key('keydown', 'Process', 'Space', 229, true), { type: 'end', data: last }, tick, key('keydown', ' ', 'Space', 32, false), { type: 'keypress', init: { key: ' ', code: 'Space', keyCode: 32, charCode: 32, which: 32 } }, tick, key('keyup', ' ', 'Space', 32, false), tick],
  // A digit typed while the IME is on: 229 outside a composition, the text arrives as an input event.
  'Windows digit': (last) => [{ type: 'end', data: last }, tick, key('keydown', 'Process', 'Digit1', 229, false), { type: 'insertText', data: '1' }, tick, key('keyup', '1', 'Digit1', 49, false), tick],
};

const EXPECTED_END = { 'macOS Enter': '\r', 'macOS Space': ' ', 'Windows Enter': '\r', 'Windows Enter, late keydown': '\r', 'Windows Space': ' ', 'Windows digit': '1' };

test('IME input matches xterm.js', async (t) => {
  await withPage('parity.mjs', async (page) => {
    await page.waitForFunction(() => window.ready);
    const session = await cdp(page);

    await t.test('Chrome IME: only committed text is sent, the preedit shows at the cursor', { todo: TODO }, async () => {
      await page.evaluate(() => window.api.setup());
      await page.evaluate(() => window.api.write('ab'));
      const results = {};
      for (const name of ['ox', 'xt']) {
        await page.evaluate((n) => window.api.focus(n), name);
        const views = [];
        for (const [stages, commit] of [[['ㅎ', '하', '한'], '한'], [['ㄱ', '그', '글'], '글']]) {
          for (const text of stages) {
            await session.send('Input.imeSetComposition', { text, selectionStart: text.length, selectionEnd: text.length });
            views.push(await page.evaluate((n) => {
              const view = window.api.terminal(n).element.querySelector('.composition-view');
              return { active: view.classList.contains('active'), text: view.textContent, left: view.style.left, top: view.style.top, sent: window.api.peek(n).join('') };
            }, name));
          }
          await session.send('Input.insertText', { text: commit });
          // Echo the commit like a shell would, so the next preedit moves on.
          await page.evaluate(([n, s]) => new Promise((resolve) => window.api.terminal(n).write(s, resolve)).then(window.api.frame), [name, commit]);
        }
        await page.keyboard.press('Enter');
        await new Promise((resolve) => setTimeout(resolve, 50));
        results[name] = { views, data: (await page.evaluate(() => window.api.take()))[name] };
      }
      const width = await page.evaluate(() => window.api.terminal('ox').dimensions.css.cell.width);
      assert.deepEqual(results.ox.data, results.xt.data);
      assert.equal(results.ox.data.join(''), '한글\r');
      for (const [i, view] of results.ox.views.entries()) {
        assert.equal(view.active, true);
        assert.equal(view.sent, i < 3 ? '' : '한', 'only committed syllables are sent');
        assert.equal(view.text, `‎${['ㅎ', '하', '한', 'ㄱ', '그', '글'][i]}‎`);
        assert.equal(view.top, '0px');
        // The preedit sits at the cursor: after "ab", then after "ab한" (a wide cell).
        assert.equal(view.left, `${(i < 3 ? 2 : 4) * width}px`);
      }
      assert.deepEqual(results.ox.views.map((v) => [v.active, v.text, v.sent]), results.xt.views.map((v) => [v.active, v.text, v.sent]));
    });

    await t.test('Chrome IME: Enter with the preedit still open', { todo: TODO }, async () => {
      await page.evaluate(() => window.api.setup());
      for (const name of ['ox', 'xt']) {
        await page.evaluate((n) => window.api.focus(n), name);
        for (const text of ['ㅎ', '하', '한']) await session.send('Input.imeSetComposition', { text, selectionStart: 1, selectionEnd: 1 });
        await page.keyboard.press('Enter');
        await new Promise((resolve) => setTimeout(resolve, 50));
      }
      const { ox, xt } = await page.evaluate(() => window.api.take());
      assert.deepEqual(ox, xt);
      // Chrome commits the preedit before the key, so the syllable goes out first, once.
      assert.deepEqual(ox, ['한', '\r']);
    });

    for (const [word, syllables] of Object.entries(WORDS)) {
      for (const [ending, steps] of Object.entries(ENDINGS)) {
        for (const os of ['mac', 'windows']) {
          if (!ending.toLowerCase().startsWith(os.slice(0, 3))) continue;
          await t.test(`${word} then ${ending}`, { todo: TODO }, async () => {
            const trace = [...compose(syllables, os), ...steps(syllables.at(-1).at(-1)[1])];
            await page.evaluate(() => window.api.setup());
            for (const name of ['ox', 'xt']) {
              await page.evaluate((n) => window.api.focus(n), name);
              await page.evaluate(([n, s]) => window.api.replay(n, s), [name, trace]);
            }
            const { ox, xt } = await page.evaluate(() => window.api.take());
            assert.deepEqual(ox, xt);
            assert.equal(ox.join(''), word + EXPECTED_END[ending]);
          });
        }
      }
    }
  });
});
