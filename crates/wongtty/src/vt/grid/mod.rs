//! A specialized 2D grid implementation optimized for use in a terminal.

use std::cmp::{max, min};
use std::fmt;
use std::mem;
use std::ops::{Bound, Deref, Index, IndexMut, Range, RangeBounds};
use std::sync::{Arc, Mutex, PoisonError};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::vt::index::{Column, Line, Point};
use crate::vt::term::cell::{Flags, ResetDiscriminant};
use crate::vt::vte::ansi::{CharsetIndex, StandardCharset};

pub mod resize;
mod row;
mod storage;
#[cfg(test)]
mod tests;

pub use self::row::Row;
use self::storage::Storage;

pub trait GridCell: Sized {
    /// Check if the cell contains any content.
    fn is_empty(&self) -> bool;

    /// Perform an opinionated cell reset based on a template cell.
    fn reset(&mut self, template: &Self);

    fn flags(&self) -> &Flags;
    fn flags_mut(&mut self) -> &mut Flags;
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Cursor<T> {
    /// The location of this cursor.
    pub point: Point,

    /// Template cell when using this cursor.
    pub template: T,

    /// Currently configured graphic character sets.
    pub charsets: Charsets,

    /// Tracks if the next call to input will need to first handle wrapping.
    ///
    /// This is true after the last column is set with the input function. Any function that
    /// implicitly sets the line or column needs to set this to false to avoid wrapping twice.
    ///
    /// Tracking `input_needs_wrap` makes it possible to not store a cursor position that exceeds
    /// the number of columns, which would lead to index out of bounds when interacting with arrays
    /// without sanitization.
    pub input_needs_wrap: bool,
}

#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
pub struct Charsets([StandardCharset; 4]);

impl Index<CharsetIndex> for Charsets {
    type Output = StandardCharset;

    fn index(&self, index: CharsetIndex) -> &StandardCharset {
        &self.0[index as usize]
    }
}

impl IndexMut<CharsetIndex> for Charsets {
    fn index_mut(&mut self, index: CharsetIndex) -> &mut StandardCharset {
        &mut self.0[index as usize]
    }
}

#[derive(Debug, Copy, Clone)]
pub enum Scroll {
    Delta(i32),
    PageUp,
    PageDown,
    Top,
    Bottom,
}

/// Grid based terminal content storage.
///
/// ```notrust
/// ┌─────────────────────────┐  <-- max_scroll_limit + lines
/// │                         │
/// │      UNINITIALIZED      │
/// │                         │
/// ├─────────────────────────┤  <-- self.raw.inner.len()
/// │                         │
/// │      RESIZE BUFFER      │
/// │                         │
/// ├─────────────────────────┤  <-- self.history_size() + lines
/// │                         │
/// │     SCROLLUP REGION     │
/// │                         │
/// ├─────────────────────────┤v lines
/// │                         │|
/// │     VISIBLE  REGION     │|
/// │                         │|
/// ├─────────────────────────┤^ <-- display_offset
/// │                         │
/// │    SCROLLDOWN REGION    │
/// │                         │
/// └─────────────────────────┘  <-- zero
///                           ^
///                        columns
/// ```
#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Grid<T> {
    /// Current cursor for writing data.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub cursor: Cursor<T>,

    /// Last saved cursor.
    #[cfg_attr(feature = "serde", serde(skip))]
    pub saved_cursor: Cursor<T>,

    /// Lines in the grid. Each row holds a list of cells corresponding to the
    /// columns in that row.
    raw: Storage<T>,

    /// Number of columns.
    columns: usize,

    /// Number of visible lines.
    lines: usize,

    /// Offset of displayed area.
    ///
    /// If the displayed region isn't at the bottom of the screen, it stays
    /// stationary while more text is emitted. The scrolling implementation
    /// updates this offset accordingly.
    display_offset: usize,

    /// Maximum number of lines in history.
    max_scroll_limit: usize,

    /// wongtty: do not reflow the paragraph holding the cursor on resize (xterm.js `reflowCursorLine: false`).
    /// The default `false` is the original Alacritty behavior.
    #[cfg_attr(feature = "serde", serde(skip))]
    keep_cursor_paragraph: bool,

    /// wongtty: more rows are added blank at the bottom instead of bringing history back into view (xterm.js with
    /// `windowsPty`). The default `false` is the original Alacritty behavior.
    #[cfg_attr(feature = "serde", serde(skip))]
    grow_blank: bool,

    /// wongtty: receives lines that would be dropped because the history is full (wongtty keeps them compressed).
    /// If `None`, they are dropped as in the original Alacritty behavior.
    #[cfg_attr(feature = "serde", serde(skip))]
    history_sink: Option<Arc<Mutex<dyn HistorySink<T>>>>,

    /// wongtty: number of lines scrolled up so far. Counted under the same conditions that raise `display_offset`.
    #[cfg_attr(feature = "serde", serde(skip))]
    scrolled_lines: u64,

    /// wongtty: number of lines that have entered the history so far (scrolls of a region at the top).
    #[cfg_attr(feature = "serde", serde(skip))]
    history_entered: u64,

    /// wongtty: history rows held outside the grid, above its own, during a resize.
    #[cfg_attr(feature = "serde", serde(skip))]
    history_above: Option<Arc<Mutex<dyn HistoryAbove<T>>>>,

    /// wongtty: how screen rows moved or were cleared since the last `take_row_moves`; `None` while not tracked.
    #[cfg_attr(feature = "serde", serde(skip))]
    row_moves: Option<Vec<RowMove>>,
}

/// wongtty: one way the grid moved or cleared screen rows, for a host that follows lines (markers).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowMove {
    /// The rows of `region` moved up by `positions`; the top ones went into the history when `history` is set
    /// (the rows below the region then stay where they are on screen), else they were dropped.
    Up { region: Range<Line>, positions: usize, history: bool },
    /// The rows of `region` moved down by `positions`; the bottom ones were dropped.
    Down { region: Range<Line>, positions: usize },
    /// The rows were cleared.
    Reset { start: Line, end: Line },
}

/// wongtty: receives the rows a full history drops, oldest first, so they can
/// be kept outside the grid.
pub trait HistorySink<T>: fmt::Debug + Send {
    fn push(&mut self, row: &Row<T>);

    /// The history was cleared (`CSI 3 J`, reset).
    fn clear(&mut self);
}

/// wongtty: the oldest history rows, held outside the grid while it resizes.
/// They count towards the history, and a column change reflows them first,
/// as if they were the top of the grid's storage.
pub trait HistoryAbove<T>: fmt::Debug + Send {
    /// Rows held.
    fn rows(&self) -> usize;

    /// Reflows the rows as `grow_columns` reflows history rows, oldest first.
    /// The newest row has storage index `base`. Returns the newest `keep`
    /// rows (oldest first), which the grid takes back.
    fn grow_columns(
        &mut self,
        reflow: bool,
        columns: usize,
        base: usize,
        display_offset: &mut usize,
        keep: usize,
    ) -> Vec<Row<T>>;

    /// Reflows the rows as `shrink_columns` reflows history rows, oldest
    /// first. The newest row has storage index `base`. Returns the cells
    /// carried into the next row.
    fn shrink_columns(
        &mut self,
        reflow: bool,
        columns: usize,
        base: usize,
        display_offset: &mut usize,
    ) -> Option<Vec<T>>;

    /// Drops the oldest rows beyond `max`.
    fn truncate(&mut self, max: usize);
}

impl<T: GridCell + Default + PartialEq> Grid<T> {
    pub fn new(lines: usize, columns: usize, max_scroll_limit: usize) -> Grid<T> {
        Grid {
            raw: Storage::with_capacity(lines, columns),
            max_scroll_limit,
            display_offset: 0,
            saved_cursor: Cursor::default(),
            cursor: Cursor::default(),
            lines,
            columns,
            keep_cursor_paragraph: false,
            grow_blank: false,
            history_sink: None,
            scrolled_lines: 0,
            history_entered: 0,
            history_above: None,
            row_moves: None,
        }
    }

    /// wongtty: starts or stops logging how screen rows move (`take_row_moves`).
    pub fn track_row_moves(&mut self, on: bool) {
        if on != self.row_moves.is_some() {
            self.row_moves = on.then(Vec::new);
        }
    }

    /// wongtty: the row moves logged since the last call, oldest first.
    pub fn take_row_moves(&mut self) -> Vec<RowMove> {
        self.row_moves.as_mut().map(mem::take).unwrap_or_default()
    }

    fn log_row_move(&mut self, row_move: RowMove) {
        if let Some(moves) = &mut self.row_moves {
            moves.push(row_move);
        }
    }

    /// wongtty: sets the history rows held above the grid; returns the previous ones.
    pub fn set_history_above(
        &mut self,
        above: Option<Arc<Mutex<dyn HistoryAbove<T>>>>,
    ) -> Option<Arc<Mutex<dyn HistoryAbove<T>>>> {
        mem::replace(&mut self.history_above, above)
    }

    /// wongtty: history rows held above the grid.
    fn rows_above(&self) -> usize {
        self.history_above
            .as_ref()
            .map_or(0, |above| above.lock().unwrap_or_else(PoisonError::into_inner).rows())
    }

    /// wongtty: sets where rows dropped from a full history go; returns the previous sink.
    pub fn set_history_sink(
        &mut self,
        sink: Option<Arc<Mutex<dyn HistorySink<T>>>>,
    ) -> Option<Arc<Mutex<dyn HistorySink<T>>>> {
        mem::replace(&mut self.history_sink, sink)
    }

    /// wongtty: sets the view offset as it is, without clamping to the history
    /// (scrolling can leave it past the history; see `scroll_up`).
    pub fn set_display_offset(&mut self, offset: usize) {
        self.display_offset = offset;
    }

    /// wongtty: lines scrolled up so far; the view moves by this much while scrolled back.
    pub fn scrolled_lines(&self) -> u64 {
        self.scrolled_lines
    }

    /// wongtty: lines that have entered the history so far, whether it kept them or not.
    pub fn history_entered(&self) -> u64 {
        self.history_entered
    }

    /// wongtty: puts `older` rows (oldest first) above the current history and
    /// raises the history limit to `max_scroll_limit`.
    pub fn prepend_history(&mut self, older: Vec<Row<T>>, max_scroll_limit: usize) {
        let mut inner = self.raw.take_all();
        inner.extend(older.into_iter().rev());
        self.raw.replace_inner(inner);
        self.max_scroll_limit = max_scroll_limit;
    }

    /// wongtty: removes the history above the newest `keep` rows and returns it
    /// oldest first; the history limit becomes `keep` and the view returns to
    /// the bottom.
    pub fn split_off_history(&mut self, keep: usize) -> Vec<Row<T>> {
        let mut inner = self.raw.take_all();
        let mut older = inner.split_off(min(self.lines + keep, inner.len()));
        older.reverse();
        self.raw.replace_inner(inner);
        self.max_scroll_limit = keep;
        self.display_offset = 0;
        older
    }

    /// wongtty: sets whether to skip reflowing the cursor paragraph on resize (xterm.js `reflowCursorLine: false`).
    pub fn set_keep_cursor_paragraph(&mut self, keep: bool) {
        self.keep_cursor_paragraph = keep;
    }

    /// wongtty: sets whether more rows are added blank at the bottom instead of bringing history back into view.
    pub fn set_grow_blank(&mut self, grow_blank: bool) {
        self.grow_blank = grow_blank;
    }

    /// Update the size of the scrollback history.
    pub fn update_history(&mut self, history_size: usize) {
        let current_history_size = self.history_size();
        if current_history_size > history_size {
            self.raw.shrink_lines(current_history_size - history_size);
        }
        self.display_offset = min(self.display_offset, history_size);
        self.max_scroll_limit = history_size;
    }

    pub fn scroll_display(&mut self, scroll: Scroll) {
        self.display_offset = match scroll {
            Scroll::Delta(count) => {
                min(max((self.display_offset as i32) + count, 0) as usize, self.history_size())
            },
            Scroll::PageUp => min(self.display_offset + self.lines, self.history_size()),
            Scroll::PageDown => self.display_offset.saturating_sub(self.lines),
            Scroll::Top => self.history_size(),
            Scroll::Bottom => 0,
        };
    }

    fn increase_scroll_limit(&mut self, count: usize) {
        let count = min(count, self.max_scroll_limit - self.history_size());
        if count != 0 {
            self.raw.initialize_up_to(count, self.columns, self.max_scroll_limit + self.lines);
        }
    }

    fn decrease_scroll_limit(&mut self, count: usize) {
        let count = min(count, self.history_size());
        if count != 0 {
            self.raw.shrink_lines(min(count, self.history_size()));
            self.display_offset = min(self.display_offset, self.history_size() + self.rows_above());
        }
    }

    #[inline]
    pub fn scroll_down<D>(&mut self, region: &Range<Line>, positions: usize)
    where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        self.log_row_move(RowMove::Down { region: region.clone(), positions });

        // When rotating the entire region, just reset everything.
        if region.end - region.start <= positions {
            for i in (region.start.0..region.end.0).map(Line::from) {
                self.raw[i].reset(&self.cursor.template);
            }

            return;
        }

        // Which implementation we can use depends on the existence of a scrollback history.
        //
        // Since a scrollback history prevents us from rotating the entire buffer downwards, we
        // instead have to rely on a slower, swap-based implementation.
        if self.max_scroll_limit == 0 {
            // Swap the lines fixed at the bottom to their target positions after rotation.
            //
            // Since we've made sure that the rotation will never rotate away the entire region, we
            // know that the position of the fixed lines before the rotation must already be
            // visible.
            //
            // We need to start from the top, to make sure the fixed lines aren't swapped with each
            // other.
            let screen_lines = self.screen_lines() as i32;
            for i in (region.end.0..screen_lines).map(Line::from) {
                self.raw.swap(i, i - positions as i32);
            }

            // Rotate the entire line buffer downward.
            self.raw.rotate_down(positions);

            // Ensure all new lines are fully cleared.
            for i in (0..positions).map(Line::from) {
                self.raw[i].reset(&self.cursor.template);
            }

            // Swap the fixed lines at the top back into position.
            for i in (0..region.start.0).map(Line::from) {
                self.raw.swap(i, i + positions);
            }
        } else {
            // Subregion rotation.
            self.raw.rotate_region(region.clone(), positions, false);

            let range = region.start.0..(region.start + positions).0;
            for line in range.rev().map(Line::from) {
                self.raw[line].reset(&self.cursor.template);
            }
        }
    }

    /// Move lines at the bottom toward the top.
    ///
    /// This is the performance-sensitive part of scrolling.
    pub fn scroll_up<D>(&mut self, region: &Range<Line>, positions: usize)
    where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        self.log_row_move(RowMove::Up { region: region.clone(), positions, history: region.start == 0 });

        // When rotating the entire region with fixed lines at the top, just reset everything.
        if region.end - region.start <= positions && region.start != 0 {
            for i in (region.start.0..region.end.0).map(Line::from) {
                self.raw[i].reset(&self.cursor.template);
            }

            return;
        }

        // Update display offset when not pinned to active area.
        if self.display_offset != 0 {
            self.display_offset = min(self.display_offset + positions, self.max_scroll_limit);
        }
        self.scrolled_lines += positions as u64;

        // Only rotate the entire history if the active region starts at the top.
        if region.start == 0 {
            self.history_entered += positions as u64;
            // wongtty: hand the rows a full history is about to drop to the sink. They are the
            // oldest history rows, then (with less history than `positions`) the top screen rows.
            if let Some(sink) = &self.history_sink {
                let history = self.history_size();
                let dropped = (history + positions).saturating_sub(self.max_scroll_limit);
                let mut sink = sink.lock().unwrap_or_else(PoisonError::into_inner);
                for i in 0..dropped {
                    sink.push(&self.raw[Line(i as i32 - history as i32)]);
                }
            }

            // Create scrollback for the new lines.
            self.increase_scroll_limit(positions);

            // Swap the lines fixed at the top to their target positions after rotation.
            //
            // Since we've made sure that the rotation will never rotate away the entire region, we
            // know that the position of the fixed lines before the rotation must already be
            // visible.
            //
            // We need to start from the bottom, to make sure the fixed lines aren't swapped with
            // each other.
            for i in (0..region.start.0).rev().map(Line::from) {
                self.raw.swap(i, i + positions);
            }

            // Rotate the entire line buffer upward.
            self.raw.rotate(-(positions as isize));

            // Swap the fixed lines at the bottom back into position.
            let screen_lines = self.screen_lines() as i32;
            for i in (region.end.0..screen_lines).rev().map(Line::from) {
                self.raw.swap(i, i - positions);
            }
        } else {
            // Rotate lines without moving anything into history.
            self.raw.rotate_region(region.clone(), positions, true);
        }

        // Ensure all new lines are fully cleared.
        for i in (region.end.0 - positions as i32..region.end.0).map(Line::from) {
            self.raw[i].reset(&self.cursor.template);
        }
    }

    pub fn clear_viewport<D>(&mut self)
    where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        // Determine how many lines to scroll up by.
        let end = Point::new(Line(self.lines as i32 - 1), Column(self.columns()));
        let mut iter = self.iter_from(end);
        while let Some(cell) = iter.prev() {
            if !cell.is_empty() || cell.point.line < 0 {
                break;
            }
        }
        debug_assert!(iter.point.line >= -1);
        let positions = (iter.point.line.0 + 1) as usize;
        let region = Line(0)..Line(self.lines as i32);

        // Clear the viewport.
        self.scroll_up(&region, positions);

        // Reset rotated lines.
        for line in (0..(self.lines - positions)).map(Line::from) {
            self.raw[line].reset(&self.cursor.template);
        }
    }

    /// Completely reset the grid state.
    pub fn reset<D>(&mut self)
    where
        T: ResetDiscriminant<D>,
        D: PartialEq,
    {
        self.clear_history();

        self.saved_cursor = Cursor::default();
        self.cursor = Cursor::default();
        self.display_offset = 0;

        // Reset all visible lines.
        let range = self.topmost_line().0..(self.screen_lines() as i32);
        for line in range.map(Line::from) {
            self.raw[line].reset(&self.cursor.template);
        }
    }
}

impl<T> Grid<T> {
    /// Reset a visible region within the grid.
    pub fn reset_region<D, R: RangeBounds<Line>>(&mut self, bounds: R)
    where
        T: ResetDiscriminant<D> + GridCell + Default,
        D: PartialEq,
    {
        let start = match bounds.start_bound() {
            Bound::Included(line) => *line,
            Bound::Excluded(line) => *line + 1,
            Bound::Unbounded => Line(0),
        };

        let end = match bounds.end_bound() {
            Bound::Included(line) => *line + 1,
            Bound::Excluded(line) => *line,
            Bound::Unbounded => Line(self.screen_lines() as i32),
        };

        debug_assert!(start < self.screen_lines() as i32);
        debug_assert!(end <= self.screen_lines() as i32);

        if let Some(moves) = &mut self.row_moves {
            moves.push(RowMove::Reset { start, end });
        }

        for line in (start.0..end.0).map(Line::from) {
            self.raw[line].reset(&self.cursor.template);
        }
    }

    #[inline]
    pub fn clear_history(&mut self) {
        if let Some(sink) = &self.history_sink {
            sink.lock().unwrap_or_else(PoisonError::into_inner).clear();
        }

        // Explicitly purge all lines from history.
        self.raw.shrink_lines(self.history_size());

        // Reset display offset.
        self.display_offset = 0;
    }

    /// This is used only for initializing after loading ref-tests.
    #[inline]
    pub fn initialize_all(&mut self)
    where
        T: GridCell + Default,
    {
        // Remove all cached lines to clear them of any content.
        self.truncate();

        // Initialize everything with empty new lines.
        self.raw.initialize(self.max_scroll_limit - self.history_size(), self.columns);
    }

    /// This is used only for truncating before saving ref-tests.
    #[inline]
    pub fn truncate(&mut self) {
        self.raw.truncate();
    }

    /// Iterate over all cells in the grid starting at a specific point.
    #[inline]
    pub fn iter_from(&self, point: Point) -> GridIterator<'_, T> {
        let end = Point::new(self.bottommost_line(), self.last_column());
        GridIterator { grid: self, point, end }
    }

    /// Iterate over all visible cells.
    ///
    /// This is slightly more optimized than calling `Grid::iter_from` in combination with
    /// `Iterator::take_while`.
    #[inline]
    pub fn display_iter(&self) -> GridIterator<'_, T> {
        let last_column = self.last_column();
        let start = Point::new(Line(-(self.display_offset() as i32) - 1), last_column);
        let end_line = min(start.line + self.screen_lines(), self.bottommost_line());
        let end = Point::new(end_line, last_column);

        GridIterator { grid: self, point: start, end }
    }

    #[inline]
    pub fn display_offset(&self) -> usize {
        self.display_offset
    }

    #[inline]
    pub fn cursor_cell(&mut self) -> &mut T {
        let point = self.cursor.point;
        &mut self[point.line][point.column]
    }
}

impl<T: PartialEq> PartialEq for Grid<T> {
    fn eq(&self, other: &Self) -> bool {
        // Compare struct fields and check result of grid comparison.
        self.raw.eq(&other.raw)
            && self.columns.eq(&other.columns)
            && self.lines.eq(&other.lines)
            && self.display_offset.eq(&other.display_offset)
    }
}

impl<T> Index<Line> for Grid<T> {
    type Output = Row<T>;

    #[inline]
    fn index(&self, index: Line) -> &Row<T> {
        &self.raw[index]
    }
}

impl<T> IndexMut<Line> for Grid<T> {
    #[inline]
    fn index_mut(&mut self, index: Line) -> &mut Row<T> {
        &mut self.raw[index]
    }
}

impl<T> Index<Point> for Grid<T> {
    type Output = T;

    #[inline]
    fn index(&self, point: Point) -> &T {
        &self[point.line][point.column]
    }
}

impl<T> IndexMut<Point> for Grid<T> {
    #[inline]
    fn index_mut(&mut self, point: Point) -> &mut T {
        &mut self[point.line][point.column]
    }
}

/// Grid dimensions.
pub trait Dimensions {
    /// Total number of lines in the buffer, this includes scrollback and visible lines.
    fn total_lines(&self) -> usize;

    /// Height of the viewport in lines.
    fn screen_lines(&self) -> usize;

    /// Width of the terminal in columns.
    fn columns(&self) -> usize;

    /// Index for the last column.
    #[inline]
    fn last_column(&self) -> Column {
        Column(self.columns() - 1)
    }

    /// Line farthest up in the grid history.
    #[inline]
    fn topmost_line(&self) -> Line {
        Line(-(self.history_size() as i32))
    }

    /// Line farthest down in the grid history.
    #[inline]
    fn bottommost_line(&self) -> Line {
        Line(self.screen_lines() as i32 - 1)
    }

    /// Number of invisible lines part of the scrollback history.
    #[inline]
    fn history_size(&self) -> usize {
        self.total_lines().saturating_sub(self.screen_lines())
    }
}

impl<G> Dimensions for Grid<G> {
    #[inline]
    fn total_lines(&self) -> usize {
        self.raw.len()
    }

    #[inline]
    fn screen_lines(&self) -> usize {
        self.lines
    }

    #[inline]
    fn columns(&self) -> usize {
        self.columns
    }
}

#[cfg(test)]
impl Dimensions for (usize, usize) {
    fn total_lines(&self) -> usize {
        self.0
    }

    fn screen_lines(&self) -> usize {
        self.0
    }

    fn columns(&self) -> usize {
        self.1
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Indexed<T> {
    pub point: Point,
    pub cell: T,
}

impl<T> Deref for Indexed<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        &self.cell
    }
}

/// Grid cell iterator.
pub struct GridIterator<'a, T> {
    /// Immutable grid reference.
    grid: &'a Grid<T>,

    /// Current position of the iterator within the grid.
    point: Point,

    /// Last cell included in the iterator.
    end: Point,
}

impl<'a, T> GridIterator<'a, T> {
    /// Current iterator position.
    pub fn point(&self) -> Point {
        self.point
    }

    /// Cell at the current iterator position.
    pub fn cell(&self) -> &'a T {
        &self.grid[self.point]
    }
}

impl<'a, T> Iterator for GridIterator<'a, T> {
    type Item = Indexed<&'a T>;

    fn next(&mut self) -> Option<Self::Item> {
        // Stop once we've reached the end of the grid.
        if self.point >= self.end {
            return None;
        }

        match self.point {
            Point { column, .. } if column >= self.grid.last_column() => {
                self.point.column = Column(0);
                self.point.line += 1;
            },
            _ => self.point.column += Column(1),
        }

        Some(Indexed { cell: &self.grid[self.point], point: self.point })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        if self.point >= self.end {
            return (0, Some(0));
        }

        let size = if self.point.line == self.end.line {
            (self.end.column - self.point.column).0
        } else {
            let cols_on_first_line = self.grid.columns.saturating_sub(self.point.column.0 + 1);
            let middle_lines = (self.end.line - self.point.line).0 as usize - 1;
            let cols_on_last_line = self.end.column + 1;

            cols_on_first_line + middle_lines * self.grid.columns + cols_on_last_line.0
        };

        (size, Some(size))
    }
}

/// Bidirectional iterator.
pub trait BidirectionalIterator: Iterator {
    fn prev(&mut self) -> Option<Self::Item>;
}

impl<T> BidirectionalIterator for GridIterator<'_, T> {
    fn prev(&mut self) -> Option<Self::Item> {
        let topmost_line = self.grid.topmost_line();
        let last_column = self.grid.last_column();

        // Stop once we've reached the end of the grid.
        if self.point <= Point::new(topmost_line, Column(0)) {
            return None;
        }

        match self.point {
            Point { column: Column(0), .. } => {
                self.point.column = last_column;
                self.point.line -= 1;
            },
            _ => self.point.column -= Column(1),
        }

        Some(Indexed { cell: &self.grid[self.point], point: self.point })
    }
}
