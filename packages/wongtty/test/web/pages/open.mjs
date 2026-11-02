// Opens an wongtty/xterm terminal and exposes it as window.term once ready.
import { init, Terminal } from 'wongtty/xterm';

await init();
const term = new Terminal({ cols: 40, rows: 8 });
term.open(document.getElementById('terminal'));
window.term = term;
window.ready = true;
