//! The selection in buffer cells, before select-all, word and line lengths are applied.
//!
//! Ported from xterm.js `src/browser/selection/SelectionModel.ts` @ 08829c7eb6279dda2a9e86df901353a1a89858ca (npm
//! `@xterm/xterm` 6.1.0-beta.336), Copyright (c) 2017 The xterm.js authors, MIT license
//! (`crates/wongtty/LICENSE-XTERM`), lines 13-144. Coordinates are JavaScript numbers (`f64`) so API input behaves
//! like xterm.js, and points are shared like JavaScript arrays ([`Point`]): xterm.js remembers the points it last
//! reported by reference and moves them on trims, which decides when `onSelectionChange` fires.
use std::cell::Cell;
use std::rc::Rc;

/// `[x, y]`, shared like a JavaScript array.
pub(crate) type Point = Rc<Cell<[f64; 2]>>;

pub(crate) fn point(x: f64, y: f64) -> Point {
    Rc::new(Cell::new([x, y]))
}

#[derive(Default)]
pub(crate) struct Model {
    pub select_all: bool,
    /// The minimal length from the start: a double-clicked word or a triple-clicked line.
    pub start_length: f64,
    pub start: Option<Point>,
    pub end: Option<Point>,
}

impl Model {
    pub fn clear(&mut self) {
        self.start = None;
        self.end = None;
        self.select_all = false;
        self.start_length = 0.0;
    }

    /// The final selection start, taking select-all into account.
    pub fn final_start(&self) -> Option<Point> {
        if self.select_all {
            return Some(point(0.0, 0.0));
        }
        match (&self.start, &self.end) {
            (Some(start), Some(end)) => Some(if self.reversed() { end } else { start }.clone()),
            (start, _) => start.clone(),
        }
    }

    /// The final selection end, taking select-all and word and line lengths into account; `ybase` is the
    /// scrollback length.
    pub fn final_end(&self, cols: f64, ybase: f64, rows: f64) -> Option<Point> {
        if self.select_all {
            return Some(point(cols, ybase + rows - 1.0));
        }
        let start = self.start.as_ref()?.get();
        // Use the selection start + length if the end doesn't exist or they're reversed
        let end = match &self.end {
            Some(end) if !self.reversed() => end,
            _ => {
                let start_plus_length = start[0] + self.start_length;
                if start_plus_length > cols {
                    // Ensure the trailing EOL isn't included when the selection ends on the right edge
                    if start_plus_length % cols == 0.0 {
                        return Some(point(
                            cols,
                            start[1] + (start_plus_length / cols).floor() - 1.0,
                        ));
                    }
                    return Some(point(
                        start_plus_length % cols,
                        start[1] + (start_plus_length / cols).floor(),
                    ));
                }
                return Some(point(start_plus_length, start[1]));
            }
        };
        // Ensure the the word/line is selected after a double/triple click
        let e = end.get();
        if self.start_length != 0.0 && !self.start_length.is_nan() && e[1] == start[1] {
            // Keep the whole wrapped word/line selected if the content wraps multiple lines
            let start_plus_length = start[0] + self.start_length;
            if start_plus_length > cols {
                return Some(point(
                    start_plus_length % cols,
                    start[1] + (start_plus_length / cols).floor(),
                ));
            }
            return Some(point(js_max(start_plus_length, e[0]), e[1]));
        }
        Some(end.clone())
    }

    /// Whether the selection start and end are reversed.
    pub fn reversed(&self) -> bool {
        let (Some(start), Some(end)) = (&self.start, &self.end) else {
            return false;
        };
        let (start, end) = (start.get(), end.get());
        start[1] > end[1] || (start[1] == end[1] && start[0] > end[0])
    }

    /// The scrollback dropped `amount` lines; returns whether the selection must be redrawn.
    pub fn handle_trim(&mut self, amount: f64) -> bool {
        // Adjust the selection position based on the trimmed amount.
        for p in [&self.start, &self.end].into_iter().flatten() {
            let [x, y] = p.get();
            p.set([x, y - amount]);
        }
        // The selection has moved off the buffer, clear it.
        if self.end.as_ref().is_some_and(|end| end.get()[1] < 0.0) {
            self.clear();
            return true;
        }
        // If the selection start row is trimmed away, reset to the buffer origin.
        if self
            .start
            .as_ref()
            .is_some_and(|start| start.get()[1] < 0.0)
        {
            self.start = Some(point(0.0, 0.0));
            return true;
        }
        false
    }
}

/// JavaScript's `Math.max`: NaN wins.
pub(crate) fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.max(b)
    }
}
