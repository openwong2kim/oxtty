//! The built-in `SearchAddon`, `@xterm/addon-search` in Rust: `findNext`/`findPrevious` select a match and scroll it
//! into view; with `decorations` every match is painted through the renderer's `"search"` highlight layer.
//!
//! Ported from xterm.js `addons/addon-search/src/SearchAddon.ts` lines 33-256, `SearchEngine.ts` lines 44-342,
//! `SearchState.ts` lines 12-106, `SearchResultTracker.ts` lines 23-119 and the row split of
//! `DecorationManager.ts` lines 116-128 at 08829c7 (as published in `@xterm/addon-search` 0.17.0-beta.330, MIT),
//! quirks included: a whole-word search looks only at the first match
//! of each line, and `findNext` goes on from the end of the selection only when the term is unchanged (the
//! `incremental` option changes nothing). Lines come from the engine's `searchLine` (rows joined across soft wraps,
//! each UTF-16 unit mapped to its cell) instead of `translateToString` and walks over the cells; plain text is
//! matched with the JS string methods and regular expressions with `RegExp`, so no matcher is compiled into the wasm.
//!
//! A plain ASCII term without `wholeWord` is matched in the engine instead (`searchRowsView`), without reading the
//! lines into JS: the highlight-all pass, `findNext` past the line it starts on, and the line `findPrevious` stops on
//! past its own, whose last match is then taken as above. Its matches are the addon's: with its quirks left out
//! (whole words, regular expressions) both find each occurrence after the previous one, and the engine lowercases
//! the characters that lowercase to ASCII (U+212A, U+0130) as `toLowerCase` does. Where they could still differ the
//! search goes the JS way: the host lines of viewport-only mode, and lines with a match and U+0130, which
//! lowercases to two units and so moves the addon's matches (it reads indices of the lowercased text in the
//! original one).
//!
//! Memory: an addon keeps its term, its options and, with decorations, the matches (up to `highlightLimit`, 16 bytes
//! each), in rows that stay put as the scrollback trims. Lines are read for one search and dropped.
//!
//! The selected match is the terminal's selection (`select`), and the next search starts from the selection, whatever
//! made it (`getSelectionPosition`), like the addon.
//!
//! Viewport-only mode ([`crate::remote`]): a search that must read host lines the terminal does not hold returns
//! false, fetches them a page of [`PAGE`] lines at a time and starts over until it is done, then selects and scrolls to
//! the match and fires `onDidChangeResults` (also without decorations). A page without any match of the term is
//! remembered as empty; at most [`HELD_PAGES`] pages with matches are held, as text. Logical lines are cut at page
//! boundaries and where the engine's own lines begin. With decorations, only held lines are highlighted.
use std::cell::{Cell, OnceCell, RefCell};
use std::rc::{Rc, Weak};

use js_sys::{Array, Function, JsString, Object, Reflect, Uint32Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::emitter::{Disposable, Emitter};
use crate::engine::{attr, get, Engine, LocalEngineView};
use crate::events;
use crate::remote::{self, Row};
use crate::render::Highlight;
use crate::terminal::{Shared, Terminal};
use crate::timer;

/// `@xterm/addon-search` `NON_WORD_CHARACTERS`.
const NON_WORD: &[u8] = b" ~!@#$%^&*()+`-=[]{}|\\;:\"',./<>?";
const DEFAULT_HIGHLIGHT_LIMIT: u32 = 1000;
/// With decorations, matches are searched again this long after output or a resize.
const UPDATE_DELAY_MS: i32 = 200;
/// Bit 31 of a `searchLine` cell: the unit is a wide character.
const CELL_WIDE: u32 = 1 << 31;
/// Host lines fetched at a time by a search in viewport-only mode.
const PAGE: u64 = 256;
/// Pages with matches a search keeps (pages without any are kept as one flag each).
const HELD_PAGES: usize = 4;

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
export interface ISearchOptions {
  regex?: boolean;
  wholeWord?: boolean;
  caseSensitive?: boolean;
  incremental?: boolean;
  decorations?: ISearchDecorationOptions;
}
export interface ISearchDecorationOptions {
  matchBackground?: string;
  matchBorder?: string;
  matchOverviewRuler: string;
  activeMatchBackground?: string;
  activeMatchBorder?: string;
  activeMatchColorOverviewRuler: string;
}
export interface ISearchResultChangeEvent {
  resultIndex: number;
  resultCount: number;
}
export interface ISearchAddonOptions {
  highlightLimit: number;
}
"#;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = RegExp)]
    type Pattern;
    /// `RegExp(source, flags)`; a bad pattern throws, out of `findNext`, like the addon.
    #[wasm_bindgen(catch, js_name = RegExp)]
    fn pattern(source: &JsString, flags: &str) -> Result<Pattern, JsValue>;
    #[wasm_bindgen(method, catch)]
    fn exec(this: &Pattern, text: &JsString) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(method, getter = lastIndex)]
    fn last_index(this: &Pattern) -> u32;
    #[wasm_bindgen(method, setter = lastIndex)]
    fn set_last_index(this: &Pattern, index: u32);

    /// A JS string, for `indexOf` with a JS string to look for (js-sys takes a Rust `&str`).
    #[wasm_bindgen(js_name = String)]
    type Text;
    #[wasm_bindgen(method, js_name = indexOf)]
    fn index_of(this: &Text, search: &JsString, from: i32) -> i32;
    #[wasm_bindgen(method, js_name = lastIndexOf)]
    fn last_index_of(this: &Text, search: &JsString, from: i32) -> i32;
}

#[derive(Clone, Default)]
struct Decorations {
    match_background: Option<String>,
    match_border: Option<String>,
    active_match_background: Option<String>,
    active_match_border: Option<String>,
}

#[derive(Clone, Default)]
struct SearchOptions {
    regex: bool,
    whole_word: bool,
    case_sensitive: bool,
    decorations: Option<Decorations>,
}

impl SearchOptions {
    /// `None` for anything but an object, like the addon's `searchOptions?.`.
    fn parse(value: &JsValue) -> Option<Self> {
        if !value.is_object() {
            return None;
        }
        let decorations = get(value, "decorations");
        let color = |key: &str| get(&decorations, key).as_string();
        Some(Self {
            regex: get(value, "regex").is_truthy(),
            whole_word: get(value, "wholeWord").is_truthy(),
            case_sensitive: get(value, "caseSensitive").is_truthy(),
            decorations: decorations.is_truthy().then(|| Decorations {
                match_background: color("matchBackground"),
                match_border: color("matchBorder"),
                active_match_background: color("activeMatchBackground"),
                active_match_border: color("activeMatchBorder"),
            }),
        })
    }
}

/// A match as the search engine finds it: buffer row, column and length in cells (running on into later rows).
#[derive(Clone, Copy, PartialEq)]
struct FoundMatch {
    row: i32,
    col: u32,
    size: u32,
}

/// A match kept across searches: `row` counts from the first line that ever entered the scrollback, so the buffer
/// row is `row - trimmed lines` whenever it is read.
#[derive(Clone, Copy, PartialEq)]
struct Hit {
    row: f64,
    col: u32,
    size: u32,
}

impl Hit {
    fn new(found: FoundMatch, trimmed: f64) -> Self {
        Self {
            row: f64::from(found.row) + trimmed,
            col: found.col,
            size: found.size,
        }
    }
}

/// The terminal's `getSelectionPosition()`: buffer cells, end exclusive.
#[derive(Clone, Copy)]
struct SelectionPosition {
    start: (u32, i32),
    end: (u32, i32),
}

impl SelectionPosition {
    fn of(shared: &Rc<Shared>) -> Option<Self> {
        let (start, end) = crate::selection::selection_position(shared)?;
        Some(Self {
            start: (start[0] as u32, start[1] as i32),
            end: (end[0] as u32, end[1] as i32),
        })
    }
}

/// One logical line, as the addon's line cache holds it.
struct Line {
    /// Its first buffer row.
    first: i32,
    rows: i32,
    text: JsString,
    /// Lower-cased once, for plain searches that ignore case.
    lower: OnceCell<JsString>,
    /// The cell of each UTF-16 unit, counted from the start of `first`; [`CELL_WIDE`] for wide characters.
    cells: Vec<u32>,
}

/// `searchPosition` of the addon: `col` runs on past the row into the rows its line wraps into.
struct Pos {
    row: i32,
    col: u32,
}

/// A host history page a search fetched: no match of the term at all, or its logical lines.
enum Page {
    Empty,
    Lines(Vec<Rc<Line>>),
}

/// Host history pages of one term and options, by absolute page (`(dropped + row) / PAGE`).
#[derive(Default)]
struct HistoryPages {
    /// Term, regex, case sensitivity, remote generation and dropped host lines the pages were read for.
    key: Option<(String, bool, bool, u32, u64)>,
    pages: std::collections::HashMap<u64, Page>,
    /// Pages with lines, least recently fetched first.
    held: Vec<u64>,
    /// Bumped by every search; a history search that sees another value stops.
    token: u32,
}

impl HistoryPages {
    /// Forgets the pages unless they were read for this term, options and history.
    fn keep_for(&mut self, key: (String, bool, bool, u32, u64)) {
        if self.key.as_ref() != Some(&key) {
            self.key = Some(key);
            self.pages.clear();
            self.held.clear();
        }
    }

    fn insert(&mut self, page: u64, content: Page) {
        if matches!(content, Page::Lines(_)) {
            self.held.retain(|&p| p != page);
            self.held.push(page);
            if self.held.len() > HELD_PAGES {
                let old = self.held.remove(0);
                self.pages.remove(&old);
            }
        }
        self.pages.insert(page, content);
    }
}

/// One search over the engine (SearchEngine.ts), reading each line once.
struct Search<'a> {
    engine: &'a Engine,
    cols: u32,
    /// Buffer rows above the engine's first line: its line `l` is row `history + l`.
    history: i32,
    /// Host lines above the engine's own (viewport-only mode), read from `pages`.
    remote: i32,
    dropped: u64,
    pages: Option<&'a HistoryPages>,
    /// The first host row the search needed and did not have: its result does not count.
    missing: Option<i32>,
    /// One past the last buffer row (`baseY + rows`).
    end: i32,
    term: &'a JsString,
    /// What is looked for in plain searches: the term, lower-cased when case is ignored.
    needle: JsString,
    regex: Option<Pattern>,
    fold_case: bool,
    whole_word: bool,
    line: Option<Rc<Line>>,
    /// The `searchRowsView` options when the engine matches the term (module docs).
    engine_options: Option<Object>,
}

impl<'a> Search<'a> {
    fn new(
        view: &'a LocalEngineView,
        pages: Option<&'a HistoryPages>,
        term: &'a JsString,
        options: &SearchOptions,
    ) -> Result<Self, JsValue> {
        let engine = view.engine();
        let flags = if options.case_sensitive { "g" } else { "gi" };
        let regex = if options.regex {
            Some(pattern(term, flags)?)
        } else {
            None
        };
        let fold_case = !options.regex && !options.case_sensitive;
        let history = view.history_length() as i32;
        let remote = view.remote_lines() as i32;
        let engine_options = (!options.regex
            && !options.whole_word
            && remote == 0
            && term.length() > 0
            && term.iter().all(|unit| unit < 0x80))
        .then(|| {
            let o = Object::new();
            let _ = Reflect::set(&o, &"caseSensitive".into(), &options.case_sensitive.into());
            let _ = Reflect::set(&o, &"keepLength".into(), &true.into());
            o
        });
        Ok(Self {
            engine,
            cols: engine.cols().max(1),
            history,
            remote,
            dropped: view.remote().map_or(0, |r| r.dropped()),
            pages: pages.filter(|_| remote > 0),
            missing: None,
            end: history + engine.rows() as i32,
            term,
            needle: if fold_case {
                term.to_lower_case()
            } else {
                term.clone()
            },
            regex,
            fold_case,
            whole_word: options.whole_word,
            line: None,
            engine_options,
        })
    }

    /// The first `limit` matches in the logical lines of buffer rows `first..=last` that touch those rows, matched in
    /// the engine; `None` where the addon's matching may differ (module docs).
    fn engine_matches(&self, first: i32, last: i32, limit: u32) -> Option<Vec<FoundMatch>> {
        let options = self.engine_options.as_ref()?;
        let (first, last) = (first - self.history, last - self.history);
        let words = self
            .engine
            .search_rows_view(self.term, options, first, last, limit)
            .ok()??
            .to_vec();
        Some(
            words
                .chunks_exact(4)
                .map(|m| FoundMatch {
                    row: m[0] + self.history,
                    col: m[1] as u32,
                    size: ((m[2] - m[0]) as u32 * self.cols + m[3] as u32)
                        .saturating_sub(m[1] as u32),
                })
                .collect(),
        )
    }

    /// The last buffer row in `lo..=hi` a match touches, `Some(None)` for none: single-match engine searches over
    /// ranges ending at `hi`, doubling until one finds a match, then halving. Each search stops above the rows
    /// already known to have none (a match touching them would have been found there), so each row is read about
    /// once. `None` where the engine does not match the term.
    fn last_touched_row(&self, lo: i32, hi: i32) -> Option<Option<i32>> {
        let touched = |from: i32, to: i32| Some(!self.engine_matches(from, to, 1)?.is_empty());
        // `yes` has a match in `yes..=hi`, `no` none in `no..=hi`
        let (mut yes, mut no) = (None, hi + 1);
        let mut span = 1;
        while yes.is_none() && no > lo {
            let from = hi.saturating_sub(span - 1).max(lo);
            if touched(from, no - 1)? {
                yes = Some(from);
            } else {
                no = from;
            }
            span = span.saturating_mul(2);
        }
        let Some(mut yes) = yes else {
            return Some(None);
        };
        while no - yes > 1 {
            let mid = yes + (no - yes) / 2;
            if touched(mid, no - 1)? {
                yes = mid;
            } else {
                no = mid;
            }
        }
        Some(Some(yes))
    }

    /// The last match of the logical line buffer row `row` is on, as `_findInLine` going backwards finds it there.
    fn last_in_line(&mut self, row: i32) -> Result<Option<FoundMatch>, JsValue> {
        let Some(line) = self.line(row) else {
            return Ok(None);
        };
        let mut pos = Pos {
            row: line.first,
            col: u32::MAX,
        };
        self.find_in_line(&mut pos, true)
    }

    /// The logical line buffer row `row` is on; `None` outside the buffer.
    fn line(&mut self, row: i32) -> Option<Rc<Line>> {
        if let Some(line) = &self.line {
            if row >= line.first && row < line.first + line.rows {
                return Some(line.clone());
            }
        }
        if row < 0 || row >= self.end {
            return None;
        }
        if row < self.remote {
            return self.host_line(row);
        }
        let value = self.engine.search_line(row - self.history).ok()?;
        if !value.is_object() {
            return None;
        }
        let line = Rc::new(Line {
            first: get(&value, "first").as_f64()? as i32 + self.history,
            rows: get(&value, "rows").as_f64()?.max(1.0) as i32,
            text: get(&value, "text").dyn_into().ok()?,
            lower: OnceCell::new(),
            cells: get(&value, "cells")
                .dyn_into::<Uint32Array>()
                .ok()?
                .to_vec(),
        });
        self.line = Some(line.clone());
        Some(line)
    }

    /// Host row `row`'s logical line from the fetched pages; notes it as missing when its page is not held.
    fn host_line(&mut self, row: i32) -> Option<Rc<Line>> {
        let abs = self.dropped + row as u64;
        let page = match self.pages.and_then(|pages| pages.pages.get(&(abs / PAGE))) {
            Some(page) => page,
            None => {
                self.missing.get_or_insert(row);
                return None;
            }
        };
        let line = match page {
            Page::Lines(lines) => lines
                .iter()
                .find(|l| row >= l.first && row < l.first + l.rows)?
                .clone(),
            // One line of nothing over the page's rows.
            Page::Empty => {
                let first = ((abs / PAGE * PAGE).saturating_sub(self.dropped)) as i32;
                let end = (((abs / PAGE + 1) * PAGE - self.dropped) as i32).min(self.remote);
                Rc::new(Line {
                    first,
                    rows: (end - first).max(1),
                    text: JsString::from(""),
                    lower: OnceCell::new(),
                    cells: Vec::new(),
                })
            }
        };
        self.line = Some(line.clone());
        Some(line)
    }

    /// Whether the term occurs anywhere in `text` (whole words or not): pages without it need not be kept.
    fn occurs_in(&self, text: &JsString) -> Result<bool, JsValue> {
        Ok(match &self.regex {
            Some(regex) => {
                regex.set_last_index(0);
                !regex.exec(text)?.is_null()
            }
            None => {
                let text = if self.fold_case {
                    text.to_lower_case()
                } else {
                    text.clone()
                };
                text.unchecked_ref::<Text>().index_of(&self.needle, 0) >= 0
            }
        })
    }

    /// `SearchEngine.find`: the first match at or after (`row`, `col`), down to the bottom of the screen.
    fn find(&mut self, row: i32, col: u32) -> Result<Option<FoundMatch>, JsValue> {
        if col >= self.cols {
            // The addon throws; its callers never get here.
            return Ok(None);
        }
        let mut pos = Pos { row, col };
        let mut result = self.find_in_line(&mut pos, false)?;
        let mut y = row + 1;
        while result.is_none() && y < self.end {
            pos = Pos { row: y, col: 0 };
            result = self.find_in_line(&mut pos, false)?;
            y += 1;
        }
        Ok(result)
    }

    /// Every match from the top, as `_highlightAllMatches` collects them.
    fn find_all(&mut self, limit: u32) -> Result<Vec<FoundMatch>, JsValue> {
        if let Some(results) = self.engine_matches(0, self.end - 1, limit) {
            return Ok(results);
        }
        let mut results = Vec::new();
        let mut previous: Option<(i32, u32)> = None;
        let mut result = self.find(0, 0)?;
        while let Some(found) = result {
            if previous == Some((found.row, found.col)) || results.len() >= limit as usize {
                break;
            }
            previous = Some((found.row, found.col));
            results.push(found);
            let mut col = found.col.saturating_add(found.size);
            let mut row = found.row;
            if col >= self.cols {
                row += (col / self.cols) as i32;
                col %= self.cols;
            }
            result = self.find(row, col)?;
        }
        Ok(results)
    }

    /// `findNextWithSelection`, after the selection was read and cleared.
    fn find_next(
        &mut self,
        selection: Option<SelectionPosition>,
        same_term: bool,
    ) -> Result<Option<FoundMatch>, JsValue> {
        let (start_col, start_row) = match selection {
            Some(s) if same_term => s.end,
            Some(s) => s.start,
            None => (0, 0),
        };
        let mut pos = Pos {
            row: start_row,
            col: start_col,
        };
        let mut result = self.find_in_line(&mut pos, false)?;
        if result.is_none() {
            if let Some(result) = self.find_next_in_engine(start_row, selection)? {
                return Ok(result);
            }
        }
        let mut y = start_row + 1;
        while result.is_none() && y < self.end {
            pos = Pos { row: y, col: 0 };
            result = self.find_in_line(&mut pos, false)?;
            y += 1;
        }
        // Wrap around to the top.
        if result.is_none() && start_row != 0 {
            for y in 0..start_row {
                pos = Pos { row: y, col: 0 };
                result = self.find_in_line(&mut pos, false)?;
                if result.is_some() {
                    break;
                }
            }
        }
        // Only one match: the selected one again.
        if let (None, Some(s)) = (result, selection) {
            pos = Pos {
                row: s.start.1,
                col: 0,
            };
            result = self.find_in_line(&mut pos, false)?;
        }
        Ok(result)
    }

    /// The rest of `find_next` once the line `start_row` is on has no match after the start, with the engine finding
    /// the first match below that line and, wrapping around, above it; `None` where the engine does not match the
    /// term.
    fn find_next_in_engine(
        &mut self,
        start_row: i32,
        selection: Option<SelectionPosition>,
    ) -> Result<Option<Option<FoundMatch>>, JsValue> {
        let Some(line) = self
            .engine_options
            .is_some()
            .then(|| self.line(start_row))
            .flatten()
        else {
            return Ok(None);
        };
        let below = line.first + line.rows;
        let Some(found) = self.engine_matches(below, self.end - 1, 1) else {
            return Ok(None);
        };
        let mut result = found.first().copied();
        // Wrap around to the top: the addon goes over the lines that start above `start_row`, so the start line
        // only when `start_row` continues it.
        if result.is_none() && start_row != 0 {
            let Some(found) = self.engine_matches(0, below - 1, 1) else {
                return Ok(None);
            };
            result = found
                .first()
                .copied()
                .filter(|m| m.row < line.first || line.first < start_row);
        }
        // Only one match: the selected one again.
        if let (None, Some(s)) = (result, selection) {
            let mut pos = Pos {
                row: s.start.1,
                col: 0,
            };
            result = self.find_in_line(&mut pos, false)?;
        }
        Ok(Some(result))
    }

    /// The rest of `find_previous` once the start cell (`start_row`, `col`) has no match before it in its row, with
    /// the engine finding the line of the last match above the start line and, wrapping around, from the bottom;
    /// `None` where the engine does not match the term.
    fn find_previous_in_engine(
        &mut self,
        start_row: i32,
        col: u32,
    ) -> Result<Option<Option<FoundMatch>>, JsValue> {
        let Some(line) = self
            .engine_options
            .is_some()
            .then(|| self.line(start_row))
            .flatten()
        else {
            return Ok(None);
        };
        // The rows of the start line above `start_row`, as the addon goes up them
        let mut pos = Pos {
            row: start_row,
            col: col.max(self.cols),
        };
        for y in (line.first..start_row).rev() {
            pos.row = y;
            if let Some(found) = self.find_in_line(&mut pos, true)? {
                return Ok(Some(Some(found)));
            }
        }
        let Some(above) = self.last_touched_row(0, line.first - 1) else {
            return Ok(None);
        };
        if let Some(row) = above {
            return self.last_in_line(row).map(Some);
        }
        // Wrap around to the bottom, down to `start_row`: the start line only when it starts there.
        let bottom = self.end - 1;
        if start_row == bottom {
            return Ok(Some(None));
        }
        let lo = if line.first == start_row {
            line.first
        } else {
            line.first + line.rows
        };
        match self.last_touched_row(lo, bottom) {
            None => Ok(None),
            Some(None) => Ok(Some(None)),
            Some(Some(row)) => self.last_in_line(row).map(Some),
        }
    }

    /// `findPreviousWithSelection`, after the selection was read and cleared.
    fn find_previous(
        &mut self,
        selection: Option<SelectionPosition>,
        same_term: bool,
    ) -> Result<Option<FoundMatch>, JsValue> {
        let bottom = self.end - 1;
        let mut start_row = bottom;
        let mut pos = Pos {
            row: start_row,
            col: self.cols,
        };
        let mut result = None;
        if let Some(s) = selection {
            start_row = s.start.1;
            pos = Pos {
                row: s.start.1,
                col: s.start.0,
            };
            if !same_term {
                // Try to grow the selection to the right first.
                result = self.find_in_line(&mut pos, false)?;
                if result.is_none() {
                    start_row = s.end.1;
                    pos = Pos {
                        row: s.end.1,
                        col: s.end.0,
                    };
                }
            }
        }
        if result.is_none() {
            result = self.find_in_line(&mut pos, true)?;
        }
        if result.is_none() {
            if let Some(result) = self.find_previous_in_engine(start_row, pos.col)? {
                return Ok(result);
            }
        }
        if result.is_none() {
            pos.col = pos.col.max(self.cols);
            for y in (0..start_row).rev() {
                pos.row = y;
                result = self.find_in_line(&mut pos, true)?;
                if result.is_some() {
                    break;
                }
            }
        }
        // Wrap around to the bottom.
        if result.is_none() && start_row != bottom {
            let mut y = bottom;
            while y >= start_row {
                pos.row = y;
                result = self.find_in_line(&mut pos, true)?;
                if result.is_some() {
                    break;
                }
                y -= 1;
            }
        }
        Ok(result)
    }

    /// `_findInLine`: searches the logical line `pos.row` starts, from (forwards) or before (`reverse`) `pos.col`.
    /// A row that continues a wrapped line is searched from its line's first row going forwards, and skipped
    /// (widening `pos.col` to cover it) going backwards.
    #[inline(never)]
    fn find_in_line(
        &mut self,
        pos: &mut Pos,
        reverse: bool,
    ) -> Result<Option<FoundMatch>, JsValue> {
        let Some(line) = self.line(pos.row) else {
            return Ok(None);
        };
        if pos.row > line.first {
            if reverse {
                pos.col = pos.col.saturating_add(self.cols);
                return Ok(None);
            }
            pos.col = pos
                .col
                .saturating_add(((pos.row - line.first) as u32).saturating_mul(self.cols));
            pos.row = line.first;
        }
        let text = if self.fold_case {
            line.lower.get_or_init(|| line.text.to_lower_case()).clone()
        } else {
            line.text.clone()
        };
        let offset = line
            .cells
            .iter()
            .take_while(|&&cell| cell & !CELL_WIDE < pos.col)
            .count() as u32;
        let (index, len) = match &self.regex {
            Some(regex) => {
                let mut found = (-1, 0);
                if reverse {
                    // The last match in 0..offset.
                    let head = text.slice(0, offset);
                    regex.set_last_index(0);
                    loop {
                        let m = regex.exec(&head)?;
                        if m.is_null() {
                            break;
                        }
                        let n = matched_len(&m);
                        let end = regex.last_index();
                        found = (end.saturating_sub(n) as i64, n);
                        regex.set_last_index((end + 1).saturating_sub(n));
                    }
                } else {
                    regex.set_last_index(0);
                    let m = regex.exec(&text.slice(offset, text.length()))?;
                    let n = matched_len(&m);
                    if n > 0 {
                        found = ((offset + regex.last_index() - n) as i64, n);
                    }
                }
                found
            }
            None => {
                let n = self.needle.length();
                let index = if !reverse {
                    text.unchecked_ref::<Text>()
                        .index_of(&self.needle, offset as i32)
                } else if offset >= n {
                    text.unchecked_ref::<Text>()
                        .last_index_of(&self.needle, (offset - n) as i32)
                } else {
                    -1
                };
                (index as i64, self.term.length())
            }
        };
        if index < 0 {
            return Ok(None);
        }
        let index = index as usize;
        if self.whole_word && !is_whole_word(&text, index as u32, len) {
            return Ok(None);
        }
        let Some(&start) = line.cells.get(index) else {
            return Ok(None);
        };
        let start = start & !CELL_WIDE;
        let end = match (index + len as usize).checked_sub(1) {
            Some(last) if len > 0 => {
                let cell = line
                    .cells
                    .get(last)
                    .or(line.cells.last())
                    .copied()
                    .unwrap_or(start);
                (cell & !CELL_WIDE) + if cell & CELL_WIDE != 0 { 2 } else { 1 }
            }
            _ => start,
        };
        Ok(Some(FoundMatch {
            row: line.first + (start / self.cols) as i32,
            col: start % self.cols,
            size: end.saturating_sub(start),
        }))
    }
}

/// What one search pass gave.
enum Found {
    Hit(FoundMatch),
    None,
    /// The first host row it needed and the terminal does not hold.
    Missing(i32),
}

/// A `findNext`/`findPrevious` call, kept while a history search goes on.
struct Find {
    term: JsString,
    options: SearchOptions,
    forward: bool,
    no_scroll: bool,
    selection: Option<SelectionPosition>,
    same_term: bool,
}

/// The fetch of the history page holding a host row.
struct PageFetch {
    provider: Object,
    page: u64,
    start: u32,
    count: u32,
    cols: u32,
    generation: u32,
}

/// Pages are read for one term, options and host history.
fn page_key(
    view: &LocalEngineView,
    term: &JsString,
    options: &SearchOptions,
) -> (String, bool, bool, u32, u64) {
    let remote = view.remote();
    (
        String::from(term),
        options.regex,
        options.case_sensitive,
        remote.map_or(0, |r| r.generation()),
        remote.map_or(0, |r| r.dropped()),
    )
}

/// The page of host row `row`, clipped to the host lines; `None` when the mode is off.
fn page_request(shared: &Rc<Shared>, row: i32) -> Option<PageFetch> {
    let core = shared.core.try_borrow().ok()?;
    if core.disposed {
        return None;
    }
    let remote = core.view.remote()?;
    let lines = u64::from(core.view.remote_lines());
    let abs = remote.dropped() + u64::try_from(row).ok()?;
    let page = abs / PAGE;
    let start = (page * PAGE).saturating_sub(remote.dropped());
    let end = ((page + 1) * PAGE - remote.dropped()).min(lines);
    (start < end).then(|| PageFetch {
        provider: remote.provider().clone(),
        page,
        start: start as u32,
        count: (end - start) as u32,
        cols: core.view.engine().cols(),
        generation: remote.generation(),
    })
}

/// The logical lines of fetched host rows starting at buffer row `first`, as the engine's `searchLine` reads its
/// own (crates/wongtty/src/search.rs `read_line`): cut where the page ends.
fn page_lines(rows: &[Option<Row>], first: i32, cols: u32) -> Vec<Rc<Line>> {
    let mut lines = Vec::new();
    let mut i = 0;
    let mut row_cells = vec![0u32; cols as usize * 4];
    while i < rows.len() {
        let start = i;
        let (mut units, mut cells) = (Vec::<u16>::new(), Vec::new());
        let mut keep = 0;
        loop {
            let row = rows[i].as_ref();
            match row {
                Some(row) => row.copy_cells(&mut row_cells),
                None => crate::engine::blank_row(&mut row_cells),
            }
            let base = (i - start) as u32 * cols;
            for (col, cell) in row_cells.chunks_exact(4).enumerate() {
                let attrs = cell[3];
                if attrs & (attr::WIDE_SPACER | attr::LEADING_WIDE_SPACER) != 0 {
                    continue;
                }
                let at = (base + col as u32)
                    | if attrs & attr::WIDE != 0 {
                        CELL_WIDE
                    } else {
                        0
                    };
                let marks = row.and_then(|r| {
                    r.extras
                        .iter()
                        .find(|e| e.col == col as u32)
                        .and_then(|e| e.text.as_deref())
                });
                let text = match marks {
                    Some(text) => text.to_string(),
                    None => char::from_u32(cell[0])
                        .filter(|&c| c != '\0')
                        .unwrap_or(' ')
                        .to_string(),
                };
                for unit in text.encode_utf16() {
                    units.push(unit);
                    cells.push(at);
                }
                let empty = marks.is_none() && text == " " && attrs & attr::TYPED_SPACE == 0;
                if !empty {
                    keep = units.len();
                }
            }
            i += 1;
            if !row.is_some_and(|r| r.wrapped) || i >= rows.len() {
                break;
            }
        }
        units.truncate(keep);
        cells.truncate(keep);
        lines.push(Rc::new(Line {
            first: first + start as i32,
            rows: (i - start) as i32,
            text: JsString::from_char_code(&units),
            lower: OnceCell::new(),
            cells,
        }));
    }
    lines
}

/// Length of `exec`'s whole match, 0 for no match.
fn matched_len(m: &JsValue) -> u32 {
    if !Array::is_array(m) {
        return 0;
    }
    Array::from(m)
        .get(0)
        .dyn_into::<JsString>()
        .map_or(0, |s| s.length())
}

/// `_isWholeWord`: a non-word character or the line end on both sides.
fn is_whole_word(text: &JsString, at: u32, len: u32) -> bool {
    let non_word = |i: u32| {
        let unit = text.char_code_at(i);
        unit < 128.0 && NON_WORD.contains(&(unit as u8))
    };
    (at == 0 || non_word(at - 1)) && (at + len == text.length() || non_word(at + len))
}

/// The `onWriteParsed`/`onResize` and `onScroll` subscriptions of a loaded addon, removed when dropped.
struct Listeners {
    subscriptions: Vec<Disposable>,
    _closures: [Closure<dyn FnMut()>; 2],
}

impl Drop for Listeners {
    fn drop(&mut self) {
        for subscription in &self.subscriptions {
            subscription.dispose();
        }
    }
}

#[derive(Default)]
struct Inner {
    terminal: Weak<Shared>,
    listeners: Option<Listeners>,
    update: Option<timer::Timeout>,
    cached_term: Option<JsString>,
    last_options: Option<SearchOptions>,
    /// The matches `_highlightAllMatches` found (SearchResultTracker).
    results: Vec<Hit>,
    /// The active match (the selected decoration).
    active: Option<Hit>,
    colors: Option<Decorations>,
    /// The renderer was last given something to draw.
    painted: bool,
}

struct SearchState {
    id: u32,
    highlight_limit: u32,
    inner: RefCell<Inner>,
    /// Host history pages read for the current term (viewport-only mode).
    history: RefCell<HistoryPages>,
    before_search: Rc<Emitter>,
    after_search: Rc<Emitter>,
    did_change_results: Rc<Emitter>,
}

thread_local! {
    /// For `loadAddon`, which gets the JS object: the addon is found by the id it carries.
    static ADDONS: RefCell<Vec<Weak<SearchState>>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: Cell<u32> = const { Cell::new(1) };
}

/// `loadAddon` for the built-in `SearchAddon`: activates it on the terminal and returns true, or false for any
/// other addon.
pub fn load(terminal: &Terminal, addon: &JsValue) -> bool {
    let Some(id) = get(addon, "__wongttySearchAddon").as_f64() else {
        return false;
    };
    let state = ADDONS.with(|a| {
        a.borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .find(|s| f64::from(s.id) == id)
    });
    match state {
        Some(state) => {
            state.activate(&terminal.shared);
            true
        }
        None => false,
    }
}

impl SearchState {
    fn terminal(&self) -> Option<Rc<Shared>> {
        self.inner.borrow().terminal.upgrade()
    }

    /// `activate`: follows output and resizes (to search again) and scrolling (to repaint).
    fn activate(self: &Rc<Self>, shared: &Rc<Shared>) {
        let weak = Rc::downgrade(self);
        let on_change = Closure::<dyn FnMut()>::new(move || {
            if let Some(state) = weak.upgrade() {
                state.update_matches();
                state.paint();
            }
        });
        let weak = Rc::downgrade(self);
        let on_scroll = Closure::<dyn FnMut()>::new(move || {
            if let Some(state) = weak.upgrade() {
                state.paint();
            }
        });
        let change: &Function = on_change.as_ref().unchecked_ref();
        let scroll: &Function = on_scroll.as_ref().unchecked_ref();
        let e = &shared.events;
        let listeners = Listeners {
            subscriptions: vec![
                e.write_parsed.event(change.clone()),
                e.resize.event(change.clone()),
                e.scroll.event(scroll.clone()),
            ],
            _closures: [on_change, on_scroll],
        };
        let old = {
            let mut inner = self.inner.borrow_mut();
            inner.terminal = Rc::downgrade(shared);
            inner.listeners.replace(listeners)
        };
        drop(old);
    }

    /// `_updateMatches`: with decorations, searches the cached term again shortly after output or a resize.
    fn update_matches(self: &Rc<Self>) {
        let mut inner = self.inner.borrow_mut();
        inner.update = None;
        let decorated = inner
            .last_options
            .as_ref()
            .is_some_and(|o| o.decorations.is_some());
        if inner.cached_term.is_none() || !decorated {
            return;
        }
        let weak = Rc::downgrade(self);
        inner.update = Some(timer::Timeout::new(UPDATE_DELAY_MS, move || {
            let Some(state) = weak.upgrade() else {
                return;
            };
            let (term, options) = {
                let mut inner = state.inner.borrow_mut();
                inner.update = None;
                (inner.cached_term.take(), inner.last_options.clone())
            };
            if let Some(term) = term {
                if let Err(error) = state.search(term.into(), options, false, true) {
                    web_sys::console::error_1(&error);
                }
            }
        }));
    }

    fn clear_decorations(&self, retain_cached_term: bool) {
        let mut inner = self.inner.borrow_mut();
        inner.active = None;
        inner.results.clear();
        if !retain_cached_term {
            inner.cached_term = None;
        }
    }

    /// `findNext` (`forward`) or `findPrevious`.
    fn search(
        self: &Rc<Self>,
        term: JsValue,
        options: Option<SearchOptions>,
        forward: bool,
        no_scroll: bool,
    ) -> Result<bool, JsValue> {
        let Some(shared) = self.terminal() else {
            return Err(js_sys::Error::new("Cannot use addon until it has been loaded").into());
        };
        self.before_search.fire(&[]);
        {
            let mut history = self.history.borrow_mut();
            history.token = history.token.wrapping_add(1);
        }
        let term: JsString = term.dyn_into().unwrap_or_else(|_| JsString::from(""));
        let decorations = options.as_ref().and_then(|o| o.decorations.clone());
        let highlight = {
            let mut inner = self.inner.borrow_mut();
            inner.last_options = options.clone();
            decorations.is_some() && inner.cached_term.as_ref() != Some(&term)
        };
        let options = options.unwrap_or_default();
        if highlight {
            self.highlight_all(&shared, &term, &options)?;
        }
        let (found, scroll) = self.find_and_select(&shared, &term, &options, forward, no_scroll)?;
        if let Some(amount) = scroll {
            let _ = Terminal {
                shared: shared.clone(),
            }
            .scroll_lines(amount);
        }
        if decorations.is_some() {
            self.fire_results();
        }
        self.inner.borrow_mut().cached_term = Some(term);
        self.after_search.fire(&[]);
        self.paint();
        Ok(found)
    }

    /// `_highlightAllMatches`.
    fn highlight_all(
        &self,
        shared: &Rc<Shared>,
        term: &JsString,
        options: &SearchOptions,
    ) -> Result<(), JsValue> {
        if term.length() == 0 {
            self.clear_decorations(false);
            return Ok(());
        }
        self.clear_decorations(true);
        let (results, trimmed) = {
            let core = shared.core.borrow();
            if core.disposed {
                return Ok(());
            }
            let mut history = self.history.borrow_mut();
            history.keep_for(page_key(&core.view, term, options));
            let results = Search::new(&core.view, Some(&history), term, options)?
                .find_all(self.highlight_limit)?;
            (results, core.view.trimmed_lines())
        };
        let mut inner = self.inner.borrow_mut();
        inner.results = results.into_iter().map(|f| Hit::new(f, trimmed)).collect();
        if options.decorations.is_some() {
            inner.colors = options.decorations.clone();
        }
        Ok(())
    }

    /// `_findNextAndSelect`/`_findPreviousAndSelect` and `_selectResult`: whether a match was selected, and the
    /// lines to scroll to bring it into view.
    fn find_and_select(
        self: &Rc<Self>,
        shared: &Rc<Shared>,
        term: &JsString,
        options: &SearchOptions,
        forward: bool,
        no_scroll: bool,
    ) -> Result<(bool, Option<i32>), JsValue> {
        let terminal = Terminal {
            shared: shared.clone(),
        };
        if term.length() == 0 {
            terminal.clear_selection();
            self.clear_decorations(false);
            return Ok((false, None));
        }
        // `findNextWithSelection`/`findPreviousWithSelection` read the selection, then clear it.
        let selection = SelectionPosition::of(shared);
        terminal.clear_selection();
        let same_term = self.inner.borrow().cached_term.as_ref() == Some(term);
        let request = Find {
            term: term.clone(),
            options: options.clone(),
            forward,
            no_scroll,
            selection,
            same_term,
        };
        match self.find(shared, &request)? {
            Some(Found::Missing(row)) => {
                // Host lines are needed: go on without blocking, and leave nothing selected until then.
                self.inner.borrow_mut().active = None;
                self.search_history(shared, request, row);
                Ok((false, None))
            }
            Some(found) => Ok(self.select(shared, &request, found)),
            None => Ok((false, None)),
        }
    }

    /// One pass of `findNextWithSelection`/`findPreviousWithSelection` over what the terminal holds: the match,
    /// none, or the first host row it needed and does not have; `None` after dispose.
    fn find(&self, shared: &Rc<Shared>, request: &Find) -> Result<Option<Found>, JsValue> {
        let core = shared.core.borrow();
        if core.disposed {
            return Ok(None);
        }
        let mut history = self.history.borrow_mut();
        history.keep_for(page_key(&core.view, &request.term, &request.options));
        let mut search = Search::new(&core.view, Some(&history), &request.term, &request.options)?;
        let found = if request.forward {
            search.find_next(request.selection, request.same_term)?
        } else {
            search.find_previous(request.selection, request.same_term)?
        };
        Ok(Some(match (search.missing, found) {
            (Some(row), _) => Found::Missing(row),
            (None, Some(hit)) => Found::Hit(hit),
            (None, None) => Found::None,
        }))
    }

    /// `_selectResult`: selects the match (or clears the selection); whether there was one and the lines to scroll
    /// to bring it into view.
    fn select(&self, shared: &Rc<Shared>, request: &Find, found: Found) -> (bool, Option<i32>) {
        let terminal = Terminal {
            shared: shared.clone(),
        };
        let options = &request.options;
        let (trimmed, rows, viewport_y) = {
            let core = shared.core.borrow();
            if core.disposed {
                return (false, None);
            }
            let view = &core.view;
            (
                view.trimmed_lines(),
                view.engine().rows() as i32,
                view.history_length() as i32 - view.display_offset() as i32,
            )
        };
        let no_scroll = request.no_scroll;
        let found = match found {
            Found::Hit(found) => Some(found),
            _ => None,
        };
        // `_selectResult`
        self.inner.borrow_mut().active = None;
        let Some(found) = found else {
            terminal.clear_selection();
            return (false, None);
        };
        terminal.select(found.col.into(), found.row.into(), found.size.into());
        if options.decorations.is_some() {
            let mut inner = self.inner.borrow_mut();
            inner.active = Some(Hit::new(found, trimmed));
            inner.colors = options.decorations.clone();
        }
        let scroll = (!no_scroll && (found.row >= viewport_y + rows || found.row < viewport_y))
            .then(|| found.row - viewport_y - rows / 2);
        (true, scroll)
    }

    /// Goes on with a search that needs host lines: fetches the page of `row`, searches again, until it is done or
    /// another search starts. Then selects and scrolls like `findNext` and fires `onDidChangeResults`.
    fn search_history(self: &Rc<Self>, shared: &Rc<Shared>, request: Find, row: i32) {
        let token = self.history.borrow().token;
        let state = Rc::downgrade(self);
        let terminal = Rc::downgrade(shared);
        wasm_bindgen_futures::spawn_local(async move {
            let mut row = row;
            let mut tried = Vec::new();
            loop {
                let (Some(state), Some(shared)) = (state.upgrade(), terminal.upgrade()) else {
                    return;
                };
                let Some(fetch) = page_request(&shared, row) else {
                    return;
                };
                // A page asked for twice did not help: the host does not have it.
                let found = if !tried.contains(&fetch.page) {
                    tried.push(fetch.page);
                    let rows =
                        remote::fetch_rows(&fetch.provider, fetch.start, fetch.count, fetch.cols)
                            .await;
                    if state.history.borrow().token != token {
                        return;
                    }
                    match rows {
                        Ok(rows) => {
                            state.store_page(&shared, &request, &fetch, &rows);
                            match state.find(&shared, &request) {
                                Ok(found) => found,
                                Err(error) => {
                                    web_sys::console::error_1(&error);
                                    return;
                                }
                            }
                        }
                        Err(error) => {
                            web_sys::console::warn_1(&error);
                            Some(Found::None)
                        }
                    }
                } else {
                    Some(Found::None)
                };
                match found {
                    None => return,
                    Some(Found::Missing(next)) => row = next,
                    Some(found) => {
                        state.finish_history(&shared, &request, found);
                        return;
                    }
                }
            }
        });
    }

    /// Keeps a fetched page: its logical lines when the term occurs in it, else a note that it does not.
    fn store_page(
        &self,
        shared: &Rc<Shared>,
        request: &Find,
        fetch: &PageFetch,
        rows: &[Option<Row>],
    ) {
        let Ok(core) = shared.core.try_borrow() else {
            return;
        };
        if core.disposed {
            return;
        }
        let mut history = self.history.borrow_mut();
        let key = page_key(&core.view, &request.term, &request.options);
        if history.key.as_ref() != Some(&key) || key.3 != fetch.generation {
            return;
        }
        let Ok(search) = Search::new(&core.view, None, &request.term, &request.options) else {
            return;
        };
        let lines = page_lines(rows, fetch.start as i32, search.cols);
        let mut occurs = false;
        for line in &lines {
            match search.occurs_in(&line.text) {
                Ok(true) => {
                    occurs = true;
                    break;
                }
                Ok(false) => {}
                Err(_) => return,
            }
        }
        history.insert(
            fetch.page,
            if occurs {
                Page::Lines(lines)
            } else {
                Page::Empty
            },
        );
    }

    /// Ends a history search like `search` ends a synchronous one.
    fn finish_history(self: &Rc<Self>, shared: &Rc<Shared>, request: &Find, found: Found) {
        if shared.core.borrow().disposed {
            return;
        }
        let (_, scroll) = self.select(shared, request, found);
        if let Some(amount) = scroll {
            let _ = Terminal {
                shared: shared.clone(),
            }
            .scroll_lines(amount);
        }
        self.fire_results();
        self.paint();
    }

    /// `onDidChangeResults` with the active match's index and the match count.
    fn fire_results(&self) {
        let (index, count) = {
            let inner = self.inner.borrow();
            let index = inner
                .active
                .and_then(|a| inner.results.iter().position(|&r| r == a))
                .map_or(-1, |i| i as i32);
            (index, inner.results.len() as u32)
        };
        let event = Object::new();
        let _ = Reflect::set(&event, &"resultIndex".into(), &index.into());
        let _ = Reflect::set(&event, &"resultCount".into(), &count.into());
        self.did_change_results.fire(&[&event.into()]);
    }

    /// Hands the decorations in view to the renderer.
    fn paint(&self) {
        let Some(shared) = self.terminal() else {
            return;
        };
        let Ok(mut inner) = self.inner.try_borrow_mut() else {
            return;
        };
        let Ok(mut core) = shared.core.try_borrow_mut() else {
            return;
        };
        if core.disposed || core.renderer.is_none() {
            return;
        }
        let view = &core.view;
        let (cols, rows, trimmed) = (
            view.engine().cols(),
            view.engine().rows(),
            view.trimmed_lines(),
        );
        let viewport_y = f64::from(view.history_length()) - f64::from(view.display_offset());
        let mut highlights = Vec::new();
        if let Some(colors) = &inner.colors {
            let mut add = |hit: Hit, background: &Option<String>, border: &Option<String>| {
                if background.is_none() && border.is_none() {
                    return;
                }
                // A match that wraps gets one highlight per row (DecorationManager._createResultDecorations).
                let (mut col, mut left) = (hit.col, hit.size);
                let mut y = hit.row - trimmed - viewport_y;
                while left > 0 && col < cols && y < f64::from(rows) {
                    let n = left.min(cols - col);
                    if y >= 0.0 {
                        highlights.push(Highlight {
                            row: y as u32,
                            start: col,
                            end: col + n,
                            background: background.clone(),
                            border: border.clone(),
                            ..Highlight::default()
                        });
                    }
                    col = 0;
                    left -= n;
                    y += 1.0;
                }
            };
            for &hit in &inner.results {
                add(hit, &colors.match_background, &colors.match_border);
            }
            if let Some(hit) = inner.active {
                add(
                    hit,
                    &colors.active_match_background,
                    &colors.active_match_border,
                );
            }
        }
        let painted = !highlights.is_empty();
        if !painted && !inner.painted {
            return;
        }
        inner.painted = painted;
        let renderer = core.renderer.as_mut().unwrap();
        renderer.set_highlights("search", highlights);
        drop(core);
        drop(inner);
        shared.schedule_frame();
    }
}

/// xterm.js `@xterm/addon-search`, built in: load it with `terminal.loadAddon(new SearchAddon())`.
#[wasm_bindgen]
pub struct SearchAddon {
    state: Rc<SearchState>,
}

#[wasm_bindgen]
impl SearchAddon {
    #[wasm_bindgen(constructor)]
    pub fn new(
        #[wasm_bindgen(unchecked_optional_param_type = "Partial<ISearchAddonOptions>")]
        options: Option<Object>,
    ) -> SearchAddon {
        let limit = options
            .map(|o| get(&o, "highlightLimit"))
            .filter(|v| !v.is_undefined() && !v.is_null())
            .map_or(f64::from(DEFAULT_HIGHLIGHT_LIMIT), |v| {
                v.as_f64().unwrap_or(f64::NAN)
            });
        // `results.length >= limit` never holds for NaN.
        let highlight_limit = if limit.is_nan() {
            u32::MAX
        } else {
            limit.ceil().clamp(0.0, f64::from(u32::MAX)) as u32
        };
        let state = Rc::new(SearchState {
            id: NEXT_ID.with(|n| n.replace(n.get().wrapping_add(1))),
            highlight_limit,
            inner: RefCell::default(),
            history: RefCell::default(),
            before_search: Rc::default(),
            after_search: Rc::default(),
            did_change_results: Rc::default(),
        });
        ADDONS.with(|a| a.borrow_mut().push(Rc::downgrade(&state)));
        SearchAddon { state }
    }

    /// `terminal.loadAddon(searchAddon)` calls this.
    pub fn activate(&self, terminal: &Terminal) {
        self.state.activate(&terminal.shared);
    }

    /// Stops following the terminal and clears the decorations; searching still works, like the addon.
    pub fn dispose(&self) {
        let (listeners, update) = {
            let mut inner = self.state.inner.borrow_mut();
            (inner.listeners.take(), inner.update.take())
        };
        drop((listeners, update));
        self.state.clear_decorations(false);
        self.state.paint();
        for emitter in [
            &self.state.before_search,
            &self.state.after_search,
            &self.state.did_change_results,
        ] {
            emitter.clear();
        }
    }

    /// Selects the next match after the selection (from the top without one), wrapping around, and scrolls it into
    /// view. False when there is none; a bad regular expression throws.
    #[wasm_bindgen(js_name = findNext)]
    pub fn find_next(
        &self,
        #[wasm_bindgen(unchecked_param_type = "string")] term: JsValue,
        #[wasm_bindgen(js_name = searchOptions, unchecked_optional_param_type = "ISearchOptions")]
        search_options: JsValue,
    ) -> Result<bool, JsValue> {
        self.state
            .search(term, SearchOptions::parse(&search_options), true, false)
    }

    /// Like `findNext`, backwards from the selection (from the bottom without one).
    #[wasm_bindgen(js_name = findPrevious)]
    pub fn find_previous(
        &self,
        #[wasm_bindgen(unchecked_param_type = "string")] term: JsValue,
        #[wasm_bindgen(js_name = searchOptions, unchecked_optional_param_type = "ISearchOptions")]
        search_options: JsValue,
    ) -> Result<bool, JsValue> {
        self.state
            .search(term, SearchOptions::parse(&search_options), false, false)
    }

    /// Clears the decorations and forgets the term, so the next search starts over from the selection start.
    #[wasm_bindgen(js_name = clearDecorations)]
    pub fn clear_decorations(&self) {
        self.state.clear_decorations(false);
        self.state.paint();
    }

    /// Clears the active match's decoration (meant for the search box's `blur`).
    #[wasm_bindgen(js_name = clearActiveDecoration)]
    pub fn clear_active_decoration(&self) {
        self.state.inner.borrow_mut().active = None;
        self.state.paint();
    }

    #[wasm_bindgen(getter = onBeforeSearch, unchecked_return_type = "IEvent<void>")]
    pub fn on_before_search(&self) -> Function {
        events::emitter_event(&self.state.before_search)
    }

    #[wasm_bindgen(getter = onAfterSearch, unchecked_return_type = "IEvent<void>")]
    pub fn on_after_search(&self) -> Function {
        events::emitter_event(&self.state.after_search)
    }

    /// With decorations: the active match's index among all matches (-1 if none or past `highlightLimit`) and their
    /// count, after each search.
    #[wasm_bindgen(
        getter = onDidChangeResults,
        unchecked_return_type = "IEvent<ISearchResultChangeEvent>"
    )]
    pub fn on_did_change_results(&self) -> Function {
        events::emitter_event(&self.state.did_change_results)
    }

    /// Internal: marks the built-in addon for `loadAddon`.
    #[wasm_bindgen(getter, js_name = __wongttySearchAddon, skip_typescript)]
    pub fn marker(&self) -> u32 {
        self.state.id
    }
}

impl Drop for SearchAddon {
    fn drop(&mut self) {
        let state = Rc::downgrade(&self.state);
        ADDONS.with(|a| {
            if let Ok(mut a) = a.try_borrow_mut() {
                a.retain(|w| w.strong_count() > 0 && !Weak::ptr_eq(w, &state));
            }
        });
    }
}
