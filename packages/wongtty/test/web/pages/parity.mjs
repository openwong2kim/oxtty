// Page for the input tests: an wongtty/xterm Terminal and a real xterm.js 6.1 Terminal side by side, with the same
// options. The tests drive both with the same browser input (CDP key events, IME, paste) and compare their onData.
import { init, Terminal } from 'wongtty/xterm';
import { Terminal as XTerm } from '@xterm/xterm';

let pair;
const frame = () => new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));

window.api = {
  /** Fresh terminals in fresh containers; `options` go to both. */
  async setup(options = {}) {
    if (pair) {
      pair.ox.dispose();
      pair.xt.dispose();
    }
    document.body.replaceChildren();
    const make = () => document.body.appendChild(document.createElement('div'));
    const common = { cols: 40, rows: 6, fontFamily: 'Menlo, monospace', fontSize: 14, ...options };
    const ox = new Terminal(common);
    const xt = new XTerm(common);
    ox.open(make());
    xt.open(make());
    const data = { ox: [], xt: [] };
    ox.onData((d) => data.ox.push(d));
    xt.onData((d) => data.xt.push(d));
    pair = { ox, xt, data };
    await frame();
  },
  /** Writes to both and waits until both parsed it and wongtty drew a frame. */
  async write(data) {
    const { ox, xt } = pair;
    await Promise.all([new Promise((r) => ox.write(data, r)), new Promise((r) => xt.write(data, r))]);
    await frame();
  },
  focus(name) {
    pair[name].focus();
  },
  blur(name) {
    pair[name].blur();
  },
  paste(name, text) {
    pair[name].paste(text);
  },
  setOption(name, key, value) {
    pair[name].options[key] = value;
  },
  /**
   * Replays an IME `trace` on the textarea of `name`, then waits for the timers the composition armed. Steps:
   * keydown/keyup/keypress with an `init`; `start`; `update`/`end`/`insertText` with `data` (`update` and
   * `insertText` also edit the textarea the way the browser does); `tick` yields one macrotask (the browser
   * delivers each native event as its own task, so timers armed by a handler can run in between).
   */
  async replay(name, trace) {
    const textarea = pair[name].textarea;
    const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
    let start = 0;
    let end = 0;
    const input = (inputType, data, isComposing) => {
      textarea.dispatchEvent(new InputEvent('input', { inputType, data, isComposing, composed: true, bubbles: true }));
    };
    for (const step of trace) {
      switch (step.type) {
        case 'tick':
          await tick();
          break;
        case 'keydown':
        case 'keyup':
        case 'keypress':
          textarea.dispatchEvent(new KeyboardEvent(step.type, { bubbles: true, cancelable: true, composed: true, ...step.init }));
          break;
        case 'start':
          start = end = textarea.selectionStart;
          textarea.dispatchEvent(new CompositionEvent('compositionstart', { data: '', bubbles: true }));
          break;
        case 'update':
          textarea.value = textarea.value.slice(0, start) + step.data + textarea.value.slice(end);
          end = start + step.data.length;
          textarea.selectionStart = textarea.selectionEnd = end;
          textarea.dispatchEvent(new CompositionEvent('compositionupdate', { data: step.data, bubbles: true }));
          input('insertCompositionText', step.data, true);
          break;
        case 'end':
          textarea.dispatchEvent(new CompositionEvent('compositionend', { data: step.data, bubbles: true }));
          break;
        case 'insertText': {
          const at = textarea.selectionStart;
          textarea.value = textarea.value.slice(0, at) + step.data + textarea.value.slice(textarea.selectionEnd);
          textarea.selectionStart = textarea.selectionEnd = at + step.data.length;
          input('insertText', step.data, false);
          break;
        }
      }
    }
    await new Promise((resolve) => setTimeout(resolve, 20));
  },
  /** Data `name` sent since the last `take`, without taking it. */
  peek(name) {
    return [...pair.data[name]];
  },
  /** Data each terminal sent since the last call. */
  take() {
    const { data } = pair;
    return { ox: data.ox.splice(0), xt: data.xt.splice(0) };
  },
  terminal(name) {
    return pair[name];
  },
  frame,
};

init().then(() => { window.ready = true; }, (err) => { window.ready = String(err); });
