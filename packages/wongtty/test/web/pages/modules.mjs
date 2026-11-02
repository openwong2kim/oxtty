// Page for scripted browser tests: loads wongtty/xterm, the engine and real xterm.js and exposes them on `window`
// (`ox`, `engine`, `xterm`, `WebglAddon`), plus `memories`, the linear memory of every WebAssembly instance init()
// created (the engine's and the front end's), for leak checks.
import * as ox from 'wongtty/xterm';
import * as engine from 'wongtty';
import { Terminal } from '@xterm/xterm';
import { WebglAddon } from '@xterm/addon-webgl';

const memories = [];
const keep = (result) => {
  const instance = result.instance ?? result;
  if (instance.exports?.memory) memories.push(instance.exports.memory);
  return result;
};
for (const name of ['instantiate', 'instantiateStreaming']) {
  const original = WebAssembly[name];
  WebAssembly[name] = (...args) => original.apply(WebAssembly, args).then(keep);
}

Object.assign(window, { ox, engine, xterm: { Terminal }, WebglAddon, memories });
ox.init().then(() => { window.ready = true; }, (err) => { window.ready = String(err?.stack ?? err); });
