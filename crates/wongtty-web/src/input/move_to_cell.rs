//! Alt+click moves the cursor (`altClickMovesCursor`): the arrow keys that walk the cursor to the clicked cell.
//!
//! Ported from xterm.js `src/browser/input/MoveToCell.ts` @ 08829c7eb6279dda2a9e86df901353a1a89858ca (npm
//! `@xterm/xterm` 6.1.0-beta.336), lines 8-251, Copyright (c) 2018 The xterm.js authors, MIT license
//! (`crates/wongtty/LICENSE-XTERM`). The rules are unchanged; the strings xterm.js builds only to measure are
//! counted instead (in UTF-16 units, as JavaScript's `length`).
use crate::buffer::BufferView;

const UP: char = 'A';
const DOWN: char = 'B';
const RIGHT: char = 'C';
const LEFT: char = 'D';

/// The buffer as the walk sees it: lines are `buffer.active` lines; `x` and `y` the cursor on the screen.
pub(super) struct Buffer {
    pub view: BufferView,
    pub x: i32,
    pub y: i32,
    pub cols: i32,
    pub rows: i32,
    pub has_scrollback: bool,
}

impl Buffer {
    fn is_wrapped(&self, y: i32) -> bool {
        y >= 0
            && self
                .view
                .get_line(f64::from(y))
                .is_some_and(|line| line.is_wrapped())
    }

    /// `translateBufferLineToString(y, false, start, end).length`.
    fn text_len(&self, y: i32, start: i32, end: i32) -> i32 {
        if y < 0 {
            return 0;
        }
        self.view.get_line(f64::from(y)).map_or(0, |line| {
            line.translate_to_string(Some(false), Some(f64::from(start)), Some(f64::from(end)))
                .encode_utf16()
                .count() as i32
        })
    }

    fn lines(&self) -> i32 {
        self.view.length() as i32
    }
}

/// Concatenates all the arrow sequences together: resets the starting row to an unwrapped row, moves to the
/// requested row, then to the requested column.
pub(super) fn move_to_cell_sequence(
    target_x: i32,
    target_y: i32,
    b: &Buffer,
    application_cursor: bool,
) -> String {
    let (start_x, start_y) = (b.x, b.y);
    // The alt buffer should try to navigate between rows
    if !b.has_scrollback {
        return reset_starting_row(start_x, start_y, target_y, b, application_cursor)
            + &move_to_requested_row(start_y, target_y, b, application_cursor)
            + &move_to_requested_col(start_x, start_y, target_x, target_y, b, application_cursor);
    }
    // Only move horizontally for the normal buffer
    if start_y == target_y {
        let direction = if start_x > target_x { LEFT } else { RIGHT };
        return repeat((start_x - target_x).abs(), direction, application_cursor);
    }
    let direction = if start_y > target_y { LEFT } else { RIGHT };
    let row_difference = (start_y - target_y).abs();
    let cells_to_move = cols_from_row_end(if start_y > target_y { target_x } else { start_x }, b)
        + (row_difference - 1) * b.cols
        + 1 // wrap around 1 row
        + cols_from_row_beginning(if start_y > target_y { start_x } else { target_x });
    repeat(cells_to_move, direction, application_cursor)
}

fn cols_from_row_beginning(x: i32) -> i32 {
    x - 1
}

fn cols_from_row_end(x: i32, b: &Buffer) -> i32 {
    b.cols - x
}

/// If the cursor's row is wrapped, moves left to the first row that is not, for accurate vertical positioning.
fn reset_starting_row(start_x: i32, start_y: i32, target_y: i32, b: &Buffer, app: bool) -> String {
    if rows_to_move(start_y, target_y, b) <= 0 {
        return String::new();
    }
    let len = buffer_line(
        start_x,
        start_y,
        start_x,
        start_y - wrapped_rows_for_row(start_y, b),
        false,
        b,
    );
    repeat(len, LEFT, app)
}

fn rows_to_move(start_y: i32, target_y: i32, b: &Buffer) -> i32 {
    let start_row = start_y - wrapped_rows_for_row(start_y, b);
    let end_row = target_y - wrapped_rows_for_row(target_y, b);
    (start_row - end_row).abs() - wrapped_rows_count(start_y, target_y, b)
}

/// From the reset starting row to the requested row, skipping wrapped rows.
fn move_to_requested_row(start_y: i32, target_y: i32, b: &Buffer, app: bool) -> String {
    repeat(
        rows_to_move(start_y, target_y, b),
        vertical_direction(start_y, target_y),
        app,
    )
}

/// To the requested column on the ending row.
fn move_to_requested_col(
    start_x: i32,
    start_y: i32,
    target_x: i32,
    target_y: i32,
    b: &Buffer,
    app: bool,
) -> String {
    let start_row = if rows_to_move(start_y, target_y, b) > 0 {
        target_y - wrapped_rows_for_row(target_y, b)
    } else {
        start_y
    };
    let direction = horizontal_direction(start_x, start_y, target_x, target_y, b);
    let len = buffer_line(
        start_x,
        start_row,
        target_x,
        target_y,
        direction == RIGHT,
        b,
    );
    repeat(len, direction, app)
}

/// The wrapped rows between the unwrapped starting and ending rows, which the cursor skips over.
fn wrapped_rows_count(start_y: i32, target_y: i32, b: &Buffer) -> i32 {
    let start_row = start_y - wrapped_rows_for_row(start_y, b);
    let end_row = target_y - wrapped_rows_for_row(target_y, b);
    let direction = if vertical_direction(start_y, target_y) == UP {
        -1
    } else {
        1
    };
    (0..(start_row - end_row).abs())
        .filter(|i| b.is_wrapped(start_row + direction * i))
        .count() as i32
}

/// The wrapped rows that make up the row `current_row` is in.
fn wrapped_rows_for_row(mut current_row: i32, b: &Buffer) -> i32 {
    let mut row_count = 0;
    let mut line_wraps = b.is_wrapped(current_row);
    while line_wraps && current_row >= 0 && current_row < b.rows {
        row_count += 1;
        current_row -= 1;
        line_wraps = b.is_wrapped(current_row);
    }
    row_count
}

fn horizontal_direction(
    start_x: i32,
    start_y: i32,
    target_x: i32,
    target_y: i32,
    b: &Buffer,
) -> char {
    let start_row = if rows_to_move(start_y, target_y, b) > 0 {
        target_y - wrapped_rows_for_row(target_y, b)
    } else {
        start_y
    };
    if (start_x < target_x && start_row <= target_y) // down/right or same y/right
        || (start_x >= target_x && start_row < target_y)
    // down/left or same y/left
    {
        RIGHT
    } else {
        LEFT
    }
}

fn vertical_direction(start_y: i32, target_y: i32) -> char {
    if start_y > target_y {
        UP
    } else {
        DOWN
    }
}

/// The length of the buffer text from a starting row and column to an ending row and column.
fn buffer_line(
    mut start_col: i32,
    start_row: i32,
    end_col: i32,
    end_row: i32,
    forward: bool,
    b: &Buffer,
) -> i32 {
    let mut current_col = start_col;
    let mut current_row = start_row;
    let mut len = 0;
    let lines = b.lines();
    while (current_col != end_col || current_row != end_row)
        && current_row >= 0
        && current_row < lines
    {
        current_col += if forward { 1 } else { -1 };
        if forward && current_col > b.cols - 1 {
            len += b.text_len(current_row, start_col, current_col);
            current_col = 0;
            start_col = 0;
            current_row += 1;
        } else if !forward && current_col < 0 {
            len += b.text_len(current_row, 0, start_col + 1);
            current_col = b.cols - 1;
            start_col = current_col;
            current_row -= 1;
        }
    }
    len + b.text_len(current_row, start_col, current_col)
}

/// `count` arrow keys.
fn repeat(count: i32, direction: char, application_cursor: bool) -> String {
    let csi = if application_cursor { 'O' } else { '[' };
    format!("\x1b{csi}{direction}").repeat(count.max(0) as usize)
}
