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

After canvas compaction (below), the same idle phase on a loaded machine
(2026-11-02, one run): oxtty 167 MB with blink on, 157 MB off; xterm.js 403
and 313 MB. The focused oxtty pane now makes one draw (3 WebGL draw calls, one
`drawImage`) in the window: the compaction a second after its last frame.

## Breakdown

```sh
node bench/memory/breakdown.mjs [panes,...] [lines] [dpr,...]   # defaults: 1,17 10000 1,2
```

For each pane count and devicePixelRatio it opens a fresh browser like
`bench.mjs` and reports, after a GC: the physical footprint (`footprint`) of
the renderer and of the GPU process over the empty page, with the region
categories that grew; the WebAssembly memory of the engine (`oxtty.wasm`) and
of the front end (`xterm_web_bg.wasm`); the JS heap; DOM nodes and listeners
(CDP `Memory.getDOMCounters`); and canvas backing stores (width × height × 4).
Then the footprints again with every terminal `display: none`, and after every
terminal is disposed. With two pane counts it prints the per-pane and fixed
costs.

What 120×40 panes with 10,000 lines of scrollback cost (Apple M4,
2026-11-02, 1 and 17 panes):

| | fixed | per pane |
|---|---|---|
| Renderer process footprint | ~12 MB | ~4.5 MB |
| · engine wasm memory | 1.1 MB | 4.0 MB (69 MB at 17 panes) |
| · front-end wasm memory | 1.9 MB | 0.35 MB (7.8 MB at 17) |
| · JS heap | 2.8 MB | ~0 |
| · DOM | 21 nodes | 12 nodes, 27 listeners |
| · glyph atlas (one 512² page, 2D canvas) | 1 MB | 0 |
| Shared WebGL surface (`OffscreenCanvas`) | 1.8 MB (dpr 1), 7.2 MB (dpr 2) | 0 |
| Pane canvas pixels | 0 | 1.8 MB (dpr 1), 7.2 MB (dpr 2) |
| GPU process footprint, before compaction | | 9–14 MB (dpr 1), 25 MB (dpr 2) |

Neither the canvases nor the glyph atlas count in the renderer: Chromium keeps
accelerated canvas pixels in the GPU process (IOSurfaces on macOS). The
renderer's per-pane cost is the engine's wasm memory, which only grows, so
disposing every terminal gives back about 3 MB of the renderer's footprint.

Chromium kept two to four GPU buffers of a pane canvas's size for every pane
that drew in consecutive frames, and kept them while the pane idled. Two
changes in `crates/oxtty-web/src/render/webgl.rs` cut that:

- **Compaction.** A second after a pane's last frame, its canvas is resized to
  nothing and back and the same frame drawn again in the same task: the page
  never paints the empty canvas, and Chromium is left with one buffer.
- **Collapse.** A pane whose container takes no room (`display: none`) has no
  canvas pixels. A `ResizeObserver`, which runs before the paint, gives them
  back with the last frame when it takes room again.

GPU process footprint over the empty page, three runs each (machine loaded by
other builds), with the renderer process for comparison:

```
                    GPU process                       renderer
                    before         after              before         after
17 panes, dpr 1     156 221 173    92  90  92         90 92 91       91 92 99
17 panes, dpr 2     504 441 473    153 214 240        91 90 102      91 93 94
20 panes, dpr 1     136 173 200    58  97  98         131 102 111    125 108 103
20 panes, dpr 2     472 549 512    218 221 214        107 113 104    106 106 106
17 hidden, dpr 1    117 136 121    21  17  -19
17 hidden, dpr 2    490 401 379    22  49  77
20 hidden, dpr 2    472 534 470    33  -9  29
```
