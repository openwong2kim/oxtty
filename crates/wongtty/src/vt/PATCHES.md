# Built-in Alacritty fork

Source: the published `alacritty_terminal` 0.26.0 crate from crates.io
(Apache-2.0, `../../LICENSE-APACHE`). The upstream README is kept as
`UPSTREAM-README.md`. The fork started in wmux (`core/wmux-term/vendor`) and is
built into the `wongtty` crate as the `wongtty::vt` module, so `wongtty` publishes as a
single crate.

## Local changes

- `wmux_unicode11.rs` is generated from the xterm.js Unicode 11 provider by
  wmux's `scripts/generate-rust-unicode11.mjs`. Its header records the source
  hash. The xterm.js MIT license is included as `../../LICENSE-XTERM`.
  Regenerate deliberately after a provider update.
- `mod.rs` registers that module; `term/mod.rs` uses its width lookup for
  character input and exposes read-only `inactive_grid()` and
  `scroll_region()` for snapshot replay.
- `term/mod.rs` also exposes read-only `active_charset()` and `tab_stops()`
  for snapshot replay.
- Cursor-paragraph resize (xterm.js `reflowCursorLine: false`) is an option:
  `term::Config::reflow_cursor_line` (default `true`, upstream behaviour) sets
  `Grid::set_keep_cursor_paragraph`. When kept, `grid/resize.rs` leaves the
  cursor paragraph without reflow, reflows other paragraphs as upstream, and on
  shrink clamps the cursor and clears pending wrap. Clipped fullwidth cells are
  retained to match xterm. Rows added by a shrink follow xterm.js
  `_reflowSmaller`: while history is empty and the cursor is above the bottom
  row, each added row drops the bottom row and moves the cursor down; the rest
  go to history with the cursor row unchanged, even when the reflowed paragraph
  is below the cursor. Widening also frees a pending wrap on a grid that does
  not reflow (the alternate screen), as xterm.js leaves the cursor past the
  old last column, which is then a real column; upstream frees it only when
  reflowing. `wongtty::Engine` sets `reflow_cursor_line: false`.
- Scroll damage is an option: `term::Config::scroll_damage` (default `false`,
  upstream full damage on every scroll). When on, a whole-screen scroll at the
  bottom of history moves line damage with the content, damages the new
  bottom lines, moves the last cursor point along, and counts the rows in
  `Term::damage_scrolled()`. Region scrolls, a scrolled-back view, or scrolling
  a full screen height still give full damage. `wongtty::Engine` turns it on.
- `Term::damage` damages the cursor only when it moved, and no longer damages
  the whole screen on every call in insert mode: `input` damages the cells it
  writes (a combining mark's cell, the rest of the line when inserting). So a
  frame without output reports no lines; upstream damages the cursor every
  time because its renderer draws the cursor from the damage.
- RIS keeping the cursor hidden is an option:
  `term::Config::ris_keeps_cursor_visibility` (default `false`, upstream: RIS
  shows the cursor again). When on, `reset_state` restores DECTCEM
  (`TermMode::SHOW_CURSOR`) after resetting the modes, as xterm.js does.
  `wongtty::Engine` turns it on.
- ED2 scrolling into history is an option:
  `term::Config::scroll_on_erase_in_display` (default `true`, upstream: on the
  primary screen `CSI 2 J` pushes the screen into history through
  `Grid::clear_viewport`). When off, `clear_screen` resets the screen in place
  as on the alternate screen, like xterm.js `scrollOnEraseInDisplay: false`.
  `wongtty::Engine` turns it off.
- Line continuation as xterm.js keeps it is an option:
  `term::Config::xterm_line_wrap` (default `false`, upstream). xterm stores
  "this row continues the previous one" on the lower row; Alacritty stores it
  as `WRAPLINE` on the upper row's last cell. When on, a line feed that moves
  the cursor down without scrolling, EL0/EL2 from the first column, ED0 from
  the first column and ED1/ED2 (no scroll) end that continuation, while EL,
  ECH, ICH and DCH keep the erased row's own `WRAPLINE` (DCH no longer leaves
  it mid-row). IL and DL move the boundary flags as xterm's line splices do.
  vte routes IND and NEL through `linefeed` as well, so they also end a
  continuation there, unlike xterm. A restored cursor (DECRC, `CSI u`,
  1048, 1049) drops its pending wrap one column on, clamped to the screen,
  as xterm.js `restoreCursor` does. ECH, DCH and ICH also end a pending
  wrap (xterm.js clamps its cursor, one past the edge, back onto the last
  column first). ED0 in a pending wrap leaves the cursor row alone, as EL0
  does, and ED1 with the cursor on line 1 erases line 0 too (upstream only
  resets the rows above from line 2). `wongtty::Engine` turns it on.
- Writing over a right half blanks the cell to its left only when that cell
  is the left half, so a wide char a resize clipped in the last column (or a
  lone right half) does not take its neighbour with it.
- Three cell rules from xterm.js are options, each off (upstream) by default
  and turned on by `wongtty::Engine`: `term::Config::xterm_wide_leader`
  (overwriting the right half of a wide char leaves the left half as a blank
  in the current pen, not a blank that keeps the old colours; and as in
  xterm.js `BufferLine`, ECH, EL, ED, DCH, ICH and insert-mode printing that
  take one half of a wide char blank the other half too, keeping the row's
  WRAPLINE. ICH at a right half moves it on alone, as xterm.js does),
  `term::Config::store_tabs` (`false`: a tab no longer writes `'\t'` into the
  first blank cell it passes) and `term::Config::sgr21_double_underline`
  (vte reports SGR 21 as `Attr::CancelBold`; read it as a double underline).
- `term::Config::xterm_device_attributes`, off (upstream) by default and
  turned on by `wongtty::Engine`, answers DA1 with `CSI ? 1 ; 2 c` and DA2 with
  `CSI > 0 ; 276 ; 0 c` as xterm.js does, instead of `CSI ? 6 c` and
  Alacritty's version.
- `term::Config::decrqss`, off (upstream) by default and turned on by
  `wongtty::Engine`, answers DECRQSS (`DCS $ q Pt ST`) as xterm.js
  `requestStatusString` does: `0m` for SGR, the scroll region for DECSTBM,
  `Config::default_cursor_style` for DECSCUSR (xterm.js reports its
  `cursorStyle`/`cursorBlink` options; `Term::set_default_cursor_style` sets
  it), 0 for DECSCA, `61;1` for DECSCL, `DCS 0 $ r ST` for anything else.
  vte has no DCS actions: the `Performer` collects an unhooked `DCS $ q`
  payload and stops parsing after the byte that ends it, then the
  `Processor` calls `Handler::request_status_string`, unless that byte was
  a CAN or SUB, which abort it (xterm.js does not answer then either). A
  hooked one is answered by `finish_hook`; a payload past the hook limit is
  not answered.
- Private mode 9 (X10 mouse reporting) sets `TermMode::MOUSE_X10`, which is
  part of `TermMode::MOUSE_MODE` so the mouse protocols stay exclusive, and
  DECRQM reports it.
- Private mode 1016 (SGR mouse reports in pixels) sets
  `TermMode::SGR_PIXELS_MOUSE`, exclusive with the SGR and UTF-8 encodings,
  and DECRQM reports it. `term::Config::xterm_mouse_modes`, off (upstream) by
  default and turned on by `wongtty::Engine`, resets as xterm.js does: resetting
  any tracking mode (9, 1000, 1002, 1003) turns tracking off, and resetting
  1006 or 1016 returns to the default encoding.
- `Grid::history_entered` counts the lines that scrolled into the history
  (whether a full history then dropped them), so the engine can tell how many
  lines left the top of the scrollback.
- `Term::resets` counts full resets (RIS), which vte hands to `reset_state`
  without telling the host.
- `Grid::track_row_moves` / `take_row_moves` log how `scroll_up`,
  `scroll_down` and `reset_region` moved or cleared screen rows (`RowMove`),
  off until a host turns it on. `wongtty::Engine` turns it on while it has
  markers, to keep them on their lines.
- `Term::set_kitty_keyboard` switches the kitty keyboard protocol at runtime
  and drops its flag stacks, as `set_options` does, without re-sending the
  title or damaging the screen.
- `Term::cursor_style_set` tells a cursor style the program chose (DECSCUSR
  1–6) from the configured default, so a host can keep its own default.
- `Term::soft_reset` is DECSTR (`CSI ! p`), which vte drops. It resets what
  xterm.js `softReset` does: cursor shown, IRM/DECOM/DECCKM/DECKPAM, bracketed
  paste and focus reports off, autowrap on, full scroll region, default pen
  and charsets, saved cursor at the origin. `wongtty::Engine` finds the
  sequence in its input (split across feeds too), applies the bytes before
  it, ends a pending synchronized update and calls it.
- `term/mod.rs` handles the xterm private modes 47, 1047 and 1048, which the
  `vte` table reports as `PrivateMode::Unknown` and upstream ignores. 47/1047
  switch screens through `swap_alt` like 1049 but keep the primary saved
  cursor; while on the alternate screen the primary cursor is that saved
  cursor, so a later `?1049l` returns to it as xterm.js `restoreCursor` does.
  Leaving with 47/1047 gives the primary screen the alternate cursor (xterm.js
  `activateNormalBuffer`). 1048 saves/restores the cursor like
  DECSC/DECRC.
- `term/cell.rs` uses the remaining u16 flag bit for SGR blink; `term/mod.rs`
  retains SGR 5, ignores SGR 6 like xterm, and clears blink on SGR 25/reset.
  This keeps the 24-byte cell layout while allowing ANSI replay to preserve blink.
- `Flags` is 32 bits (the cell is still 24 bytes) for `Flags::TYPED_SPACE`:
  printing a space sets it, so a typed space can be told from an erased or
  never written blank as xterm.js does. The blanks around a wide char and a
  stored tab never carry it.
- The PTY and event-loop modules (`tty`, `event_loop`, `sync`, `thread`) and
  their dependencies are removed: the engine only takes bytes, and they do not
  build for `wasm32-unknown-unknown`. `Term`, `Grid` and the parser are
  unchanged.
- A synchronized update (`CSI ? 2026 h` … `CSI ? 2026 l`) no longer holds
  bytes back: as in xterm.js, the output is parsed and applied at once and
  only the host's frames wait. `Processor` lost its sync buffer, its
  `Timeout` parameter (`StdSyncHandler`, whose `Instant::now()` traps on
  wasm32) and `stop_sync`; BSU and ESU reach the handler as private modes.
  `Term` keeps the update as `TermMode::SYNC_UPDATE`: DECRQM reports it,
  and RIS and DECSTR (`soft_reset`) end it, as xterm.js resets its
  `synchronizedOutput` mode. The host ends a timed-out update with
  `unset_private_mode`.
- History kept outside the grid: `grid::HistorySink` receives the rows a full
  history drops, oldest first (`Grid::set_history_sink`; `scroll_up` hands
  them over before rotating, `clear_history` calls `HistorySink::clear`).
  The grid owns its sink (a `Box`, no lock: one thread drives a terminal);
  `Grid::history_sink` and `HistorySink::as_any` let its owner read the rows
  it keeps, and a clone of the grid has no sink.
  `Grid::scrolled_lines` counts scrolled lines under the same condition that
  moves `display_offset`. `Grid::prepend_history` / `split_off_history` move
  history rows in and out (wongtty puts its compressed history back for a
  resize reflow), `Grid::set_display_offset` sets the view without clamping,
  and `Term::inactive_grid_mut` reaches the primary grid on the alternate
  screen. Without a sink the grid behaves as upstream. `wongtty::Engine` keeps
  16 recent history rows in the grid and compresses older ones
  (`src/history.rs`).
- History held above the grid during a resize: `grid::HistoryAbove`
  (`Grid::set_history_above`) holds the oldest history rows outside the grid,
  so a resize does not decode the whole compressed history. They count
  towards the history where a resize clamps `display_offset`
  (`decrease_scroll_limit`, the end of `grow_columns` and `shrink_columns`).
  A column change reflows them first, as the top of the storage they would
  be: `grow_columns` takes back the newest `2 * lines` of them (enough for its
  viewport fill and cursor pull-down counts) and `shrink_columns` takes the
  cells they carry into the next row; both pass `display_offset` along.
  `shrink_columns` drops the oldest of them when the history limit cuts.
  Without them the grid behaves as upstream. wongtty reflows them one row at a
  time, as `grow_columns`/`shrink_columns` reflow history rows
  (`src/history.rs`, checked against the whole history in the grid by a
  property test).
- `Storage::initialize_up_to` caps the row cache (`MAX_CACHE_SIZE` spare rows)
  at the history limit; `Grid::increase_scroll_limit` uses it so a short
  history does not allocate 1000 spare rows.
- `Row::grow` and `Row::append` count the old cells as occupied. Upstream left
  `occ` short there, so the lazy `Row::reset` could skip cells whose
  background differed from the last cell, and a reused row kept a stale
  background after a resize.
- `HistorySink::push` takes the row mutably: the sink may blank the cells it
  takes, which the grid resets afterwards anyway (`grid/mod.rs`).
- While the history is held above the grid during a resize
  (`history_above`), `Grid::increase_scroll_limit` caps the storage at the
  rows it holds plus those asked for, and `grow_lines` grows the visible
  lines with `Storage::grow_visible_lines_exact`: no `MAX_CACHE_SIZE` spare
  rows that the resize drops again right after.
- The alternate screen's grid is parked at one row while the primary screen
  is up (`Grid::park`, `Grid::unpark`): `Term::new` makes it one row,
  `swap_alt` unparks it at the primary screen's size on the way in and parks
  it on the way out, a resize parks it at the new size instead of resizing
  it, and RIS parks it after the reset. Its cursors, settings and counters
  stay; the cursors are clamped as a resize of a grid without history clamps
  them.
- Mechanical changes by `scripts/vendor-vt.sh`: `lib.rs` → `mod.rs` without
  Alacritty's crate-wide lint attributes, `crate::` → `crate::vt::`,
  `alacritty_terminal::` → `wongtty::vt::` in doc examples, and
  `env!("CARGO_PKG_VERSION")` pinned to `"0.26.0"` so the secondary device
  attributes reply keeps reporting Alacritty's version instead of wongtty's.
- The module is `#[rustfmt::skip]` so it stays byte-close to upstream.
- `vte/` is the published `vte` 0.15.0 crate (crates.io, git 3b3da71c34cc1256c7e20981cf03f8eb95e08ffc; Apache-2.0 OR MIT), built in:
  `vte/mod.rs` is its `lib.rs`, next to its `params.rs` and `ansi.rs`, so
  parsing can stop at a sequence the host hooked and an empty sub-parameter
  can be told from 0. Mechanical changes: the no-std code (the fixed-size
  OSC buffer, `Parser::new_with_size`, its tests) and the `std` and `ansi`
  feature gates are gone, the no-std `Processor` too, and `crate::` paths
  point at `super::`. The parser notes an empty sub-parameter
  (`Parser::param_digits`, `Params::mark_empty`), read with
  `Params::is_empty_subparam`; its value stays 0, so the default actions
  (SGR `38:2::r:g:b`) are unchanged, and hooks report it as -1 as xterm.js
  `Params` does (an empty parameter stays 0, xterm.js's ZDM). Parser hooks
  (marked `wongtty` in the code): `Processor::set_hook` registers CSI and DCS sequences by `hook_id` (intermediates, private marker
  first, then the final byte) and OSC by number; the `Performer` keeps a
  hooked sequence as `Processor::pending_hook` instead of dispatching it and
  stops `advance_until_terminated` after it (`advance_until_hook`). A DCS
  payload is collected for its hook; a payload past xterm.js's 10 MB limit is
  not stopped at, and CAN/SUB aborting an OSC or DCS runs no hook (the OSC
  still applies, as vte does). `finish_hook` runs the default action (a CSI
  is parsed again from its parameters, as vte's `Params` cannot be built) or
  drops it. `advance` runs hook defaults itself, as before hooks existed. One `debug!`
  argument lost a borrow for a newer clippy. To upgrade, diff `vte/` against
  the old crate's `src/` and apply that to the new one.
- xterm.js `windowsPty` behaviour is three options, each upstream by
  default, set together by `Term::set_windows_pty`: `term::Config::reflow`
  (`false`: resizing reflows neither screen), `term::Config::grow_from_history`
  (`false`: `Grid::set_grow_blank` makes more rows come in blank at the bottom
  while history stays history) and `term::Config::windows_wrap_heuristic`
  (xterm.js `updateWindowsModeWrappedState`: the vte fork calls
  `Handler::windows_wrap_heuristic` after a C0 line feed and before `CSI H`,
  and `Term` sets or clears `WRAPLINE` on the row above the cursor by whether
  its last cell is blank). With the xterm.js resize (`keep_cursor_paragraph`),
  a row that is not reflowed keeps `WRAPLINE` on its new last cell
  (`Row::resize_keeping_wrap`), as xterm.js keeps a line's wrapped flag; this
  also applies to the alternate screen.
- `Term::set_focus_report` gives the host's focus; with it, setting private
  mode 1004 sends `CSI I` or `CSI O` as a `PtyWrite` each time, as xterm.js
  reports focus when the mode is set. Unset (upstream), nothing is sent.
- `CellExtra` has an `image: Option<ImageTile>` (`term/cell.rs`): which tile
  of which host image covers the cell, as `@xterm/addon-image` keeps it in
  xterm.js extended attributes. `Cell::set_image`/`Cell::image` set and read it,
  and dropping the extra when its last field is cleared counts it.
  `Term::set_image_tile` (`term/mod.rs`) puts a tile on a screen cell and
  damages it. Writing or erasing a cell replaces its extra, so the tile goes.
- Printing is batched for speed, with the same result as upstream's
  character-at-a-time path. `vte/mod.rs`: `ground_dispatch` checks UTF-8 as
  it dispatches (`utf8_len`, the rules of `str::from_utf8`; the error is read
  only where the bytes are not valid) and hands the characters between
  controls to the new `Perform::print_str`, which `vte/ansi.rs` passes to the
  new `Handler::input_str` (both default to one call per character).
  `term/mod.rs`: `Term::input_str` writes printable ASCII a row at a time
  (`input_ascii`) and other characters one or two cells wide in one pass over
  the row (`input_run`), with the damage, occupied count and cursor `input`
  leaves; a pending wrap, insert mode, a charset that maps the character, a
  wide character's cell to overwrite or a wide character that does not fit
  go through `input`. `char_width` skips the Unicode 11 table search for
  Latin text, CJK ideographs and Hangul syllables.

These changes are compatibility policy, not upstream behaviour.

## Upgrading

1. Download the current (0.26.0) and the new `alacritty_terminal` crates from
   crates.io and unpack them.
2. Run `scripts/vendor-vt.sh <old> <tmp>/old` and
   `scripts/vendor-vt.sh <new> <tmp>/new`.
3. `diff -ruN <tmp>/old/src/vt crates/wongtty/src/vt > wongtty-vt.patch` — these are
   wongtty's changes.
4. Replace `crates/wongtty/src/vt` with `<tmp>/new/src/vt`, apply
   `wongtty-vt.patch` (`patch -p0` from the right directory) and resolve rejects.
5. Update the pinned version in this file and in the dependency list of
   `crates/wongtty/Cargo.toml`, then run `cargo test -p wongtty` and the wmux checks
   (`scripts/rust-terminal-core-bench.mjs --verify-only`,
   `scripts/rust-snapshot-verify.mjs`).

## Screen model speed

Same results as upstream, in less time:

- `grid/storage.rs`: `Storage::rotate_region` moves the rows of a scroll
  region that does not start at the top (DECSTBM, IL, DL) in one
  `slice::rotate` where they lie in order in the ring buffer, instead of one
  swap per row (`Grid::scroll_up`, `Grid::scroll_down`); a region that wraps
  around the buffer's end is rotated in its two pieces, which then swap the
  rows that cross between them (row by row only when the move is longer than
  a piece). The rows moved out are reset either way, so their order is not
  kept.
- `grid/row.rs`: `Row::reset` compares the template with the cell at `occ`
  (all cells from there on are alike) rather than the last cell, which on a
  long row is a cache miss of its own.
- `grid/row.rs`: `Row::plain` marks a row whose cells all have the default
  colours and no extra and are default blanks, narrow characters (no flags
  but `TYPED_SPACE`) or a `WIDE_CHAR` with its `WIDE_CHAR_SPACER` right after
  it. `Row::new` and a reset to a default blank set it, every mutable
  accessor clears it, and `term/mod.rs` `input_ascii` and `input_run` keep it
  while they write with the default pen (`input_ascii` then writes only the
  character and flags). The compressed history (`src/history.rs`) takes such
  a row as one run without checking each cell, and a reset of one writes
  only the characters and flags (`GridCell::reset_plain`). `Row::occ` is a
  `u16` (rows are at most `u16::MAX` wide) so the flag fits in the row's four
  words (`Storage::swap`).
- `grid/storage.rs`: a one-line region scroll holds one row aside and moves
  the others with one `memmove` (`rotate_one`) instead of `slice::rotate`'s
  three calls, and the other region rotates go through one function.
- `term/mod.rs`: `input_ascii` checks and writes each cell in one pass, and
  `input_run` counts the bytes it took from what is left of the text and
  puts the pen's extra on the cells it wrote after writing them;
  `char_width` is always inlined and also skips the table search for the
  emoticon (U+1F5FB–U+1F64F) and transport (U+1F680–U+1F6C5) emoji, which
  the table gives two cells; `scroll_up_relative` and
  `scroll_down_relative` touch the selection only when there is one.
- `term/cell.rs`: a cell whose extra holds only zerowidth characters (up to
  8: combining marks, variation selectors, joiners) shares one interned
  `CellExtra` per cluster instead of allocating an `Arc` and a `Vec` of its
  own. `intern` is a per-thread open-addressed table of 1024 slots, each with
  the cluster's hash: a lookup looks at most at 8 slots, and when those all
  hold other clusters the new one replaces one of them (cells keep the one
  replaced), so the cost stays bounded and the table follows what is printed
  whatever the number of distinct clusters. `push_zerowidth` interns the
  cluster with the new character, and past `MAX_MARKS` or with other extras
  falls back to upstream's copy-on-write. The field stays
  `Option<Arc<CellExtra>>`, so every other cell write compiles as before.
  `term/mod.rs` `input_run` puts the zerowidth characters that follow a cell
  it wrote, or that start the run after the cell before the cursor, on that
  cell (`put_marks`, as `input` does, interned once for the cluster) instead
  of leaving the run, and after them goes back to `input_ascii` for a stretch
  of eight or more ASCII bytes. WebAssembly builds leave this to `input`, for
  code size.

## Parser fast paths

The ground state and CSI parsing take shortcuts with the same actions as
upstream's byte-at-a-time state machine (checked against it by feeding the
same input one byte at a time, `vte/mod.rs` `chunked_input_matches_byte_by_byte`).

- `vte/mod.rs`: `advance_ground` no longer searches for ESC first;
  `ground_dispatch` stops at it, so the text is read once. Printable ASCII is
  skipped eight bytes at a time (`printable_ascii`), other text goes to
  `text_end`, which on aarch64 first checks 16 bytes at a time with NEON
  (`vte/scan.rs`: UTF-8 by Keiser and Lemire's lookup algorithm, as simdjson
  and simdutf do, plus C0 and C1 controls), and so on WebAssembly built with
  SIMD128 (`scripts/build-wasm.sh` turns it on). Other targets use the
  scalar path.
- A whole CSI after ESC (private marker, parameters, intermediates, final byte,
  all within the bytes given) is dispatched by `csi_fast` without leaving
  ground. A CSI of one parameter of at most two digits, or none (`CSI m`,
  `CSI 0 m`, `CSI 31 m`, `CSI 2 J`), is read from its next three bytes
  (`Params::set_one`). Otherwise `Params::parse` (`vte/params.rs`) finds the separators of a list
  longer than eight bytes a word at a time (`classify`), then reads each
  parameter's digits from a word at once (`word_value`). Anything else — a
  control or DEL inside, a cut-off sequence, a full parameter list, too many
  intermediates — goes through the state machine from the ESC as before.
- `advance_partial_utf8` took the bytes of every valid character after the
  first one it completed but printed only the first, dropping the others (an
  ASCII byte after a split `é` was lost when an incomplete character followed
  within four bytes); it now takes only the first character's bytes.
- `vte/ansi.rs`: `parse_sgr_color` is generic over its iterator instead of
  `dyn`, and while no hook is set `csi_dispatch` neither builds the hook id
  nor looks it up. CUP (`H`, `f`), DECSTBM (`r`) and SGR (`m`, without the
  colors 38, 48 and 58) with at most two plain parameters are handled before
  the general match, from `Params::two` instead of the parameter iterator;
  `sgr_attr` maps a plain SGR parameter for both paths.
