// Page for the renderer tests: the kit as `window.T` for page.evaluate.
import { init, Terminal, FitAddon, WebglAddon } from 'wongtty/xterm';
import { Pane, diff, read, sleep } from './kit.mjs';

await init();
window.T = { Pane, Terminal, FitAddon, WebglAddon, diff, read, sleep };
window.ready = true;
