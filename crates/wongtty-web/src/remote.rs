//! Viewport-only mode (PRD M4, "Viewport-only mode: design"): the terminal keeps only its own short scrollback (the
//! `scrollback` option, a margin) and reads older history from the host, which parses the same output and keeps all
//! of it (wmux's daemon). `term.setHistoryProvider(provider)` turns it on.
//!
//! Buffer line `y < H` is host history line `y`; the engine's own lines follow. The terminal keeps `H` itself: the
//! lines that leave its full scrollback go to the host's side, ED3 and RIS empty it, and past the host's `limit` its
//! oldest lines drop off (they count as trimmed). Rows the view shows are fetched in one request (the visible rows
//! plus a screen each way) and drawn as blank placeholders until they come.
//!
//! Memory: fetched rows are JavaScript `Uint32Array`s, so they are garbage collected rather than held in the
//! front end's wasm memory (which never shrinks); at most `cacheLines` of them, the farthest from the view going
//! first.
use std::cell::Cell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::rc::Rc;

use js_sys::{Array, Function, Object, Promise, Reflect, Uint32Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::engine::{self, get, mode, CellExtra, Engine};
use crate::render::Dirty;
use crate::terminal::{Shared, Terminal};

/// Host rows kept for the view when the provider names no `cacheLines`.
const DEFAULT_CACHE_LINES: u32 = 256;
/// Requests are rounded out to multiples of this many lines, so reading lines one by one fetches them in blocks.
const BLOCK: u32 = 32;
/// Rows the engine parses per ANSI batch (its `MAX_ROWS`).
const ANSI_BATCH: usize = 1000;

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
/** One host history row: packed cells as the engine's `rowCells` gives them, or ANSI text for the row. */
export interface IHistoryRow {
  cells?: Uint32Array;
  ansi?: string;
  /** The row wraps into the next one (the engine's `rowWrapped`). */
  wrapped?: boolean;
  /** The engine's `rowExtras` for `cells`: combining marks, underline colours, OSC 8 links. */
  extras?: unknown[];
}
/** The host's history for viewport-only mode (`setHistoryProvider`). */
export interface IHistoryProvider {
  /** History lines the host holds now, this terminal's own included (the host engine's `historyLength`). */
  readonly length: number;
  /** Most history lines the host keeps; past it, its oldest lines drop off. No limit by default. */
  readonly limit?: number;
  /** Host rows kept for the view (256 by default). */
  readonly cacheLines?: number;
  /** Host history lines `start .. start + count`, 0 its oldest line; `null` for a line it no longer has. */
  fetch(start: number, count: number): Promise<ReadonlyArray<IHistoryRow | Uint32Array | string | null | undefined>>;
}
"#;

/// A host row as fetched.
pub(crate) struct Row {
    /// 4 words per cell, as the host sent them (any width; drawn padded or cut to the terminal's).
    pub cells: Uint32Array,
    pub wrapped: bool,
    pub extras: Vec<CellExtra>,
}

impl Row {
    /// Copies the cells into a frame row of 4 words per cell, blank past what the host sent.
    pub fn copy_cells(&self, row: &mut [u32]) {
        let n = (self.cells.length() as usize).min(row.len()) & !3;
        self.cells.subarray(0, n as u32).copy_to(&mut row[..n]);
        engine::blank_row(&mut row[n..]);
    }
}

/// What the view looked like at the last [`Remote::sync`].
#[derive(Clone, Copy, Default)]
struct Seen {
    trimmed: f64,
    total: u32,
}

/// The host's history above the engine's own, owned by [`engine::LocalEngineView`].
pub(crate) struct Remote {
    provider: Object,
    /// `H`: host lines above the engine's oldest line.
    pub lines: u32,
    limit: Option<u32>,
    cache_lines: u32,
    /// The engine's `scrollback`: a history shorter than it after lines were trimmed was cleared.
    scrollback: u32,
    alternate: bool,
    /// Host lines dropped off the top since the provider was set: host line `y` is kept under `dropped + y`.
    dropped: u64,
    /// `trimmedLines` of the whole history, continuing the engine's count from when the provider was set.
    pub trimmed: f64,
    /// Rows the view is scrolled past the engine's oldest line (the engine stays at the top of its history then).
    pub extra: Cell<u32>,
    /// The engine's view offset after the last change this module saw.
    pub seen_offset: Cell<u32>,
    seen: Seen,
    /// Bumped whenever fetched rows stop matching the host's lines; older answers are dropped.
    generation: u32,
    cache: HashMap<u64, Row>,
    /// Absolute ranges asked for and not answered yet.
    pending: Vec<(u64, u64)>,
    /// Host lines the view or `getLine` needed and did not have, to fetch after the current borrow.
    wanted: Cell<Option<(u32, u32)>>,
    /// The absolute line at the top of the view and the screen height, at the last frame.
    view: Cell<(u64, u32)>,
    /// A [`fetch_wanted`] is queued.
    queued: Cell<bool>,
}

thread_local! {
    static GENERATION: Cell<u32> = const { Cell::new(0) };
}

fn next_generation() -> u32 {
    GENERATION.with(|g| {
        g.set(g.get().wrapping_add(1));
        g.get()
    })
}

impl Remote {
    /// Reads `provider` (`TypeError` for a bad one).
    fn new(provider: &JsValue, scrollback: u32) -> Result<Self, JsValue> {
        let number = |key: &str| -> Result<Option<u32>, JsValue> {
            let value = get(provider, key);
            if value.is_undefined() || value.is_null() {
                return Ok(None);
            }
            match value.as_f64() {
                Some(n) if n >= 0.0 => Ok(Some(n.min(f64::from(u32::MAX)) as u32)),
                _ => Err(js_sys::TypeError::new(&format!(
                    "wongtty/xterm: history provider {key} must be a non-negative number"
                ))
                .into()),
            }
        };
        if !get(provider, "fetch").is_function() {
            return Err(js_sys::TypeError::new(
                "wongtty/xterm: a history provider needs fetch(start, count)",
            )
            .into());
        }
        Ok(Self {
            provider: provider.clone().unchecked_into(),
            lines: number("length")?.unwrap_or(0),
            limit: number("limit")?,
            cache_lines: number("cacheLines")?.unwrap_or(DEFAULT_CACHE_LINES),
            scrollback,
            alternate: false,
            dropped: 0,
            trimmed: 0.0,
            extra: Cell::new(0),
            seen_offset: Cell::new(0),
            seen: Seen::default(),
            generation: next_generation(),
            cache: HashMap::new(),
            pending: Vec::new(),
            wanted: Cell::new(None),
            view: Cell::new((0, 0)),
            queued: Cell::new(false),
        })
    }

    /// Takes the engine as it is now: `lines` held the host's whole history count until here.
    pub fn attach(&mut self, e: &Engine) {
        self.alternate = e.modes().unwrap_or(0) & mode::ALT_SCREEN != 0;
        let local = e.history_length();
        self.lines = self.lines.saturating_sub(local);
        self.cap(local);
        self.trimmed = e.trimmed_lines() - f64::from(self.lines);
        self.seen = Seen {
            trimmed: e.trimmed_lines(),
            total: self.lines + local,
        };
        self.seen_offset.set(e.display_offset());
    }

    /// Host lines in the buffer now: none while the alternate screen is up.
    pub fn visible_lines(&self) -> u32 {
        if self.alternate {
            0
        } else {
            self.lines
        }
    }

    /// Keeps `lines + local` within the host's limit, dropping the host's oldest lines.
    fn cap(&mut self, local: u32) {
        if let Some(limit) = self.limit {
            let most = limit.saturating_sub(local);
            if self.lines > most {
                let dropped = self.lines - most;
                self.lines = most;
                self.dropped += u64::from(dropped);
                self.trimmed += f64::from(dropped);
                self.extra.set(self.extra.get().min(self.lines));
            }
        }
    }

    fn forget_rows(&mut self) {
        self.cache.clear();
        self.pending.clear();
        self.wanted.set(None);
        self.generation = next_generation();
    }

    /// See [`engine::LocalEngineView::sync`]; true when the view must be redrawn.
    pub fn sync(&mut self, e: &Engine, reset: bool) -> bool {
        self.alternate = e.modes().unwrap_or(0) & mode::ALT_SCREEN != 0;
        let local = e.history_length();
        let trimmed = e.trimmed_lines();
        let moved = trimmed - self.seen.trimmed;
        let offset = e.display_offset();
        let mut redraw = false;
        // A full history drops its oldest lines to the host's side; a shorter one was cleared (ED3, RIS), and the
        // host cleared the same lines. The alternate screen hides the history: lines counted then were trimmed
        // before the switch.
        let cleared = reset || (moved > 0.0 && !self.alternate && local < self.scrollback);
        if cleared {
            self.trimmed += f64::from(self.lines) + moved;
            self.lines = 0;
            self.extra.set(0);
            self.forget_rows();
            redraw = true;
        } else if moved > 0.0 {
            self.lines = self
                .lines
                .saturating_add(moved.min(f64::from(u32::MAX)) as u32);
            self.cap(local);
        }
        let total = self.lines + local;
        if self.alternate {
            self.extra.set(0);
        } else if total != self.seen.total && !cleared {
            // Output went into the history: a scrolled-back view stays on its text, like xterm.js.
            let before = self.seen_offset.get() + self.extra.get();
            if before > 0 {
                let extra = i64::from(self.extra.get())
                    + i64::from(self.seen_offset.get())
                    + i64::from(total)
                    - i64::from(self.seen.total)
                    - i64::from(offset);
                self.extra.set(extra.clamp(0, i64::from(self.lines)) as u32);
                redraw = true;
            }
        }
        // The engine stays at the top of its own history while the view is above it, so a scroll the engine made
        // by itself moves the whole view.
        let extra = self.extra.get().min(self.lines);
        self.extra.set(extra);
        if extra > 0 && offset < local {
            let wanted = offset + extra;
            let engine = wanted.min(local);
            let _ = e.scroll_display((engine - offset) as i32);
            self.extra.set(wanted - engine);
        }
        self.seen = Seen { trimmed, total };
        self.seen_offset.set(e.display_offset());
        redraw
    }

    /// The engine started over empty after a crash: so does the host's side, until the host sets it again.
    pub fn restarted(&mut self, e: &Engine) {
        self.lines = 0;
        self.extra.set(0);
        self.forget_rows();
        self.seen = Seen {
            trimmed: e.trimmed_lines(),
            total: e.history_length(),
        };
        self.seen_offset.set(e.display_offset());
    }

    /// The host reflows its history its own way: keep `H` until the host says otherwise, drop the rows.
    pub fn resized(&mut self, e: &Engine) {
        self.forget_rows();
        self.seen = Seen {
            trimmed: e.trimmed_lines(),
            total: self.lines + e.history_length(),
        };
        self.seen_offset.set(e.display_offset());
    }

    /// The fetched row of host line `y`.
    pub fn row(&self, y: u32) -> Option<&Row> {
        self.cache.get(&(self.dropped + u64::from(y)))
    }

    fn held_or_asked(&self, y: u32) -> bool {
        let abs = self.dropped + u64::from(y);
        self.cache.contains_key(&abs) || self.pending.iter().any(|&(s, e)| s <= abs && abs < e)
    }

    /// Asks for host lines `start..end`, fetched once no borrow is held ([`fetch_wanted`]).
    pub fn want(&self, start: u32, end: u32) {
        let range = match self.wanted.get() {
            Some((a, b)) => (a.min(start), b.max(end)),
            None => (start, end),
        };
        self.wanted.set(Some(range));
    }

    /// Notes where the view is, for what the cache keeps.
    pub fn viewed(&self, display_offset: u32, local: u32, rows: u32) {
        let top = i64::from(self.lines) + i64::from(local) - i64::from(display_offset);
        self.view
            .set((self.dropped + top.max(0) as u64, rows.max(1)));
    }

    /// The request for the wanted rows, grown by a screen each way, without the rows held or asked for already.
    fn take_request(&mut self, rows: u32) -> Option<Request> {
        let (start, end) = self.wanted.take()?;
        let mut a = start.saturating_sub(rows) / BLOCK * BLOCK;
        let mut b = end
            .saturating_add(rows)
            .div_ceil(BLOCK)
            .saturating_mul(BLOCK)
            .min(self.lines);
        while a < b && self.held_or_asked(a) {
            a += 1;
        }
        while b > a && self.held_or_asked(b - 1) {
            b -= 1;
        }
        // Never more than the cache keeps, centred on what was wanted.
        let most = self.cache_lines.max(1);
        if b - a > most {
            a = a.max(start.saturating_sub((most - (end - start).min(most)) / 2));
            b = b.min(a + most);
        }
        if a >= b {
            return None;
        }
        let range = (self.dropped + u64::from(a), self.dropped + u64::from(b));
        if self
            .pending
            .iter()
            .any(|&(s, e)| s <= range.0 && range.1 <= e)
        {
            return None;
        }
        self.pending.push(range);
        Some(Request {
            provider: self.provider.clone(),
            start: a,
            count: b - a,
            first: range.0,
            generation: self.generation,
        })
    }

    /// Keeps an answer; true when it covers a row in view.
    fn store(&mut self, request: &Request, rows: Option<Vec<Option<Row>>>) -> bool {
        if request.generation != self.generation {
            return false;
        }
        let end = request.first + u64::from(request.count);
        self.pending.retain(|&r| r != (request.first, end));
        let Some(rows) = rows else {
            return false;
        };
        for (k, row) in rows.into_iter().enumerate() {
            if let Some(row) = row {
                self.cache.insert(request.first + k as u64, row);
            }
        }
        // Over the limit, the rows farthest from this answer go first, but not those in or next to the view.
        let (top, height) = self.view.get();
        let keep = top.saturating_sub(u64::from(height))..top + 2 * u64::from(height);
        let centre = request.first + u64::from(request.count / 2);
        while self.cache.len() > self.cache_lines as usize {
            let far = self
                .cache
                .keys()
                .filter(|abs| !keep.contains(abs))
                .max_by_key(|&&abs| centre.abs_diff(abs))
                .copied();
            match far {
                Some(abs) => self.cache.remove(&abs),
                None => break,
            };
        }
        request.first < top + u64::from(height) && end > top
    }

    /// Fetched rows held now (for tests and the memory bench).
    pub fn cached(&self) -> usize {
        self.cache.len()
    }

    pub fn provider(&self) -> &Object {
        &self.provider
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    pub fn generation(&self) -> u32 {
        self.generation
    }
}

/// One fetch in flight.
struct Request {
    provider: Object,
    start: u32,
    count: u32,
    /// Absolute line of `start` when asked.
    first: u64,
    generation: u32,
}

/// [`fetch_wanted`] on the next task, when rows are wanted.
pub(crate) fn fetch_soon(shared: &Rc<Shared>) {
    // One queued fetch at a time: reading many lines in a row asks for them together.
    let wanted = shared.core.try_borrow().is_ok_and(|core| {
        core.view
            .remote()
            .is_some_and(|remote| remote.wanted.get().is_some() && !remote.queued.replace(true))
    });
    if wanted {
        let weak = Rc::downgrade(shared);
        crate::timer::queue(move || {
            if let Some(shared) = weak.upgrade() {
                fetch_wanted(&shared);
            }
        });
    }
}

/// Fetches the rows the view or `getLine` wanted; the frame after the answer draws them.
pub(crate) fn fetch_wanted(shared: &Rc<Shared>) {
    let request = {
        let Ok(mut core) = shared.core.try_borrow_mut() else {
            return;
        };
        if core.disposed {
            return;
        }
        let rows = core.view.engine().rows();
        let Some(remote) = core.view.remote_mut() else {
            return;
        };
        remote.queued.set(false);
        remote.take_request(rows)
    };
    let Some(request) = request else {
        return;
    };
    let cols = shared.core.borrow().view.engine().cols();
    let weak = Rc::downgrade(shared);
    wasm_bindgen_futures::spawn_local(async move {
        let rows = fetch_rows(&request.provider, request.start, request.count, cols).await;
        let Some(shared) = weak.upgrade() else {
            return;
        };
        let rows = match rows {
            Ok(rows) => Some(rows),
            Err(error) => {
                web_sys::console::warn_1(&error);
                None
            }
        };
        let redraw = {
            let Ok(mut core) = shared.core.try_borrow_mut() else {
                return;
            };
            if core.disposed {
                return;
            }
            let shown = core
                .view
                .remote_mut()
                .is_some_and(|remote| remote.store(&request, rows));
            if shown {
                core.view.invalidate();
                core.dirty = Dirty::All;
            }
            shown
        };
        if redraw {
            shared.schedule_frame();
        }
        fetch_wanted(&shared);
    });
}

/// Asks the provider for host lines `start .. start + count`; [`parse_rows`] reads the answer.
pub(crate) fn request_rows(provider: &Object, start: u32, count: u32) -> Result<JsFuture, JsValue> {
    let fetch: Function = get(provider, "fetch").dyn_into()?;
    let answer = fetch.call2(provider, &start.into(), &count.into())?;
    Ok(JsFuture::from(Promise::resolve(&answer)))
}

/// Fetches host lines `start .. start + count` as rows the terminal's width can draw.
pub(crate) async fn fetch_rows(
    provider: &Object,
    start: u32,
    count: u32,
    cols: u32,
) -> Result<Vec<Option<Row>>, JsValue> {
    let answer = request_rows(provider, start, count)?.await?;
    parse_rows(&answer, count, cols)
}

/// An answer of the provider as rows; `None` for lines the host does not have. Out of line: both the view's fetches
/// and searches use it.
#[inline(never)]
fn parse_rows(answer: &JsValue, count: u32, cols: u32) -> Result<Vec<Option<Row>>, JsValue> {
    if !Array::is_array(answer) {
        return Err(js_sys::TypeError::new(
            "wongtty/xterm: a history provider's fetch must resolve to an array of rows",
        )
        .into());
    }
    let list = Array::from(answer);
    let n = list.length().min(count) as usize;
    let mut rows: Vec<Option<Row>> = Vec::with_capacity(n);
    // Rows given as ANSI text, parsed together below: (index, text).
    let mut ansi = Vec::new();
    for k in 0..n {
        let item = list.get(k as u32);
        let (cells, text, wrapped, extras) = if let Some(cells) = item.dyn_ref::<Uint32Array>() {
            (Some(cells.clone()), None, false, JsValue::NULL)
        } else if let Some(text) = item.as_string() {
            (None, Some(text), false, JsValue::NULL)
        } else if item.is_object() {
            (
                get(&item, "cells").dyn_into::<Uint32Array>().ok(),
                get(&item, "ansi").as_string(),
                get(&item, "wrapped").is_truthy(),
                get(&item, "extras"),
            )
        } else {
            (None, None, false, JsValue::NULL)
        };
        rows.push(match (cells, text) {
            (Some(cells), _) => Some(Row {
                cells: cells.slice(0, cells.length()),
                wrapped,
                extras: engine::parse_extras(&extras),
            }),
            (None, Some(text)) => {
                ansi.push((k, text));
                Some(Row {
                    cells: Uint32Array::new_with_length(0),
                    wrapped,
                    extras: Vec::new(),
                })
            }
            (None, None) => None,
        });
    }
    for batch in ansi.chunks(ANSI_BATCH) {
        parse_ansi(batch, cols, &mut rows)?;
    }
    Ok(rows)
}

/// Parses ANSI rows in a scratch engine `cols` wide, one row each, autowrap off.
fn parse_ansi(
    batch: &[(usize, String)],
    cols: u32,
    rows: &mut [Option<Row>],
) -> Result<(), JsValue> {
    let options = Object::new();
    Reflect::set(&options, &"cols".into(), &cols.into())?;
    Reflect::set(&options, &"rows".into(), &(batch.len() as u32).into())?;
    Reflect::set(&options, &"scrollback".into(), &0.into())?;
    let scratch = Engine::new(&options)?;
    let mut input = String::from("\x1b[?7l");
    for (i, (_, text)) in batch.iter().enumerate() {
        let _ = write!(input, "\x1b[{};1H\x1b[0m{text}", i + 1);
    }
    let parsed = scratch.write(&input.into()).map(|()| {
        for (i, (k, _)) in batch.iter().enumerate() {
            let line = i as i32;
            let Some(row) = rows.get_mut(*k).and_then(Option::as_mut) else {
                continue;
            };
            // A view into engine memory: copy it before the next engine call.
            if let Ok(Some(view)) = scratch.row_cells_view(line) {
                row.cells = Uint32Array::new(&view);
            }
            row.extras = engine::parse_extras(&scratch.row_extras(line).unwrap_or(JsValue::NULL));
        }
    });
    scratch.dispose();
    parsed
}

#[wasm_bindgen]
impl Terminal {
    /// Viewport-only mode (PRD M4): history older than this terminal's own `scrollback` is read from `provider` as
    /// the view scrolls to it. `null` turns it off. Set it again after a resize, with the host's new `length`.
    #[wasm_bindgen(js_name = setHistoryProvider)]
    pub fn set_history_provider(
        &self,
        #[wasm_bindgen(unchecked_param_type = "IHistoryProvider | null")] provider: JsValue,
    ) -> Result<(), JsValue> {
        {
            let mut core = self.shared.core.borrow_mut();
            if core.disposed {
                return Ok(());
            }
            let remote = if provider.is_null() || provider.is_undefined() {
                None
            } else {
                let scrollback = get(&core.options, "scrollback").as_f64().unwrap_or(1000.0);
                Some(Remote::new(
                    &provider,
                    scrollback.clamp(0.0, f64::from(u32::MAX)) as u32,
                )?)
            };
            core.view.set_remote(remote);
            core.view.invalidate();
            core.dirty = Dirty::All;
        }
        self.shared.after_change();
        Ok(())
    }

    /// Host rows held for the view in viewport-only mode, 0 otherwise (for tests and the memory bench).
    #[wasm_bindgen(js_name = _historyCacheSize, skip_typescript)]
    pub fn history_cache_size(&self) -> u32 {
        self.shared
            .core
            .try_borrow()
            .ok()
            .and_then(|core| core.view.remote().map(|r| r.cached() as u32))
            .unwrap_or(0)
    }
}
