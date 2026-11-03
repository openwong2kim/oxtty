# wongtty

A terminal engine in Rust, compiled to WebAssembly, plus `wongtty/xterm`: the
xterm.js 6.x `Terminal` API over that engine, with a WebGL2 renderer, also in
Rust. Built to keep memory low with many terminals open: all panes on a page
share one engine module, one WebGL2 context and one glyph atlas.

**Status: 0.0.2, experimental.**

## Install

```sh
npm install wongtty
```

To build it from the repository instead (Rust, the `wasm-bindgen` CLI and
Node ≥ 18):

```sh
git clone https://github.com/openwong2kim/wongtty && cd wongtty
bash scripts/build-wasm.sh && bash scripts/build-web.sh
npm install ./packages/wongtty    # from your app
```

## `wongtty/xterm`: the xterm.js API

```js
import { init, Terminal, FitAddon } from 'wongtty/xterm';

await init();                    // once per page: loads wongtty.wasm and xterm_web_bg.wasm
const term = new Terminal({ fontFamily: 'Menlo, monospace', fontSize: 14 });
const fit = new FitAddon();
term.loadAddon(fit);
term.open(document.getElementById('terminal'));
fit.fit();
pty.onData((data) => term.write(data));
term.onData((data) => pty.write(data));
```

Coming from xterm.js: `await init()` once, import `FitAddon`, `SearchAddon`,
`WebLinksAddon`, `ImageAddon` and `WebglAddon` from `wongtty/xterm` instead of
`@xterm/addon-*`, and drop the `xterm.css` import. `loadAddon` absorbs
`@xterm/addon-webgl`, `@xterm/addon-unicode11` and `@xterm/addon-image`, and
throws for other addons. WebGL2 is required to draw. Browser only.

## `wongtty`: the engine, your own renderer

```js
import { init, Terminal } from 'wongtty';

await init();
const term = new Terminal({ cols: 80, rows: 24, scrollback: 5000 });
term.write('안녕 \x1b[1;31mworld\x1b[0m\r\n');

term.rowText(0);       // '안녕 world'
term.rowCells(0);      // Uint32Array: [codepoint, fg, bg, attrs] per cell
term.takeDamage();     // { full: true, lines: [] } → then only changed rows
term.takeReplies();    // bytes to write back to the PTY
```

Visible rows are `0..rows`; scrollback is `-1` (newest) to `-historyLength`.
Works in browsers, bundlers (Vite, webpack) and Node ≥ 18, with no
dependencies.

The xterm.js API support table, known differences, measurements and the Rust
crate: https://github.com/openwong2kim/wongtty

License: Apache-2.0. Includes `alacritty_terminal` (Apache-2.0) and code ported
from xterm.js and node-sixel (MIT); see `THIRD_PARTY_NOTICES.md`.
