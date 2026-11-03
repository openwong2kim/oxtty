# Memory bench: many terminals on one page

Compares the memory of `wongtty/xterm` with xterm.js (`@xterm/xterm` and
`@xterm/addon-webgl`) when many terminals are open at once, the case wongtty is
built for: 17 panes of 120×40, each with 10,000 lines of scrollback filled by
coloured log output.

```sh
bash scripts/build-wasm.sh && bash scripts/build-web.sh   # repository root
cd packages/wongtty && npm ci
npm run bench:memory                # 17 panes, 10,000 lines each
npm run bench:memory -- 8 2000      # [panes] [lines]
npm run bench:memory -- 17 10000 wongtty,viewport   # [kinds]: wongtty, viewport, xterm
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
wongtty  canvases=18 | JS heap 3 MB  | renderer +123 MB | GPU process +21 MB
xterm    canvases=35 | JS heap 41 MB | renderer +413 MB | GPU process +48 MB
```

RSS moves by a few MB between runs, the GPU process the most.

The idle phase (2026-11-02, same machine and corpus):

```
idle 10 s with the first terminal focused
wongtty  blink=on  | GPU process footprint 216 MB | WebGL draws 0, drawImage 0
wongtty  blink=off | GPU process footprint 217 MB | WebGL draws 0, drawImage 0
xterm    blink=on  | GPU process footprint 437 MB | WebGL draws 43, drawImage 0
xterm    blink=off | GPU process footprint 315 MB | WebGL draws 0, drawImage 0
```

The footprint is bimodal from run to run (wongtty reads about 216 or about
250 MB with blink on or off alike). Before the cursor layer, a blinking wongtty
pane redrew through WebGL every 600 ms (60 draw calls and 17 `drawImage` in
10 s) and the GPU process stayed at 366 MB.

After canvas compaction (below), the same idle phase on a loaded machine
(2026-11-02, one run): wongtty 167 MB with blink on, 157 MB off; xterm.js 403
and 313 MB. The focused wongtty pane now makes one draw (3 WebGL draw calls, one
`drawImage`) in the window: the compaction a second after its last frame.

## Breakdown

```sh
node bench/memory/breakdown.mjs [panes,...] [lines] [dpr,...]   # defaults: 1,17 10000 1,2
```

For each pane count and devicePixelRatio it opens a fresh browser like
`bench.mjs` and reports, after a GC: the physical footprint (`footprint`) of
the renderer and of the GPU process over the empty page, with the region
categories that grew; the WebAssembly memory of the engine (`wongtty.wasm`) and
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
changes in `crates/wongtty-web/src/render/webgl.rs` cut that:

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

## Busy

```sh
node bench/memory/busy.mjs [panes] [seconds] [dpr] [interval]   # defaults: 17 12 1 50
MIXED=1 node bench/memory/busy.mjs                               # sizes from 92×30 to 120×40
TYPING=1 node bench/memory/busy.mjs                              # the first pane focused, typed in, not printing
```

With `TYPING=1` the first terminal takes focus and gets one echoed character
every 100 ms instead of output, and the echo latency (write to the frame that
shows it) is reported.

Every terminal gets three lines of output every `interval` ms, as agents
printing; near the end of the window the GPU process's footprint is read, and
the page counts WebGL draw calls, `drawImage` calls and animation frames. Then
the footprint again after 5 s of idling.

Since 2026-11-03 `busy.mjs` sets the dpr as a real screen scale factor of a
4500×4500 window and reads the empty page after 6 s (a fresh GPU process drops
its startup surfaces about 5 s after launch); `SCALE=emulate` gives the old
page emulation. The table below and the breakdown tables above were taken with
emulated dpr and a 1 s settle. Under emulation ResizeObserver's
`device-pixel-content-box` reports CSS pixels, so xterm.js's addon-webgl draws
at dpr 1 while `devicePixelRatio` says 2 (7.1 Mpx of drawing buffers for 15
panes instead of 28.3): emulated dpr-2 xterm.js figures under-count it.
wongtty sizes by `devicePixelRatio` and draws at full size either way, but a
real scale factor also enlarges the window's own surfaces, so absolute dpr-2
figures from the two modes do not compare. `breakdown.mjs` still emulates.

17 panes of 120×40, dpr 1, 12 s (Apple M4, loaded machine, 2026-11-02; GPU
process footprint while busy, three runs, and over the empty page):

```
                     footprint      over empty     draws/frame  drawImage/frame
wongtty before       457 462 440    270 232 208    22.6         5.7
wongtty after        438 462 461    218 274 233    16.9         5.6
xterm.js             407 409 409    189 190 182    16.9         0
wongtty, mixed sizes 399 382 391    180 194 165    16.9–21.3    5.6–7.1
xterm.js, mixed      384 399        154 171        17.0         0
```

A pane frame made four WebGL draw calls (backgrounds, glyphs, the highlight
overlay and the cursor), the overlay and often the cursor with nothing in
them; empty rectangle layers are now skipped, three calls like xterm.js. The
memory did not move. What the busy wongtty page holds over xterm.js is
`IOAccelerator (graphics)`, Chromium's working set for drawing into 2D
canvases every frame; it is gone 5 s after the output stops (wongtty 173–186 MB
then, xterm.js about 270 MB). Measured with probes:

- Without the copy into the pane canvases, the busy page is +61 MB over the
  empty page; the WebGL drawing alone costs little.
- 17 plain 2D canvases filled every frame (no WebGL) take +66 MB of
  `IOAccelerator (graphics)`; `copy` or `source-over` makes no difference.
- `bitmaprenderer` canvases fed by `transferToImageBitmap` halve the busy cost
  (+113 to +150 MB) but freeze the page when the WebGL context is lost (the
  context-restore test hangs right after `loseContext`), as `gpu.rs` notes.
  `transferToImageBitmap` + `drawImage` + `close` did not measurably help.
- Keeping one surface sized for the largest pane, instead of resizing it at
  every draw of a pane of another size, kept the frame rate steady with mixed
  sizes but held about 55 MB more while busy (the copy snapshots the whole
  surface), so the surface still takes each pane's size.

Two more ways to present a pane were tried and not kept (2026-11-02, Apple M4,
17 panes, three runs; patches kept out of the tree):

- **Every pane through `bitmaprenderer` fed by `createImageBitmap`** (a
  snapshot of the shared `OffscreenCanvas`, shown a task later; no freeze on
  context loss, the context tests passed five times in a row). Busy: +87, +121
  and +91 MB over the empty page against xterm.js's +182 to +190. But every
  frame reaches the screen one frame later (snapshots arrived 0.3–0.6 ms after
  the draw, after that frame's paint), and hidden panes kept +177 to +194 MB at
  dpr 2: Chromium frees what a `bitmaprenderer` canvas showed only when the
  element goes (`transferFromImageBitmap(null)` and a zero size keep it).
- **Hybrid**: the focused pane copied as before, the others through a second,
  `bitmaprenderer` canvas, swapped without a blank frame, the bitmap canvas
  dropped while hidden and once the pane idles. With one pane typed in: busy
  +151, +169 and +113 MB against xterm.js's +210, +194 and +242; echo median
  14.3–18.7 ms against 15.8–21.6 ms before. But once snapshots had been used,
  Chromium kept about 80 MB more in the GPU process for the rest of the page's
  life (+81 to +94 MB after every terminal was disposed, against +11 to +38
  without them), so idle panes read +118 to +124 MB at dpr 1 (+90 to +92
  before) and hidden ones +26 to +62 MB (+17 to +77 before).

## Atlas mipmaps

The atlas pages were uploaded with `generateMipmap`, rebuilt at every upload;
glyphs are drawn at the atlas's own scale, so the pages now upload with
`LINEAR` filtering and no mipmaps, as @xterm/addon-webgl 0.20 does. With 20
panes printing a different truecolor in every cell at agent speed
(`PANES=20 WORKLOADS=dense-cells MODES=agent npm run bench:speed`, 3 runs):
wongtty 46 fps, write→frame p95 42 ms before; 58 fps, p95 29 ms after; xterm.js
59 fps, p95 24 ms. A pane that adds glyphs every frame re-uploads the pages
it changed; rebuilding the mipmaps of those pages was most of the gap.
