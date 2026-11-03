//! Compressed history: rows pushed off the top of the screen, kept as text
//! plus runs of attributes.
//!
//! Instead of 24 bytes per cell (Alacritty) or 12 (xterm.js), cells that share
//! colours and flags form one run and only their characters are written, in
//! as many bytes as UTF-8 or fewer. A 120-column log line takes about 40
//! bytes, and the default blanks that end a row take none. A wide character
//! and the cell after it are one unit. Combining marks, underline colours and
//! links (`Cell::extra`) are rare, so they are kept apart by row and column.
//!
//! Rows are appended to 64 KiB blocks; a block is dropped once all its rows
//! have fallen off the oldest end. Every row that scrolls off is encoded, so
//! the encoder writes into room it has made sure of, by index, rather than
//! growing a `Vec` byte by byte.

use std::cmp::min;
use std::collections::VecDeque;
use std::mem;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::vt::event::EventListener;
use crate::vt::grid::{Dimensions, HistoryAbove, HistorySink, Row};
use crate::vt::index::{Column, Line};
use crate::vt::term::cell::{Cell, CellExtra, Flags};
use crate::vt::term::{Term, TermMode};
use crate::vt::vte::ansi::{Color, NamedColor, Rgb};

const BLOCK_BYTES: usize = 64 << 10;

// Run header byte: a run of blanks writes no characters, and only the
// attributes that changed are written.
const BLANK: u8 = 1 << 0;
const FG: u8 = 1 << 1;
const BG: u8 = 1 << 2;
const FLAGS: u8 = 1 << 3;

// A run's units, by their first byte: 0 an erased blank, other ASCII a narrow
// character (' ' a typed space), and the rest the first of two or three bytes
// that hold any other character, highest bits first: the two-byte form for
// characters below U+2000, as short as UTF-8 or shorter. A wide character
// (with the blank right half that makes it a pair) is written so too, marked
// WIDE, so that mixed text stays one run.
const UNIT_2: u8 = 0x80;
const UNIT_3: u8 = 0xc0;
const WIDE: u8 = 1 << 5;

const DEFAULT_FG: Color = Color::Named(NamedColor::Foreground);
const DEFAULT_BG: Color = Color::Named(NamedColor::Background);

#[derive(Debug)]
struct Block {
    /// Absolute number of the block's first row.
    first: u64,
    /// The rows take `bytes[..end]`; the rest is room for the next row.
    bytes: Vec<u8>,
    end: usize,
    /// Where each row starts in `bytes`: below `BLOCK_BYTES`, so a `u16`.
    starts: Vec<u16>,
}

type Extras = Box<[(u16, Arc<CellExtra>)]>;

#[derive(Debug)]
pub(crate) struct History {
    limit: usize,
    blocks: VecDeque<Block>,
    /// Absolute number of the oldest row kept.
    first: u64,
    len: usize,
    /// Rows are appended in order and dropped from the front, so a deque
    /// sorted by row number is enough.
    extras: VecDeque<(u64, Extras)>,
    /// The last block of `BLOCK_BYTES` dropped from the oldest end, kept for the next one: the system allocator
    /// of macOS does not always hand a freed 64 KiB back, which costs more footprint than the block.
    spare: Option<Block>,
    /// The program cleared the history (`CSI 3 J`, RIS). The engine returns
    /// the view to the bottom and lowers this.
    pub(crate) cleared: bool,
}

impl History {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            blocks: VecDeque::new(),
            first: 0,
            len: 0,
            extras: VecDeque::new(),
            spare: None,
            cleared: false,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// A number for row `i` that no other row ever gets: rows are never
    /// changed once stored, and a reflow stores them again under new numbers.
    pub(crate) fn id(&self, i: usize) -> u64 {
        self.first + i as u64
    }

    /// Row `i`, 0 being the oldest.
    pub(crate) fn row(&self, i: usize) -> Row<Cell> {
        let abs = self.first + i as u64;
        let at = self
            .blocks
            .partition_point(|b| b.first + b.starts.len() as u64 <= abs);
        let block = &self.blocks[at];
        let k = (abs - block.first) as usize;
        let end = block.starts.get(k + 1).map_or(block.end, |&e| e as usize);
        decode(
            &block.bytes[block.starts[k] as usize..end],
            self.extras
                .binary_search_by_key(&abs, |(at, _)| *at)
                .ok()
                .map(|i| &self.extras[i].1),
        )
    }

    /// Removes every row, oldest first.
    #[cfg(test)]
    fn take_all(&mut self) -> Vec<Row<Cell>> {
        let rows = (0..self.len).map(|i| self.row(i)).collect();
        self.drop_all();
        rows
    }

    fn drop_all(&mut self) {
        self.first += self.len as u64;
        self.len = 0;
        self.blocks.clear();
        self.extras.clear();
    }

    /// Stores a row that leaves the grid (or a reflow): the sink may blank the cells it takes (`encode_take`).
    ///
    /// A row is written straight into the room left in the newest block. Where that room may be too small, it is
    /// encoded aside first and goes in where it fits, so that no block grows past `BLOCK_BYTES` (into a larger
    /// allocation) but a block holding one row that is larger.
    fn push_row(&mut self, row: &mut Row<Cell>) {
        if self.limit == 0 {
            return;
        }
        let abs = self.first + self.len as u64;
        let scan = scan_len(row);
        let room = max_len(scan);
        let mut extras = Vec::new();
        match self.blocks.back_mut() {
            Some(block) if block.end + room <= block.bytes.len() => {
                block.starts.push(block.end as u16);
                block.end = encode_take(row, scan, &mut block.bytes, block.end, &mut extras);
            }
            _ => {
                let mut aside = vec![0; room];
                let len = encode_take(row, scan, &mut aside, 0, &mut extras);
                if self
                    .blocks
                    .back()
                    .is_none_or(|b| b.end + len > b.bytes.len())
                {
                    let spare = self.spare.take().filter(|_| len <= BLOCK_BYTES);
                    let mut block = spare.unwrap_or_else(|| Block {
                        first: 0,
                        bytes: vec![0; BLOCK_BYTES.max(len)],
                        end: 0,
                        starts: Vec::new(),
                    });
                    block.first = abs;
                    block.end = 0;
                    block.starts.clear();
                    self.blocks.push_back(block);
                }
                let Some(block) = self.blocks.back_mut() else {
                    return;
                };
                debug_assert!(block.end < BLOCK_BYTES);
                block.starts.push(block.end as u16);
                block.bytes[block.end..block.end + len].copy_from_slice(&aside[..len]);
                block.end += len;
            }
        }
        if !extras.is_empty() {
            self.extras.push_back((abs, extras.into_boxed_slice()));
        }
        self.len += 1;
        if self.len > self.limit {
            self.pop_oldest();
        }
    }

    fn pop_oldest(&mut self) {
        if self.extras.front().is_some_and(|(at, _)| *at == self.first) {
            self.extras.pop_front();
        }
        self.first += 1;
        self.len -= 1;
        if let Some(front) = self.blocks.front() {
            if self.first >= front.first + front.starts.len() as u64 {
                let block = self.blocks.pop_front();
                // A larger block (one row past `BLOCK_BYTES`) is freed: rows start below `BLOCK_BYTES` only.
                if self.spare.is_none()
                    && block.as_ref().is_some_and(|b| b.bytes.len() == BLOCK_BYTES)
                {
                    self.spare = block;
                }
            }
        }
    }

    /// Keeps the oldest `keep` rows.
    fn truncate_newest(&mut self, keep: usize) {
        let end = self.first + keep as u64;
        while let Some(block) = self.blocks.back_mut() {
            if block.first >= end {
                self.blocks.pop_back();
                continue;
            }
            let k = (end - block.first) as usize;
            if k < block.starts.len() {
                block.end = block.starts[k] as usize;
                block.starts.truncate(k);
            }
            break;
        }
        while self.extras.back().is_some_and(|(at, _)| *at >= end) {
            self.extras.pop_back();
        }
        self.len = min(self.len, keep);
    }

    /// Numbers the rows again from `first` on, which is past their numbers.
    fn renumber(&mut self, first: u64) {
        let delta = first - self.first;
        for block in &mut self.blocks {
            block.first += delta;
        }
        for (at, _) in &mut self.extras {
            *at += delta;
        }
        self.first = first;
    }
}

impl HistorySink<Cell> for History {
    fn push(&mut self, row: &mut Row<Cell>) {
        self.push_row(row);
    }

    fn clear(&mut self) {
        self.drop_all();
        self.cleared = true;
    }
}

/// The older history rows while the grid resizes. A column change reflows
/// them one row at a time, decoding each and storing the result in a new
/// [`History`] whose rows are numbered from `next` on.
#[derive(Debug)]
struct Above {
    rows: History,
    next: u64,
}

impl Above {
    /// Takes the rows for a reflow, leaving an empty history in their place.
    fn take(&mut self) -> History {
        let mut empty = History::new(usize::MAX);
        empty.first = self.next;
        mem::replace(&mut self.rows, empty)
    }
}

impl HistoryAbove<Cell> for Above {
    fn rows(&self) -> usize {
        self.rows.len()
    }

    fn grow_columns(
        &mut self,
        reflow: bool,
        columns: usize,
        base: usize,
        display_offset: &mut usize,
        keep: usize,
    ) -> Vec<Row<Cell>> {
        let mut older = self.take();
        // Reflowed rows not stored yet. The newest can still take cells from
        // the next row.
        let mut rows = VecDeque::new();
        for i in (base..base + older.len()).rev() {
            let row = older.row(0);
            older.pop_oldest();
            grow_row(&mut rows, row, reflow, columns, i, display_offset);
            while rows.len() > keep.max(1) {
                if let Some(mut row) = rows.pop_front() {
                    // Without reflow a row keeps its wrapped mark, as the grid does with the xterm.js resize.
                    if reflow {
                        row.grow(columns);
                    } else {
                        row.resize_keeping_wrap(columns);
                    }
                    self.rows.push_row(&mut row);
                }
            }
        }
        rows.into()
    }

    fn shrink_columns(
        &mut self,
        reflow: bool,
        columns: usize,
        base: usize,
        display_offset: &mut usize,
    ) -> Option<Vec<Cell>> {
        let mut older = self.take();
        let mut rows = Vec::new();
        let mut buffered = None;
        for i in (base..base + older.len()).rev() {
            let row = older.row(0);
            older.pop_oldest();
            shrink_row(
                &mut rows,
                &mut buffered,
                row,
                reflow,
                columns,
                i,
                display_offset,
            );
            for mut row in rows.drain(..) {
                self.rows.push_row(&mut row);
            }
        }
        buffered
    }

    fn truncate(&mut self, max: usize) {
        while self.rows.len() > max {
            self.rows.pop_oldest();
        }
    }
}

/// One history row through `Grid::grow_columns` (`vt/grid/resize.rs`), which
/// it follows line by line. `rows` ends with the rows reflowed so far.
fn grow_row(
    rows: &mut VecDeque<Row<Cell>>,
    mut row: Row<Cell>,
    reflow: bool,
    columns: usize,
    i: usize,
    display_offset: &mut usize,
) {
    // Check if a row needs to be wrapped.
    let should_reflow = |row: &Row<Cell>| -> bool {
        let len = row.len();
        reflow && len > 0 && len < columns && row[Column(len - 1)].flags.contains(Flags::WRAPLINE)
    };

    let last_row = match rows.back_mut() {
        Some(last_row) if should_reflow(last_row) => last_row,
        _ => {
            rows.push_back(row);
            return;
        }
    };

    // Remove wrap flag before appending additional cells.
    if let Some(cell) = last_row.last_mut() {
        cell.flags.remove(Flags::WRAPLINE);
    }

    // Remove leading spacers when reflowing wide char to the previous line.
    let mut last_len = last_row.len();
    if last_len >= 1
        && last_row[Column(last_len - 1)]
            .flags
            .contains(Flags::LEADING_WIDE_CHAR_SPACER)
    {
        last_row.shrink(last_len - 1);
        last_len -= 1;
    }

    // Don't try to pull more cells from the next line than available.
    let len = min(row.len(), columns - last_len);

    // Insert leading spacer when there's not enough room for reflowing wide char.
    let mut cells = if row[Column(len - 1)].flags.contains(Flags::WIDE_CHAR) {
        let mut cells = row.front_split_off(len - 1);

        let mut spacer = Cell::default();
        spacer.flags.insert(Flags::LEADING_WIDE_CHAR_SPACER);
        cells.push(spacer);

        cells
    } else {
        row.front_split_off(len)
    };

    // Add removed cells to previous row and reflow content.
    last_row.append(&mut cells);

    if row.is_clear() {
        if i < *display_offset {
            // Since we removed a line, rotate down the viewport.
            *display_offset = display_offset.saturating_sub(1);
        }

        // Don't push line into the new buffer.
        return;
    }

    if let Some(cell) = last_row.last_mut() {
        // Set wrap flag if next line still has cells.
        cell.flags.insert(Flags::WRAPLINE);
    }

    rows.push_back(row);
}

/// One history row through `Grid::shrink_columns` (`vt/grid/resize.rs`),
/// which it follows line by line. `buffered` carries cells into the next row.
fn shrink_row(
    rows: &mut Vec<Row<Cell>>,
    buffered: &mut Option<Vec<Cell>>,
    mut row: Row<Cell>,
    reflow: bool,
    columns: usize,
    i: usize,
    display_offset: &mut usize,
) {
    // Without reflow a row keeps its wrapped mark, as the grid does with the xterm.js resize.
    if !reflow {
        row.resize_keeping_wrap(columns);
        rows.push(row);
        return;
    }

    // Append lines left over from the previous row.
    if let Some(buffered) = buffered.take() {
        row.append_front(buffered);
    }

    loop {
        // Remove all cells which require reflowing.
        let mut wrapped = match row.shrink(columns) {
            Some(wrapped) if reflow => wrapped,
            _ => {
                // Since it fits, just push the existing line without any reflow.
                rows.push(row);
                break;
            }
        };

        // Insert spacer if a wide char would be wrapped into the last column.
        if row.len() >= columns && row[Column(columns - 1)].flags.contains(Flags::WIDE_CHAR) {
            let mut spacer = Cell::default();
            spacer.flags.insert(Flags::LEADING_WIDE_CHAR_SPACER);

            let wide_char = mem::replace(&mut row[Column(columns - 1)], spacer);
            wrapped.insert(0, wide_char);
        }

        // Remove wide char spacer before shrinking.
        let len = wrapped.len();
        if len > 0
            && wrapped[len - 1]
                .flags
                .contains(Flags::LEADING_WIDE_CHAR_SPACER)
        {
            if len == 1 {
                row[Column(columns - 1)].flags.insert(Flags::WRAPLINE);
                rows.push(row);
                break;
            } else {
                // Remove the leading spacer from the end of the wrapped row.
                wrapped[len - 2].flags.insert(Flags::WRAPLINE);
                wrapped.truncate(len - 1);
            }
        }

        // Set line as wrapped if cells got removed.
        if let Some(cell) = row.last_mut() {
            cell.flags.insert(Flags::WRAPLINE);
        }
        rows.push(row);

        if wrapped
            .last()
            .is_some_and(|c| c.flags.contains(Flags::WRAPLINE) && i >= 1)
            && wrapped.len() < columns
        {
            // Make sure previous wrap flag doesn't linger around.
            if let Some(cell) = wrapped.last_mut() {
                cell.flags.remove(Flags::WRAPLINE);
            }

            // Add removed cells to start of next row.
            *buffered = Some(wrapped);
            break;
        }

        // Make sure new row is at least as long as new width.
        let occ = wrapped.len();
        if occ < columns {
            wrapped.resize_with(columns, Cell::default);
        }
        row = Row::from_vec(wrapped, occ);

        if i < *display_offset {
            // Since we added a new line, rotate up the viewport.
            *display_offset += 1;
        }
    }
}

/// Compressed scrollback for a caller that drives its own [`Term`] instead of
/// an [`Engine`](crate::Engine).
///
/// Build the term with `scrolling_history: TermHistory::RECENT` and attach
/// this before feeding it: the primary grid then keeps the newest `RECENT`
/// history rows and hands older ones here. Rows are numbered oldest first and
/// sit above the grid's own history; they belong to the primary screen. Resize
/// through [`TermHistory::resize`] so that reflow sees the whole history.
#[derive(Clone)]
pub struct TermHistory {
    inner: Arc<Mutex<History>>,
    scrollback: usize,
}

impl TermHistory {
    /// History rows the grid itself keeps.
    // Overwriting a wide character fixes up the row just above it, which can
    // be a history row, so this cannot be 0.
    pub const RECENT: usize = 16;

    /// Attaches to the primary grid of `term`, keeping `scrollback` history
    /// rows in all.
    pub fn attach<T>(term: &mut Term<T>, scrollback: usize) -> Self {
        let inner = Arc::new(Mutex::new(History::new(
            scrollback.saturating_sub(Self::RECENT),
        )));
        let sink: Arc<Mutex<dyn HistorySink<Cell>>> = inner.clone();
        term.grid_mut().set_history_sink(Some(sink));
        Self { inner, scrollback }
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, History> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Rows held here, above the grid's history.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Row `i`, 0 being the oldest.
    pub fn row(&self, i: usize) -> Row<Cell> {
        self.lock().row(i)
    }

    /// Resizes `term`, reflowing these rows with the grid's.
    pub fn resize<T: EventListener>(&self, term: &mut Term<T>, size: impl Dimensions) {
        self.reflow(term, size, 0);
    }

    /// Resizes `term` as [`Term::resize`] would with the whole history in the
    /// primary grid. `offset` is the primary screen's view offset; returns it
    /// after the resize.
    ///
    /// Only the newest rows go back into the grid: enough for every step that
    /// looks at history near the screen (rows a taller screen pulls down, the
    /// cursor paragraph, the counts the reflow makes). The older rows stay
    /// above the grid ([`HistoryAbove`]), and a column change reflows them one
    /// row at a time, so the extra memory is about a screen and one logical
    /// line, not the history.
    pub(crate) fn reflow<T: EventListener>(
        &self,
        term: &mut Term<T>,
        size: impl Dimensions,
        offset: usize,
    ) -> usize {
        let (lines, columns) = (size.screen_lines(), size.columns());
        if (lines, columns) == (term.screen_lines(), term.columns()) {
            return offset;
        }
        let alt = term.mode().contains(TermMode::ALT_SCREEN);
        let grid = if alt {
            term.inactive_grid_mut()
        } else {
            term.grid_mut()
        };
        let mut stored = self.lock();
        let sink = grid.set_history_sink(None);
        let (next, cleared, limit) = (stored.id(stored.len()), stored.cleared, stored.limit);

        // The rows the grid takes back, newest first: two screens' worth (old
        // and new height), and the rest of the cursor paragraph if it reaches
        // past them. The paragraph runs up from the cursor through rows that
        // wrap; if it reaches the top of the grid, it goes on into these rows.
        let old_lines = grid.screen_lines();
        let last = grid.last_column();
        let mut line = grid.cursor.point.line;
        while line > grid.topmost_line()
            && grid[Line(line.0 - 1)][last].flags.contains(Flags::WRAPLINE)
        {
            line -= 1;
        }
        let mut paragraph = line == grid.topmost_line();
        let n = stored.len();
        let mut tail = Vec::new();
        while tail.len() < n {
            let row = stored.row(n - 1 - tail.len());
            paragraph &= row
                .last()
                .is_some_and(|c| c.flags.contains(Flags::WRAPLINE));
            if tail.len() >= old_lines + lines && !paragraph {
                break;
            }
            tail.push(row);
        }

        // A shorter screen scrolls the rows above the cursor into history, and
        // a full history drops its oldest rows for them. Those are above the
        // grid, so drop them here.
        let scrolled = if lines < old_lines {
            (grid.cursor.point.line.0 as usize + 1).saturating_sub(lines)
        } else {
            0
        };
        let mut dropped = (n + grid.history_size() + scrolled).saturating_sub(self.scrollback);
        if dropped > n - tail.len() {
            // They reach the rows the grid takes back: the grid takes all.
            while tail.len() < n {
                tail.push(stored.row(n - 1 - tail.len()));
            }
            dropped = 0;
        }
        stored.truncate_newest(n - tail.len());
        for _ in 0..dropped {
            stored.pop_oldest();
        }
        tail.reverse();

        let above = Arc::new(Mutex::new(Above {
            rows: mem::replace(&mut *stored, History::new(usize::MAX)),
            next,
        }));
        let held: Arc<Mutex<dyn HistoryAbove<Cell>>> = above.clone();
        grid.prepend_history(tail, self.scrollback);
        grid.set_display_offset(offset);
        grid.set_history_above(Some(held));
        term.resize(size);
        let grid = if alt {
            term.inactive_grid_mut()
        } else {
            term.grid_mut()
        };
        let offset = grid.display_offset();
        grid.set_history_above(None);
        let mut rows = above.lock().unwrap_or_else(PoisonError::into_inner).take();
        // Rows that were not reflowed get new numbers too: the rows the grid
        // took back are stored again after them.
        if rows.first < next {
            rows.renumber(next);
        }
        // The grid keeps the newest `RECENT` history rows, wherever the reflow
        // left them.
        let short = Self::RECENT
            .saturating_sub(grid.history_size())
            .min(rows.len());
        if short > 0 {
            let back = (rows.len() - short..rows.len())
                .map(|i| rows.row(i))
                .collect();
            rows.truncate_newest(rows.len() - short);
            grid.prepend_history(back, self.scrollback);
        }
        let mut older = grid.split_off_history(Self::RECENT);
        grid.set_history_sink(sink);
        rows.limit = limit;
        rows.cleared = cleared;
        for row in &mut older {
            rows.push_row(row);
        }
        while rows.len() > limit {
            rows.pop_oldest();
        }
        *stored = rows;
        offset
    }

    /// The resize before [`HistoryAbove`]: decodes the whole history into the
    /// grid, resizes, and compresses it again. The reference for `reflow`.
    #[cfg(test)]
    fn reflow_take_all<T: EventListener>(
        &self,
        term: &mut Term<T>,
        size: impl Dimensions,
        offset: usize,
    ) -> usize {
        let older = self.lock().take_all();
        let alt = term.mode().contains(TermMode::ALT_SCREEN);
        let grid = if alt {
            term.inactive_grid_mut()
        } else {
            term.grid_mut()
        };
        let sink = grid.set_history_sink(None);
        grid.prepend_history(older, self.scrollback);
        grid.set_display_offset(offset);
        term.resize(size);
        let grid = if alt {
            term.inactive_grid_mut()
        } else {
            term.grid_mut()
        };
        let offset = grid.display_offset();
        let mut older = grid.split_off_history(Self::RECENT);
        grid.set_history_sink(sink);
        let mut stored = self.lock();
        for row in &mut older {
            stored.push_row(row);
        }
        offset
    }
}

/// A wide character cell and the cell right after it (a blank with the same
/// attributes) are written as one unit.
#[inline]
fn is_pair(cells: &[Cell], i: usize) -> bool {
    let (Some(wide), Some(spacer)) = (cells.get(i), cells.get(i + 1)) else {
        return false;
    };
    wide.flags.contains(Flags::WIDE_CHAR)
        && !wide.flags.contains(Flags::TYPED_SPACE)
        && wide.c != ' '
        && spacer.c == ' '
        && spacer.extra.is_none()
        && spacer.fg == wide.fg
        && spacer.bg == wide.bg
        && spacer.flags == (wide.flags - Flags::WIDE_CHAR) | Flags::WIDE_CHAR_SPACER
}

/// `a == b`, branching on `b`: in a run it is the same for every cell.
#[inline(always)]
fn same_color(a: Color, b: Color) -> bool {
    match b {
        Color::Named(n) => matches!(a, Color::Named(m) if m == n),
        Color::Spec(x) => matches!(a, Color::Spec(y) if y == x),
        Color::Indexed(i) => matches!(a, Color::Indexed(j) if j == i),
    }
}

#[inline]
fn is_default(c: &Cell) -> bool {
    c.c == ' '
        && c.flags.is_empty()
        && c.fg == DEFAULT_FG
        && c.bg == DEFAULT_BG
        && c.extra.is_none()
}

/// The cells `encode` looks at: past the last cell that is not a default blank,
/// all are default blanks, and those at the end of the row are left out. Cells
/// past `occ` all equal the last cell (the Row::reset invariant), so with a
/// default blank last the scan can stop there.
#[inline]
/// What `Row::plain` promises of the cells of a row.
fn is_plain(cells: &[Cell]) -> bool {
    let attrs = |c: &Cell| c.fg == DEFAULT_FG && c.bg == DEFAULT_BG && c.extra.is_none();
    let mut i = 0;
    while let Some(c) = cells.get(i) {
        let pair = c.flags == Flags::WIDE_CHAR
            && c.c != ' '
            && cells
                .get(i + 1)
                .is_some_and(|r| attrs(r) && r.c == ' ' && r.flags == Flags::WIDE_CHAR_SPACER);
        if !(attrs(c) && (pair || (c.flags - Flags::TYPED_SPACE).is_empty())) {
            return false;
        }
        i += 1 + pair as usize;
    }
    true
}

fn scan_len(row: &Row<Cell>) -> usize {
    let cells = &row[..];
    if cells.last().is_some_and(is_default) {
        (row.occ as usize).min(cells.len())
    } else {
        cells.len()
    }
}

/// The most bytes `encode` writes for a row with `scan` cells to look at (while
/// it works, too): the column count, then per cell a run header with both
/// colours and the flags, and a unit written as four bytes.
#[inline]
fn max_len(scan: usize) -> usize {
    scan * 18 + 16
}

/// Writes into a buffer that has room for all of it (`max_len`).
struct Out<'a> {
    buf: &'a mut [u8],
    at: usize,
}

impl Out<'_> {
    #[inline(always)]
    fn byte(&mut self, b: u8) {
        self.buf[self.at] = b;
        self.at += 1;
    }

    /// Writes `len` bytes of `word`, first byte lowest.
    #[inline(always)]
    fn word(&mut self, word: u32, len: usize) {
        self.buf[self.at..self.at + 4].copy_from_slice(&word.to_le_bytes());
        self.at += len;
    }

    fn varint(&mut self, mut n: usize) {
        while n >= 0x80 {
            self.byte(n as u8 | 0x80);
            n >>= 7;
        }
        self.byte(n as u8);
    }

    #[inline(always)]
    fn color(&mut self, color: Color) {
        let (bytes, len) = match color {
            Color::Named(n) => {
                let [lo, hi] = (n as u16).to_le_bytes();
                ([0, lo, hi, 0], 3)
            }
            Color::Indexed(i) => ([1, i, 0, 0], 2),
            Color::Spec(Rgb { r, g, b }) => ([2, r, g, b], 4),
        };
        self.word(u32::from_le_bytes(bytes), len);
    }

    /// Writes a run header (kind, two bytes for the unit count, the
    /// attributes that changed from the previous run) and returns where it
    /// starts.
    #[inline(always)]
    fn head(&mut self, prev: &mut (Color, Color, Flags), key: (Color, Color, Flags)) -> usize {
        let head = self.at;
        let mut tag = 0;
        self.at += 3;
        if key != *prev {
            if key.0 != prev.0 {
                tag |= FG;
                self.color(key.0);
            }
            if key.1 != prev.1 {
                tag |= BG;
                self.color(key.1);
            }
            if key.2 != prev.2 {
                tag |= FLAGS;
                // Every flag but TYPED_SPACE, which runs leave out, fits in 16 bits.
                debug_assert!(key.2.bits() >> 16 == 0);
                let [lo, hi] = (key.2.bits() as u16).to_le_bytes();
                self.byte(lo);
                self.byte(hi);
            }
            *prev = key;
        }
        self.buf[head] = tag;
        head
    }
}

/// A unit (`UNIT_2`, `UNIT_3`) as the word to write and its length, without
/// branches: in mixed text the kind and the length change all the time.
#[inline(always)]
fn unit(c: char, wide: bool) -> (u32, usize) {
    let (n, w) = (c as u32, if wide { WIDE as u32 } else { 0 });
    // The marked first byte goes first, then the rest of the character, highest byte first. Below U+2000 the
    // first of the three bytes is the marker alone, and the other two are the two-byte form but for its marker.
    let three = ((UNIT_3 as u32 | w) << 16 | n).swap_bytes() >> 8;
    let two = three >> 8 | UNIT_2 as u32 | w;
    // Selects, not branches: in mixed text the length changes from one character to the next.
    let (long, big) = ((n >= 0x80) | wide, n >= 0x2000);
    let word = if big { three } else { two };
    (
        if long { word } else { n },
        1 + long as usize + big as usize,
    )
}

/// Writes the leading `cells` that are narrow ASCII characters (typed spaces
/// too) with the flags `key` and colours that pass `colors`, one byte each, and
/// returns how many.
#[inline(always)]
fn ascii(cells: &[Cell], out: &mut Out, key: Flags, colors: impl Fn(&Cell) -> bool) -> usize {
    let dst = &mut out.buf[out.at..out.at + cells.len()];
    let typed = Flags::TYPED_SPACE.bits();
    let mut n = 0;
    for (d, cell) in dst.iter_mut().zip(cells) {
        let (c, flags) = (cell.c as u32, cell.flags.bits());
        if !(colors(cell)
            & (flags & !typed == key.bits())
            & cell.extra.is_none()
            & (c < 0x80)
            & ((c != b' ' as u32) | (flags & typed != 0)))
        {
            break;
        }
        *d = c as u8;
        n += 1;
    }
    out.at += n;
    n
}

/// Writes the leading units of a run in `cells` that are text (not erased
/// blanks) with colours that pass `colors`, and returns the cells and units
/// taken. Cells with extras are left to the caller unless `extras`.
#[inline(always)]
fn text(
    cells: &[Cell],
    out: &mut Out,
    key: Flags,
    extras: bool,
    colors: impl Fn(&Cell) -> bool,
) -> (usize, usize) {
    let at = ascii(cells, out, key, &colors);
    // What stops `ascii` at a narrow ASCII cell without extras (other colours or flags, an erased blank) stops
    // the run.
    if cells
        .get(at)
        .is_none_or(|c| c.c.is_ascii() & c.extra.is_none() & !c.flags.contains(Flags::WIDE_CHAR))
    {
        return (at, at);
    }
    let (n, u) = if key.intersects(Flags::WIDE_CHAR | Flags::WIDE_CHAR_SPACER) {
        units_of(&cells[at..], out, key, extras, colors)
    } else {
        mixed(&cells[at..], out, key, extras, colors)
    };
    (at + n, at + u)
}

/// `text` for mixed narrow and wide characters. Their kinds and lengths
/// change all the time, so the loop goes one cell at a time and works them out
/// without branches: taking a unit at a time would make each cell wait for the
/// kind of the one before. A wide character is written before its right half
/// is looked at, and taken back if that is not a match.
#[inline(always)]
fn mixed(
    cells: &[Cell],
    out: &mut Out,
    key: Flags,
    extras: bool,
    colors: impl Fn(&Cell) -> bool,
) -> (usize, usize) {
    let (wide, right) = (key | Flags::WIDE_CHAR, key | Flags::WIDE_CHAR_SPACER);
    let (mut at, mut units) = (0, 0);
    // The cell is the right half of the wide character before it, written at `back`.
    let (mut half, mut back) = (false, out.at);
    while let Some(cell) = cells.get(at) {
        let (c, flags) = (cell.c, cell.flags);
        let (space, none) = (c == ' ', cell.extra.is_none());
        let narrow =
            (flags - Flags::TYPED_SPACE == key) & (!space | flags.contains(Flags::TYPED_SPACE));
        let pair = (flags == wide) & !space;
        let ok = colors(cell)
            & ((half & space & none & (flags == right))
                | (!half & (narrow | pair) & (extras | none)));
        if !ok {
            break;
        }
        let (word, len) = unit(c, pair);
        back = if pair { out.at } else { back };
        out.word(word, if half { 0 } else { len });
        units += !half as usize;
        half = pair;
        at += 1;
    }
    if half {
        // A wide character without its right half: not part of the run.
        out.at = back;
        (at - 1, units - 1)
    } else {
        (at, units)
    }
}

/// `text` one unit at a time, for runs whose flags hold a wide character's.
fn units_of(
    cells: &[Cell],
    out: &mut Out,
    key: Flags,
    extras: bool,
    colors: impl Fn(&Cell) -> bool,
) -> (usize, usize) {
    let (wide, right) = (key | Flags::WIDE_CHAR, key | Flags::WIDE_CHAR_SPACER);
    let (mut at, mut units) = (0, 0);
    while let Some(cell) = cells.get(at) {
        let (c, flags) = (cell.c, cell.flags);
        if !(colors(cell) & (extras | cell.extra.is_none())) {
            break;
        }
        let pair = if flags - Flags::TYPED_SPACE == key
            && (c != ' ' || flags.contains(Flags::TYPED_SPACE))
        {
            false
        } else if flags == wide
            && c != ' '
            && cells
                .get(at + 1)
                .is_some_and(|r| (r.c == ' ') & r.extra.is_none() & (r.flags == right) & colors(r))
        {
            true
        } else {
            break;
        };
        let (word, len) = unit(c, pair);
        out.word(word, len);
        at += 1 + pair as usize;
        units += 1;
    }
    (at, units)
}

/// Writes `row` into `buf` from `at` on and returns where it ends. `buf` has
/// room for [`max_len`] bytes from `at` for the row's [`scan_len`] `scan`.
fn encode(
    row: &Row<Cell>,
    scan: usize,
    buf: &mut [u8],
    at: usize,
    extras: &mut Vec<(u16, Arc<CellExtra>)>,
) -> usize {
    let cells = &row[..];
    let mut out = Out { buf, at };
    out.varint(cells.len());
    debug_assert!(cells[scan..].iter().all(is_default), "cells past occ");
    let cells = &cells[..scan];
    // Most rows are narrow ASCII text with the default attributes, then default blanks: one run, written here.
    let head = out.at;
    out.at += 3;
    let n = ascii(cells, &mut out, Flags::empty(), |c| {
        matches!(c.fg, DEFAULT_FG) & matches!(c.bg, DEFAULT_BG)
    });
    // Then most others are text of any width with the default attributes: the run goes on through them.
    let (end, units) = if n == scan {
        (n, n)
    } else {
        plain(cells, n, &mut out)
    };
    if units == 0 {
        out.at = head;
        return if end == scan {
            head
        } else {
            runs(cells, 0, out, extras)
        };
    }
    let [lo, hi] = (units as u16).to_le_bytes();
    out.buf[head..head + 3].copy_from_slice(&[0, lo, hi]);
    if end == scan {
        return out.at;
    }
    runs(cells, end, out, extras)
}

/// `encode` for a row that leaves the grid. A plain row (`Row::plain`: default
/// blanks, narrow characters and wide pairs with the default attributes) is
/// one run of its units, read without checking each cell's attributes or
/// pairs, and left blank for the grid's reset (`occ` 0).
fn encode_take(
    row: &mut Row<Cell>,
    scan: usize,
    buf: &mut [u8],
    at: usize,
    extras: &mut Vec<(u16, Arc<CellExtra>)>,
) -> usize {
    if !row.plain {
        return encode(row, scan, buf, at, extras);
    }
    debug_assert!(is_plain(&row[..]), "stale Row::plain");
    let mut out = Out { buf, at };
    out.varint(row.len());
    let head = out.at;
    out.at += 3;
    let cells = &mut row[..][..scan];
    // Narrow ASCII first, a byte a cell (a wide character is never ASCII, and its spacer comes after it).
    let dst = &mut out.buf[out.at..out.at + scan];
    let mut i = 0;
    for (d, cell) in dst.iter_mut().zip(cells.iter_mut()) {
        let c = cell.c as u32;
        if c >= 0x80 {
            break;
        }
        // An erased blank is 0 in a run, a typed space ' '.
        let erased = (c == b' ' as u32) & cell.flags.is_empty();
        *d = if erased { 0 } else { c as u8 };
        cell.c = ' ';
        cell.flags = Flags::empty();
        i += 1;
    }
    let mut units = i;
    // After the last unit that is not an erased blank.
    let mut end_units = dst[..i].iter().rposition(|&b| b != 0).map_or(0, |n| n + 1);
    out.at += i;
    let mut end = out.at - (i - end_units);
    // Then the rest a cell at a time, a spacer as nothing: the next cell does not wait on this one's width.
    for cell in &mut cells[i..] {
        let flags = cell.flags.bits();
        let spacer = flags & Flags::WIDE_CHAR_SPACER.bits() != 0;
        // An erased blank is 0 in a run, a typed space ' '.
        let erased = (cell.c == ' ') & (flags == 0);
        let (word, len) = unit(cell.c, flags & Flags::WIDE_CHAR.bits() != 0);
        out.word(
            word & (erased as u32).wrapping_sub(1),
            if spacer { 0 } else { len },
        );
        units += !spacer as usize;
        let text = !(erased | spacer);
        end = if text { out.at } else { end };
        end_units = if text { units } else { end_units };
        cell.c = ' ';
        cell.flags = Flags::empty();
    }
    row.occ = 0;
    row.plain = true;
    if end_units == 0 {
        return head;
    }
    let [lo, hi] = (end_units as u16).to_le_bytes();
    out.buf[head..head + 3].copy_from_slice(&[0, lo, hi]);
    end
}

/// Goes on with the first run of `encode` from cell `at`, which narrow ASCII
/// text with the default attributes ends: takes the cells with the default
/// colours and no extras that are narrow characters (typed spaces too),
/// erased blanks, or wide characters with their right half (`is_pair`), and
/// writes each as a unit. Returns the cell and the unit count after the last
/// unit that is not an erased blank, and leaves `out` there: the cells after
/// it are for `runs`. A unit at a time, a wide character and its right half
/// in one step.
#[inline(always)]
fn plain(cells: &[Cell], mut at: usize, out: &mut Out) -> (usize, usize) {
    let (wide, right, typed) = (
        Flags::WIDE_CHAR.bits(),
        Flags::WIDE_CHAR_SPACER.bits(),
        Flags::TYPED_SPACE.bits(),
    );
    let attrs =
        |c: &Cell| matches!(c.fg, DEFAULT_FG) & matches!(c.bg, DEFAULT_BG) & c.extra.is_none();
    let scan = cells.len();
    let mut units = at;
    let (mut end, mut end_units, mut end_out) = (at, at, out.at);
    while at < scan {
        let cell = &cells[at];
        // The cell after it, which a wide character's right half has to be. Past the end the cell itself, which
        // then is no right half.
        let next = &cells[min(at + 1, scan - 1)];
        let (c, flags) = (cell.c, cell.flags.bits());
        let pair = flags == wide;
        let half = (next.c == ' ') & (next.flags.bits() == right) & attrs(next);
        let ok = attrs(cell) & ((pair & half & (c != ' ')) | (flags & !typed == 0));
        if !ok {
            break;
        }
        let erased = (c == ' ') & (flags == 0);
        let (word, len) = unit(c, pair);
        out.word(word & (erased as u32).wrapping_sub(1), len);
        at += 1 + pair as usize;
        units += 1;
        // Erased blanks that end the run are left to `runs`.
        let text = !erased;
        end = if text { at } else { end };
        end_units = if text { units } else { end_units };
        end_out = if text { out.at } else { end_out };
    }
    out.at = end_out;
    (end, end_units)
}

/// `encode` for the other rows: the runs of `cells` from cell `start` on,
/// written from `out.at` on.
#[inline(never)]
fn runs(
    cells: &[Cell],
    mut start: usize,
    mut out: Out,
    extras: &mut Vec<(u16, Arc<CellExtra>)>,
) -> usize {
    let scan = cells.len();
    let mut prev = (DEFAULT_FG, DEFAULT_BG, Flags::empty());
    while start < scan {
        let first = &cells[start];
        let (fg, bg) = (first.fg, first.bg);
        // A typed space keeps its run: it is written as ' ', an erased blank inside a run as '\0'.
        let key = if is_pair(cells, start) {
            first.flags - Flags::WIDE_CHAR
        } else {
            first.flags - Flags::TYPED_SPACE
        };
        // Write the header first and the units right after it, then fill in
        // the unit count and BLANK.
        let head = out.head(&mut prev, (fg, bg, key));
        let body = out.at;
        // A character whose next cell has other colours is a run alone (as with a colour per cell).
        if first.extra.is_none()
            && first.flags == key
            && first.c != ' '
            && cells
                .get(start + 1)
                .is_none_or(|c| (c.fg, c.bg) != (fg, bg))
        {
            let (word, len) = unit(first.c, false);
            out.word(word, len);
            out.buf[head + 1..head + 3].copy_from_slice(&1u16.to_le_bytes());
            start += 1;
            continue;
        }
        let default = (fg, bg) == (DEFAULT_FG, DEFAULT_BG);
        let colors = |c: &Cell| same_color(c.fg, fg) & same_color(c.bg, bg);
        let (mut at, mut units, mut extra) = (start, 0, false);
        // Cell, unit count and byte after the last unit that is not an erased blank: the erased blanks after it are
        // read again by the next run, as one blank run.
        let (mut text_end, mut text_units, mut text_at) = (start, 0, body);
        while at < scan {
            // A run takes the cells with its colours: narrow ones with its flags, and wide characters each with the
            // blank right half that makes it a pair (`is_pair`).
            let (n, u) = if default {
                text(&cells[at..], &mut out, key, false, |c| {
                    matches!(c.fg, DEFAULT_FG) & matches!(c.bg, DEFAULT_BG)
                })
            } else {
                text(&cells[at..], &mut out, key, false, colors)
            };
            if u > 0 {
                (at, units) = (at + n, units + u);
                (text_end, text_units, text_at) = (at, units, out.at);
            }
            let Some(cell) = cells.get(at) else {
                break;
            };
            let erased = cell.c == ' ' && cell.flags == key;
            if !(erased | cell.extra.is_some()) || !colors(cell) {
                break;
            }
            if erased {
                out.byte(0);
                extra |= cell.extra.is_some();
                at += 1;
                units += 1;
            } else {
                // A unit with extras, as `text` takes it.
                let (n, u) = text(&cells[at..scan.min(at + 2)], &mut out, key, true, colors);
                if u == 0 {
                    break;
                }
                extra = true;
                (at, units) = (at + n, units + u);
                (text_end, text_units, text_at) = (at, units, out.at);
            }
        }
        if units == 0 {
            // Not reached: the first cell of a run is always one of its units. Should it be missed, it is taken here
            // so that the loop goes on.
            let (n, u) = units_of(&cells[start..], &mut out, key, true, colors);
            (at, units, extra) = (start + n.max(1), u, true);
            (text_end, text_units, text_at) = (at, units, out.at);
        }
        if text_units == 0 {
            // Erased blanks only. Default ones that end the row are left out.
            if at == scan && !extra && default && key.is_empty() {
                out.at = head;
                break;
            }
            out.buf[head] |= BLANK;
            (text_end, text_units, text_at) = (at, units, body);
        }
        out.at = text_at;
        // A blank run keeps the extras of all its cells, a run with text those of its units.
        if extra {
            for (col, c) in cells[start..text_end].iter().enumerate() {
                if let Some(extra) = &c.extra {
                    extras.push(((start + col) as u16, extra.clone()));
                }
            }
        }
        out.buf[head + 1..head + 3].copy_from_slice(&(text_units as u16).to_le_bytes());
        start = text_end;
    }
    out.at
}

/// Decodes one row. Bytes that do not hold a row decode to blanks, as many as
/// the width they start with when it is readable, otherwise one.
fn decode(mut bytes: &[u8], extras: Option<&Extras>) -> Row<Cell> {
    let cols = take_varint(&mut bytes).filter(|n| (1..=u16::MAX as usize).contains(n));
    cols.and_then(|cols| decode_cells(bytes, cols, extras))
        .unwrap_or_else(|| Row::new(cols.unwrap_or(1)))
}

fn decode_cells(mut bytes: &[u8], cols: usize, extras: Option<&Extras>) -> Option<Row<Cell>> {
    let mut cells = Vec::with_capacity(cols);
    let (mut fg, mut bg, mut flags) = (DEFAULT_FG, DEFAULT_BG, Flags::empty());
    // The default blanks that end the row are not written.
    while cells.len() < cols && !bytes.is_empty() {
        let head = take(&mut bytes, 3)?;
        let (tag, n) = (head[0], u16::from_le_bytes([head[1], head[2]]) as usize);
        if tag & FG != 0 {
            fg = take_color(&mut bytes)?;
        }
        if tag & BG != 0 {
            bg = take_color(&mut bytes)?;
        }
        if tag & FLAGS != 0 {
            let b = take(&mut bytes, 2)?;
            flags = Flags::from_bits_retain(u16::from_le_bytes([b[0], b[1]]) as u32);
        }
        let cell = |c, flags| Cell {
            c,
            fg,
            bg,
            flags,
            extra: None,
        };
        if tag & BLANK != 0 {
            if cells.len() + n > cols {
                return None;
            }
            cells.resize(cells.len() + n, cell(' ', flags));
            continue;
        }
        for _ in 0..n {
            let (&b, rest) = bytes.split_first()?;
            let (c, wide) = match b {
                0 => (' ', false),
                b' ' => {
                    bytes = rest;
                    cells.push(cell(' ', flags | Flags::TYPED_SPACE));
                    continue;
                }
                0x01..0x80 => (b as char, false),
                _ => {
                    let len = if b < UNIT_3 { 2 } else { 3 };
                    let unit = take(&mut bytes, len)?;
                    let n = unit[1..]
                        .iter()
                        .fold((b & 0x1f) as u32, |n, &b| n << 8 | b as u32);
                    (char::from_u32(n)?, b & WIDE != 0)
                }
            };
            if b < 0x80 {
                bytes = rest;
            }
            if wide {
                cells.push(cell(c, flags | Flags::WIDE_CHAR));
                cells.push(cell(' ', flags | Flags::WIDE_CHAR_SPACER));
            } else {
                cells.push(cell(c, flags));
            }
        }
        if cells.len() > cols {
            return None;
        }
    }
    cells.resize(cols, Cell::default());
    for (col, extra) in extras.into_iter().flat_map(|e| e.iter()) {
        cells.get_mut(*col as usize)?.extra = Some(extra.clone());
    }
    Some(Row::from_vec(cells, cols))
}

fn take_color(bytes: &mut &[u8]) -> Option<Color> {
    Some(match take(bytes, 1)?[0] {
        0 => {
            let b = take(bytes, 2)?;
            Color::Named(named(u16::from_le_bytes([b[0], b[1]]))?)
        }
        1 => Color::Indexed(take(bytes, 1)?[0]),
        _ => {
            let b = take(bytes, 3)?;
            Color::Spec(Rgb {
                r: b[0],
                g: b[1],
                b: b[2],
            })
        }
    })
}

fn named(index: u16) -> Option<NamedColor> {
    use NamedColor::*;
    const ANSI: [NamedColor; 16] = [
        Black,
        Red,
        Green,
        Yellow,
        Blue,
        Magenta,
        Cyan,
        White,
        BrightBlack,
        BrightRed,
        BrightGreen,
        BrightYellow,
        BrightBlue,
        BrightMagenta,
        BrightCyan,
        BrightWhite,
    ];
    const SPECIAL: [NamedColor; 13] = [
        Foreground,
        Background,
        Cursor,
        DimBlack,
        DimRed,
        DimGreen,
        DimYellow,
        DimBlue,
        DimMagenta,
        DimCyan,
        DimWhite,
        BrightForeground,
        DimForeground,
    ];
    match index {
        0..=15 => Some(ANSI[index as usize]),
        _ => SPECIAL.get(usize::from(index.checked_sub(256)?)).copied(),
    }
}

fn take_varint(bytes: &mut &[u8]) -> Option<usize> {
    let mut n = 0;
    for shift in (0..usize::BITS).step_by(7) {
        let b = take(bytes, 1)?[0];
        n |= ((b & 0x7f) as usize) << shift;
        if b < 0x80 {
            return Some(n);
        }
    }
    None
}

fn take<'a>(bytes: &mut &'a [u8], n: usize) -> Option<&'a [u8]> {
    let (head, rest) = bytes.split_at_checked(n)?;
    *bytes = rest;
    Some(head)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vt::term::cell::Hyperlink;

    fn encoded(row: &Row<Cell>) -> Vec<u8> {
        let scan = scan_len(row);
        let mut out = vec![0; max_len(scan)];
        let end = encode(row, scan, &mut out, 0, &mut Vec::new());
        out.truncate(end);
        out
    }

    fn cell(c: char, fg: Color, bg: Color, flags: Flags) -> Cell {
        Cell {
            c,
            fg,
            bg,
            flags,
            extra: None,
        }
    }

    fn sample_rows() -> Vec<Row<Cell>> {
        let red = Color::Named(NamedColor::Red);
        let rgb = Color::Spec(Rgb { r: 1, g: 200, b: 3 });
        let idx = Color::Indexed(208);
        let mut rows = Vec::new();

        rows.push(Row::new(120));

        let mut log = Row::new(120);
        for (i, c) in "PASS build src/x.ts".chars().enumerate() {
            log[Column(i)] = cell(
                c,
                if i < 4 { red } else { DEFAULT_FG },
                DEFAULT_BG,
                Flags::empty(),
            );
        }
        rows.push(log);

        // A wide character and the cell after it, a wrap at the row end, a
        // coloured blank, a combining mark, a link, an underline colour, a tab.
        let mut mixed = Row::new(10);
        mixed[Column(0)] = cell('한', idx, rgb, Flags::WIDE_CHAR | Flags::BOLD);
        mixed[Column(1)] = cell(' ', idx, rgb, Flags::WIDE_CHAR_SPACER | Flags::BOLD);
        mixed[Column(2)] = cell('😀', DEFAULT_FG, DEFAULT_BG, Flags::WIDE_CHAR);
        mixed[Column(3)] = cell(' ', DEFAULT_FG, DEFAULT_BG, Flags::WIDE_CHAR_SPACER);
        mixed[Column(4)] = cell('\t', DEFAULT_FG, red, Flags::empty());
        let mut e = cell('e', DEFAULT_FG, DEFAULT_BG, Flags::UNDERCURL);
        e.push_zerowidth('\u{301}');
        e.set_underline_color(Some(rgb));
        mixed[Column(5)] = e;
        let mut link = cell('L', DEFAULT_FG, DEFAULT_BG, Flags::empty());
        link.set_hyperlink(Some(Hyperlink::new(
            Some("id"),
            "https://example.com".into(),
        )));
        mixed[Column(6)] = link;
        mixed[Column(8)] = cell(
            ' ',
            DEFAULT_FG,
            Color::Named(NamedColor::DimForeground),
            Flags::empty(),
        );
        mixed[Column(9)] = cell('z', DEFAULT_FG, DEFAULT_BG, Flags::WRAPLINE | Flags::BLINK);
        rows.push(mixed);
        rows
    }

    #[test]
    fn rows_round_trip() {
        let mut history = History::new(100);
        let rows = sample_rows();
        for row in &rows {
            history.push(&mut row.clone());
        }
        assert_eq!(history.len(), rows.len());
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(&history.row(i)[..], &row[..], "row {i}");
        }
    }

    /// Rows of random cells from a small set (so that runs form), with wide
    /// pairs, broken pairs, typed and erased spaces, blanks to the end and
    /// extras: each decodes to the cells it was encoded from.
    #[test]
    fn random_rows_round_trip() {
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let colors = [
            DEFAULT_FG,
            DEFAULT_BG,
            Color::Named(NamedColor::Red),
            Color::Indexed(208),
            Color::Spec(Rgb { r: 1, g: 2, b: 3 }),
        ];
        let chars = [
            'a',
            'Z',
            '~',
            'é',
            '한',
            '😀',
            '\u{1}',
            '\u{7f}',
            '\u{1fff}',
            '\u{2000}',
            '\u{10ffff}',
        ];
        let flags = [Flags::empty(), Flags::BOLD, Flags::INVERSE | Flags::ITALIC];
        let link = Arc::new(CellExtra::default());
        let mut history = History::new(usize::MAX);
        let mut rows = Vec::new();
        for _ in 0..3000 {
            let cols = 1 + next(40) as usize;
            let mut row = Row::<Cell>::new(cols);
            // Mostly a few attributes per row, sometimes one per cell.
            let mixed = next(4) == 0;
            let (fg, bg, flag) = (next(5) as usize, next(5) as usize, next(3) as usize);
            let used = next(cols as u64 + 1) as usize;
            let mut col = 0;
            while col < used {
                let pick = |next: &mut dyn FnMut(u64) -> u64, k: usize, n: usize| {
                    if mixed { next(n as u64) as usize } else { k }
                };
                let mut cell = cell(
                    ' ',
                    colors[pick(&mut next, fg, 5)],
                    colors[pick(&mut next, bg, 5)],
                    flags[pick(&mut next, flag, 3)],
                );
                match next(8) {
                    0 => {}
                    1 => cell.flags |= Flags::TYPED_SPACE,
                    2 if col + 1 < cols => {
                        // A wide character, its right half sometimes not a match.
                        cell.c = ['한', '😀', 'W', '\u{1fff}', '\u{2000}'][next(5) as usize];
                        let mut right = cell.clone();
                        right.c = ' ';
                        right.flags |= Flags::WIDE_CHAR_SPACER;
                        cell.flags |= Flags::WIDE_CHAR;
                        if next(6) == 0 {
                            right.bg = colors[next(5) as usize];
                        }
                        row[Column(col)] = cell;
                        row[Column(col + 1)] = right;
                        col += 2;
                        continue;
                    }
                    3 => {
                        cell.c = chars[next(chars.len() as u64) as usize];
                        cell.flags |= [Flags::WIDE_CHAR, Flags::WIDE_CHAR_SPACER, Flags::WRAPLINE]
                            [next(3) as usize];
                    }
                    _ => cell.c = chars[next(chars.len() as u64) as usize],
                }
                if next(20) == 0 {
                    cell.extra = Some(link.clone());
                }
                row[Column(col)] = cell;
                col += 1;
            }
            history.push(&mut row.clone());
            rows.push(row);
        }
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(&history.row(i)[..], &row[..], "row {i}");
        }
    }

    #[test]
    fn log_row_is_small() {
        let rows = sample_rows();
        let out = encoded(&rows[1]);
        // 120 columns take 1440 bytes in xterm.js.
        assert!(out.len() < 40, "{} bytes", out.len());
    }

    #[test]
    fn wide_pairs_cost_their_text() {
        // "한글 " × 24: the run must not break at the narrow spaces between
        // wide characters.
        let mut row = Row::new(120);
        for rep in 0..24 {
            for k in 0..2 {
                let col = rep * 5 + k * 2;
                row[Column(col)] = cell('한', DEFAULT_FG, DEFAULT_BG, Flags::WIDE_CHAR);
                row[Column(col + 1)] = cell(' ', DEFAULT_FG, DEFAULT_BG, Flags::WIDE_CHAR_SPACER);
            }
        }
        let out = encoded(&row);
        // 48 characters × 3 bytes + 23 spaces + a few header bytes
        assert!(out.len() <= 175, "{} bytes", out.len());
        assert_eq!(&decode(&out, None)[..], &row[..]);
    }

    #[test]
    fn oldest_rows_fall_off_across_blocks() {
        let mut history = History::new(3000);
        let mut row = Row::<Cell>::new(80);
        for n in 0..10_000u32 {
            for (i, c) in format!("line {n} {}", "x".repeat((n % 70) as usize))
                .chars()
                .enumerate()
            {
                row[Column(i)] = cell(c, DEFAULT_FG, DEFAULT_BG, Flags::empty());
            }
            history.push(&mut row.clone());
            row.reset(&Cell::default());
        }
        assert_eq!(history.len(), 3000);
        assert!(history.blocks.len() > 1);
        let text = |r: &Row<Cell>| {
            r[..]
                .iter()
                .map(|c| c.c)
                .collect::<String>()
                .trim_end()
                .to_owned()
        };
        assert_eq!(text(&history.row(0)), "line 7000");
        assert_eq!(
            text(&history.row(2999)),
            format!("line 9999 {}", "x".repeat(59))
        );
        HistorySink::clear(&mut history);
        assert_eq!(history.len(), 0);
        history.push(&mut Row::new(80));
        assert_eq!(history.len(), 1);
    }

    #[test]
    fn truncating_and_renumbering_keep_rows() {
        let mut history = History::new(usize::MAX);
        let mut row = Row::<Cell>::new(300);
        for n in 0..2000u32 {
            row[Column(0)] = cell(
                char::from(b'a' + (n % 26) as u8),
                DEFAULT_FG,
                DEFAULT_BG,
                Flags::empty(),
            );
            if n % 7 == 0 {
                row[Column(1)].push_zerowidth('\u{301}');
            }
            history.push(&mut row.clone());
            row.reset(&Cell::default());
        }
        let kept: Vec<_> = (0..1500).map(|i| history.row(i)).collect();
        history.truncate_newest(1500);
        history.renumber(5000);
        assert_eq!(history.len(), 1500);
        assert_eq!(history.id(0), 5000);
        for (i, row) in kept.iter().enumerate() {
            assert_eq!(history.row(i), *row, "row {i}");
        }
        history.push(&mut Row::new(300));
        assert_eq!(history.row(1500), Row::new(300));
    }

    #[test]
    fn garbage_decodes_without_panicking() {
        let mut rows = Vec::new();
        for row in sample_rows() {
            let out = encoded(&row);
            rows.push(out);
        }
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let extras: Extras = vec![(9999, Arc::new(CellExtra::default()))].into_boxed_slice();
        for _ in 0..20_000 {
            let mut bytes = rows[next() as usize % rows.len()].clone();
            match next() % 3 {
                // Flip bytes.
                0 => {
                    for _ in 0..next() % 4 + 1 {
                        let at = next() as usize % bytes.len();
                        bytes[at] = next() as u8;
                    }
                }
                // Cut the row short.
                1 => bytes.truncate(next() as usize % bytes.len()),
                // Random bytes.
                _ => bytes = (0..next() % 40).map(|_| next() as u8).collect(),
            }
            let row = decode(&bytes, None);
            assert!(row.len() >= 1);
            decode(&bytes, Some(&extras));
        }
        assert_eq!(&decode(&[0xff; 12], None)[..], &Row::<Cell>::new(1)[..]);
    }

    /// No block grows past `BLOCK_BYTES` but one that holds a single larger row, and every row reads back, with
    /// rows that fill a block in a few (a colour per cell) and one larger than a block.
    #[test]
    fn blocks_keep_their_size() {
        let mut history = History::new(1000);
        let mut rows = Vec::new();
        for n in 0..300u32 {
            let cols = if n == 151 {
                30000
            } else {
                2000 + n as usize % 7
            };
            let mut row = Row::<Cell>::new(cols);
            for col in 0..cols.min(if n % 3 == 0 { 40 } else { cols }) {
                let rgb = Rgb {
                    r: col as u8,
                    g: n as u8,
                    b: 7,
                };
                row[Column(col)] = cell('x', Color::Spec(rgb), DEFAULT_BG, Flags::empty());
            }
            history.push(&mut row.clone());
            rows.push(row);
        }
        for (i, row) in rows.iter().enumerate() {
            assert_eq!(history.row(i), *row, "row {i}");
        }
        assert!(history.blocks.len() > 3);
        assert!(history.blocks.iter().any(|b| b.bytes.len() > BLOCK_BYTES));
        for block in &history.blocks {
            assert!(
                block.bytes.len() == BLOCK_BYTES || block.starts.len() == 1,
                "{}",
                block.bytes.len()
            );
            assert!(block.end <= block.bytes.len());
        }
    }

    /// The default-attribute first run of `encode` (`plain`) writes the bytes `runs` writes for the whole row, on
    /// random rows of narrow, wide and broken wide characters, typed spaces, bold and coloured cells.
    #[test]
    fn plain_wide_matches_runs() {
        let mut rng = 0x2545_f491_4f6c_dd1du64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let chars = ['a', ' ', 'é', '가', '字', '😀', '~'];
        for _ in 0..20000 {
            let cols = 1 + (next() % 24) as usize;
            let mut row = Row::<Cell>::new(cols);
            let mut col = 0;
            while col < cols {
                let r = next();
                let c = chars[(r % 7) as usize];
                let wide = matches!(c, '가' | '字' | '😀');
                let mut flags = if c == ' ' && r & 8 != 0 {
                    Flags::TYPED_SPACE
                } else {
                    Flags::empty()
                };
                if r & 0x300 == 0 {
                    flags |= Flags::BOLD;
                }
                if wide && col + 1 < cols {
                    row[Column(col)] = cell(c, DEFAULT_FG, DEFAULT_BG, flags | Flags::WIDE_CHAR);
                    let spacer = if r & 0x7000 == 0 {
                        Flags::empty()
                    } else {
                        Flags::WIDE_CHAR_SPACER
                    };
                    row[Column(col + 1)] = cell(' ', DEFAULT_FG, DEFAULT_BG, flags | spacer);
                    col += 2;
                } else if !wide {
                    let fg = if r & 0xf0000 == 0 {
                        Color::Named(NamedColor::Red)
                    } else {
                        DEFAULT_FG
                    };
                    row[Column(col)] = cell(c, fg, DEFAULT_BG, flags);
                    col += 1;
                } else {
                    col += 1;
                }
            }
            let scan = scan_len(&row);
            let a = encoded(&row);
            let mut b = vec![0; max_len(scan)];
            let mut out = Out { buf: &mut b, at: 0 };
            out.varint(row.len());
            let end = runs(&row[..][..scan], 0, out, &mut Vec::new());
            assert_eq!(a, &b[..end], "{row:?}");
            assert_eq!(decode(&a, None), row);
        }
    }

    /// A plain row (`Row::plain`) taken by `encode_take` gives the bytes `encode` writes for it, and is left blank.
    #[test]
    fn plain_take_matches_encode() {
        let mut rng = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            rng ^= rng << 13;
            rng ^= rng >> 7;
            rng ^= rng << 17;
            rng
        };
        let chars = ['a', ' ', 'é', '가', '字', '😀', '~', '\u{2500}'];
        for _ in 0..20000 {
            let cols = 1 + (next() % 24) as usize;
            let mut row = Row::<Cell>::new(cols);
            let mut col = (next() % 3) as usize;
            while col < cols {
                let r = next();
                let c = chars[(r % 8) as usize];
                let wide = matches!(c, '가' | '字' | '😀');
                if wide && col + 1 < cols {
                    row[Column(col)] = cell(c, DEFAULT_FG, DEFAULT_BG, Flags::WIDE_CHAR);
                    row[Column(col + 1)] =
                        cell(' ', DEFAULT_FG, DEFAULT_BG, Flags::WIDE_CHAR_SPACER);
                    col += 2;
                } else if !wide {
                    let typed = c == ' ' && r & 8 != 0;
                    let flags = if typed {
                        Flags::TYPED_SPACE
                    } else {
                        Flags::empty()
                    };
                    row[Column(col)] = cell(c, DEFAULT_FG, DEFAULT_BG, flags);
                    col += 1;
                }
                col += (r >> 20) as usize % 4 / 3;
            }
            row.plain = true;
            assert!(is_plain(&row[..]));
            let want = encoded(&row);
            let scan = scan_len(&row);
            let mut got = vec![0; max_len(scan)];
            let mut taken = row.clone();
            taken.plain = true;
            let end = encode_take(&mut taken, scan, &mut got, 0, &mut Vec::new());
            assert_eq!(&got[..end], want, "{row:?}");
            assert_eq!(taken, Row::new(cols));
            assert_eq!(decode(&want, None), row);
        }
    }
}

/// The reflow against [`TermHistory::reflow_take_all`], the resize with the
/// whole history decoded into the grid.
#[cfg(test)]
mod oracle {
    use super::*;
    use crate::vt::event::VoidListener;
    use crate::vt::grid::Cursor;
    use crate::vt::term::Config;
    use crate::vt::term::test::TermSize;
    use crate::vt::vte::ansi::Processor;

    /// Reproducible pseudo-random numbers (xorshift64*).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }

        fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
            items[self.below(items.len() as u64) as usize]
        }
    }

    /// Output of mixed short and long wrapped lines, wide characters at row
    /// edges, combining marks, links and underline colours. Links carry ids:
    /// generated ones come from a process-wide counter and differ per term.
    fn chunk(rng: &mut Rng, tokens: u64) -> String {
        let mut s = String::new();
        for _ in 0..tokens {
            let token = match rng.below(40) {
                0..=9 => rng
                    .pick(&[
                        "hello ", "build ", "한글 ", "😀", "e\u{301}", "ab", "\t", "x", " ",
                    ])
                    .to_owned(),
                10..=15 => "\r\n".to_owned(),
                16 => "\n".to_owned(),
                17..=18 => format!(
                    "\x1b[{}m",
                    rng.pick(&[
                        "0",
                        "1",
                        "31",
                        "42",
                        "38;5;208",
                        "48;2;1;2;3",
                        "4:3",
                        "58:2::9:8:7",
                        "7",
                        "5"
                    ])
                ),
                19 => rng
                    .pick(&[
                        "\x1b]8;id=a;https://a.example\x07",
                        "\x1b]8;id=b;https://b.example\x07",
                        "\x1b]8;;\x07",
                    ])
                    .to_owned(),
                20..=22 => "가".repeat(rng.below(60) as usize),
                23..=25 => "y".repeat(rng.below(200) as usize),
                26 => format!("\x1b[{};{}H", rng.below(10) + 1, rng.below(40) + 1),
                27 => rng
                    .pick(&["\x1b[K", "\x1b[1K", "\x1b[2K", "\x1b[J"])
                    .to_owned(),
                28 => format!("\x1b[{}{}", rng.below(4) + 1, rng.pick(&["@", "P", "X"])),
                29 => rng.pick(&["\x1b[?1049h", "\x1b[?1049l"]).to_owned(),
                30 => "\x1bM".to_owned(),
                31 => format!("\x1b[{}S", rng.below(4) + 1),
                32 => {
                    if rng.below(30) == 0 {
                        rng.pick(&["\x1b[3J", "\x1bc", "\x1b[2;5r", "\x1b[r"])
                            .to_owned()
                    } else {
                        "z".to_owned()
                    }
                }
                33 => rng.pick(&["\x1b[?7l", "\x1b[?7h"]).to_owned(),
                _ => format!("{}\r\n", "w".repeat(rng.below(30) as usize)),
            };
            s.push_str(&token);
        }
        s
    }

    struct Side {
        term: Term<VoidListener>,
        history: TermHistory,
        parser: Processor,
    }

    /// The term's settings, as `Engine` sets them up or Alacritty's.
    fn config(engine: bool, scrolling_history: usize) -> Config {
        if engine {
            Config {
                scrolling_history,
                reflow_cursor_line: false,
                scroll_damage: true,
                ris_keeps_cursor_visibility: true,
                scroll_on_erase_in_display: false,
                xterm_line_wrap: true,
                xterm_wide_leader: true,
                store_tabs: false,
                sgr21_double_underline: true,
                ..Config::default()
            }
        } else {
            Config {
                scrolling_history,
                ..Config::default()
            }
        }
    }

    impl Side {
        fn new(engine: bool, cols: usize, rows: usize, scrollback: usize) -> Self {
            let config = config(engine, TermHistory::RECENT);
            let mut term = Term::new(config, &TermSize::new(cols, rows), VoidListener);
            let history = TermHistory::attach(&mut term, scrollback);
            Self {
                term,
                history,
                parser: Processor::new(),
            }
        }

        fn feed(&mut self, bytes: &[u8]) {
            self.parser.advance(&mut self.term, bytes);
        }

        fn primary(&self) -> &crate::vt::grid::Grid<Cell> {
            if self.term.mode().contains(TermMode::ALT_SCREEN) {
                self.term.inactive_grid()
            } else {
                self.term.grid()
            }
        }
    }

    #[derive(Debug, PartialEq)]
    struct GridState {
        rows: Vec<Row<Cell>>,
        cursor: Cursor<Cell>,
        saved_cursor: Cursor<Cell>,
        display_offset: usize,
    }

    #[derive(Debug, PartialEq)]
    struct State {
        size: (usize, usize),
        alt: bool,
        stored: Vec<Row<Cell>>,
        cleared: bool,
        grids: Vec<GridState>,
        vi: crate::vt::index::Point,
    }

    fn state(side: &Side) -> State {
        let stored = side.history.lock();
        let grid = |g: &crate::vt::grid::Grid<Cell>| GridState {
            rows: (-(g.history_size() as i32)..g.screen_lines() as i32)
                .map(|l| g[Line(l)].clone())
                .collect(),
            cursor: g.cursor.clone(),
            saved_cursor: g.saved_cursor.clone(),
            display_offset: g.display_offset(),
        };
        State {
            size: (side.term.columns(), side.term.screen_lines()),
            alt: side.term.mode().contains(TermMode::ALT_SCREEN),
            stored: (0..stored.len()).map(|i| stored.row(i)).collect(),
            cleared: stored.cleared,
            grids: vec![grid(side.term.grid()), grid(side.term.inactive_grid())],
            vi: side.term.vi_mode_cursor.point,
        }
    }

    fn assert_same(a: &Side, b: &Side, what: &str) {
        let (x, y) = (state(a), state(b));
        if x == y {
            return;
        }
        assert_eq!(x.size, y.size, "{what}: size");
        assert_eq!(x.alt, y.alt, "{what}: alternate screen");
        assert_eq!(x.stored.len(), y.stored.len(), "{what}: stored rows");
        for (i, (r, s)) in x.stored.iter().zip(&y.stored).enumerate() {
            assert_eq!(r, s, "{what}: stored row {i}");
        }
        assert_eq!(x.cleared, y.cleared, "{what}: cleared");
        for (k, (g, h)) in x.grids.iter().zip(&y.grids).enumerate() {
            assert_eq!(g.rows.len(), h.rows.len(), "{what}: grid {k} rows");
            for (i, (r, s)) in g.rows.iter().zip(&h.rows).enumerate() {
                assert_eq!(r, s, "{what}: grid {k} row {i}");
            }
            assert_eq!(g.cursor, h.cursor, "{what}: grid {k} cursor");
            assert_eq!(
                g.saved_cursor, h.saved_cursor,
                "{what}: grid {k} saved cursor"
            );
            assert_eq!(
                g.display_offset, h.display_offset,
                "{what}: grid {k} display offset"
            );
        }
        assert_eq!(x.vi, y.vi, "{what}: vi cursor");
    }

    /// Whether the cursor paragraph runs to the top of the primary grid, so
    /// that it reaches into the compressed rows.
    fn paragraph_reaches_history(side: &Side) -> bool {
        let g = side.primary();
        let last = g.last_column();
        let mut line = g.cursor.point.line;
        while line > g.topmost_line() && g[Line(line.0 - 1)][last].flags.contains(Flags::WRAPLINE) {
            line -= 1;
        }
        line == g.topmost_line() && !side.history.is_empty()
    }

    #[derive(Default, Debug)]
    struct Seen {
        resizes: usize,
        split: usize,
        paragraph: usize,
        deep: usize,
        full: usize,
        alt: usize,
        dropped: usize,
    }

    fn run(seed: u64, seen: &mut Seen) {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let engine = seed % 3 != 0;
        let (mut cols, mut rows) = (rng.below(38) as usize + 2, rng.below(8) as usize + 1);
        let scrollback = [17, 24, 60, 150, 400, 1500][rng.below(6) as usize];
        let mut a = Side::new(engine, cols, rows, scrollback);
        let mut b = Side::new(engine, cols, rows, scrollback);
        let mut offset = 0;
        for step in 0..10 {
            let tokens = rng.below(400);
            let mut bytes = chunk(&mut rng, tokens);
            if rng.below(3) == 0 {
                // End inside a long line: the cursor paragraph reaches far up.
                bytes.push_str(&"q".repeat(rng.below(1200) as usize));
            }
            a.feed(bytes.as_bytes());
            b.feed(bytes.as_bytes());
            let what = format!("seed {seed} step {step}");
            assert_same(&a, &b, &format!("{what} before the resize"));

            let total = a.history.len() + a.primary().history_size();
            offset = match rng.below(5) {
                0 => 0,
                1 => offset,
                2 => rng.below(rows as u64 + 20) as usize,
                3 => rng.below(total as u64 + 1) as usize,
                _ => rng.below(scrollback as u64 + 10) as usize,
            };
            let (old_cols, old_rows) = (cols, rows);
            match rng.below(4) {
                0 => cols = rng.below(38) as usize + 2,
                1 => rows = rng.below(10) as usize + 1,
                _ => {
                    cols = rng.below(38) as usize + 2;
                    rows = rng.below(10) as usize + 1;
                }
            }
            seen.resizes += 1;
            let n = a.history.len();
            if cols != old_cols && n > 2 * (old_rows + rows) + 20 {
                seen.split += 1;
            }
            if n > 0 && n <= old_rows + rows {
                seen.full += 1;
            }
            if paragraph_reaches_history(&a) {
                seen.paragraph += 1;
            }
            if offset > a.primary().history_size() + old_rows {
                seen.deep += 1;
            }
            if a.term.mode().contains(TermMode::ALT_SCREEN) {
                seen.alt += 1;
            }
            if rows < old_rows && total == scrollback {
                seen.dropped += 1;
            }
            if (cols, rows) == (old_cols, old_rows) {
                // Nothing to do: the rows keep their ids (the reference stores them again).
                continue;
            }
            let next = {
                let stored = a.history.lock();
                stored.id(stored.len())
            };
            let size = TermSize::new(cols, rows);
            let got = a.history.reflow(&mut a.term, size, offset);
            let want = b
                .history
                .reflow_take_all(&mut b.term, TermSize::new(cols, rows), offset);
            let what = format!("{what}: {old_cols}x{old_rows} -> {cols}x{rows}, offset {offset}");
            assert_same(&a, &b, &what);
            assert_eq!(got, want, "{what}: returned offset");
            // Every row stored again has a new id, so cached rows cannot come back.
            assert!(a.history.lock().id(0) >= next, "{what}: row ids");
            offset = got;
        }
    }

    /// The compressed history holds the rows a grid with the whole history in
    /// it holds, after any output.
    #[test]
    fn rows_match_the_whole_history_in_the_grid() {
        for seed in 1..=300u64 {
            let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
            let (cols, rows) = (rng.below(38) as usize + 2, rng.below(8) as usize + 1);
            let scrollback = [17, 24, 60, 150, 400][rng.below(5) as usize];
            let engine = seed % 2 == 0;
            let mut side = Side::new(engine, cols, rows, scrollback);
            let size = TermSize::new(cols, rows);
            let mut whole = Term::new(config(engine, scrollback), &size, VoidListener);
            let mut parser = Processor::new();
            for step in 0..8 {
                let tokens = rng.below(400);
                let bytes = chunk(&mut rng, tokens);
                side.feed(bytes.as_bytes());
                parser.advance(&mut whole, bytes.as_bytes());
                let grid = |term: &Term<VoidListener>| {
                    let g = if term.mode().contains(TermMode::ALT_SCREEN) {
                        term.inactive_grid()
                    } else {
                        term.grid()
                    };
                    (-(g.history_size() as i32)..g.screen_lines() as i32)
                        .map(|l| g[Line(l)].clone())
                        .collect::<Vec<_>>()
                };
                let stored = side.history.lock();
                let got: Vec<_> = (0..stored.len())
                    .map(|i| stored.row(i))
                    .chain(grid(&side.term))
                    .collect();
                drop(stored);
                let want = grid(&whole);
                assert_eq!(got.len(), want.len(), "seed {seed} step {step}: rows");
                for (i, (g, w)) in got.iter().zip(&want).enumerate() {
                    assert_eq!(g, w, "seed {seed} step {step}: row {i}");
                }
            }
        }
    }

    #[test]
    fn reflow_matches_the_whole_history_in_the_grid() {
        let mut seen = Seen::default();
        for seed in 1..=600 {
            run(seed, &mut seen);
        }
        // The cases the reflow has to get right all came up.
        assert!(seen.split > 1000, "{seen:?}");
        assert!(seen.paragraph > 50, "{seen:?}");
        assert!(seen.deep > 300, "{seen:?}");
        assert!(seen.full > 50, "{seen:?}");
        assert!(seen.alt > 100, "{seen:?}");
        assert!(seen.dropped > 50, "{seen:?}");
        eprintln!("{seen:?}");
    }
}
