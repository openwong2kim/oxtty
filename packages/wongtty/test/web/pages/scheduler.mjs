// The write scheduler's page: exposes the Terminal class once both modules are loaded.
import { init, Terminal } from 'wongtty/xterm';

await init();
window.Terminal = Terminal;
window.ready = true;
