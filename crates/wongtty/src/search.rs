//! Text search over the scrollback and the screen, reading the text as `@xterm/addon-search` 0.17 does: a logical
//! line is its rows joined across soft wraps, blank cells read as spaces, the right half of a wide character and the
//! blank a wide character left at the end of a wrapped row are skipped, and the empty cells that end the line are
//! dropped. Matches never span logical lines.
//!
//! Regular expressions are left to the host ([`Engine::search_line`] hands it the text and where each character
//! sits): a regex engine would more than double the WebAssembly build, and a JavaScript host matches with the same
//! `RegExp` xterm.js uses.

use crate::Engine;
use crate::vt::grid::Row;
use crate::vt::term::cell::{Cell as AlacCell, Flags};

/// What ends a word for [`SearchOptions::whole_word`] (`@xterm/addon-search` `NON_WORD_CHARACTERS`).
const NON_WORD: &str = " ~!@#$%^&*()+`-=[]{}|\\;:\"',./<>?";

/// How [`Engine::search_next`] and friends compare text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions {
    /// Off: both sides are lowercased character by character first.
    pub case_sensitive: bool,
    /// A match must have a non-word character (or the end of its line) on both sides.
    pub whole_word: bool,
}

/// A match in cells, lines numbered as in [`Engine::row_text`]. The end is exclusive and lies on `end_line`, so
/// `end_col` is in `1..=cols`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub start_line: i32,
    pub start_col: u16,
    pub end_line: i32,
    pub end_col: u16,
}

/// One logical line as search reads it, for a host that matches the text itself (a JavaScript `RegExp`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchLine {
    /// Its first row.
    pub first: i32,
    pub rows: u16,
    pub text: String,
    /// The cell of each character of `text`: `row * cols + col` counted from `first`, with [`SearchLine::WIDE`]
    /// set for a wide character. The characters of one cell (a base and its combining marks) share it.
    pub cells: Vec<u32>,
}

impl SearchLine {
    pub const WIDE: u32 = 1 << 31;
}

/// A logical line's characters and their cells, lowercased when the search ignores case.
struct Text {
    first: i32,
    rows: i32,
    chars: Vec<char>,
    cells: Vec<u32>,
}

impl Text {
    fn start(&self, i: usize) -> u32 {
        self.cells[i] & !SearchLine::WIDE
    }

    /// The cell just past character `i`.
    fn end(&self, i: usize) -> u32 {
        self.start(i)
            + if self.cells[i] & SearchLine::WIDE != 0 {
                2
            } else {
                1
            }
    }

    fn fold(mut self) -> Self {
        // In place while each character lowercases to one; the first that lowercases to several rebuilds the rest
        for i in 0..self.chars.len() {
            let c = &mut self.chars[i];
            if c.is_ascii() {
                c.make_ascii_lowercase();
                continue;
            }
            let mut lower = c.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(l), None) => *c = l,
                _ => return self.fold_from(i),
            }
        }
        self
    }

    /// `fold` as an ASCII query sees it, without the case tables: only characters that lowercase to ones with ASCII
    /// in them can match it, ASCII letters and two others (`ascii_lowercase_is_ascii_letters_and_two_others`). The
    /// rest stay as they are, which no query character matches.
    fn fold_ascii(mut self) -> Self {
        let mut dotted = None;
        for (i, c) in self.chars.iter_mut().enumerate() {
            if c.is_ascii() {
                c.make_ascii_lowercase();
            } else if *c == '\u{212a}' {
                // KELVIN SIGN
                *c = 'k';
            } else if *c == '\u{130}' {
                dotted = Some(i);
            }
        }
        // İ lowercases to `i` and U+0307 (which is not a word separator, for `whole_word`)
        while let Some(i) = dotted {
            self.chars.splice(i..=i, ['i', '\u{307}']);
            self.cells.insert(i, self.cells[i]);
            dotted = self.chars[..i].iter().rposition(|&c| c == '\u{130}');
        }
        self
    }

    /// Lowercases characters `i..` into new buffers (some lowercase to several characters).
    fn fold_from(self, i: usize) -> Self {
        let mut chars = Vec::with_capacity(self.chars.len() + 1);
        let mut cells = Vec::with_capacity(self.cells.len() + 1);
        chars.extend_from_slice(&self.chars[..i]);
        cells.extend_from_slice(&self.cells[..i]);
        for (c, at) in self.chars[i..].iter().zip(&self.cells[i..]) {
            for l in c.to_lowercase() {
                chars.push(l);
                cells.push(*at);
            }
        }
        Self {
            chars,
            cells,
            ..self
        }
    }

    fn is_match(&self, query: &[char], s: usize, whole_word: bool) -> bool {
        let e = s + query.len();
        // NON_WORD is ASCII
        let separator = |c: char| c.is_ascii() && NON_WORD.as_bytes().contains(&(c as u8));
        self.chars[s..].starts_with(query)
            && (!whole_word
                || ((s == 0 || separator(self.chars[s - 1]))
                    && (e == self.chars.len() || separator(self.chars[e]))))
    }

    /// The first match starting at character `from` or later (`query` is not empty).
    fn next(&self, query: &[char], from: usize, whole_word: bool) -> Option<usize> {
        let last = self.chars.len().checked_sub(query.len())?;
        let mut s = from;
        // Only where the first character matches
        while s <= last {
            s += self.chars[s..=last].iter().position(|&c| c == query[0])?;
            if self.is_match(query, s, whole_word) {
                return Some(s);
            }
            s += 1;
        }
        None
    }

    /// The last match ending at character `before` or earlier.
    fn previous(&self, query: &[char], before: usize, whole_word: bool) -> Option<usize> {
        let last = before.min(self.chars.len()).checked_sub(query.len())?;
        (0..=last)
            .rev()
            .find(|&s| self.is_match(query, s, whole_word))
    }

    fn found(&self, s: usize, len: usize, cols: u32) -> SearchMatch {
        let (start, end) = (self.start(s), self.end(s + len - 1) - 1);
        SearchMatch {
            start_line: self.first + (start / cols) as i32,
            start_col: (start % cols) as u16,
            end_line: self.first + (end / cols) as i32,
            end_col: (end % cols + 1) as u16,
        }
    }
}

/// Whether xterm.js would hold nothing in this cell (an erased or never written blank), which ends of lines drop.
fn is_empty(cell: &AlacCell) -> bool {
    cell.c == ' '
        && !cell.flags.contains(Flags::TYPED_SPACE)
        && cell.zerowidth().is_none_or(|marks| marks.is_empty())
}

/// Appends a row's characters; returns the length the text keeps if the logical line ends here. A row that `ends` the
/// line stops at its last cell that is not empty.
fn push_row(
    row: &Row<AlacCell>,
    base: u32,
    ends: bool,
    chars: &mut Vec<char>,
    cells: &mut Vec<u32>,
) -> usize {
    let mut content = chars.len();
    let cells_in = if ends {
        row[..]
            .iter()
            .rposition(|c| !is_empty(c))
            .map_or(0, |i| i + 1)
    } else {
        row.len()
    };
    for (col, cell) in row[..][..cells_in].iter().enumerate() {
        if cell
            .flags
            .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        {
            continue;
        }
        let wide = if cell.flags.contains(Flags::WIDE_CHAR) {
            SearchLine::WIDE
        } else {
            0
        };
        let at = (base + col as u32) | wide;
        chars.push(cell.c);
        cells.push(at);
        for &mark in cell.zerowidth().unwrap_or_default() {
            chars.push(mark);
            cells.push(at);
        }
        if !is_empty(cell) {
            content = chars.len();
        }
    }
    content
}

/// Lines a search walking down has looked at and skipped (`Engine::lacks`). Looking at a line costs about its bytes
/// and reading it about its cells, so where few lines are skipped (text with a colour for every cell takes 12 bytes
/// a cell), looking costs more than it saves: past 64 lines a search stops looking unless the cells of the lines it
/// skipped are at least a 32nd of the bytes of all lines it looked at.
#[derive(Default)]
struct Skips {
    looked: u32,
    bytes: usize,
    skipped: usize,
    /// The rows of the line skipped last.
    rows: i32,
}

impl Skips {
    #[inline]
    fn lacks(
        &mut self,
        engine: &Engine,
        line: i32,
        required: Option<u128>,
        options: SearchOptions,
    ) -> bool {
        let cols = engine.cols() as usize;
        let Some(required) =
            required.filter(|_| self.looked < 64 || self.skipped * cols * 32 >= self.bytes)
        else {
            return false;
        };
        self.looked += 1;
        let (rows, bytes) = engine.lacks(line, required, options);
        self.bytes += bytes;
        let Some(rows) = rows else {
            return false;
        };
        self.skipped += rows as usize;
        self.rows = rows;
        true
    }
}

/// Rows read bottom up by [`Engine::search_previous`], so each is decoded once: finding where a line starts reads the
/// row above it, which ends the next line up and stays here for it.
#[derive(Default)]
struct RowsUp {
    /// Each row's characters and cells (counted from its own column 0), the bottom row first.
    chars: Vec<char>,
    cells: Vec<u32>,
    /// Where each row starts in `chars`.
    starts: Vec<usize>,
}

impl Engine {
    fn top_line(&self) -> i32 {
        -(self.history_len() as i32)
    }

    /// The first row of the logical line `line` belongs to.
    fn line_start(&self, mut line: i32) -> i32 {
        let top = self.top_line();
        while line > top && self.row_wrapped(line - 1) {
            line -= 1;
        }
        line
    }

    /// The logical line starting at row `first`.
    fn read_line(&self, first: i32) -> Option<Text> {
        let (mut chars, mut cells) = (Vec::new(), Vec::new());
        let rows = self.read_line_into(first, &mut chars, &mut cells)?;
        Some(Text {
            first,
            rows,
            chars,
            cells,
        })
    }

    /// Reads the logical line starting at row `first` into reused buffers; returns its row count.
    fn read_line_into(
        &self,
        first: i32,
        chars: &mut Vec<char>,
        cells: &mut Vec<u32>,
    ) -> Option<i32> {
        chars.clear();
        cells.clear();
        let cols = self.cols() as u32;
        let bottom = self.rows() as i32 - 1;
        let mut rows = 0;
        let mut keep;
        loop {
            let line = first + rows;
            let (content, wrapped) = self.with_row(line, |row| {
                let wrapped = row
                    .last()
                    .is_some_and(|c| c.flags.contains(Flags::WRAPLINE));
                let ends = !wrapped || line >= bottom;
                let content = push_row(row, rows as u32 * cols, ends, chars, cells);
                (content, wrapped)
            })?;
            keep = content;
            rows += 1;
            if !wrapped || line >= bottom {
                break;
            }
        }
        chars.truncate(keep);
        cells.truncate(keep);
        Some(rows)
    }

    /// Pushes row `line` onto `up`; returns whether it wraps.
    fn push_up(&self, line: i32, up: &mut RowsUp) -> Option<bool> {
        up.starts.push(up.chars.len());
        self.with_row(line, |row| {
            let wrapped = row
                .last()
                .is_some_and(|c| c.flags.contains(Flags::WRAPLINE));
            push_row(row, 0, !wrapped, &mut up.chars, &mut up.cells);
            wrapped
        })
    }

    /// The logical line that ends at row `last` (above a row that starts one), as [`Engine::read_line`] reads it.
    fn read_line_up(&self, last: i32, up: &mut RowsUp) -> Option<Text> {
        if up.starts.is_empty() {
            self.push_up(last, up)?;
        }
        let top = self.top_line();
        let mut first = last;
        while first > top && self.push_up(first - 1, up)? {
            first -= 1;
        }
        let rows = (last - first + 1) as usize;
        let end = up.starts.get(rows).copied().unwrap_or(up.chars.len());
        let cols = self.cols() as u32;
        let (mut chars, mut cells) = (Vec::with_capacity(end), Vec::with_capacity(end));
        for k in (0..rows).rev() {
            let row = up.starts[k]..up.starts.get(k + 1).copied().unwrap_or(end);
            let base = (rows - 1 - k) as u32 * cols;
            chars.extend_from_slice(&up.chars[row.clone()]);
            cells.extend(up.cells[row].iter().map(|&at| at + base));
        }
        // What is left is the row above, the last of the next line up
        let carried = up.starts.len() > rows;
        up.chars.drain(..end);
        up.cells.drain(..end);
        up.starts.clear();
        if carried {
            up.starts.push(0);
        }
        Some(Text {
            first,
            rows: rows as i32,
            chars,
            cells,
        })
    }

    fn read_folded(&self, first: i32, options: SearchOptions, ascii: bool) -> Option<Text> {
        let text = self.read_line(first)?;
        Some(if options.case_sensitive {
            text
        } else if ascii {
            text.fold_ascii()
        } else {
            text.fold()
        })
    }

    /// The characters a line must hold for an ASCII `needle` to be in it, as a set (bit `c` for `c`), lowercase
    /// when the search ignores case. A space is left out (blanks read as spaces), and when ignoring case so are `i`
    /// and `k`, which other characters lowercase to (`fold_ascii`).
    fn required(needle: &[char], options: SearchOptions) -> u128 {
        let other = |c| c == ' ' || !options.case_sensitive && (c == 'i' || c == 'k');
        needle
            .iter()
            .filter(|&&c| !other(c))
            .fold(0, |set, &c| set | 1 << c as u32)
    }

    /// The rows of the logical line starting at history row `line` when its text lacks a character of `required`
    /// (`History::ascii_in`), so that it holds no match, `None` when it may hold one or goes on into the grid's
    /// rows; and the bytes looked at.
    fn lacks(&self, line: i32, required: u128, options: SearchOptions) -> (Option<i32>, usize) {
        let Some((stored, first)) = self.history_row(line).filter(|_| required != 0) else {
            return (None, 0);
        };
        let (mut held, mut i, mut bytes) = (0, first, 0);
        loop {
            let Some((ascii, wraps, len)) = stored.ascii_in(i) else {
                return (None, bytes);
            };
            (held, bytes) = (held | ascii, bytes + len);
            i += 1;
            if !wraps {
                break;
            }
            if i == stored.len() {
                return (None, bytes);
            }
        }
        if !options.case_sensitive {
            // `A`..=`Z` count as `a`..=`z`
            held |= ((held >> b'A') & ((1 << 26) - 1)) << b'a';
        }
        let rows = (i - first) as i32;
        ((required & !held != 0).then_some(rows), bytes)
    }

    /// Clamps a cell position to the buffer: `col` past the row goes on to the next rows.
    fn position(&self, line: i32, col: u16) -> (i32, u32) {
        let cols = self.cols() as i64;
        let line = line as i64 + col as i64 / cols;
        (
            line.clamp(i32::MIN as i64, i32::MAX as i64) as i32,
            (col as i64 % cols) as u32,
        )
    }

    /// The logical line of `line` (`None` past the buffer), and `(line, col)` as a cell counted from its first row.
    fn locate(&self, line: i32, col: u32) -> Option<(i32, u32)> {
        if line < self.top_line() || line >= self.rows() as i32 {
            return None;
        }
        let first = self.line_start(line);
        Some((first, (line - first) as u32 * self.cols() as u32 + col))
    }

    fn query(query: &str, options: SearchOptions) -> Vec<char> {
        if options.case_sensitive {
            query.chars().collect()
        } else {
            query.chars().flat_map(char::to_lowercase).collect()
        }
    }

    /// The first match of `query` that starts at or after cell (`line`, `col`), scanning down to the bottom of the
    /// screen (no wrap-around). A `col` past the row continues on the next rows; a `line` above the history starts
    /// at the top.
    pub fn search_next(
        &self,
        query: &str,
        options: SearchOptions,
        line: i32,
        col: u16,
    ) -> Option<SearchMatch> {
        let needle = Self::query(query, options);
        if needle.is_empty() {
            return None;
        }
        let (mut line, mut col) = self.position(line, col);
        if line < self.top_line() {
            (line, col) = (self.top_line(), 0);
        }
        let cols = self.cols() as u32;
        let ascii = query.is_ascii();
        let required = ascii.then(|| Self::required(&needle, options));
        let (mut first, mut from) = self.locate(line, col)?;
        let mut skips = Skips::default();
        loop {
            // Past the first line, lines whose text lacks the query are skipped without being read
            if from == 0 && skips.lacks(self, first, required, options) {
                first += skips.rows;
                continue;
            }
            let Some(text) = self.read_folded(first, options, ascii) else {
                break;
            };
            let at = (0..text.cells.len())
                .find(|&i| text.start(i) >= from)
                .unwrap_or(text.cells.len());
            if let Some(s) = text.next(&needle, at, options.whole_word) {
                return Some(text.found(s, needle.len(), cols));
            }
            first += text.rows;
            from = 0;
        }
        None
    }

    /// The last match of `query` that ends at or before cell (`line`, `col`), scanning up to the top of the history
    /// (no wrap-around). (`rows - 1`, `cols`) is the end of the buffer; a `line` below the screen starts there.
    pub fn search_previous(
        &self,
        query: &str,
        options: SearchOptions,
        line: i32,
        col: u16,
    ) -> Option<SearchMatch> {
        let needle = Self::query(query, options);
        if needle.is_empty() {
            return None;
        }
        let cols = self.cols() as u32;
        let bottom = self.rows() as i32 - 1;
        let (line, col) = match self.position(line, col) {
            (line, 0) if line > self.top_line() => (line - 1, cols),
            position => position,
        };
        let (line, col) = if line > bottom {
            (bottom, cols)
        } else {
            (line, col)
        };
        let (first, mut before) = self.locate(line, col)?;
        let top = self.top_line();
        let ascii = query.is_ascii();
        let mut text = self.read_folded(first, options, ascii)?;
        let mut up = RowsUp::default();
        loop {
            let bound = (0..text.cells.len())
                .find(|&i| text.end(i) > before)
                .unwrap_or(text.cells.len());
            if let Some(s) = text.previous(&needle, bound, options.whole_word) {
                return Some(text.found(s, needle.len(), cols));
            }
            if text.first <= top {
                return None;
            }
            text = self.read_line_up(text.first - 1, &mut up)?;
            if !options.case_sensitive {
                text = if ascii {
                    text.fold_ascii()
                } else {
                    text.fold()
                };
            }
            before = u32::MAX;
        }
    }

    /// Every match in the logical lines that rows `first..=last` belong to, as a highlight-all pass finds them
    /// (each search goes on from the end of the previous match), keeping those that touch those rows.
    pub fn search_rows(
        &self,
        query: &str,
        options: SearchOptions,
        first: i32,
        last: i32,
        out: &mut Vec<SearchMatch>,
    ) {
        if self
            .search_rows_ascii(query, options, first, last, (i32::MIN, 0), usize::MAX, out)
            .is_none()
        {
            let needle = Self::query(query, options);
            self.search_rows_with(
                &needle,
                options,
                (first, last),
                (i32::MIN, 0),
                None,
                Text::fold,
                usize::MAX,
                out,
            );
        }
    }

    /// The first `limit` matches of [`Engine::search_rows`] for an ASCII query that start at or after cell `from`
    /// (line, col), which needs no case tables (a WebAssembly host links only this); `None`, finding nothing, for
    /// any other query. History lines whose text lacks a character of the query are skipped without being read.
    /// A host pages through many matches with a small `limit`, each page `from` just after the last match found.
    ///
    /// `Some(true)` when, ignoring case, a line with a match holds U+0130 (İ), which lowercases to two characters:
    /// a host that lowercases a whole line and reads match positions in the original text (`@xterm/addon-search`)
    /// places that line's matches elsewhere, so it must match there itself.
    #[allow(clippy::too_many_arguments)]
    pub fn search_rows_ascii(
        &self,
        query: &str,
        options: SearchOptions,
        first: i32,
        last: i32,
        from: (i32, u16),
        limit: usize,
        out: &mut Vec<SearchMatch>,
    ) -> Option<bool> {
        out.clear();
        if !query.is_ascii() {
            return None;
        }
        let needle: Vec<char> = if options.case_sensitive {
            query.chars().collect()
        } else {
            query.chars().map(|c| c.to_ascii_lowercase()).collect()
        };
        let required = Self::required(&needle, options);
        Some(self.search_rows_with(
            &needle,
            options,
            (first, last),
            from,
            Some(required),
            Text::fold_ascii,
            limit,
            out,
        ))
    }

    /// `search_rows` of a folded `needle` over rows `first..=last`, ASCII when `required` holds the bytes its lines
    /// need (`lacks`), with lines lowercased by `fold` when ignoring case (`Text::fold_ascii` links in no case
    /// tables), up to `limit` matches starting at or after cell `from`; whether a line with a match grew when folded
    /// (U+0130, `search_rows_ascii`).
    #[allow(clippy::too_many_arguments)]
    fn search_rows_with(
        &self,
        needle: &[char],
        options: SearchOptions,
        (first, last): (i32, i32),
        from: (i32, u16),
        required: Option<u128>,
        fold: impl Fn(Text) -> Text,
        limit: usize,
        out: &mut Vec<SearchMatch>,
    ) -> bool {
        out.clear();
        let last = last.min(self.rows() as i32 - 1);
        // Lines that end before `from` hold no match to keep
        let start = first.max(from.0).max(self.top_line());
        if needle.is_empty() || last < start || limit == 0 {
            return false;
        }
        let mut grew = false;
        let cols = self.cols() as u32;
        let mut line = self.line_start(start);
        let (mut chars, mut cells) = (Vec::new(), Vec::new());
        let mut skips = Skips::default();
        while line <= last {
            if skips.lacks(self, line, required, options) {
                line += skips.rows;
                continue;
            }
            let Some(rows) = self.read_line_into(line, &mut chars, &mut cells) else {
                return grew;
            };
            let read = chars.len();
            let mut text = Text {
                first: line,
                rows,
                chars: std::mem::take(&mut chars),
                cells: std::mem::take(&mut cells),
            };
            if !options.case_sensitive {
                text = fold(text);
            }
            let mut at = 0;
            while let Some(s) = text.next(needle, at, options.whole_word) {
                let found = text.found(s, needle.len(), cols);
                if found.end_line >= first
                    && found.start_line <= last
                    && (found.start_line, found.start_col) >= from
                {
                    grew |= text.chars.len() != read;
                    out.push(found);
                    if out.len() >= limit {
                        return grew;
                    }
                }
                at = s + needle.len();
            }
            line += text.rows;
            (chars, cells) = (text.chars, text.cells);
        }
        grew
    }

    /// The logical line row `line` belongs to, as search reads it; `None` outside the buffer.
    pub fn search_line(&self, line: i32) -> Option<SearchLine> {
        let (first, _) = self.locate(line, 0)?;
        let text = self.read_line(first)?;
        Some(SearchLine {
            first,
            rows: text.rows as u16,
            text: text.chars.iter().collect(),
            cells: text.cells,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TermHistory;

    fn found(
        t: &Engine,
        q: &str,
        o: SearchOptions,
        line: i32,
        col: u16,
    ) -> Option<(i32, u16, i32, u16)> {
        t.search_next(q, o, line, col)
            .map(|m| (m.start_line, m.start_col, m.end_line, m.end_col))
    }

    /// Every match `search_previous` steps through from the end of the buffer.
    fn all_previous(t: &Engine, q: &str, o: SearchOptions) -> Vec<SearchMatch> {
        let (mut line, mut col) = (t.rows() as i32, 0);
        let mut out = Vec::new();
        while let Some(m) = t.search_previous(q, o, line, col) {
            out.push(m);
            (line, col) = (m.start_line, m.start_col);
        }
        out
    }

    #[test]
    fn previous_reads_compressed_history_as_the_grid_does_and_decodes_each_row_once() {
        let mut packed = Engine::new(12, 4, 500);
        let mut plain = Engine::new_uncompressed(12, 4, 500);
        for i in 0..300 {
            // Single rows, lines that wrap over up to four rows, wide characters and İ (two characters lowercased)
            let line = match i % 5 {
                0 => format!("Ab{i} "),
                1 => format!("{}ab", "x Ab ".repeat(i % 9)),
                2 => format!("漢字Ab{}", "İab".repeat(i % 4)),
                3 => String::new(),
                _ => format!("{}AB", "-".repeat(11)),
            };
            packed.feed(format!("{line}\r\n").as_bytes());
            plain.feed(format!("{line}\r\n").as_bytes());
        }
        let options = [
            SearchOptions::default(),
            SearchOptions {
                case_sensitive: true,
                whole_word: false,
            },
            SearchOptions {
                case_sensitive: false,
                whole_word: true,
            },
        ];
        let mut matches = 0;
        for offset in [0, 150] {
            packed.scroll_to_bottom();
            packed.scroll_display(offset);
            for o in options {
                for q in ["ab", "Ab", "i̇ab", "字a"] {
                    let want = all_previous(&plain, q, o);
                    matches += want.len();
                    assert_eq!(all_previous(&packed, q, o), want, "{q} {o:?} at {offset}");
                }
            }
        }
        assert!(matches > 1000, "{matches}");
        packed.scroll_to_bottom();
        let before = crate::DECODES.with(std::cell::Cell::get);
        assert_eq!(packed.search_previous("absent", options[0], 4, 0), None);
        let decoded = crate::DECODES.with(std::cell::Cell::get) - before;
        assert_eq!(decoded, packed.history_len() - TermHistory::RECENT);
    }

    /// `fold_ascii` and `required` hold: outside ASCII only these two characters lowercase to ASCII.
    #[test]
    fn ascii_lowercase_is_ascii_letters_and_two_others() {
        let others: Vec<char> = (0x80..=0x10ffff)
            .filter_map(char::from_u32)
            .filter(|c| c.to_lowercase().any(|l| l.is_ascii()))
            .collect();
        assert_eq!(others, ['\u{130}', '\u{212a}']);
        assert!("\u{130}".to_lowercase() == "i\u{307}" && "\u{212a}".to_lowercase() == "k");
    }

    /// An ASCII query skips the history lines whose bytes lack it and folds case without the tables, and finds
    /// what the full fold over every line finds: on runs of colours, wide characters, tabs, erased blanks, wrapped
    /// lines, the two characters that lowercase to ASCII, and rows read at another width after a resize.
    #[test]
    fn ascii_queries_find_what_the_full_fold_finds() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let tokens = [
            "ab",
            "Ab",
            "aB",
            " ",
            "a b",
            "\t",
            "x",
            "\x1b[31mk\x1b[0mi",
            "\u{212a}",
            "\u{130}",
            "İk",
            "字",
            "\x1b[1mA\x1b[22mb",
            "-",
            "\x1b[2Xz",
            "i\u{301}",
            "\r\n",
            "\r\n",
            "\r\n",
        ];
        let queries = [
            "ab", "AB", "k", "ik", "a b", "i", "b-", "  ", "x\t", "zz", "qqzzxx", "b A",
        ];
        for round in 0..6 {
            let cols = 6 + next(20) as u16;
            let mut packed = Engine::new(cols, 4, 400);
            let mut plain = Engine::new_uncompressed(cols, 4, 400);
            let mut bytes = String::new();
            for _ in 0..3000 {
                bytes.push_str(tokens[next(tokens.len() as u64) as usize]);
            }
            packed.feed(bytes.as_bytes());
            plain.feed(bytes.as_bytes());
            if round % 2 == 1 {
                // Rows the resize changes none of are read at the new width
                packed.resize(cols + 7, 4);
                plain.resize(cols + 7, 4);
            }
            let (top, bottom) = (-(packed.history_len() as i32), packed.rows() as i32 - 1);
            assert_eq!(top, -(plain.history_len() as i32));
            for q in queries {
                for (case_sensitive, whole_word) in [(false, false), (true, false), (false, true)] {
                    let o = SearchOptions {
                        case_sensitive,
                        whole_word,
                    };
                    let (mut got, mut full, mut want) = (Vec::new(), Vec::new(), Vec::new());
                    packed.search_rows(q, o, top, bottom, &mut got);
                    packed.search_rows_with(
                        &Engine::query(q, o),
                        o,
                        (top, bottom),
                        (i32::MIN, 0),
                        None,
                        Text::fold,
                        usize::MAX,
                        &mut full,
                    );
                    plain.search_rows(q, o, top, bottom, &mut want);
                    assert_eq!(got, full, "round {round} {q:?} {o:?}");
                    assert_eq!(got, want, "round {round} {q:?} {o:?}");
                    // Stepping through them with search_next finds the same
                    let mut steps = Vec::new();
                    let (mut line, mut col) = (top, 0);
                    while let Some(m) = packed.search_next(q, o, line, col) {
                        steps.push(m);
                        (line, col) = (m.start_line, m.start_col + 1);
                    }
                    let mut walk = Vec::new();
                    let (mut line, mut col) = (top, 0);
                    while let Some(m) = plain.search_next(q, o, line, col) {
                        walk.push(m);
                        (line, col) = (m.start_line, m.start_col + 1);
                    }
                    assert_eq!(steps, walk, "round {round} {q:?} {o:?}");
                }
            }
        }
    }

    #[test]
    fn ascii_rows_stop_at_the_limit_and_flag_lines_that_fold_longer() {
        let mut t = Engine::new(20, 4, 100);
        t.feed(b"foo foo\r\nx\xc4\xb0 foo\r\nFOO");
        let o = SearchOptions::default();
        let (mut all, mut some) = (Vec::new(), Vec::new());
        t.search_rows("foo", o, -9, 9, &mut all);
        assert_eq!(all.len(), 4);
        assert_eq!(
            t.search_rows_ascii("foo", o, -9, 9, (i32::MIN, 0), 2, &mut some),
            Some(false)
        );
        assert_eq!(some, all[..2]);
        // The line with İ has a match: lowercased, it is a character longer
        assert_eq!(
            t.search_rows_ascii("foo", o, -9, 9, (i32::MIN, 0), 3, &mut some),
            Some(true)
        );
        assert_eq!(some, all[..3]);
        assert_eq!(
            t.search_rows_ascii("x", o, -9, 9, (i32::MIN, 0), 9, &mut some),
            Some(true)
        );
        // Case-sensitive, nothing is lowercased; the second row matches nothing
        let cs = SearchOptions {
            case_sensitive: true,
            whole_word: false,
        };
        assert_eq!(
            t.search_rows_ascii("FOO", cs, -9, 9, (i32::MIN, 0), 9, &mut some),
            Some(false)
        );
        assert_eq!(some.len(), 1);
        assert_eq!(
            t.search_rows_ascii("한", o, -9, 9, (i32::MIN, 0), 9, &mut some),
            None
        );
        assert_eq!(
            t.search_rows_ascii("foo", o, -9, 9, (i32::MIN, 0), 0, &mut some),
            Some(false)
        );
        assert!(some.is_empty());
    }

    #[test]
    fn ascii_rows_page_from_just_after_the_last_match() {
        // Matches on wrapped rows, several per row and per line, some starting above the rows asked for
        let mut t = Engine::new(7, 3, 100);
        for i in 0..40 {
            t.feed(format!("ab{i} ab ab xab\r\n").as_bytes());
        }
        let o = SearchOptions::default();
        let (top, bottom) = (-(t.history_len() as i32), t.rows() as i32 - 1);
        for (first, last) in [(top, bottom), (top + 9, top + 30)] {
            let mut all = Vec::new();
            t.search_rows("ab", o, first, last, &mut all);
            for limit in [1, 2, 5] {
                let (mut paged, mut page, mut from) = (Vec::new(), Vec::new(), (i32::MIN, 0));
                loop {
                    t.search_rows_ascii("ab", o, first, last, from, limit, &mut page);
                    paged.extend_from_slice(&page);
                    match page.last() {
                        Some(m) if page.len() == limit => from = (m.start_line, m.start_col + 1),
                        _ => break,
                    }
                }
                assert_eq!(paged, all, "{first}..={last} by {limit}");
            }
        }
    }

    #[test]
    fn next_and_previous_walk_the_matches_in_order() {
        let mut t = Engine::new(20, 4, 100);
        t.feed(b"foo bar foo\r\nbaz foo");
        let o = SearchOptions::default();
        assert_eq!(found(&t, "foo", o, 0, 0), Some((0, 0, 0, 3)));
        assert_eq!(found(&t, "foo", o, 0, 3), Some((0, 8, 0, 11)));
        assert_eq!(found(&t, "foo", o, 0, 11), Some((1, 4, 1, 7)));
        assert_eq!(found(&t, "foo", o, 1, 7), None);
        let prev = |line, col| {
            t.search_previous("foo", o, line, col)
                .map(|m| (m.start_line, m.start_col))
        };
        assert_eq!(prev(4, 0), Some((1, 4)));
        assert_eq!(prev(1, 4), Some((0, 8)));
        assert_eq!(prev(0, 8), Some((0, 0)));
        assert_eq!(prev(0, 0), None);
    }

    #[test]
    fn a_wrapped_line_is_searched_as_one_line() {
        let mut t = Engine::new(10, 4, 100);
        t.feed(b"0123456789abcdef");
        let o = SearchOptions::default();
        assert_eq!(found(&t, "89ab", o, 0, 0), Some((0, 8, 1, 2)));
        // Starting on the continuation row still sees the line from that cell on
        assert_eq!(found(&t, "cd", o, 1, 0), Some((1, 2, 1, 4)));
        assert_eq!(found(&t, "89", o, 1, 0), None);
        let line = t.search_line(1).unwrap();
        assert_eq!(
            (line.first, line.rows, line.text.as_str()),
            (0, 2, "0123456789abcdef")
        );
    }

    #[test]
    fn wide_and_combining_characters_map_to_their_cells() {
        let mut t = Engine::new(20, 4, 100);
        t.feed("가나e\u{301}다 x".as_bytes());
        let o = SearchOptions::default();
        assert_eq!(found(&t, "나", o, 0, 0), Some((0, 2, 0, 4)));
        assert_eq!(found(&t, "e\u{301}다", o, 0, 0), Some((0, 4, 0, 7)));
        assert_eq!(found(&t, "x", o, 0, 0), Some((0, 8, 0, 9)));
        // A start on the right half of a wide character goes on after it
        assert_eq!(found(&t, "나", o, 0, 3), None);
    }

    #[test]
    fn a_wide_character_moved_to_the_next_row_leaves_no_blank_in_the_text() {
        let mut t = Engine::new(5, 4, 100);
        t.feed("abcd가z".as_bytes());
        let line = t.search_line(0).unwrap();
        assert_eq!(line.text, "abcd가z");
        assert_eq!(
            found(&t, "d가", SearchOptions::default(), 0, 0),
            Some((0, 3, 1, 2))
        );
    }

    #[test]
    fn case_and_whole_word() {
        let mut t = Engine::new(30, 4, 100);
        t.feed(b"FooBar foo_x (foo) Foo");
        let cs = SearchOptions {
            case_sensitive: true,
            whole_word: false,
        };
        let word = SearchOptions {
            case_sensitive: false,
            whole_word: true,
        };
        assert_eq!(found(&t, "foo", cs, 0, 0), Some((0, 7, 0, 10)));
        assert_eq!(
            found(&t, "foo", SearchOptions::default(), 0, 0),
            Some((0, 0, 0, 3))
        );
        // Not "FooBar", not "foo_x" (`_` is a word character for xterm.js): the first whole word is in the parentheses
        assert_eq!(found(&t, "foo", word, 0, 0), Some((0, 14, 0, 17)));
        let mut all = Vec::new();
        t.search_rows("foo", word, 0, 0, &mut all);
        assert_eq!(
            all.iter().map(|m| m.start_col).collect::<Vec<_>>(),
            [14, 19]
        );
    }

    #[test]
    fn scrollback_is_searched_and_rows_select_lines() {
        let mut t = Engine::new(10, 2, 100);
        for i in 0..6 {
            t.feed(format!("line {i}\r\n").as_bytes());
        }
        assert_eq!(t.history_len(), 5);
        let o = SearchOptions::default();
        assert_eq!(found(&t, "line 0", o, -100, 0), Some((-5, 0, -5, 6)));
        let mut rows = Vec::new();
        t.search_rows("line", o, -2, -1, &mut rows);
        assert_eq!(
            rows.iter().map(|m| m.start_line).collect::<Vec<_>>(),
            [-2, -1]
        );
        assert_eq!(
            t.search_previous("line", o, 2, 0).map(|m| m.start_line),
            Some(0)
        );
    }

    #[test]
    fn empty_query_and_out_of_range() {
        let mut t = Engine::new(10, 2, 100);
        t.feed(b"abc");
        let o = SearchOptions::default();
        assert_eq!(found(&t, "", o, 0, 0), None);
        assert_eq!(found(&t, "abc", o, 2, 0), None);
        assert!(t.search_line(2).is_none());
        assert!(t.search_line(-1).is_none());
    }

    #[test]
    fn a_character_that_lowercases_to_two_keeps_the_cells_after_it() {
        let mut t = Engine::new(20, 2, 100);
        // U+0130 lowercases to "i\u{307}": the characters after it still map to their own cells
        t.feed("aB \u{130}x Ab".as_bytes());
        let o = SearchOptions::default();
        assert_eq!(found(&t, "ab", o, 0, 0), Some((0, 0, 0, 2)));
        assert_eq!(found(&t, "ab", o, 0, 1), Some((0, 6, 0, 8)));
        assert_eq!(found(&t, "x", o, 0, 0), Some((0, 4, 0, 5)));
        assert_eq!(found(&t, "\u{130}", o, 0, 0), Some((0, 3, 0, 4)));
    }
}
