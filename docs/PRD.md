# oxtty PRD — a Rust-based xterm (terminal engine)

| Item | Details |
| --- | --- |
| Document version | v0.5 |
| Written | 2026-10-31 (v0.1·v0.2·v0.3), 2026-11-01 (v0.4), 2026-11-02 (v0.5) |
| Repository | github.com/openwong2kim/oxtty (private) |
| Current release | 0.0.1 — engine core, wasm, N-API, tests (not published to registries) |
| First customer | wmux |

### v0.5 change summary

- **Status sync with the code (2026-11-02)**: M2 addons done (search with its matching in JS, serialize as `ansiSnapshot`, images in Rust) and the API checklist complete with the README support table; graphics non-goal narrowed to kitty graphics; snapshot, WebGL context and size rows brought up to date; the license row names the ported xterm.js input encoders.

### v0.4 change summary

- **D3 replaced (owner decision, 2026-11-01)**: the web front end is rewritten in Rust and compiled to wasm; the TypeScript facade with borrowed xterm.js parts (`packages/oxtty/xterm`, last at `0510a7d`) and the D3 spike are removed. Only generated JS glue and the JS harnesses that compare against xterm.js remain. The parity it reached (keys, IME, mouse, selection, WebGL context loss) is the bar the Rust front end must meet. Engineering review: GO with conditions, applied as follows.
  - **Two WebAssembly modules.** The engine (`oxtty.wasm`, raw ABI, no imports, ≤300 KB, its hand-written loader `index.js` kept by owner decision) and the front end (`crates/oxtty-web`, wasm-bindgen, `xterm_web_bg.wasm`, ≤600 KB enforced by `scripts/build-web.sh`). The front end reaches the engine through `index.js`, so an engine crash still rebuilds only the engine instance and the page keeps running; `oxtty/xterm`'s `init()` loads both.
  - **D7 kept**: rendering, input, selection and accessibility read only `Events`/`Frame`/`Viewport` (`crates/oxtty-web/src/engine.rs`).
  - **Rules** (crate docs of `oxtty-web`): no JS calls while a `RefCell` is borrowed (listeners may write, resize or dispose), front-end panics are bugs (`panic = "abort"` stops every terminal on the page), no `Closure::forget`, only macrotask timers, borrowed xterm.js logic credited per file.
  - **Renderer**: raw WebGL2 through web-sys, glyphs rasterised with Canvas2D `fillText` (same font fallback as xterm.js). For the memory goal it shares **one WebGL2 context and one glyph atlas per page** across all panes (not one per pane as xterm.js and the old front end did), so 17+ panes neither hit Chrome's ~16-context limit nor multiply GPU memory; hidden panes skip drawing.
  - **Input encoding moves into the engine crate** (`crates/oxtty/src/input`, platform-neutral `KeyInput`/`MouseInput`), pulling M3 forward.
  - The xterm.js parity harnesses come back as JS tests against the new build (`packages/oxtty/test/web`).
- **Images in Rust (owner decision, 2026-11-01; answers §10.2 question 1)**: wmux uses `@xterm/addon-image`, so oxtty implements what it supports (sixel and the iTerm2 inline image protocol, OSC 1337) instead of leaving images on the xterm.js path; graphics move from "after 1.0" into the wmux adoption scope. Image memory counts against the primary goal: bounded storage per page, images freed when their cells scroll out or are overwritten.

### v0.3 change summary

- **One name**: the public name is `oxtty` alone. The Alacritty fork is embedded as the `vt` module inside the `oxtty` crate instead of a separate crate (`oxtty-vt`), and the web front end ships as the `oxtty/xterm` subpath.
- **Targets reassessed**: Orca uses the same xterm.js stack as wmux (§2.2). herdr and t3code have already chosen libghostty-vt. The competitor assessment was corrected to reflect real-world usage.

### v0.2 change summary

- **D3 changed**: xterm.js fork → a thin in-house front end + borrowed xterm.js parts. Confirmed by an M1 spike; if it fails, fall back to the fork.
- **D8 added**: develop in the oxtty repository, and wmux uses it as a git dependency from the start. No "build it inside wmux and split it out later".
- **Milestone order**: the web front end and wmux renderer swap (M2) moves ahead of input encoding (M3).
- **M1 additions**: unify the Alacritty fork in wmux-term, fill the engine API gaps M2 needs, the D3 validation spike, better scroll damage.
- **Release gate replaced**: after the swap, shadow comparison verifies nothing, so it is replaced by differential tests and wmux e2e.
- **Factual corrections**: not published to registries, libghostty license (MIT), the memory share being based on a peak sample, the location of the wasm32 patch.
- **Open questions**: v0.1 questions 1–5 were decided and moved to the §10 decision log. Newly raised questions remain.

---

## 1. One-line definition

**oxtty is a terminal engine that replaces the "insides" of xterm.js with Rust.** It interprets the bytes a program writes to the PTY, holds the screen state, and lets any renderer fetch only the changed lines to draw. The same engine runs identically in a daemon (native), in browser and Electron renderers (WebAssembly), and in future native apps.

xterm.js splits into an **engine layer** (`common`: bytes → screen state) and a **screen layer** (`browser`: drawing, key input, IME, selection, accessibility). `@xterm/headless` is the package that runs only the engine layer in Node. oxtty replaces the engine layer with Rust and builds a new, thin screen layer (§5 D3).

---

## 2. Background and problem

### 2.1 The problem inside wmux: two engines

- The wmux daemon (Node/TypeScript) can use a Rust engine (`core/wmux-term`, based on Alacritty 0.26) over N-API when building snapshots. The default is `@xterm/headless`, and the change that makes Rust the Windows default (#2806) is on a separate branch. Whether it has shipped in a release needs checking.
- The screen (Electron renderer) is entirely the xterm.js 6.1 beta.
- Because there are two engines, a separate shadow comparison harness and fixtures are maintained to guarantee "same bytes, same screen". It has been brought to text 31/31, ANSI 52 matches + 1 intentional rejection.
- When the daemon snapshot and the screen diverge, core wmux features such as mobile approval and reconnect restore become unreliable.
- There are already two copies of the Alacritty fork. `wmux-term/vendor` and `oxtty/vendor` differ in 3 files (`Cargo.toml`, `src/lib.rs`, `WMUX-PATCH.md`), and the wasm32 build patch exists only in oxtty.

### 2.2 The gap in the market

- `@xterm/xterm` was downloaded **8,608,088** times on npm last week (2026-10-23~29). It is the de facto standard for web terminals.
- A move toward "keep the xterm.js API, swap the insides for a WASM core" has already started. None of the three forks xterm.js; all use their own renderer.
  - ghostty-web (Coder, Zig): **314,582** downloads in the same period, aims for xterm.js API compatibility
  - vercel-labs/wterm (Zig): DOM renderer, its own API
  - riotermjs (Rust): 0.1.8, early stage
- Engine choices of agent terminal apps (2026-10-31, checked in each repository's dependency files)
  - **Orca** (stablyai, ★111k, MIT, Electron): xterm.js `6.1.0-beta.304` + `@xterm/headless` + serialize — the same versions as wmux. Once oxtty works in wmux it can be attached almost as is, making it the top external target.
  - **herdr** (★47k, Apache-2.0, Rust TUI): vendors libghostty-vt into its repository and uses it over FFI.
  - **t3code** (pingdotgg, ★33k, MIT, Electron + web): builds libghostty to wasm itself.
  - So the real competitor is libghostty-vt. oxtty targets the spot where "apps that already use xterm.js" (wmux, Orca) move over by swapping an import.
- **There is no mature, xterm.js-compatible engine written in Rust yet.** That is the spot oxtty aims for.

### 2.3 Long-term direction

- When wmux moves off Electron to a native Rust app, it will need a terminal engine anyway. oxtty is groundwork for that path.
- **Primary goal (owner decision, 2026-11-01): minimal memory with many terminals at once.** wmux fans out 17 terminals in parallel, and the terminal side must not blow up RAM: per-pane cost (cells, scrollback, WebGL context, glyph atlas) is the first design criterion, measured as the memory of 17 concurrent panes against xterm.js. Correctness (one engine) and platform independence come with it. For scale: in a single peak sample the agent processes were about 82% of wmux's memory (2,309 / 2,819 MiB), so the terminal's share is what oxtty can cut.

---

## 3. Goals and non-goals

### 3.1 Goals

| ID | Goal | Criterion |
| --- | --- | --- |
| G1 | **One engine** — daemon, web and native use the same core | wmux-term depends on `oxtty` (M1), wmux's daemon and screen use the same oxtty build (M2) |
| G2 | **xterm.js-compatible front end** — migrating is as simple as swapping an import | Passes the defined API checklist (§6 M2) |
| G3 | **Adopted in wmux** — swap the renderer engine to oxtty | Before the swap: 0 differential mismatches against `@xterm/headless`. After the swap: 0 regressions in wmux e2e, IME and screen snapshots |
| G4 | **Logic in Rust** — down to input encoding, selection, search and links | Platform-specific code is only IME, drawing and OS input |
| G5 | **Adopted as open source** — used outside wmux too | External use cases after going public |

### 3.2 Non-goals

- **Building a terminal app** — windows, tabs, splits and settings UI are wmux's job.
- **Writing a VT parser from scratch** — use the Alacritty fork (§5 D1).
- **Forking all of xterm.js** — the screen layer borrows only the parts it needs (§5 D3).
- **Compatibility with the original xterm (X11, C)** — the reference is xterm.js behavior.
- **Kitty graphics before 1.0** — it comes after 1.0. Sixel and the iTerm2 inline image protocol are in M2 (v0.4 decision), because wmux uses `@xterm/addon-image`.
- **PTY and process management inside the engine** — the engine only receives bytes. If a PTY becomes necessary, it goes in a separate crate, `oxtty-pty`.

---

## 4. Users and use cases

| User | Use case | Delivery path |
| --- | --- | --- |
| **wmux** (first customer) | Daemon snapshots, Electron renderer, web and mobile clients on one engine | N-API + wasm, git dependency (D8) |
| **Agent terminals using xterm.js** (Orca etc.) | Solve the same "two engines" problem as wmux | npm `oxtty`, `oxtty/xterm` |
| **Web terminal developers** (xterm.js users) | Cloud IDEs, web SSH, AI agent UIs | npm `oxtty/xterm` |
| **Rust app developers** | Native terminals, TUI test tools, agent runtimes | crates.io |

---

## 5. Architecture

### 5.1 Layers

```
┌──────────────────────────────────────────────────────────────┐
│ Front end                                                    │
│  `oxtty/xterm` thin in-house web front + xterm.js-compat API │
│    (borrowed from xterm.js: WebGL glyph atlas,               │
│     IME composition, key input)                              │
│  Native renderer (wgpu + cosmic-text, after 1.0)             │
├──────────────────────────────────────────────────────────────┤
│ Bindings   npm `oxtty` (wasm)   │  N-API `oxtty-node`  │  Rust │
├──────────────────────────────────────────────────────────────┤
│ Engine `oxtty`  renderer-agnostic API: feed / rows / cells / │
│              damage / cursor / modes / replies / title /     │
│              bell / sync                                     │
│              + (M1) viewport, cursor shape, cell extras      │
│                (underline color, links)                      │
│              + (M1~) serialize, search / (M3~) input         │
│                encoding / (M4~) selection, link detection    │
├──────────────────────────────────────────────────────────────┤
│ Core `oxtty::vt`  embedded Alacritty 0.26 fork (vte + grid)  │
└──────────────────────────────────────────────────────────────┘
```

### 5.2 Design decisions

**D1. The core is an Alacritty fork** (`alacritty_terminal` 0.26, Apache-2.0)
- Why: it is Rust-native and already patched and validated. Unicode 11 width, resize and blink were patched in wmux; the wasm32 build was patched in oxtty 0.0.1.
- Alternatives considered: libghostty-vt is feature-rich and herdr and t3code actually use it, but it needs the Zig toolchain alongside and its C API makes the Rust and N-API bindings cumbersome. Re-validating the 91 fixtures (85 unique inputs) wmux aligned against Alacritty is also costly. Writing from scratch costs too much for its value.
- libghostty-vt serves as **a comparison target and a behavioral reference point**.
- Keep only one copy of the fork. Rather than shipping it as a separate crate, embed it as the `vt` module inside the `oxtty` crate. crates.io requires every dependency to be in the registry too, so embedding it leaves `oxtty` as the only public name. The 89 internal paths (`crate::`) are rewritten by a script, and the same script is rerun when Alacritty is upgraded.
- In M1, wmux-term switches from its vendored fork to depending on `oxtty` (D8).

**D2. Renderer-agnostic API**
- The engine exposes only "a list of changed lines + cell arrays". The web front end and the future native renderer use the same API.
- A cell is 4 u32s: `[codepoint, fg, bg, attrs]`. Colors carry their kind (named/256/RGB) in the high byte, and the renderer decides the theme.
- Underline color (SGR 58) and OSC 8 hyperlinks, which do not fit in 4 u32s, are exposed separately as a **per-row extras table** (column → underline color, link ID). The cell format is frozen after this decision (M1).
- Damage also reports the number of scrolled lines. Alacritty marks the whole screen as damaged on every scroll (`mark_fully_damaged`), so left as is, a pane streaming logs copies the whole screen every frame.

**D3. The web front end is a thin in-house front end + borrowed xterm.js parts** (replaced in v0.4: Rust front end)
- From xterm.js (MIT), take only the WebGL glyph atlas and render model, IME composition (`CompositionHelper`) and key input (`Keyboard.ts`), and adapt them to oxtty rows. Only the public API is made compatible with xterm.js.
- Why
  - A fork would have to emulate, on top of oxtty, the xterm.js internal interfaces the screen layer is tied to (`IBufferLine.loadCell`, `CellData`, `BufferService`), and follow every change in the 6.x beta. That is the heaviest cost for a solo developer.
  - Borrowed parts carry no obligation to track upstream, and their boundary can be designed around oxtty's "changed lines" API (fits naturally with D7).
  - ghostty-web, riotermjs and wterm, which solved the same problem, all avoided forking.
- Cost: detaching the borrowed modules from xterm.js internal services (once), implementing selection UI and accessibility ourselves, rewriting wmux's 19 xterm.js internal workarounds (in 4 files).
- Validation: M1 spike → **confirmed (2026-10-31, `spike/d3-webgl/README.md`)**. 22 files / 4,646 lines were copied from xterm.js 6.0.0 (`f447274`), 19 of them verbatim. 5 internal services came along, each using 1–2 members (112 lines of stubs in total). Nothing from the buffer side (BufferService, BufferLine, CellData) came along; the boundary cuts at RenderModel (4 u32s per cell). WebglRenderer was replaced by an in-house orchestrator (179 lines) and cell converter (108 lines).

**D4. Logic in Rust, a thin platform layer**
- Rust: keyboard, mouse and paste encoding, selection range computation, search, link detection, serialize
- Platform: IME (DOM textarea on the web, OS IME natively), glyph drawing, receiving OS input events

**D5. The engine has no clock and no threads**
- The synchronized output (`CSI ? 2026 h`) timeout is managed by the host. 0.0.1 confirmed that vte's default handler dies the moment it calls the clock in wasm. Since 2026-11-02 the engine applies the output at once as xterm.js does: `syncPending()` reports an open update (renderers hold their frames), `flushSync()` ends it, and the npm wrapper ends a stalled one after xterm.js's 1000 ms.
- Result: same input → same screen (deterministic), easy to test and reproduce.

**D6. The host decides scrollback ownership**
- wmux: the daemon holds the full scrollback, and the screen keeps only what is near the visible area. An Alacritty cell is 24 bytes, twice xterm.js's (12 bytes), so keeping everything on the screen side would make it heavier.
- Standalone web use: scrollback lives inside wasm (current default). 5000 lines × 80 columns is about 9.6 MB per pane, heavier than xterm.js (about 4.8 MB). This figure is published in the README, and compressed scrollback cell storage is to be considered before 1.0.

**D7. Allow parsing to move to a Web Worker**
- Like wterm, to free the main thread when many panes are busy. The engine API already returns only "changed lines", so crossing the Worker boundary is easy (M4).
- Contract (fixed in M2, now `crates/oxtty-web/src/engine.rs`): the renderer, input and selection read only `Events` (replies, title, bells, OSC 52 and the modes; taken after every write drain, not per frame, so a hidden page still answers status queries), `Frame` (changed rows, cursor and modes in viewport coordinates) and `Viewport` (a mirror of the visible rows). Only `buffer.active.getLine()`, search and serialize call the engine directly; a Worker build needs a main-thread mirror for those. Input encoding reads modes from the `Events` of the last parsed write, so a key right after the write that changed them is encoded as in xterm.js (2026-11-02; frames lagged one behind before). The focus report a program asks for with `?1004h` is not: the engine answers it in place from the focus the host gave it (`Engine::set_focus`).

**D8. Develop in the oxtty repository; wmux uses it as a dependency from the start**
- No "build it inside wmux and split it out later". Growing it inside mixes wmux policy into the engine. The daemon API (`textSnapshot`, `feedFrozen`) is already wmux-shaped.
- wmux depends on `oxtty` by git rev. Local development links directly with Cargo `[patch]` and `npm link`.
- wmux-specific code (snapshot format, frozen ring, packaging and signing) stays in wmux-term; only the engine moves to oxtty.
- wmux-specific behavior is exposed as engine options, with upstream behavior as the default. wmux picks the options that match its existing fixtures.

---

## 6. Functional requirements — milestones

### M0 · 0.0.1 engine core ✅ done (2026-10-31)

- Renderer-agnostic API: feed, resize, rowText, rowCells(packed), rowWrapped, cellText, cursor, modes, takeDamage, takeReplies, takeTitle, takeBells, syncPending/flushSync
- Scrollback uses negative line numbers (-1 is the most recent)
- npm package `oxtty` (wasm 177KB, **not published**), N-API binding, CI (Windows, macOS, Linux)
- Tests: Rust 15 + 1 doc example, npm 11, N-API 2
- **Known blocker:** cannot publish to crates.io — `cargo publish` fetches the original Alacritty and the build breaks (resolved in M1)

### M1 · 0.1 laying the foundation

| Requirement | Acceptance criteria |
| --- | --- |
| Reserve names: npm `oxtty`, crates.io `oxtty` | Registered in each registry |
| ✅ Embed the Alacritty fork as the `vt` module inside the `oxtty` crate (D1) | Reproducible with the path conversion script; `cargo test`, wasm build and `cargo publish --dry-run -p oxtty` pass — `d321607`, PTY and event loop modules removed |
| **wmux-term depends on `oxtty` instead of its vendored fork** (D8) | wmux's existing Rust tests and 91 fixtures (85 unique inputs) pass, wmux's vendored copy removed |
| ✅ Make the cursor paragraph reflow-preservation patch an option — it turned out to be the xterm.js default (`reflowCursorLine: false`), not wmux policy | `vt::term::Config::reflow_cursor_line` (default `true` = Alacritty behavior), `oxtty::Engine` keeps xterm.js behavior. 130 original Alacritty tests pass — `d321607` |
| ✅ ANSI serialize (port of wmux-term `native_serialize`) | After serialize→restore, the screen fingerprint matches the original — `Engine::ansi_snapshot`, JS/N-API `ansiSnapshot()` (`439cae5`·`0be743c`·`a077379`). The 60 wmux ANSI cases reproduce as 59 equal + `seed/14` rejected (wide edge), asserted per case |
| ✅ Differential test harness: feed the same input to oxtty and `@xterm/headless` and compare | Ported the 91 wmux fixtures (31 text + 60 ANSI, 85 unique inputs), all match or the difference is documented — `packages/oxtty/test/xterm.test.mjs`, `npm ci && npm test` (`2aaf6b4`·`cf6d63f`·`209e531`). 85 inputs: proof A (cells directly) 83 equal, 2 differ; proof B (xterm restore) 83 equal, 0 differ, 2 rejected (`seed/14`, `text/scroll-margins`). Every difference and rejection is in [known-differences.json](../packages/oxtty/test/xterm/known-differences.json) with a reason and `blocksSwap` |
| 🟡 API gaps for M2: viewport (scroll position, `scrollLines`), cursor shape and blink (DECSCUSR), cell extras table (underline color SGR 58, OSC 8 links), per-row combining-character text (currently `cellText` per cell), keep the npm package's `node:fs` branch from breaking browser bundles (D2, D3 spike) | Each item matches xterm.js in the differential tests. Cell format frozen — **implemented**: `displayOffset`·`scrollDisplay`·`scrollToBottom`, `cursor().shape/blink`, `rowExtras()` (combining characters, underline color, links, attrs bits 15·16), bundle fix, wasm ABI 2. The 4-word cell format stays as is and extras are frozen as a per-row table. **Differential comparison done**: underline color, OSC 8 and combining characters are compared against xterm.js in harnesses A and B; cursor shape and blink are outside the xterm public API, so they are pinned by an engine test (`cursor_shape_follows_decscusr`). **Remaining**: the viewport scroll position (`displayOffset`) is outside the screen record, so it was not compared against xterm.js |
| ✅ Scroll damage: also report the number of scrolled lines (D2) — `vt::term::Config::scroll_damage` (off by default = Alacritty behavior), turned on by `oxtty::Engine` | On continuous output, report the scrolled line count + only the new lines instead of `Full` |
| ✅ Host policy hooks: OSC 52 clipboard, OSC 4/10/11 color queries | Ignored when no hook is set; when set, the reply bytes match xterm.js — `Engine::set_clipboard_access`·`take_clipboard_write/read`·`reply_clipboard` (same `OSC 52;Pc;b64 BEL` as the xterm clipboard addon), `set_colors` (259 colors, `rgb:rrrr/gggg/bbbb` + ST, including OSC 12, colors changed by the program via OSC 4 take precedence). If the host provides no colors, it does not reply, like `@xterm/headless`. wasm ABI 4, same API in JS and N-API |
| ✅ **D3 validation spike**: detach the xterm.js WebGL atlas and render model and draw a Hangul/SGR/emoji screen from oxtty `rowPacked` | **D3 confirmed** — `spike/d3-webgl/`, `d3e84b7`·`afecf53`. Hangul, 16/256/RGB, attributes, emoji, combining characters and damage updates all correct, 0 console errors |
| ✅ Performance baseline measurement (vtebench-style corpus, comparing wasm, native and xterm.js) | Throughput and memory tables published in the README. Parser-only, `write`→frame end-to-end, and continuous scrolling output cases are measured separately — parser-only: `packages/oxtty/bench/throughput.mjs` (6 scenarios, 8 MiB per scenario, median of 5 runs). Native is faster than xterm.js in all 6 (1.0–4.0×); wasm is slower on logs and Hangul (0.73×) and equal or faster on the rest. Continuous scrolling output is the `scroll-flood` and `scrolling-region` rows. End-to-end is `spike/d3-webgl/bench` (README browser renderer table), memory is the README buffer memory table. This script's per-scenario RSS growth was left out of the public tables because wasm memory grows in 4 MiB steps and native comes out near 0 due to malloc reuse |

**G3 swap gate status** — ✅ 0 entries with `blocksSwap: true` (2026-11-01). The remaining 4 (`wide-leader-keeps-fg`, `sgr-21`, `tab-stored-as-tab`, `decstr-unsupported`) were aligned with xterm.js behavior via the `xterm_wide_leader`, `store_tabs` and `sgr21_double_underline` options of `vt::term::Config` and `Term::soft_reset` (DECSTR, which `oxtty::Engine` finds in the input and calls). `OXTTY_SWAP_GATE=1 npm test` passes. The 11 remaining differences are all `blocksSwap: false` ([known-differences.json](../packages/oxtty/test/xterm/known-differences.json)).

### M2 · 0.2 web front end (wmux renderer swap)

| Requirement | Acceptance criteria |
| --- | --- |
| `oxtty/xterm`: the borrowed WebGL renderer draws oxtty rows (D3) | Updates only changed lines, recovers from WebGL context loss |
| ✅ API checklist: `new Terminal(opts)`, `open`, `write`, `onData`, `onBinary`, `onResize`, `onTitleChange`, `onBell`, `resize`, `buffer.active.getLine().translateToString()`, `loadAddon`, `dispose` | 100% of the checklist + a support scope table published in the README — [api-checklist.json](../packages/oxtty/test/web/api-checklist.json) (every xterm.js member wmux uses): 104 supported, 11 not planned with the reason, 0 left (2026-11-02); `api-checklist.test.mjs` fails if a supported member is missing from the `.d.ts` or untested. The README table is built from it |
| Key input: `Keyboard.ts` and the keydown/keypress/input arbitration of xterm.js `CoreBrowserTerminal` (`_keyDown`, `_keyPress`, `_inputEvent`) borrowed verbatim, reading oxtty mode values; `attachCustomKeyEventHandler`, `paste()` (bracketed, CRLF→CR), focus events, wheel (scroll the view / arrow keys on the alternate screen with alternate scroll / mouse reports). The kitty keyboard protocol stays off | Bytes on `onData` match xterm.js for the same CDP key events (key × modifier × mode), at the DOM level |
| ✅ Replacing wmux addons: fit and webgl built in, unicode11 is the engine table, search and serialize call oxtty directly, web-links is OSC 8 + regex detection | Every feature wmux uses works — fit, webgl and unicode11 built in (`@xterm/addon-webgl` and `@xterm/addon-unicode11` objects absorbed by `loadAddon`); search is a Rust port of `@xterm/addon-search` over the engine's `searchLine`, with plain-text and regex matching in JS (string methods, `RegExp`), so no matcher is compiled into the wasm; serialize is `term.ansiSnapshot({ scrollback, excludeModes })`, compared with the addon in `buffer.test.mjs`; web links are the built-in `WebLinksAddon` and OSC 8 links. **Images in Rust** (v0.4 decision): sixel and the iTerm2 inline image protocol (OSC 1337) through the built-in `ImageAddon` or an absorbed `@xterm/addon-image`, sixel decoded in Rust, other formats by the browser, tiles in engine cells, page-wide storage; compared with the real addon in `image.test.mjs`. Not supported: kitty graphics, QOI, `getImageAtBufferCell` |
| Hangul IME composition | Characters being composed appear at the cursor position, and only committed characters go to the PTY. wmux's existing IME and CJK regression tests pass |
| Selection UI: drag selection and copy (computation moves to Rust in M4); the engine reports trimmed scrollback lines so a selection follows its text; selection clears on alternate-screen switch | Word and line selection cover the same ranges as xterm.js |
| Engine output wiring: replies → `onData`, title → `onTitleChange`, bells → `onBell`, OSC 52 and colour queries through host options | `ESC[6n` gives the CPR bytes on `onData`; DA1 matches xterm.js |
| Failure paths: write queue survives an engine crash and `dispose`; WebGL context loss recovery; reference-counted glyph atlas | Crash mid-write, drain after dispose, `WEBGL_lose_context` loss→restore, 17 contexts on one page |
| Runtime options (theme, font, cursor), font load and devicePixelRatio changes re-measure and refit | Hangul fallback widths compared side by side with xterm.js |
| Migration: `await init()` once before `new Terminal()` is the only required change; the published `.d.ts` lists only supported members; unknown addons are rejected with a clear error | Checklist built from wmux's actual usage, not from this table |
| Adopt in wmux: add "oxtty" to the Terminal engine setting, replace the 19 xterm.js internal workarounds (in 4 files) | 0 differential mismatches before the swap; 0 regressions in wmux e2e, IME and screen snapshots after the swap |

### M3 · 0.3 input in Rust

M2 runs on key input code borrowed from xterm.js. M3 moves encoding to Rust.

| Requirement | Acceptance criteria |
| --- | --- |
| Keyboard encoding: default xterm scheme, cursor and keypad application modes, kitty keyboard protocol | Bytes match xterm.js across the key × modifier × mode matrix (kitty against its spec) |
| Mouse encoding: X10, normal, SGR, drag and motion | Bytes match per mode |
| Paste: bracketed paste, handling dangerous characters | Matches xterm.js |
| Focus events | `CSI I` / `CSI O` when the mode is active |

### M4 · 0.4 slimming the screen layer

| Requirement | Acceptance criteria |
| --- | --- |
| Selection range computation and link detection in Rust | Same selection results as xterm.js (word, line, rectangular selection) |
| Viewport-only mode: the screen holds only visible lines ± a margin, fetching from the host on scroll | Per-pane renderer memory measured and published against the M1 baseline |
| Web Worker parsing option | Measure the reduction in main thread usage with 20 panes outputting concurrently |
| Skip drawing inactive panes | Hidden panes receive output but do not draw |

#### Viewport-only mode: design (2026-11-02)

In wmux the daemon keeps every pane's whole history, so the renderer's copy is duplicate memory (D6). In
viewport-only mode the terminal keeps only its own short scrollback (the margin) and reads older lines from the host
when the view scrolls to them.

**Host API.** The host makes the terminal with the margin as its `scrollback` (xterm.js meaning unchanged: the
lines this terminal keeps) and registers a provider:

```ts
term.setHistoryProvider(provider: IHistoryProvider | null): void;
interface IHistoryProvider {
  /** History lines the host holds now, this terminal's own included (the host engine's `historyLength`). */
  readonly length: number;
  /** Most history lines the host keeps; past it, its oldest lines drop off. Default: no limit. */
  readonly limit?: number;
  /** Host lines fetched for the view kept in this terminal (default 256). */
  readonly cacheLines?: number;
  /** Host history lines `start .. start + count` (0 its oldest line). */
  fetch(start: number, count: number): Promise<ReadonlyArray<IHistoryRow | Uint32Array | string | null>>;
}
interface IHistoryRow {
  /** Packed cells as the engine's `rowCells` gives them (4 words per cell). */
  cells?: Uint32Array;
  /** Or the row as ANSI text, parsed at the terminal's width. */
  ansi?: string;
  /** The row wraps into the next one (the engine's `rowWrapped`). */
  wrapped?: boolean;
  /** The engine's `rowExtras` for the row: combining marks, underline colours, OSC 8 links (with `cells`). */
  extras?: unknown[];
}
```

- Line numbering is xterm.js's over the whole history: buffer line `y < H` is host line `y`, where
  `H = length - (this terminal's history)` is fixed when the provider is set; later lines are the terminal's own.
  Both sides parse the same output, so the terminal keeps `H` itself: lines that leave its full scrollback go to
  `H`; with a `limit`, `H` stops at `limit - own history` and its oldest lines drop off (they count as trimmed for
  selection and search decorations, like xterm.js `onTrim`); ED3 and RIS empty `H`. A resize reflows both sides
  differently, so the host calls `setHistoryProvider` again afterwards (until then the old `H` stands and fetched
  rows are dropped). `null` turns the mode off.
- Fetched rows are kept by their absolute line number (`H` lines dropped so far plus `y`), so trimming does not
  move them. A host ahead of the terminal (output in flight) while at its `limit` answers rows off by the lines in
  flight; wmux keeps no limit on the history it serves.

**Scrolling.** `scrollLines`, `scrollToTop`, `scrollToLine`, `scrollPages`, the wheel, the keyboard, the scrollbar
and the accessibility tree scroll over `H` plus the terminal's own history, exactly as xterm.js does over its
buffer; scrolled-back views stay on their text as output arrives. Rows not held yet are drawn as blank placeholder
rows; the terminal fetches them (the visible rows plus one screen each way, in one request), and the frame after the
answer redraws them. Frames scrolled into host lines are full redraws.

**Buffer API.** `length`, `baseY` and `viewportY` count `H` (`baseY = H + own history`). `getLine(y)` for a host
line returns the fetched row, or, until it is fetched, a blank line of `cols` cells and asks for it (it never
returns `undefined` inside `0..length`). `isWrapped` comes from the row above when it is held.

**Search.** `findNext`/`findPrevious` search the terminal's own lines synchronously as before. A search that must
read host lines returns `false`, fetches them in pages of 256 lines (a page without a match is remembered as empty,
at most four pages with matches are held), and then selects and scrolls to the match and fires
`onDidChangeResults` (also without decorations). Logical lines are cut at page boundaries and at the boundary to the
terminal's own lines, so a match across a cut is not found. With decorations, only held lines are highlighted and
counted.

**Selection.** Selection reads `getLine`, so it covers fetched rows; copying a selection over rows not fetched gives
blank lines for them. Drag-scrolling fetches rows like any scroll.

**Unchanged, terminal-local.** `ansiSnapshot` serializes the terminal's own lines only (the host has the full
history). Markers stay on the terminal's own lines (their `line` counts `H` like every buffer line) and are disposed when
their line leaves its scrollback, as with any `scrollback`. The alternate screen has no history, so `H` is hidden while it is up.

**Memory.** The engine holds `rows + scrollback` lines; fetched rows are JavaScript `Uint32Array`s (garbage
collected, not wasm memory, which never shrinks), at most `cacheLines` (the rows farthest from the view go first);
a search holds at most four pages of logical-line text.

**wmux wiring (not done here).** The renderer builds each pane's terminal with `scrollback` 200 and, after
attaching, calls `setHistoryProvider({ length, fetch })` with `length` from the daemon. The daemon answers `fetch`
from a per-pane native oxtty engine (`oxtty-node`: `rowCells`, `rowWrapped`, `rowExtras` for lines
`start - historyLength ..`) or, without one, by replaying the pane's ring through the existing snapshot path
(`generateRustSnapshot`) and slicing the rows it needs. The renderer calls `setHistoryProvider` again after every
resize with the daemon's new `historyLength`.

### M5 · 1.0 public release

- Default engine for wmux on all platforms
- API freeze, documentation site, migration guide (xterm.js → oxtty)
- Continuous fuzzing (parser, serialization), 0 panics

### After 1.0

- Native renderer (wgpu + cosmic-text) — when wmux moves off Electron
- Kitty graphics (sixel and iTerm2 images are in M2)
- Shell integration (OSC 133 prompt regions)

---

## 7. Non-functional requirements

| Area | Requirement |
| --- | --- |
| Correctness | The reference is xterm.js behavior. Every difference is documented and only intentional ones remain |
| Unicode | Character widths use the same table as the xterm.js Unicode 11 provider |
| Determinism | No clock, threads or randomness in the engine. Same input → same screen |
| Stability | 0 panics in wasm (verified by fuzzing); invalid input is ignored and processing continues |
| Size | Engine wasm 340KB or less (302KB on 2026-11-01, with search, markers and the vte fork; 307,707 bytes, 111,732 gzipped on 2026-11-02; raised from 300KB because one instance serves every pane on a page, so the code is paid once, not per terminal); front-end wasm (`oxtty/xterm`) 800KB or less, 300KB gzipped (557KB on 2026-11-01 with the renderer, input, search, parser hooks, accessibility and scrollbar; 727,245 bytes, 274,609 gzipped on 2026-11-02 with selection, links and images; raised from 600KB for selection, links and images on the same one-instance-per-page reasoning — `opt-level = "z"` would save 13% but slows the per-frame renderer code), checked by `scripts/build-web.sh`; both also by `test/web/size.test.mjs` |
| Performance | `write`→frame end-to-end throughput at or above xterm.js on the same corpus. The target value is set after the M1 baseline measurement |
| Memory | Publish the scrollback memory caused by the cell size (24B) in the README; consider compressed storage before 1.0 (D6) |
| Platforms | Native on Windows, macOS and Linux, modern browsers, Node 18 or later |
| License | oxtty code is Apache-2.0 (owner decision on 2026-10-31, dual MIT dropped). The embedded `vt` module (Alacritty fork) is also Apache-2.0, and the bundled Unicode 11 table and the input encoders (`crates/oxtty/src/input`: keyboard, kitty keyboard, mouse, paste and focus, ported from xterm.js 6.1.0-beta.336) are xterm.js MIT (`LICENSE-XTERM`) — the crate and npm declare `Apache-2.0 AND MIT`, reflected in NOTICE. The front end's ports of xterm.js and its addons (renderer, fit, input handling, selection, links, search, accessibility, scrollbar, image addon) and of the node-sixel decoder (MIT) have their origin and commit recorded in `NOTICE.md` and `packages/oxtty/THIRD_PARTY_NOTICES.md`, and each file names its source lines. GPL code is only consulted, never copied |

---

## 8. Success metrics

| Metric | Target |
| --- | --- |
| Differential mismatches before the swap (against `@xterm/headless`) | 0, or only intentional differences, documented |
| wmux regressions after the swap (e2e, IME, screen snapshots) | 0 (over a 7-day soak) |
| wmux renderer memory | Reduction measured and published against the M1 baseline |
| Compatible API checklist | 100% (M2) |
| External adoption | Set targets based on npm downloads and stars in the first month after going public |

---

## 9. Risks and mitigations

| Risk | Impact | Mitigation |
| --- | --- | --- |
| Too much scope in the in-house front end (implementing selection UI and accessibility ourselves) | M2 delay | Borrowing the rendering is confirmed by the spike (D3). Selection, IME, key input, links, accessibility, context loss recovery, DPR changes and ligatures are not yet validated |
| Uneven widths in the Hangul fallback font | On the spike screen, "안 녕 하 세 요" spreads apart while "입니다" sticks together | Compare font fallback and glyph width handling against xterm.js in M2 |
| ✅ Resolved — one WebGL2 context per pane (M2 eng review) | Chrome keeps about 16 active contexts; past that the oldest panes lose theirs, and naive recovery can make panes evict each other in a loop | The Rust front end shares one WebGL2 context and one glyph atlas per page (v0.4, `crates/oxtty-web/src/render/gpu.rs`): a lost context waits for `webglcontextrestored`, then a new one is made under a back-off throttle, and every pane redraws |
| npm versions cannot be reused (M2 eng review) | A broken 0.2.0 stays published | Publish `0.2.0-beta.N` under the `next` tag first and move to `latest` after wmux dogfooding |
| ✅ Resolved — snapshots reflect the engine's synchronized updates (code review 2026-10-31) | Calling `ansiSnapshot()` on the engine a renderer draws from showed half-drawn frames, and when an ESU was split across a snapshot boundary, synchronization did not end until the timeout | A snapshot taken during sync carries `CSI ? 2026 h` before the raw tail, so the receiver holds its frames in the same update. (Since 2026-11-02 the engine applies synchronized output at once, as xterm.js does, instead of holding the bytes in its vte fork; only frames wait.) Snapshots do not change the engine (`&self`). Pinned by a round-trip test at every split point |
| State snapshots do not carry (code review) | Character sets (ESC ( 0, SO/SI), the DECSC saved cursor, tab stops, cursor shape and the mark on a blank a wrapped wide char left (`LEADING_WIDE_SPACER`; it comes back as an erased cell, and xterm.js has no such mark) disappear in the restored terminal, so later output differs. OSC 8 links and typed spaces (`Attr.TYPED_SPACE`) are carried (2026-11-02) | Stated in the README. Rejecting based on engine state or carrying it in the serialization is follow-up work. None of the 91 fixtures contain these sequences |
| Cases where the snapshot tail scanner differs from vte (code review, engine worker) | After ESC with C0/DEL, BEL inside DCS, or an incomplete multibyte sequence right after ESC, the tail is lost or cut incorrectly (same behavior as the wmux tracker) | Attach reproduction cases and fix as follow-up |
| Snapshot performance (code review) | A new SGR string is built for every cell, so 10k history × 200 columns means 2 million allocations | Measure in the M1 performance baseline, then replace with a comparison against the previous cell's attributes |
| Borrowed xterm.js modules miss upstream fixes | IME and rendering bugs linger | Record the list of borrowed modules and their source commits, and check upstream release notes every quarter |
| The Alacritty fork diverges between wmux and oxtty | "One engine" just becomes "two forks" | Unify wmux-term onto `oxtty` in M1 (D8) |
| Distance from Alacritty upstream | Missing bug fixes | List patches like `WMUX-PATCH.md`, and verify with differential tests on version upgrades. After M1, propose features for the wasm build (turning off `tty` and `event_loop`) upstream |
| wmux policy leaks into the engine | wmux behavior is forced on external users | wmux-specific behavior as engine options, upstream as the default (D8) |
| Behavior differences (reflow, IME, width) | Broken wmux screens | Differential tests before the swap, wmux e2e and IME regression after the swap as release gates |
| wasm boundary cost (copying rows every frame) | Frame drops in busy panes | Copy only new lines with scroll-count damage (M1), Worker option (M4), decide by measurement |
| Scrollback memory (24B cells) | Heavier than xterm.js in standalone web use | Publish the numbers, consider compressed storage (D6) |
| Competition (libghostty-vt based: ghostty-web, herdr, t3code) | Hard to get adopted | Make the differentiators clear: Rust, one engine across daemon, web and native, real-world use in wmux. Reserve names (M1) |
| License contamination | Cannot distribute | No copying code from Zed or COSMIC Terminal (GPL family) or esctest (GPL-2.0); run them only as external tools |
| Solo development | Progress stalls | Keep milestones small, validate through real use in wmux |

---

## 10. Decision log and open questions

### 10.1 Decisions (v0.2)

| v0.1 question | Decision | Reason |
| --- | --- | --- |
| 1. Compatibility layer package name | The `oxtty/xterm` subpath (changed in v0.3) | Keep `oxtty` as the only public name. Borrowing parts (D3) means no dependency on the xterm.js package, so the engine package does not get heavier |
| 2. Fork xterm.js vs in-house front end | In-house front end + borrowed parts, confirmed by the M1 spike | D3 |
| 3. Whether oxtty owns the PTY | Not in the engine. If needed, a separate crate `oxtty-pty` | Keeps the "the engine only handles bytes" principle |
| 4. Relationship with wmux-term | In M1, wmux-term depends on `oxtty` | Prerequisite for G1. D8 |
| 5. When to go public | Go public after M1 (crates.io release) | Useful for Rust TUI testing even at that point, and it is a competitive space, so claim the spot early |

### 10.2 Open questions

1. ~~**Where wmux uses `@xterm/addon-image`**~~ — answered in v0.4: sixel and iTerm2 images are implemented in Rust (M2).
2. **Accessibility level** — how far the in-house front end goes with screen reader support (including whether to borrow xterm.js `AccessibilityManager`)
3. **Environments without WebGL** — whether a DOM renderer fallback path is needed (wmux assumes WebGL)

---

## 11. Reference open source

Status and licenses were checked on 2026-10-31 on crates.io, npm and each repository.

### A. Foundation — used directly

| Project | Language · License | Status | What oxtty takes | Caveats |
| --- | --- | --- | --- | --- |
| [alacritty_terminal](https://github.com/alacritty/alacritty) | Rust · Apache-2.0 | 0.26.0 | Core grid and state (used as a fork) | Maintain the patch list, re-validate on upgrades |
| [vte](https://github.com/alacritty/vte) | Rust · Apache-2.0 OR MIT | 0.15.0 | VT parser | Do not use the default synchronized output handler in wasm |
| [xterm.js](https://github.com/xtermjs/xterm.js) | TS · MIT | 6.0.0 stable (wmux uses the 6.1 beta) | Borrowed parts (WebGL atlas and render model, IME composition, key input), behavioral reference, Unicode 11 table | Record origin and commit of borrowed modules |
| [portable-pty](https://github.com/wezterm/wezterm) | Rust · MIT | 0.9.0 | Candidate for `oxtty-pty` | Kept separate from the engine |

### B. Projects that solved the same problem — swapping the core of a web terminal

| Project | Language · License | Status | What to learn |
| --- | --- | --- | --- |
| [ghostty-web](https://github.com/coder/ghostty-web) (Coder) | Zig→WASM · MIT | 0.4.0, wasm about 400KB, 314,582 weekly downloads | How it achieved xterm.js API compatibility → M2 checklist. How it lets users migrate by swapping an import, without a fork |
| [riotermjs](https://github.com/raphamorim/riotermjs) | Rust→WASM · MIT | 0.1.8, early | Rust core + canvas/DOM renderer structure, xterm.js migration guide |
| [vercel-labs/wterm](https://github.com/vercel-labs/wterm) | Zig→WASM · Apache-2.0 | Built-in core wasm about 26KB, optional libghostty backend | Worker parsing (D7), drawing only changed lines, skipping hidden panes, IME composition display, DOM-based accessibility |

### C. Terminal core libraries — alternatives and comparison targets

| Project | Language · License | Status | What to learn |
| --- | --- | --- | --- |
| [libghostty-vt](https://mitchellh.com/writing/libghostty-is-coming) | Zig + C API · MIT (Ghostty repository) | Alpha, API unstable. Still used in practice: herdr vendors it, t3code builds it to wasm | **The real competitor.** API design of a dependency-free VT library, kitty keyboard, reflow. Behavioral reference point (D1) |
| [termwiz](https://github.com/wezterm/wezterm) (WezTerm) | Rust · MIT | 0.23.3 | Key input encoding, screen diff model, hyperlink and image models |
| [rio-backend](https://github.com/raphamorim/rio) | Rust · MIT | 0.5.28 | An example of evolving an Alacritty-family grid independently |

### D. Native rendering — after Electron

| Project | Language · License | Status | What to learn |
| --- | --- | --- | --- |
| [wgpu](https://github.com/gfx-rs/wgpu) | Rust · MIT OR Apache-2.0 | 30.0.1 | GPU rendering foundation |
| [cosmic-text](https://github.com/pop-os/cosmic-text) | Rust · MIT OR Apache-2.0 | 0.19.0 | Text shaping, font fallback (Hangul, emoji) |
| [swash](https://github.com/dfrg/swash) / [glyphon](https://github.com/grovesNL/glyphon) | Rust · MIT/Apache family | 0.2.10 / 0.12.0 | Glyph rasterization, wgpu text rendering |
| [sugarloaf](https://github.com/raphamorim/sugarloaf) (Rio) | Rust · MIT | 0.5.28 | Structure of a terminal-specific wgpu renderer |
| [Zed](https://github.com/zed-industries/zed) terminal | Rust · GPL/AGPL family | — | How it connects alacritty_terminal to its own GPU UI. **Reference the structure only, no copying code** |
| COSMIC Terminal | Rust · GPL-3 | — | Alacritty engine + its own text renderer. **Reference the structure only** |

### E. Specifications · conformance tests · benchmarks

| Resource | Type | Use |
| --- | --- | --- |
| [xterm Control Sequences](https://invisible-island.net/xterm/ctlseqs/ctlseqs.html) (Thomas Dickey) | Specification | Reference for escape sequences |
| [kitty keyboard protocol](https://sw.kovidgoyal.net/kitty/keyboard-protocol/) | Specification | Reference for M3 keyboard encoding |
| [vttest](https://invisible-island.net/vttest/vttest.html) | Interactive test | Manual VT100/220 compatibility checks |
| [esctest](https://github.com/migueldeicaza/esctest) | Automated tests · Python · GPL-2.0 | Automated verification of cell values and cursor positions. **Run only as an external tool, never add it to the repository** |
| [vtebench](https://github.com/jwilm/vtebench) | Throughput benchmark | M1 performance baseline |
| wmux internal assets | 91 fixtures (85 unique inputs) + shadow comparison harness + IME and CJK regression tests | **The most important regression assets.** The fixtures are ported to the oxtty differential tests in M1; the IME regressions keep running in wmux e2e |

---

## Appendix A. 0.0.1 API summary (npm)

```js
import { init, Terminal, Attr, Mode, decodeColor } from 'oxtty';
await init();
const t = new Terminal({ cols: 80, rows: 24, scrollback: 5000, onSyncFlush });
t.write(bytesOrString);
t.takeDamage();      // { full, lines }
t.rowCells(y);       // Uint32Array [cp, fg, bg, attrs] × cols
t.rowText(-1);       // most recent line of scrollback
t.takeReplies();     // bytes to send back to the PTY
t.cursor(); t.modes(); t.takeTitle(); t.takeBells();
```

## Appendix B. Sources for the figures in this document

- wmux measurements: `bench/rust-terminal-core.md` (fixture matches), `bench/bot-memory.md` (agent share, Run C peak sample), commit `dc6047dcd` (#2806, separate branch)
- npm downloads: api.npmjs.org, 2026-10-23~29
- Package versions and licenses: crates.io (`cargo info`), npm (`npm view`), GitHub API (Ghostty license), 2026-10-31
- Project information: each repository's README, mitchellh.com (libghostty), heise (Ghostling)
- v0.2 verification (2026-10-31)
  - Alacritty fork differences: compared `wmux-term/vendor` and `oxtty/vendor` with `diff -rq`, 3 files differ
  - Full scroll damage: `scroll_up_relative` and `scroll_down_relative` in `vendor/alacritty_terminal/src/term/mod.rs` call `mark_fully_damaged`
  - Cell extras data: `underline_color` and `hyperlink` in `vendor/alacritty_terminal/src/term/cell.rs`
  - wmux's access to xterm.js internals: 4 files under `src/renderer/terminal/`, 19 places (`_core`, `_renderService`, `_compositionHelper`, `_charSizeService`)
  - Registries: npm `oxtty` and crates.io `oxtty` both unregistered (404)
