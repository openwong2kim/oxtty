# Third-party notices for the `oxtty` npm package

`dist/oxtty.wasm` is compiled from oxtty and the third-party code below, and
`dist/xterm_web_bg.wasm` (`oxtty/xterm`) contains code ported from xterm.js
(last section). oxtty's own code is licensed under Apache-2.0
(`LICENSE-APACHE`).

---

## alacritty_terminal 0.26.0

Copyright Christian Duerr, Joe Wilm and the Alacritty contributors.
Source: https://github.com/alacritty/alacritty

Licensed under the Apache License, Version 2.0. A copy of the license is
included in this package as `LICENSE-APACHE`.

**Modified.** oxtty compiles a patched copy (`crates/oxtty/src/vt` in
https://github.com/openwong2kim/oxtty). The changes are:

- character widths use the xterm.js Unicode 11 table (below);
- resizing can leave the cursor paragraph without reflow (xterm.js
  `reflowCursorLine: false`), which oxtty enables;
- SGR 5 blink is stored in a cell flag;
- the PTY and event-loop modules and their dependencies are removed.

The full list is in `crates/oxtty/src/vt/PATCHES.md` in the repository.

---

## vte 0.15.0

Copyright the Alacritty contributors. Source: https://github.com/alacritty/vte (0.15.0 from crates.io, git 3b3da71c34cc1256c7e20981cf03f8eb95e08ffc)
Licensed under Apache-2.0 OR MIT; used here under the Apache License 2.0
(`LICENSE-APACHE`).

---

## dlmalloc 0.2.14

Copyright Alex Crichton; a port of Doug Lea's public-domain dlmalloc.
Source: https://github.com/alexcrichton/dlmalloc-rs
Licensed under Apache-2.0 OR MIT; used here under the Apache License 2.0
(`LICENSE-APACHE`).

---

## xterm.js Unicode 11 width table

The character width table compiled into `oxtty.wasm` is generated from the
xterm.js Unicode 11 addon. Source: https://github.com/xtermjs/xterm.js

```
Copyright (c) 2019, The xterm.js authors (https://github.com/xtermjs/xterm.js)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```

---

## xterm.js 6.1.0-beta.336 — input encoding

The keyboard, mouse, wheel, paste and focus encoders of oxtty
(`crates/oxtty/src/input` in https://github.com/openwong2kim/oxtty), compiled
into the WebAssembly modules that use them, are a Rust port of xterm.js
6.1.0-beta.336 (commit 08829c7eb6279dda2a9e86df901353a1a89858ca):
`src/common/input/Keyboard.ts`, `src/common/input/KittyKeyboard.ts`,
`src/common/services/MouseStateService.ts` and parts of
`src/browser/services/MouseService.ts`, `src/browser/Clipboard.ts` and
`src/browser/CoreBrowserTerminal.ts`. The textarea, IME and mouse handling
of the browser front end (`crates/oxtty-web/src/input`, compiled into
`xterm_web_bg.wasm`) is a Rust port of parts of
`src/browser/CoreBrowserTerminal.ts`, `src/browser/input/CompositionHelper.ts`,
`src/browser/input/MoveToCell.ts`, `src/browser/input/Mouse.ts`,
`src/browser/Clipboard.ts`, `src/browser/services/KeyboardService.ts`,
`src/browser/services/MouseService.ts`,
`src/browser/services/MouseCoordsService.ts` and
`src/browser/services/SelectionService.ts` of the same version. Each Rust file
names its source lines.
The parser handler identifiers of the browser front end
(`crates/oxtty-web/src/parser.rs`) are checked as in
`src/common/parser/EscapeSequenceParser.ts` of the same version.
Source: https://github.com/xtermjs/xterm.js

```
Copyright (c) 2017-2019, The xterm.js authors (https://github.com/xtermjs/xterm.js)
Copyright (c) 2014-2016, SourceLair Private Company (https://www.sourcelair.com)
Copyright (c) 2012-2013, Christopher Jeffrey (https://github.com/chjj/)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```

---

## xterm.js 6.0.0

`dist/xterm_web_bg.wasm` (`oxtty/xterm`) contains code ported to Rust in
`crates/oxtty-web` from xterm.js 6.0.0 (commit
f447274f430fd22513f6adbf9862d19524471c04) and its WebGL addon: the renderer's
texture atlas, glyph and rectangle renderers, colour resolver, custom glyphs,
cursor blink, colour helpers and theme defaults; and its fit from
`@xterm/addon-fit` 0.12.0-beta.332. Each source file in the repository names
the xterm.js files and line ranges it is ported from. Source:
https://github.com/xtermjs/xterm.js

```
Copyright (c) 2017-2019, The xterm.js authors (https://github.com/xtermjs/xterm.js)
Copyright (c) 2014-2016, SourceLair Private Company (https://www.sourcelair.com)
Copyright (c) 2012-2013, Christopher Jeffrey (https://github.com/chjj/)

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```

---

## xterm.js @xterm/addon-search 0.17.0-beta.330

The built-in `SearchAddon` of `dist/xterm_web_bg.wasm` (`crates/oxtty-web/src/search.rs`
in https://github.com/openwong2kim/oxtty) is a Rust port of `@xterm/addon-search`
0.17.0-beta.330 (xterm.js commit 08829c7eb6279dda2a9e86df901353a1a89858ca):
`addons/addon-search/src/SearchAddon.ts`, `SearchEngine.ts`, `SearchState.ts` and
`SearchResultTracker.ts`, Copyright (c) 2017 The xterm.js authors, under the MIT
license reproduced in the input encoding section above.
Source: https://github.com/xtermjs/xterm.js

---

## xterm.js 6.1.0-beta.336 — selection

The selection of `dist/xterm_web_bg.wasm` (`crates/oxtty-web/src/selection` in
https://github.com/openwong2kim/oxtty) is a Rust port of xterm.js 6.1.0-beta.336
(commit 08829c7eb6279dda2a9e86df901353a1a89858ca):
`src/browser/services/SelectionService.ts`,
`src/browser/selection/SelectionModel.ts` and parts of `src/browser/Clipboard.ts`,
`src/browser/CoreBrowserTerminal.ts`, `src/common/buffer/Buffer.ts` and
`src/common/buffer/BufferRange.ts`, Copyright (c) 2014-2021 The xterm.js
authors, under the MIT license reproduced in the input encoding section above.
Each Rust file names its source lines.
Source: https://github.com/xtermjs/xterm.js

---

## xterm.js @xterm/addon-image 0.10.0-beta.331 and xterm-wasm-parts 0.4.1

Inline images in `dist/xterm_web_bg.wasm` (`crates/oxtty-web/src/image`,
`crates/oxtty-web/src/render/images.rs`, `crates/oxtty/src/images.rs` and
`crates/oxtty/src/graphics/iip.rs` in https://github.com/openwong2kim/oxtty) are a
Rust port of `@xterm/addon-image` 0.10.0-beta.331 (xterm.js commit
08829c7eb6279dda2a9e86df901353a1a89858ca): `addons/addon-image/src/ImageAddon.ts`,
`SixelHandler.ts`, `IIPHandler.ts`, `IIPHeaderParser.ts`, `IIPMetrics.ts`,
`ImageStorage.ts`, `ImageRenderer.ts`, `SixelImageStorage.ts` and
`IIPImageStorage.ts`, Copyright (c) 2020-2023 The xterm.js authors, and of the
base64 rules of `xterm-wasm-parts` 0.4.1 (`lib/base64/Base64Decoder.wasm.js`,
Copyright (c) 2023-2026 The xterm.js authors), under the MIT license reproduced in
the input encoding section above.
Source: https://github.com/xtermjs/xterm.js

---

## xterm.js 6.1.0-beta.336 and @xterm/addon-web-links 0.13.0-beta.330 — links

The links and the built-in `WebLinksAddon` of `dist/xterm_web_bg.wasm`
(`crates/oxtty-web/src/links.rs` in https://github.com/openwong2kim/oxtty) are a
Rust port of xterm.js 6.1.0-beta.336 and `@xterm/addon-web-links` 0.13.0-beta.330
(commit 08829c7eb6279dda2a9e86df901353a1a89858ca): `src/browser/Linkifier.ts`,
`src/browser/OscLinkProvider.ts`, `src/browser/services/LinkProviderService.ts`,
`addons/addon-web-links/src/WebLinksAddon.ts` and `WebLinkProvider.ts`,
Copyright (c) 2019-2022 The xterm.js authors, under the MIT license reproduced in
the input encoding section above. The file names its source lines.
Source: https://github.com/xtermjs/xterm.js

---

## xterm.js 6.1.0-beta.336 — accessibility and scrollbar

`screenReaderMode` and the scrollbar of `dist/xterm_web_bg.wasm`
(`crates/oxtty-web/src/a11y.rs` and `scrollbar.rs` in
https://github.com/openwong2kim/oxtty, with their rules from `css/xterm.css`)
are a Rust port of xterm.js 6.1.0-beta.336 (commit
08829c7eb6279dda2a9e86df901353a1a89858ca): `src/browser/AccessibilityManager.ts`,
`TimeBasedDebouncer.ts`, `LocalizableStrings.ts`, `Viewport.ts` and
`src/browser/scrollable/*`, Copyright (c) 2017-2024 The xterm.js authors; the
scrollable files derive from VS Code, Copyright (c) Microsoft Corporation. Both
under the MIT license reproduced in the input encoding section above.
Source: https://github.com/xtermjs/xterm.js

---

## sixel 0.16.0

The sixel decoder of `dist/xterm_web_bg.wasm` (`crates/oxtty/src/graphics/sixel.rs`)
is a Rust port of the decoder of the `sixel` package 0.16.0 by Joerg Breitbart
(https://github.com/jerch/node-sixel, commit 5cd50615e29f54b956188ec2a12d999add9099be):
`wasm/decoder.cpp`, `lib/Decoder.js` and `lib/Colors.js`.

```
Copyright (c) 2019, Joerg Breitbart

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
```
