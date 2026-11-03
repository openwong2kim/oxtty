//! Markers: lines that keep their identity while the buffer moves under them, as xterm.js `registerMarker` gives.
//! A marker follows its line as output scrolls it into the history, and as scroll regions, inserted and deleted
//! lines move it on screen. It is dropped when its line leaves the top of the scrollback and when its line is
//! cleared whole (an erase in display, a line an insert or delete pushed out, RIS). A resize keeps it on its logical
//! line (the rows soft wraps join), on the same row within that line; a row that wider columns merged into the rows
//! above it takes its markers along.
//!
//! Each screen has its own markers, as each xterm.js buffer does: the alternate screen's are dropped when the
//! primary one comes back. Marker lines are buffer lines as xterm.js counts them: 0 is the oldest history line and
//! the top screen row is at the history length (on the alternate screen, which has no history, the screen row).

use crate::Engine;
use crate::vt::grid::{Dimensions, Grid, Row, RowMove};
use crate::vt::index::Line;
use crate::vt::term::cell::{Cell, Flags};

struct Marker {
    id: u32,
    /// The line counted from the first line that ever entered the history (`history_entered` plus the screen
    /// line), which output scrolling does not change.
    line: i64,
}

/// The markers of one engine and what they were last brought up to date with.
#[derive(Default)]
pub(crate) struct Markers {
    next_id: u32,
    /// The primary screen's.
    live: Vec<Marker>,
    /// The alternate screen's.
    alt: Vec<Marker>,
    /// Dropped by the engine and not taken yet ([`Engine::take_disposed_markers`]).
    disposed: Vec<u32>,
    entered: u64,
    alt_entered: u64,
    resets: u64,
}

/// Where row `line` goes after `row_move`, or `None` when the move drops it.
fn moved(line: i64, row_move: &RowMove) -> Option<i64> {
    let (start, end, p, up, history) = match row_move {
        RowMove::Up {
            region,
            positions,
            history,
        } => (region.start.0, region.end.0, *positions, true, *history),
        RowMove::Down { region, positions } => {
            (region.start.0, region.end.0, *positions, false, false)
        }
        RowMove::Reset { start, end } => {
            return (line < start.0 as i64 || line >= end.0 as i64).then_some(line);
        }
    };
    let (start, end, p) = (start as i64, end as i64, p as i64);
    if history {
        // The region and the history above it move up together; the rows below the region stay
        return Some(if line < end { line - p } else { line });
    }
    if line < start || line >= end {
        return Some(line);
    }
    // The rows a move pushes out of the region are dropped
    let (gone, shift) = if up {
        (line < start + p, -p)
    } else {
        (line >= end - p, p)
    };
    (!gone).then_some(line + shift)
}

/// Insertion sort: a few markers, and the slice sorts of std would add kilobytes to the wasm.
fn sort(items: &mut [(i64, usize)]) {
    for i in 1..items.len() {
        let mut j = i;
        while j > 0 && items[j - 1] > items[j] {
            items.swap(j - 1, j);
            j -= 1;
        }
    }
}

impl Engine {
    fn primary_mut(&mut self) -> &mut Grid<Cell> {
        self.grid_of_mut(false)
    }

    /// The alternate screen's grid (`alt`) or the primary one's, whichever is up.
    fn grid_of_mut(&mut self, alt: bool) -> &mut Grid<Cell> {
        if alt == self.alt_screen() {
            self.term.grid_mut()
        } else {
            self.term.inactive_grid_mut()
        }
    }

    fn alt_entered(&self) -> i64 {
        let grid = if self.alt_screen() {
            self.term.grid()
        } else {
            self.term.inactive_grid()
        };
        grid.history_entered() as i64
    }

    fn primary_entered(&self) -> i64 {
        self.primary().history_entered() as i64
    }

    /// The top line of the primary screen's history, as `row_text` numbers lines.
    fn primary_top(&self) -> i32 {
        -(self.primary_history_len() as i32)
    }

    /// Whether row `line` of the primary screen continues on the next one, also while it is not shown.
    fn wraps(&self, line: i32) -> bool {
        let last = |row: &Row<Cell>| {
            row.last()
                .is_some_and(|c| c.flags.contains(Flags::WRAPLINE))
        };
        let grid = self.primary();
        let recent = grid.history_size() as i32;
        if line >= -recent && line < grid.screen_lines() as i32 {
            return last(&grid[Line(line)]);
        }
        self.history.as_ref().is_some_and(|history| {
            let history = history.stored(&self.term);
            let i = history.len() as i32 + recent + line;
            i >= 0 && i < history.len() as i32 && last(&history.row(i as usize))
        })
    }

    /// The first row of the logical line `line` is on.
    fn logical_start(&self, mut line: i32) -> i32 {
        while line > self.primary_top() && self.wraps(line - 1) {
            line -= 1;
        }
        line
    }

    /// The last row of the logical line starting at `line`.
    fn logical_end(&self, mut line: i32) -> i32 {
        while line < self.rows() as i32 - 1 && self.wraps(line) {
            line += 1;
        }
        line
    }

    /// Puts a marker on buffer line `line` of the screen that is up (0 the oldest history line, or the top row of
    /// the alternate screen); returns its id, never 0, or `None` when the line is outside the buffer.
    pub fn add_marker(&mut self, line: i64) -> Option<u32> {
        let alt = self.alt_screen();
        let history = self.history_len() as i64;
        if line < 0 || line >= history + self.rows() as i64 {
            return None;
        }
        if self.markers.live.is_empty() && self.markers.alt.is_empty() {
            self.markers.resets = self.term.resets();
        }
        let entered = if alt {
            self.alt_entered()
        } else {
            self.primary_entered()
        };
        let markers = &mut self.markers;
        let (list, list_entered) = if alt {
            (&mut markers.alt, &mut markers.alt_entered)
        } else {
            (&mut markers.live, &mut markers.entered)
        };
        if list.is_empty() {
            *list_entered = entered as u64;
            self.grid_of_mut(alt).track_row_moves(true);
        }
        self.markers.next_id += 1;
        let id = self.markers.next_id;
        let marker = Marker {
            id,
            line: line - history + entered,
        };
        if alt {
            self.markers.alt.push(marker);
        } else {
            self.markers.live.push(marker);
        }
        Some(id)
    }

    /// The buffer line of marker `id`, `None` once it was dropped or removed.
    pub fn marker_line(&self, id: u32) -> Option<i64> {
        if let Some(marker) = self.markers.live.iter().find(|m| m.id == id) {
            return Some(marker.line - self.primary_entered() + self.primary_history_len() as i64);
        }
        let marker = self.markers.alt.iter().find(|m| m.id == id)?;
        Some(marker.line - self.alt_entered())
    }

    /// Drops marker `id` for the host (it is not reported by [`Engine::take_disposed_markers`]).
    pub fn remove_marker(&mut self, id: u32) {
        self.markers.live.retain(|m| m.id != id);
        self.markers.alt.retain(|m| m.id != id);
        self.untrack_if_empty();
    }

    /// Markers the engine dropped since the last call, in the order it dropped them.
    pub fn take_disposed_markers(&mut self) -> Vec<u32> {
        std::mem::take(&mut self.markers.disposed)
    }

    fn untrack_if_empty(&mut self) {
        if self.markers.live.is_empty() {
            self.primary_mut().track_row_moves(false);
        }
        if self.markers.alt.is_empty() {
            self.grid_of_mut(true).track_row_moves(false);
        }
    }

    /// Keeps the markers of the alternate screen (`alt`) or the primary one whose new line `line_of` gives
    /// (counted on screen), reporting the others as dropped.
    fn relocate(&mut self, alt: bool, mut line_of: impl FnMut(usize, &Marker) -> Option<i64>) {
        let entered = if alt {
            self.alt_entered()
        } else {
            self.primary_entered()
        };
        let markers = &mut self.markers;
        let (list, list_entered) = if alt {
            (&mut markers.alt, &mut markers.alt_entered)
        } else {
            (&mut markers.live, &mut markers.entered)
        };
        let disposed = &mut markers.disposed;
        let mut i = 0;
        list.retain_mut(|m| {
            let line = line_of(i, m);
            i += 1;
            match line {
                Some(line) => m.line = line + entered,
                None => disposed.push(m.id),
            }
            line.is_some()
        });
        *list_entered = entered as u64;
        self.untrack_if_empty();
    }

    /// Brings the markers up to date with what the last input did to the rows.
    pub(crate) fn follow_markers(&mut self) {
        if self.markers.live.is_empty() && self.markers.alt.is_empty() {
            return;
        }
        let resets = self.term.resets();
        let reset = resets != self.markers.resets;
        self.markers.resets = resets;
        if !self.markers.live.is_empty() {
            let moves = self.primary_mut().take_row_moves();
            let (before, top) = (self.markers.entered as i64, self.primary_top() as i64);
            self.relocate(false, |_, m| {
                let line = moves.iter().try_fold(m.line - before, moved)?;
                // Past the top of the scrollback
                (!reset && line >= top).then_some(line)
            });
        }
        if !self.markers.alt.is_empty() {
            // Leaving the alternate screen drops its markers, as xterm.js clears that buffer; entering it again
            // cleared its rows.
            let up = self.alt_screen();
            let moves = self.grid_of_mut(true).take_row_moves();
            let before = self.markers.alt_entered as i64;
            self.relocate(true, |_, m| {
                let line = moves.iter().try_fold(m.line - before, moved)?;
                (up && !reset && line >= 0).then_some(line)
            });
        }
    }

    /// Where each marker is before a resize: logical lines from the cursor's logical line (negative below it) and
    /// the row within its own logical line. Without `reflow` (the columns stay) the rows only move, so it is rows
    /// from the cursor's row.
    #[inline(never)]
    pub(crate) fn anchor_markers(&mut self, reflow: bool) -> Vec<(i64, i32)> {
        self.follow_markers();
        let entered = self.primary_entered();
        let cursor = self.primary().cursor.point.line.0;
        let lines: Vec<i32> = self
            .markers
            .live
            .iter()
            .map(|m| (m.line - entered) as i32)
            .collect();
        if !reflow {
            return lines
                .iter()
                .map(|&line| ((cursor - line) as i64, 0))
                .collect();
        }
        let start = self.logical_start(cursor);
        // Nearest the cursor first, up then down, so one walk counts the logical lines to all of them
        let mut order: Vec<(i64, usize)> = lines
            .iter()
            .enumerate()
            .map(|(i, &line)| (-(line as i64), i))
            .collect();
        sort(&mut order);
        let mut anchors = vec![(0, 0); lines.len()];
        let (mut up_at, mut up, mut down_at, mut down) = (start, 0, start, 0);
        for pass in [true, false] {
            let picked = order.iter().filter(|&&(_, i)| (lines[i] < start) == pass);
            let picked: Vec<usize> = if pass {
                picked.map(|&(_, i)| i).collect()
            } else {
                picked.rev().map(|&(_, i)| i).collect()
            };
            for i in picked {
                let first = self.logical_start(lines[i]);
                if pass {
                    while up_at > first {
                        up_at = self.logical_start(up_at - 1);
                        up += 1;
                    }
                    anchors[i] = (up, lines[i] - first);
                } else {
                    while down_at < first {
                        down_at = self.logical_end(down_at) + 1;
                        down += 1;
                    }
                    anchors[i] = (-down, lines[i] - first);
                }
            }
        }
        anchors
    }

    /// Puts the markers back after a resize from what [`Engine::anchor_markers`] took.
    #[inline(never)]
    pub(crate) fn place_markers(&mut self, anchors: Vec<(i64, i32)>, reflow: bool) {
        if self.markers.live.is_empty() {
            return;
        }
        // What the resize itself did to the rows is in the anchors
        self.primary_mut().take_row_moves();
        let (top, bottom) = (self.primary_top(), self.rows() as i32 - 1);
        let cursor = self.primary().cursor.point.line.0;
        let mut lines: Vec<Option<i32>> = vec![None; anchors.len()];
        if !reflow {
            for (i, &(from_cursor, _)) in anchors.iter().enumerate() {
                let line = cursor - from_cursor as i32;
                lines[i] = (top..=bottom).contains(&line).then_some(line);
            }
        } else {
            // Nearest the cursor first, so one walk each way finds all of them
            let mut order: Vec<(i64, usize)> = anchors
                .iter()
                .enumerate()
                .map(|(i, a)| (a.0.abs(), i))
                .collect();
            sort(&mut order);
            let start = self.logical_start(cursor);
            let (mut up_at, mut up, mut down_at, mut down) = (Some(start), 0, Some(start), 0);
            for (_, i) in order {
                let (d, row) = anchors[i];
                let first = if d >= 0 {
                    while let Some(at) = up_at.filter(|_| up < d) {
                        up_at = (at > top).then(|| self.logical_start(at - 1));
                        up += 1;
                    }
                    up_at
                } else {
                    while let Some(at) = down_at.filter(|_| down < -d) {
                        let next = self.logical_end(at) + 1;
                        down_at = (next <= bottom).then_some(next);
                        down += 1;
                    }
                    down_at
                };
                // A row the reflow merged into the ones above it is gone, as xterm.js drops its markers
                lines[i] = first.and_then(|first| {
                    (first + row <= self.logical_end(first)).then_some(first + row)
                });
            }
        }
        self.relocate(false, |i, _| lines[i].map(i64::from));
    }

    /// Where each marker of the alternate screen is before a resize: rows from the cursor's row, as that screen
    /// does not reflow.
    pub(crate) fn anchor_alt_markers(&mut self) -> Vec<i64> {
        self.follow_markers();
        let entered = self.alt_entered();
        let cursor = i64::from(self.term.grid().cursor.point.line.0);
        self.markers
            .alt
            .iter()
            .map(|m| cursor - (m.line - entered))
            .collect()
    }

    /// Puts the alternate screen's markers back after a resize from what [`Engine::anchor_alt_markers`] took.
    pub(crate) fn place_alt_markers(&mut self, anchors: Vec<i64>) {
        if self.markers.alt.is_empty() {
            return;
        }
        self.grid_of_mut(true).take_row_moves();
        let cursor = i64::from(self.term.grid().cursor.point.line.0);
        let rows = self.rows() as i64;
        self.relocate(true, |i, _| {
            let line = cursor - anchors[i];
            (0..rows).contains(&line).then_some(line)
        });
    }
}

#[cfg(test)]
mod tests {
    use crate::Engine;

    fn lines(t: &Engine, ids: &[u32]) -> Vec<Option<i64>> {
        ids.iter().map(|&id| t.marker_line(id)).collect()
    }

    #[test]
    fn a_marker_follows_its_line_into_the_history_and_drops_off_the_top() {
        let mut t = Engine::new(10, 3, 20);
        t.feed(b"a\r\nb\r\nc");
        let b = t.add_marker(1).unwrap();
        for i in 0..5 {
            t.feed(format!("\r\nx{i}").as_bytes());
        }
        assert_eq!(t.history_len(), 5);
        assert_eq!(t.marker_line(b), Some(1));
        assert_eq!(t.row_text(1 - t.history_len() as i32).unwrap(), "b");
        for i in 0..20 {
            t.feed(format!("\r\ny{i}").as_bytes());
        }
        assert_eq!(t.marker_line(b), None);
        assert_eq!(t.take_disposed_markers(), [b]);
        assert!(t.take_disposed_markers().is_empty());
    }

    #[test]
    fn region_scrolls_insert_and_delete_move_or_drop_markers() {
        let mut t = Engine::new(10, 5, 20);
        t.feed(b"0\r\n1\r\n2\r\n3\r\n4");
        let ids: Vec<u32> = (0..5).map(|y| t.add_marker(y).unwrap()).collect();
        // Delete line 1: line 1 goes, 2-4 move up
        t.feed(b"\x1b[2;1H\x1b[M");
        assert_eq!(lines(&t, &ids), [Some(0), None, Some(1), Some(2), Some(3)]);
        assert_eq!(t.take_disposed_markers(), [ids[1]]);
        // Insert a line at 0: everything moves down, the bottom row (blank now) falls off
        t.feed(b"\x1b[1;1H\x1b[L");
        assert_eq!(lines(&t, &ids), [Some(1), None, Some(2), Some(3), Some(4)]);
        // A scroll region 2-4 (rows 1-3) scrolled up by a line feed at its bottom: row 1 goes, 2-3 move up
        t.feed(b"\x1b[2;4r\x1b[4;1H\n\x1b[r");
        assert_eq!(lines(&t, &ids), [None, None, Some(1), Some(2), Some(4)]);
        // ED2 clears the screen and every marker on it
        t.feed(b"\x1b[2J");
        assert_eq!(lines(&t, &ids), [None; 5]);
    }

    #[test]
    fn ris_drops_markers_and_the_alternate_screen_has_its_own() {
        let mut t = Engine::new(10, 3, 20);
        let primary = t.add_marker(0).unwrap();
        t.feed(b"\x1b[?1049h");
        assert_eq!(t.add_marker(3), None);
        let alt: Vec<u32> = (1..3).map(|y| t.add_marker(y).unwrap()).collect();
        // Scrolling the alternate screen moves its markers up and drops the one past the top
        t.feed(b"x\r\ny\r\nz\r\n\n");
        assert_eq!(lines(&t, &alt), [None, Some(0)]);
        assert_eq!(t.take_disposed_markers(), [alt[0]]);
        // A resize keeps them on their rows; the cursor's row stays at the bottom
        t.resize(12, 2);
        assert_eq!(lines(&t, &alt), [None, None]);
        t.resize(12, 4);
        let kept = t.add_marker(1).unwrap();
        t.resize(10, 4);
        assert_eq!(t.marker_line(kept), Some(1));
        // Leaving the alternate screen drops its markers
        t.feed(b"\x1b[?1049l");
        assert_eq!(t.marker_line(kept), None);
        assert_eq!(t.take_disposed_markers(), [alt[1], kept]);
        assert_eq!(
            t.marker_line(primary),
            Some(0),
            "untouched by the alternate screen"
        );
        // Also when it comes back within the same write
        t.feed(b"\x1b[?1049h");
        let again = t.add_marker(0).unwrap();
        t.feed(b"\x1b[?1049l\x1b[?1049h");
        assert_eq!(t.marker_line(again), None);
        assert_eq!(t.take_disposed_markers(), [again]);
        t.feed(b"\x1b[?1049l\x1bc");
        assert_eq!(t.marker_line(primary), None);
        assert_eq!(t.take_disposed_markers(), [primary]);
        assert_eq!(t.add_marker(4), None);
    }

    #[test]
    fn a_marker_stays_on_its_logical_line_through_reflow() {
        let mut t = Engine::new(10, 4, 50);
        t.feed(b"first\r\n0123456789abcdefghij\r\nlast\r\n");
        // Rows: 0 'first', 1-2 the wrapped line, 3 'last', cursor on 4 (pushed one row into the history)
        let h = t.history_len() as i64;
        let first = t.add_marker(h - 1).unwrap();
        let wrapped = t.add_marker(h + 1).unwrap();
        let last = t.add_marker(h + 2).unwrap();
        t.resize(20, 4);
        let at = |t: &Engine, id| {
            let line = t.marker_line(id).unwrap() as i32 - t.history_len() as i32;
            t.row_text(line).unwrap()
        };
        assert_eq!(at(&t, first), "first");
        // Its row merged into the first one, which drops it as xterm.js does
        assert_eq!(t.marker_line(wrapped), None);
        assert_eq!(t.take_disposed_markers(), [wrapped]);
        assert_eq!(at(&t, last), "last");
        t.resize(5, 4);
        assert_eq!(at(&t, first), "first");
        assert_eq!(at(&t, last), "last");
        // The third row of the line ('abcde') is still its third row at 4 columns
        let third = t.add_marker(t.marker_line(first).unwrap() + 3).unwrap();
        assert_eq!(at(&t, third), "abcde");
        t.resize(4, 4);
        assert_eq!(at(&t, third), "89ab");
        assert_eq!(at(&t, last), "last");
        // Fewer rows: the rows move, the markers with them
        t.resize(4, 2);
        assert_eq!(
            (at(&t, first), at(&t, third), at(&t, last)),
            ("firs".into(), "89ab".into(), "last".into())
        );
    }
}
