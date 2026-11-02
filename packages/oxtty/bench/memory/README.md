# Memory bench: many terminals on one page

Compares the memory of `oxtty/xterm` with xterm.js (`@xterm/xterm` and
`@xterm/addon-webgl`) when many terminals are open at once, the case oxtty is
built for: 17 panes of 120×40, each with 10,000 lines of scrollback filled by
coloured log output.

```sh
bash scripts/build-wasm.sh && bash scripts/build-web.sh   # repository root
cd packages/oxtty && npm ci
npm run bench:memory                # 17 panes, 10,000 lines each
npm run bench:memory -- 8 2000      # [panes] [lines]
```

- It runs **system Chrome** through Playwright (`channel: 'chrome'`), headless
  but on the **real GPU** (`--use-angle=metal`, GPU blocklist ignored), so the
  numbers include the WebGL renderers. The `gpu=` field shows which GPU the
  page got; if it names SwiftShader, the GPU numbers are not meaningful. The
  flags are for macOS; elsewhere, change `--use-angle`.
- Each engine gets a fresh browser. The JS heap is read after a GC; the
  renderer and GPU figures are the resident memory those processes gained over
  the empty page (`ps` RSS per process from CDP `SystemInfo.getProcessInfo`).
- All panes are in view (a 4500×4500 viewport), so both engines draw all of
  them.
- Then each engine runs once more for the **idle phase**, with `cursorBlink`
  on and off: the first terminal takes focus, and after 10 s the GPU process's
  physical footprint is read with macOS `footprint` (RSS does not show the
  memory Chromium marks purgeable once nothing redraws), with the WebGL draw
  calls and canvas `drawImage` calls of those 10 s, counted in the page.
- It is not part of `npm test` or `npm run test:web`.

Results on an Apple M4 (macOS, 2026-11-02):

```
17 panes of 120×40, 10000 lines each
oxtty  canvases=18 | JS heap 3 MB  | renderer +123 MB | GPU process +21 MB
xterm  canvases=35 | JS heap 41 MB | renderer +413 MB | GPU process +48 MB
```

RSS moves by a few MB between runs, the GPU process the most.

The idle phase (2026-11-02, same machine and corpus):

```
idle 10 s with the first terminal focused
oxtty  blink=on  | GPU process footprint 216 MB | WebGL draws 0, drawImage 0
oxtty  blink=off | GPU process footprint 217 MB | WebGL draws 0, drawImage 0
xterm  blink=on  | GPU process footprint 437 MB | WebGL draws 43, drawImage 0
xterm  blink=off | GPU process footprint 315 MB | WebGL draws 0, drawImage 0
```

The footprint is bimodal from run to run (oxtty reads about 216 or about
250 MB with blink on or off alike). Before the cursor layer, a blinking oxtty
pane redrew through WebGL every 600 ms (60 draw calls and 17 `drawImage` in
10 s) and the GPU process stayed at 366 MB.
