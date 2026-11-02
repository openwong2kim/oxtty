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

    fn fold(self) -> Self {
        let mut chars = Vec::with_capacity(self.chars.len());
        let mut cells = Vec::with_capacity(self.cells.len());
        for (c, at) in self.chars.iter().zip(&self.cells) {
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

    /// The first match starting at character `from` or later.
    fn next(&self, query: &[char], from: usize, whole_word: bool) -> Option<usize> {
        let last = self.chars.len().checked_sub(query.len())?;
        (from..=last).find(|&s| self.is_match(query, s, whole_word))
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

/// Appends a row's characters; returns the length the text keeps if the logical line ends here.
fn push_row(row: &Row<AlacCell>, base: u32, chars: &mut Vec<char>, cells: &mut Vec<u32>) -> usize {
    let mut content = chars.len();
    for (col, cell) in row[..].iter().enumerate() {
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
        let cols = self.cols() as u32;
        let bottom = self.rows() as i32 - 1;
        let (mut chars, mut cells) = (Vec::new(), Vec::new());
        let mut rows = 0;
        let mut keep;
        loop {
            let line = first + rows;
            let (content, wrapped) = self.with_row(line, |row| {
                let content = push_row(row, rows as u32 * cols, &mut chars, &mut cells);
                let wrapped = row
                    .last()
                    .is_some_and(|c| c.flags.contains(Flags::WRAPLINE));
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
        Some(Text {
            first,
            rows,
            chars,
            cells,
        })
    }

    fn read_folded(&self, first: i32, options: SearchOptions) -> Option<Text> {
        let text = self.read_line(first)?;
        Some(if options.case_sensitive {
            text
        } else {
            text.fold()
        })
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
        let (mut first, mut from) = self.locate(line, col)?;
        while let Some(text) = self.read_folded(first, options) {
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
        let (mut first, mut before) = self.locate(line, col)?;
        let top = self.top_line();
        loop {
            let text = self.read_folded(first, options)?;
            let bound = (0..text.cells.len())
                .find(|&i| text.end(i) > before)
                .unwrap_or(text.cells.len());
            if let Some(s) = text.previous(&needle, bound, options.whole_word) {
                return Some(text.found(s, needle.len(), cols));
            }
            if first <= top {
                return None;
            }
            first = self.line_start(first - 1);
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
        out.clear();
        let needle = Self::query(query, options);
        let last = last.min(self.rows() as i32 - 1);
        if needle.is_empty() || last < first.max(self.top_line()) {
            return;
        }
        let cols = self.cols() as u32;
        let mut line = self.line_start(first.max(self.top_line()));
        while line <= last {
            let Some(text) = self.read_folded(line, options) else {
                return;
            };
            let mut at = 0;
            while let Some(s) = text.next(&needle, at, options.whole_word) {
                let found = text.found(s, needle.len(), cols);
                if found.end_line >= first && found.start_line <= last {
                    out.push(found);
                }
                at = s + needle.len();
            }
            line += text.rows;
        }
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
}
