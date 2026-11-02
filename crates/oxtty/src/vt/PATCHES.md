# Built-in Alacritty fork

Source: the published `alacritty_terminal` 0.26.0 crate from crates.io
(Apache-2.0, `../../LICENSE-APACHE`). The upstream README is kept as
`UPSTREAM-README.md`. The fork started in wmux (`core/wmux-term/vendor`) and is
built into the `oxtty` crate as the `oxtty::vt` module, so `oxtty` publishes as a
single crate.

## Local changes

- `wmux_unicode11.rs` is generated from the xterm.js Unicode 11 provider by
  wmux's `scripts/generate-rust-unicode11.mjs`. Its header records the source
  hash. The xterm.js MIT license is included as `../../LICENSE-XTERM`.
  Regenerate deliberately after a provider update.
- `mod.rs` registers that module; `term/mod.rs` uses its width lookup for
  character input and exposes read-only `inactive_grid()` and
  `scroll_region()` for snapshot replay.
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
  reflowing. `oxtty::Engine` sets `reflow_cursor_line: false`.
- Scroll damage is an option: `term::Config::scroll_damage` (default `false`,
  upstream full damage on every scroll). When on, a whole-screen scroll at the
  bottom of history moves line damage with the content, damages the new
  bottom lines, moves the last cursor point along, and counts the rows in
  `Term::damage_scrolled()`. Region scrolls, a scrolled-back view, or scrolling
  a full screen height still give full damage. `oxtty::Engine` turns it on.
- `Term::damage` damages the cursor only when it moved, and no longer damages
  the whole screen on every call in insert mode: `input` damages the cells it
  writes (a combining mark's cell, the rest of the line when inserting). So a
  frame without output reports no lines; upstream damages the cursor every
  time because its renderer draws the cursor from the damage.
- RIS keeping the cursor hidden is an option:
  `term::Config::ris_keeps_cursor_visibility` (default `false`, upstream: RIS
  shows the cursor again). When on, `reset_state` restores DECTCEM
  (`TermMode::SHOW_CURSOR`) after resetting the modes, as xterm.js does.
  `oxtty::Engine` turns it on.
- ED2 scrolling into history is an option:
  `term::Config::scroll_on_erase_in_display` (default `true`, upstream: on the
  primary screen `CSI 2 J` pushes the screen into history through
  `Grid::clear_viewport`). When off, `clear_screen` resets the screen in place
  as on the alternate screen, like xterm.js `scrollOnEraseInDisplay: false`.
  `oxtty::Engine` turns it off.
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
  resets the rows above from line 2). `oxtty::Engine` turns it on.
- Writing over a right half blanks the cell to its left only when that cell
  is the left half, so a wide char a resize clipped in the last column (or a
  lone right half) does not take its neighbour with it.
- Three cell rules from xterm.js are options, each off (upstream) by default
  and turned on by `oxtty::Engine`: `term::Config::xterm_wide_leader`
  (overwriting the right half of a wide char leaves the left half as a blank
  in the current pen, not a blank that keeps the old colours; and as in
  xterm.js `BufferLine`, ECH, EL, ED, DCH, ICH and insert-mode printing that
  take one half of a wide char blank the other half too, keeping the row's
  WRAPLINE. ICH at a right half moves it on alone, as xterm.js does),
  `term::Config::store_tabs` (`false`: a tab no longer writes `'\t'` into the
  first blank cell it passes) and `term::Config::sgr21_double_underline`
  (vte reports SGR 21 as `Attr::CancelBold`; read it as a double underline).
- `term::Config::xterm_device_attributes`, off (upstream) by default and
  turned on by `oxtty::Engine`, answers DA1 with `CSI ? 1 ; 2 c` and DA2 with
  `CSI > 0 ; 276 ; 0 c` as xterm.js does, instead of `CSI ? 6 c` and
  Alacritty's version.
- `term::Config::decrqss`, off (upstream) by default and turned on by
  `oxtty::Engine`, answers DECRQSS (`DCS $ q Pt ST`) as xterm.js
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
  default and turned on by `oxtty::Engine`, resets as xterm.js does: resetting
  any tracking mode (9, 1000, 1002, 1003) turns tracking off, and resetting
  1006 or 1016 returns to the default encoding.
- `Grid::history_entered` counts the lines that scrolled into the history
  (whether a full history then dropped them), so the engine can tell how many
  lines left the top of the scrollback.
- `Term::resets` counts full resets (RIS), which vte hands to `reset_state`
  without telling the host.
- `Grid::track_row_moves` / `take_row_moves` log how `scroll_up`,
  `scroll_down` and `reset_region` moved or cleared screen rows (`RowMove`),
  off until a host turns it on. `oxtty::Engine` turns it on while it has
  markers, to keep them on their lines.
- `Term::set_kitty_keyboard` switches the kitty keyboard protocol at runtime
  and drops its flag stacks, as `set_options` does, without re-sending the
  title or damaging the screen.
- `Term::cursor_style_set` tells a cursor style the program chose (DECSCUSR
  1–6) from the configured default, so a host can keep its own default.
- `Term::soft_reset` is DECSTR (`CSI ! p`), which vte drops. It resets what
  xterm.js `softReset` does: cursor shown, IRM/DECOM/DECCKM/DECKPAM, bracketed
  paste and focus reports off, autowrap on, full scroll region, default pen
  and charsets, saved cursor at the origin. `oxtty::Engine` finds the
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
  `Grid::scrolled_lines` counts scrolled lines under the same condition that
  moves `display_offset`. `Grid::prepend_history` / `split_off_history` move
  history rows in and out (oxtty puts its compressed history back for a
  resize reflow), `Grid::set_display_offset` sets the view without clamping,
  and `Term::inactive_grid_mut` reaches the primary grid on the alternate
  screen. Without a sink the grid behaves as upstream. `oxtty::Engine` keeps
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
  Without them the grid behaves as upstream. oxtty reflows them one row at a
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
- Mechanical changes by `scripts/vendor-vt.sh`: `lib.rs` → `mod.rs` without
  Alacritty's crate-wide lint attributes, `crate::` → `crate::vt::`,
  `alacritty_terminal::` → `oxtty::vt::` in doc examples, and
  `env!("CARGO_PKG_VERSION")` pinned to `"0.26.0"` so the secondary device
  attributes reply keeps reporting Alacritty's version instead of oxtty's.
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
  (marked `oxtty` in the code): `Processor::set_hook` registers CSI and DCS sequences by `hook_id` (intermediates, private marker
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

These changes are compatibility policy, not upstream behaviour.

## Upgrading

1. Download the current (0.26.0) and the new `alacritty_terminal` crates from
   crates.io and unpack them.
2. Run `scripts/vendor-vt.sh <old> <tmp>/old` and
   `scripts/vendor-vt.sh <new> <tmp>/new`.
3. `diff -ruN <tmp>/old/src/vt crates/oxtty/src/vt > oxtty-vt.patch` — these are
   oxtty's changes.
4. Replace `crates/oxtty/src/vt` with `<tmp>/new/src/vt`, apply
   `oxtty-vt.patch` (`patch -p0` from the right directory) and resolve rejects.
5. Update the pinned version in this file and in the dependency list of
   `crates/oxtty/Cargo.toml`, then run `cargo test -p oxtty` and the wmux checks
   (`scripts/rust-terminal-core-bench.mjs --verify-only`,
   `scripts/rust-snapshot-verify.mjs`).
