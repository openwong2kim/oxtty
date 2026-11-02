// A page without WebGL2 (a blocklisted GPU): canvases hand out no webgl2 context. Opens two wongtty/xterm terminals
// and counts the console warnings they give.
import { init, Terminal } from 'wongtty/xterm';

const getContext = HTMLCanvasElement.prototype.getContext;
HTMLCanvasElement.prototype.getContext = function (type, ...rest) {
  return type === 'webgl2' ? null : getContext.call(this, type, ...rest);
};
if (globalThis.OffscreenCanvas) OffscreenCanvas.prototype.getContext = () => null;
window.warnings = [];
const warn = console.warn;
console.warn = (...args) => {
  window.warnings.push(args.map(String).join(' '));
  warn.apply(console, args);
};

await init();
window.terms = [];
window.errors = [];
for (let i = 0; i < 2; i++) {
  const term = new Terminal({ cols: 20, rows: 4 });
  try {
    term.open(document.getElementById('terminal').appendChild(document.createElement('div')));
  } catch (e) {
    window.errors.push(String(e));
  }
  window.terms.push(term);
}
window.ready = true;
