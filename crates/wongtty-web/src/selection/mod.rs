//! Selection: mouse drags, double-click words, triple-click lines, Alt column selection, Shift+click extension,
//! drag auto-scroll, the copied text and the `Terminal` selection API, drawn through `Renderer::set_selection`.
//!
//! Ported from xterm.js @ 08829c7eb6279dda2a9e86df901353a1a89858ca (npm `@xterm/xterm` 6.1.0-beta.336), MIT license
//! (`crates/wongtty/LICENSE-XTERM`):
//! - `src/browser/services/SelectionService.ts` (Copyright (c) 2017 The xterm.js authors) lines 42-43 and
//!   139-1056, without the start of `handleMouseDown` and the Alt+click branch of `_handleMouseUp`, which are in
//!   `input/mouse.rs`;
//! - `src/browser/Clipboard.ts` (Copyright (c) 2016 The xterm.js authors) lines 30-41 and 61-96 (copy, the context
//!   menu, middle click on Linux);
//! - `src/browser/CoreBrowserTerminal.ts` (Copyright (c) 2014 The xterm.js authors) lines 370-407 (the copy and
//!   context menu listeners), 479 and 684-690 (`updateCursorStyle`) and 784-839 (the selection API);
//! - `src/common/buffer/Buffer.ts` lines 562-574 (`getWrappedRangeForLine`) and `src/common/buffer/BufferRange.ts`
//!   lines 8-13 (`getRangeLength`), Copyright (c) 2017 and 2021 The xterm.js authors.
//!
//! Text, widths and wrapping come from `buffer.active` (the buffer API, which PRD D7 lets read the engine): erased
//! cells and the spaces a program printed differ there as in xterm.js (`Attr.TYPED_SPACE`), so copies trim the
//! same way. Trims and screen switches come from each frame's `trimmedLines` and modes (`Viewport`, PRD D7), so the
//! selection follows them on the next animation frame.
//!
//! Memory: a few numbers per terminal. The drag timer and the document listeners exist only while the button is
//! held; lines are read when a selection needs them and dropped.
mod model;

use std::rc::Rc;

use js_sys::{Object, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{ClipboardEvent, Event, KeyboardEvent, MouseEvent};

use crate::buffer::{self, BufferLine, BufferView};
use crate::engine::mode;
use crate::engine::Viewport;
use crate::input::mouse;
use crate::options::option;
use crate::render::{Renderer, SelectionRange};
use crate::terminal::{Shared, Terminal};
use crate::timer;
use model::{js_max, point, Model, Point};

/// Pixels above or below the viewport at which drag scrolling reaches its maximum speed.
const DRAG_SCROLL_MAX_THRESHOLD: f64 = 50.0;
const DRAG_SCROLL_MAX_SPEED: f64 = 15.0;
const DRAG_SCROLL_INTERVAL: i32 = 50;
/// xterm.js `DEFAULT_OPTIONS.wordSeparator`, which an empty `wordSeparator` falls back to.
const DEFAULT_WORD_SEPARATOR: &str = " ()[]{}',\"`";

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Normal,
    Word,
    Line,
    Column,
}

/// A terminal's selection (xterm.js `SelectionService`).
#[derive(Default)]
pub(crate) struct Selection {
    model: Model,
    mode: Mode,
    /// Lines to scroll per drag scroll tick (how far the mouse is above or below the viewport).
    drag_scroll_amount: f64,
    /// The drag scroll interval, on while the button is held after a selection mousedown.
    drag_timer: Option<timer::Timeout>,
    mouse_down_time: f64,
    old_has_selection: bool,
    old_start: Option<Point>,
    old_end: Option<Point>,
    /// The trimmed line count and screen of the last frame.
    trimmed_lines: f64,
    alternate: bool,
    /// The next frame sets `trimmed_lines` without moving the selection (after `open`, a reset or an engine crash),
    /// and with `rebase_screen` also `alternate` without clearing it (after `open`).
    rebase: bool,
    rebase_screen: bool,
    /// What the renderer was last given.
    drawn: Option<SelectionRange>,
}

/// What the selection reads of the engine's size, from `buffer.active` (xterm.js `bufferService`).
#[derive(Clone, Copy)]
struct Size {
    cols: f64,
    rows: f64,
    /// Scrollback length and viewport top.
    ybase: f64,
    ydisp: f64,
}

fn size(shared: &Rc<Shared>) -> Option<Size> {
    let (cols, rows) = {
        let core = shared.core.try_borrow().ok()?;
        if core.disposed {
            return None;
        }
        core.size()
    };
    let view = buffer::active(shared);
    Some(Size {
        cols: cols.into(),
        rows: rows.into(),
        ybase: view.base_y().into(),
        ydisp: view.viewport_y().into(),
    })
}

fn opened(shared: &Shared) -> bool {
    shared
        .core
        .try_borrow()
        .is_ok_and(|core| !core.disposed && core.element.is_some())
}

/// An option as a string, `None` when it is not one.
fn string_option(shared: &Shared, key: &str) -> Option<String> {
    let core = shared.core.try_borrow().ok()?;
    option(&core.options, key).as_string()
}

fn bool_option(shared: &Shared, key: &str) -> bool {
    shared
        .core
        .try_borrow()
        .is_ok_and(|core| option(&core.options, key).is_truthy())
}

/// The final selection start and end (`selectionStart`/`selectionEnd`).
fn final_points(shared: &Rc<Shared>) -> (Option<Point>, Option<Point>) {
    let Some(size) = size(shared) else {
        return (None, None);
    };
    let s = shared.selection.borrow();
    (
        s.model.final_start(),
        s.model.final_end(size.cols, size.ybase, size.rows),
    )
}

fn differ(start: &Option<Point>, end: &Option<Point>) -> bool {
    match (start, end) {
        (Some(start), Some(end)) => {
            let (start, end) = (start.get(), end.get());
            start[0] != end[0] || start[1] != end[1]
        }
        _ => false,
    }
}

pub(crate) fn has_selection(shared: &Rc<Shared>) -> bool {
    let (start, end) = final_points(shared);
    differ(&start, &end)
}

/// A line of `buffer.active` with xterm.js `IBufferLine` reads: cells outside the line are empty with width 0.
struct Line(BufferLine);

impl Line {
    fn get(view: &BufferView, y: f64) -> Option<Line> {
        view.get_line(y).map(Line)
    }

    fn len(&self) -> i64 {
        self.0.length().into()
    }

    fn wrapped(&self) -> bool {
        self.0.is_wrapped()
    }

    fn cell(&self, x: i64) -> Option<buffer::BufferCell> {
        (x >= 0).then(|| self.0.get_cell(x as f64, None)).flatten()
    }

    fn width(&self, x: i64) -> i64 {
        self.cell(x).map_or(0, |c| c.width().into())
    }

    fn chars(&self, x: i64) -> String {
        self.cell(x).map(|c| c.get_chars()).unwrap_or_default()
    }

    fn code(&self, x: i64) -> u32 {
        self.cell(x).map_or(0, |c| c.get_code())
    }

    fn text(&self, trim_right: bool, start: Option<f64>, end: Option<f64>) -> String {
        self.0.translate_to_string(Some(trim_right), start, end)
    }
}

/// `buffer.translateBufferLineToString(y, true, start, end)`.
fn line_text(view: &BufferView, y: f64, start: f64, end: Option<f64>) -> String {
    Line::get(view, y).map_or_else(String::new, |line| line.text(true, Some(start), end))
}

/// xterm.js `selectionText`: wrapped rows joined, NBSP as spaces, lines joined with `\r\n` on Windows. Rows more
/// than one past either end of the buffer add nothing (xterm.js adds an empty line per row).
fn selection_text(shared: &Rc<Shared>) -> String {
    let (Some(start), Some(end)) = final_points(shared) else {
        return String::new();
    };
    let (start, end) = (start.get(), end.get());
    let column = shared.selection.borrow().mode == Mode::Column;
    let view = buffer::active(shared);
    let length = f64::from(view.length());
    let first = start[1].max(-1.0);
    let last = end[1].min(length);
    let mut result: Vec<String> = Vec::new();
    if column {
        // Ignore zero width selections
        if start[0] == end[0] {
            return String::new();
        }
        let (start_col, end_col) = if start[0] < end[0] {
            (start[0], end[0])
        } else {
            (end[0], start[0])
        };
        let mut y = first;
        while y <= last {
            result.push(line_text(&view, y, start_col, Some(end_col)));
            y += 1.0;
        }
    } else {
        let start_row_end_col = (start[1] == end[1]).then_some(end[0]);
        result.push(line_text(&view, start[1], start[0], start_row_end_col));
        let mut y = (start[1] + 1.0).max(first);
        while y <= (end[1] - 1.0).min(last) {
            let line = Line::get(&view, y);
            let text = line
                .as_ref()
                .map_or_else(String::new, |l| l.text(true, None, None));
            match result.last_mut() {
                Some(previous) if line.as_ref().is_some_and(Line::wrapped) => {
                    previous.push_str(&text)
                }
                _ => result.push(text),
            }
            y += 1.0;
        }
        if start[1] != end[1] {
            let line = Line::get(&view, end[1]);
            let text = line
                .as_ref()
                .map_or_else(String::new, |l| l.text(true, Some(0.0), Some(end[0])));
            match result.last_mut() {
                Some(previous) if line.as_ref().is_some_and(Line::wrapped) => {
                    previous.push_str(&text)
                }
                _ => result.push(text),
            }
        }
    }
    let separator = if mouse::platform_windows() {
        "\r\n"
    } else {
        "\n"
    };
    result
        .iter()
        .map(|line| line.replace('\u{a0}', " "))
        .collect::<Vec<_>>()
        .join(separator)
}

/// Asks for a frame, where the selection is handed to the renderer (xterm.js `refresh`). On Linux a mouse
/// selection goes into the textarea so a middle click pastes it.
fn refresh(shared: &Rc<Shared>, linux_mouse_selection: bool) {
    shared.schedule_frame();
    if linux_mouse_selection && mouse::platform_linux() {
        let text = selection_text(shared);
        if !text.is_empty() {
            if let Some(textarea) = mouse::textarea(shared) {
                textarea.set_value(&text);
                let _ = textarea.focus();
                textarea.select();
            }
        }
    }
}

fn fire(shared: &Rc<Shared>) {
    shared.events.selection_change.fire(&[]);
}

/// xterm.js `_removeMouseDownListeners`.
fn remove_mouse_down_listeners(shared: &Rc<Shared>) {
    let timer = shared.selection.borrow_mut().drag_timer.take();
    drop(timer);
    mouse::set_selection_listeners(shared, false);
}

/// xterm.js `clearSelection`.
pub(crate) fn clear_selection(shared: &Rc<Shared>) {
    shared.selection.borrow_mut().model.clear();
    remove_mouse_down_listeners(shared);
    refresh(shared, false);
    fire(shared);
}

/// xterm.js `disable`: the program takes clicks (`selection_enabled` is already off).
pub(crate) fn disable(shared: &Rc<Shared>) {
    clear_selection(shared);
}

fn fire_on_selection_change(
    shared: &Rc<Shared>,
    start: Option<Point>,
    end: Option<Point>,
    has_selection: bool,
) {
    {
        let mut s = shared.selection.borrow_mut();
        s.old_start = start;
        s.old_end = end;
        s.old_has_selection = has_selection;
    }
    fire(shared);
}

/// xterm.js `_fireEventIfSelectionChanged`.
fn fire_event_if_selection_changed(shared: &Rc<Shared>) {
    let (start, end) = final_points(shared);
    let has_selection = differ(&start, &end);
    if !has_selection {
        if shared.selection.borrow().old_has_selection {
            fire_on_selection_change(shared, start, end, has_selection);
        }
        return;
    }
    let (Some(s), Some(e)) = (&start, &end) else {
        return;
    };
    let changed = {
        let sel = shared.selection.borrow();
        match (&sel.old_start, &sel.old_end) {
            (Some(old_start), Some(old_end)) => {
                let (s, e, os, oe) = (s.get(), e.get(), old_start.get(), old_end.get());
                s[0] != os[0] || s[1] != os[1] || e[0] != oe[0] || e[1] != oe[1]
            }
            _ => true,
        }
    };
    if changed {
        fire_on_selection_change(shared, start, end, has_selection);
    }
}

/// Mousedown time, set first thing on every mousedown (xterm.js `_mouseDownTimeStamp`).
pub(crate) fn set_mouse_down_time(shared: &Shared, time: f64) {
    shared.selection.borrow_mut().mouse_down_time = time;
}

pub(crate) fn mouse_down_time(shared: &Shared) -> f64 {
    shared.selection.borrow().mouse_down_time
}

/// The 0-based buffer cell under the mouse, the right half of a cell counting as the next one.
fn mouse_buffer_coords(shared: &Rc<Shared>, event: &MouseEvent) -> Option<Point> {
    let (x, y) = mouse::screen_coords(shared, event, true)?;
    let ydisp = size(shared)?.ydisp;
    Some(point(f64::from(x) - 1.0, f64::from(y) - 1.0 + ydisp))
}

/// xterm.js `_getMouseEventScrollAmount`.
fn mouse_event_scroll_amount(shared: &Rc<Shared>, event: &MouseEvent) -> f64 {
    let Some((mut offset, height)) = mouse::screen_offset_y(shared, event) else {
        return 0.0;
    };
    if offset >= 0.0 && offset <= height {
        return 0.0;
    }
    if offset > height {
        offset -= height;
    }
    offset = offset.clamp(-DRAG_SCROLL_MAX_THRESHOLD, DRAG_SCROLL_MAX_THRESHOLD);
    offset /= DRAG_SCROLL_MAX_THRESHOLD;
    offset / offset.abs() + js_round(offset * (DRAG_SCROLL_MAX_SPEED - 1.0))
}

/// JavaScript's `Math.round`.
fn js_round(n: f64) -> f64 {
    (n + 0.5).floor()
}

/// xterm.js `shouldColumnSelect`.
fn should_column_select(shared: &Rc<Shared>, alt: bool) -> bool {
    if mouse::require_alt_active(shared) {
        return false;
    }
    alt && !(mouse::platform_mac() && bool_option(shared, "macOptionClickForcesSelection"))
}

/// The rest of xterm.js `handleMouseDown`, after `input/mouse.rs` let a primary-button selection through.
pub(crate) fn mouse_down(shared: &Rc<Shared>, event: &MouseEvent) {
    // Reset drag scroll state
    shared.selection.borrow_mut().drag_scroll_amount = 0.0;
    if mouse::selection_enabled(shared) && event.shift_key() {
        handle_incremental_click(shared, event);
    } else {
        match event.detail() {
            1 => handle_single_click(shared, event),
            2 => handle_double_click(shared, event),
            3 => handle_triple_click(shared, event),
            _ => {}
        }
    }
    add_mouse_down_listeners(shared);
    refresh(shared, true);
}

/// Listens on the document so that dragging outside of the viewport works, and scrolls while the button is held.
fn add_mouse_down_listeners(shared: &Rc<Shared>) {
    mouse::set_selection_listeners(shared, true);
    arm_drag_scroll(shared);
}

fn arm_drag_scroll(shared: &Rc<Shared>) {
    let weak = Rc::downgrade(shared);
    let timer = timer::Timeout::new(DRAG_SCROLL_INTERVAL, move || {
        if let Some(shared) = weak.upgrade() {
            let armed = shared.selection.borrow_mut().drag_timer.take().is_some();
            if armed {
                arm_drag_scroll(&shared);
                drag_scroll(&shared);
            }
        }
    });
    shared.selection.borrow_mut().drag_timer = Some(timer);
}

fn handle_incremental_click(shared: &Rc<Shared>, event: &MouseEvent) {
    if shared.selection.borrow().model.start.is_some() {
        let end = mouse_buffer_coords(shared, event);
        shared.selection.borrow_mut().model.end = end;
    }
}

fn handle_single_click(shared: &Rc<Shared>, event: &MouseEvent) {
    // Track if there was a selection before clearing
    let had_selection = has_selection(shared);
    let column = should_column_select(shared, event.alt_key());
    let start = mouse_buffer_coords(shared, event);
    {
        let mut s = shared.selection.borrow_mut();
        s.model.start_length = 0.0;
        s.model.select_all = false;
        s.mode = if column { Mode::Column } else { Mode::Normal };
        s.model.start = start.clone();
    }
    let Some(start) = start else {
        return;
    };
    shared.selection.borrow_mut().model.end = None;
    // Fire selection change event if a selection was cleared
    if had_selection {
        let (s, e) = final_points(shared);
        fire_on_selection_change(shared, s, e, false);
    }
    // Ensure the line exists
    let view = buffer::active(shared);
    let [x, y] = start.get();
    let Some(line) = Line::get(&view, y) else {
        return;
    };
    // Return early if the click event is not in the buffer (eg. in scroll bar)
    if line.len() as f64 == x {
        return;
    }
    // If the mouse is over the second half of a wide character, adjust the selection to cover the whole character
    if line.width(x as i64) == 0 {
        start.set([x + 1.0, y]);
    }
}

fn handle_double_click(shared: &Rc<Shared>, event: &MouseEvent) {
    if select_word_at_cursor(shared, event, true) {
        shared.selection.borrow_mut().mode = Mode::Word;
    }
}

fn handle_triple_click(shared: &Rc<Shared>, event: &MouseEvent) {
    if let Some(coords) = mouse_buffer_coords(shared, event) {
        shared.selection.borrow_mut().mode = Mode::Line;
        select_line_at(shared, coords.get()[1]);
    }
}

/// xterm.js `_selectWordAtCursor`: the hovered link, else the word under the mouse.
fn select_word_at_cursor(shared: &Rc<Shared>, event: &MouseEvent, allow_whitespace: bool) -> bool {
    if let Some(range) = crate::links::current_link_range(shared) {
        let cols = size(shared).map_or(0.0, |s| s.cols);
        let mut s = shared.selection.borrow_mut();
        s.model.start = Some(point(range[0] - 1.0, range[1] - 1.0));
        s.model.start_length = range_length(range, cols);
        s.model.end = None;
        return true;
    }
    if let Some(coords) = mouse_buffer_coords(shared, event) {
        select_word_at(shared, coords.get(), allow_whitespace);
        shared.selection.borrow_mut().model.end = None;
        return true;
    }
    false
}

/// xterm.js `getRangeLength` of a 1-based `[startX, startY, endX, endY]` range.
fn range_length([sx, sy, ex, ey]: [f64; 4], cols: f64) -> f64 {
    cols * (ey - sy) + (ex - sx + 1.0)
}

/// What word positions need, read once.
struct WordContext {
    view: BufferView,
    cols: i64,
    separators: String,
}

impl WordContext {
    fn new(shared: &Rc<Shared>) -> Option<Self> {
        let cols = size(shared)?.cols as i64;
        let separators = string_option(shared, "wordSeparator")
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_WORD_SEPARATOR.into());
        (cols > 0).then(|| Self {
            view: buffer::active(shared),
            cols,
            separators,
        })
    }

    /// xterm.js `_isCharWordSeparator`: zero width characters are never separators as they are always to the right
    /// of wide characters; an empty cell (`''`) always is.
    fn is_separator(&self, line: &Line, x: i64) -> bool {
        line.width(x) != 0 && self.separators.contains(line.chars(x).as_str())
    }

    /// xterm.js `_convertViewportColToCharacterIndex`.
    fn char_index(line: &Line, x: i64) -> i64 {
        let mut index = x;
        for i in 0..=x {
            let length = line.chars(i).encode_utf16().count() as i64;
            if line.width(i) == 0 {
                // Wide characters aren't included in the line string so decrement the index
                index -= 1;
            } else if length > 1 && x != i {
                // Emojis take up multiple characters, so adjust accordingly
                index += length - 1;
            }
        }
        index
    }

    /// The part of xterm.js `_getWordAt` before it follows wrapped lines: `(start, length)` in columns.
    fn word_in_line(
        &self,
        line: &Line,
        [x, _]: [i64; 2],
        allow_whitespace: bool,
    ) -> Option<(i64, i64)> {
        let text: Vec<u16> = line.text(false, None, None).encode_utf16().collect();
        let char_at = |i: i64| usize::try_from(i).ok().and_then(|i| text.get(i).copied());
        let text_len = text.len() as i64;
        let mut start_index = Self::char_index(line, x);
        let mut end_index = start_index;
        // Record offset to be used later
        let char_offset = x - start_index;
        let (mut left_wide, mut right_wide, mut left_long, mut right_long) = (0, 0, 0, 0);
        if char_at(start_index) == Some(32) {
            // Expand until non-whitespace is hit
            while start_index > 0 && char_at(start_index - 1) == Some(32) {
                start_index -= 1;
            }
            while end_index < text_len && char_at(end_index + 1) == Some(32) {
                end_index += 1;
            }
        } else {
            // Expand until whitespace is hit, keeping the string index and the column in sync
            let (mut start_col, mut end_col) = (x, x);
            if line.width(start_col) == 0 {
                left_wide += 1;
                start_col -= 1;
            }
            if line.width(end_col) == 2 {
                right_wide += 1;
                end_col += 1;
            }
            // Adjust the end index for characters whose length are > 1 (emojis)
            let length = line.chars(end_col).encode_utf16().count() as i64;
            if length > 1 {
                right_long += length - 1;
                end_index += length - 1;
            }
            // Expand the string in both directions until a space is hit
            while start_col > 0 && start_index > 0 && !self.is_separator(line, start_col - 1) {
                let length = line.chars(start_col - 1).encode_utf16().count() as i64;
                if line.width(start_col - 1) == 0 {
                    // If the next character is a wide char, record it and skip the column
                    left_wide += 1;
                    start_col -= 1;
                } else if length > 1 {
                    left_long += length - 1;
                    start_index -= length - 1;
                }
                start_index -= 1;
                start_col -= 1;
            }
            while end_col < line.len()
                && end_index + 1 < text_len
                && !self.is_separator(line, end_col + 1)
            {
                let length = line.chars(end_col + 1).encode_utf16().count() as i64;
                if line.width(end_col + 1) == 2 {
                    right_wide += 1;
                    end_col += 1;
                } else if length > 1 {
                    right_long += length - 1;
                    end_index += length - 1;
                }
                end_index += 1;
                end_col += 1;
            }
        }
        // Increment the end index so it is at the start of the next character
        end_index += 1;
        let start = start_index + char_offset - left_wide + left_long;
        let length = self
            .cols
            .min(end_index - start_index + left_wide + right_wide - left_long - right_long);
        if !allow_whitespace {
            let from = start_index.clamp(0, text_len) as usize;
            let to = end_index.clamp(0, text_len) as usize;
            let slice = text.get(from..to.max(from)).unwrap_or_default();
            if String::from_utf16_lossy(slice)
                .chars()
                .all(is_js_whitespace)
            {
                return None;
            }
        }
        Some((start, length))
    }

    /// xterm.js `_getWordAt` (its recursion over wrapped lines as loops).
    fn word_at(&self, [x, y]: [i64; 2], allow_whitespace: bool) -> Option<(i64, i64)> {
        // Ensure coords are within viewport (eg. not within scroll bar)
        if x >= self.cols {
            return None;
        }
        let line = Line::get(&self.view, y as f64)?;
        let (mut start, mut length) = self.word_in_line(&line, [x, y], allow_whitespace)?;
        let cols = self.cols;
        // Follow the word up while it wraps from the line above
        let (mut row, mut current, mut row_start, mut offset) = (y, None::<Line>, start, 0);
        loop {
            let l = current.as_ref().unwrap_or(&line);
            if row_start != 0 || l.code(0) == 32 {
                break;
            }
            let Some(previous) = Line::get(&self.view, (row - 1) as f64) else {
                break;
            };
            if !l.wrapped() || previous.code(cols - 1) == 32 {
                break;
            }
            let Some((s, _)) = self.word_in_line(&previous, [cols - 1, row - 1], false) else {
                break;
            };
            offset += cols - s;
            row_start = s;
            row -= 1;
            current = Some(previous);
        }
        start -= offset;
        length += offset;
        // Follow the word down while it wraps into the line below
        let (mut row, mut current, mut row_end) = (y, None::<Line>, start + length);
        loop {
            let l = current.as_ref().unwrap_or(&line);
            if row_end != cols || l.code(cols - 1) == 32 {
                break;
            }
            let Some(next) = Line::get(&self.view, (row + 1) as f64) else {
                break;
            };
            if !next.wrapped() || next.code(0) == 32 {
                break;
            }
            let Some((s, l)) = self.word_in_line(&next, [0, row + 1], false) else {
                break;
            };
            length += l;
            row_end = s + l;
            row += 1;
            current = Some(next);
        }
        Some((start, length))
    }
}

/// JavaScript's `String.prototype.trim` whitespace.
fn is_js_whitespace(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

/// xterm.js `_selectWordAt`.
fn select_word_at(shared: &Rc<Shared>, [x, y]: [f64; 2], allow_whitespace: bool) {
    let Some(context) = WordContext::new(shared) else {
        return;
    };
    let Some((mut start, length)) = context.word_at([x as i64, y as i64], allow_whitespace) else {
        return;
    };
    let mut row = y;
    // Adjust negative start value
    while start < 0 {
        start += context.cols;
        row -= 1.0;
    }
    let mut s = shared.selection.borrow_mut();
    s.model.start = Some(point(start as f64, row));
    s.model.start_length = length as f64;
}

/// xterm.js `_selectToWordAt`: the selection end goes to the word at `coords`.
fn select_to_word_at(shared: &Rc<Shared>, [x, y]: [f64; 2]) {
    let Some(context) = WordContext::new(shared) else {
        return;
    };
    let Some((mut start, mut length)) = context.word_at([x as i64, y as i64], true) else {
        return;
    };
    let mut end_row = y;
    // Adjust negative start value
    while start < 0 {
        start += context.cols;
        end_row -= 1.0;
    }
    let mut s = shared.selection.borrow_mut();
    let reversed = s.model.reversed();
    // Adjust wrapped length value, this only needs to happen when values are reversed as in that case we're
    // interested in the start of the word, not the end
    if !reversed {
        while start + length > context.cols {
            length -= context.cols;
            end_row += 1.0;
        }
    }
    let end_x = if reversed { start } else { start + length };
    s.model.end = Some(point(end_x as f64, end_row));
}

/// xterm.js `_selectLineAt`, with `getWrappedRangeForLine`.
fn select_line_at(shared: &Rc<Shared>, y: f64) {
    let Some(size) = size(shared) else {
        return;
    };
    let view = buffer::active(shared);
    let length = f64::from(view.length());
    let wrapped = |y: f64| Line::get(&view, y).is_some_and(|l| l.wrapped());
    let (mut first, mut last) = (y, y);
    while first > 0.0 && wrapped(first) {
        first -= 1.0;
    }
    while last + 1.0 < length && wrapped(last + 1.0) {
        last += 1.0;
    }
    let mut s = shared.selection.borrow_mut();
    s.model.start = Some(point(0.0, first));
    s.model.end = None;
    s.model.start_length = range_length([1.0, first + 1.0, size.cols, last + 1.0], size.cols);
}

/// The selection's document mousemove while the button is held (xterm.js `_handleMouseMove`).
pub(crate) fn mouse_move(shared: &Rc<Shared>, event: &MouseEvent) {
    // A selection is being made: keep the move from the program
    event.stop_immediate_propagation();
    // Do nothing if there is no selection start, this can happen if the first click in the terminal is an
    // incremental click
    if shared.selection.borrow().model.start.is_none() {
        return;
    }
    let Some(size) = size(shared) else {
        return;
    };
    let previous = shared
        .selection
        .borrow()
        .model
        .end
        .as_ref()
        .map(|end| end.get());
    // Set the initial selection end based on the mouse coordinates
    let end = mouse_buffer_coords(shared, event);
    shared.selection.borrow_mut().model.end = end.clone();
    let Some(end) = end else {
        refresh(shared, true);
        return;
    };
    let mode = shared.selection.borrow().mode;
    if mode == Mode::Line {
        // Select the entire line if line select mode is active
        let start_y = shared
            .selection
            .borrow()
            .model
            .start
            .as_ref()
            .map_or(0.0, |s| s.get()[1]);
        let [_, y] = end.get();
        end.set([if y < start_y { 0.0 } else { size.cols }, y]);
    } else if mode == Mode::Word {
        select_to_word_at(shared, end.get());
    }
    // Determine the amount of scrolling that will happen
    let amount = mouse_event_scroll_amount(shared, event);
    let end = {
        let mut s = shared.selection.borrow_mut();
        s.drag_scroll_amount = amount;
        s.model.end.clone()
    };
    let Some(end) = end else {
        return;
    };
    // If the cursor was above or below the viewport, make sure it's at the start or end of the viewport
    // respectively (not in column select mode)
    if mode != Mode::Column {
        let [_, y] = end.get();
        if amount > 0.0 {
            end.set([size.cols, y]);
        } else if amount < 0.0 {
            end.set([0.0, y]);
        }
    }
    // A wide character takes in the cell to its right; selections at the very end of the line never have one
    let view = buffer::active(shared);
    let [x, y] = end.get();
    if y < f64::from(view.length()) {
        if let Some(line) = Line::get(&view, y) {
            if line.width(x as i64) == 0 && x < size.cols {
                end.set([x + 1.0, y]);
            }
        }
    }
    // Only draw here if the selection changes
    if previous != Some(end.get()) {
        refresh(shared, true);
    }
}

/// The drag scroll interval (xterm.js `_dragScroll`).
fn drag_scroll(shared: &Rc<Shared>) {
    let (amount, mode) = {
        let s = shared.selection.borrow();
        if s.model.end.is_none() || s.model.start.is_none() {
            return;
        }
        (s.drag_scroll_amount, s.mode)
    };
    if amount == 0.0 || amount.is_nan() {
        return;
    }
    let _ = Terminal {
        shared: shared.clone(),
    }
    .scroll_lines(amount as i32);
    let Some(size) = size(shared) else {
        return;
    };
    let length = f64::from(buffer::active(shared).length());
    let end = shared.selection.borrow().model.end.clone();
    let Some(end) = end else {
        return;
    };
    let [x, _] = end.get();
    if amount > 0.0 {
        let x = if mode != Mode::Column { size.cols } else { x };
        end.set([x, (size.ydisp + size.rows - 1.0).min(length - 1.0)]);
    } else {
        let x = if mode != Mode::Column { 0.0 } else { x };
        end.set([x, size.ydisp]);
    }
    refresh(shared, false);
}

/// The selection's document mouseup: removes the listeners and tells whether its text is at most one UTF-16 unit
/// (an Alt+click that moves the cursor); `input/mouse.rs` does the rest of xterm.js `_handleMouseUp`.
pub(crate) fn mouse_up(shared: &Rc<Shared>) {
    remove_mouse_down_listeners(shared);
}

pub(crate) fn text_is_at_most_one_unit(shared: &Rc<Shared>) -> bool {
    selection_text(shared).encode_utf16().count() <= 1
}

/// The end of a selection mouseup that does not move the cursor.
pub(crate) fn mouse_up_changed(shared: &Rc<Shared>) {
    fire_event_if_selection_changed(shared);
}

/// xterm.js `rightClickSelect`: a right click outside the selection selects the word under it.
fn right_click_select(shared: &Rc<Shared>, event: &MouseEvent) {
    let in_selection = match (mouse_buffer_coords(shared, event), final_points(shared)) {
        (Some(coords), (Some(start), Some(end))) => {
            in_selection(coords.get(), start.get(), end.get())
        }
        _ => false,
    };
    if !in_selection {
        if select_word_at_cursor(shared, event, false) {
            refresh(shared, true);
        }
        fire_event_if_selection_changed(shared);
    }
}

/// xterm.js `_areCoordsInSelection`.
fn in_selection([x, y]: [f64; 2], start: [f64; 2], end: [f64; 2]) -> bool {
    (y > start[1] && y < end[1])
        || (start[1] == end[1] && y == start[1] && x >= start[0] && x < end[0])
        || (start[1] < end[1] && y == end[1] && x < end[0])
        || (start[1] < end[1] && y == start[1] && x >= start[0])
}

/// The element's `copy`: while the program reports the mouse there is no selection and copy is left to it.
pub(crate) fn copy(shared: &Rc<Shared>, event: &ClipboardEvent) {
    if !has_selection(shared) {
        return;
    }
    if let Some(data) = event.clipboard_data() {
        let _ = data.set_data("text/plain", &selection_text(shared));
    }
    // Prevent or the original text will be copied.
    event.prevent_default();
}

/// xterm.js `moveTextAreaUnderMouseCursor`.
pub(crate) fn move_textarea_under_mouse(shared: &Rc<Shared>, event: &MouseEvent) {
    let (Some(textarea), Some(screen)) = (mouse::textarea(shared), mouse::screen(shared)) else {
        return;
    };
    let rect = screen.get_bounding_client_rect();
    let left = f64::from(event.client_x()) - rect.left() - 10.0;
    let top = f64::from(event.client_y()) - rect.top() - 10.0;
    let style = textarea.style();
    let _ = style.set_property("width", "20px");
    let _ = style.set_property("height", "20px");
    let _ = style.set_property("left", &(crate::input::js_number(left) + "px"));
    let _ = style.set_property("top", &(crate::input::js_number(top) + "px"));
    let _ = style.set_property("z-index", "1000");
    let _ = textarea.focus();
}

/// xterm.js `rightClickHandler`: the textarea moves under the mouse and holds the selection, so the context menu
/// can copy it; with `rightClickSelectsWord` a right click outside the selection selects the word first.
pub(crate) fn right_click(shared: &Rc<Shared>, event: &MouseEvent) {
    move_textarea_under_mouse(shared, event);
    if bool_option(shared, "rightClickSelectsWord") {
        right_click_select(shared, event);
    }
    // Get textarea ready to copy from the context menu
    if let Some(textarea) = mouse::textarea(shared) {
        textarea.set_value(&selection_text(shared));
        textarea.select();
    }
}

/// xterm.js `updateCursorStyle`: a crosshair while Alt would make a column selection.
pub(crate) fn update_cursor_style(shared: &Rc<Shared>, event: &Event) {
    let alt = if let Some(e) = event.dyn_ref::<KeyboardEvent>() {
        e.alt_key()
    } else if let Some(e) = event.dyn_ref::<MouseEvent>() {
        e.alt_key()
    } else {
        crate::engine::get(event, "altKey").is_truthy()
    };
    let column = should_column_select(shared, alt);
    if let Some(element) = mouse::element(shared) {
        let classes = element.class_list();
        let _ = if column {
            classes.add_1("column-select")
        } else {
            classes.remove_1("column-select")
        };
    }
}

/// Follows each frame just applied to the viewport (PRD D7): lines dropped off the top of the scrollback move the
/// selection up (clearing it once it falls off), and a switch to or from the alternate screen clears it. Runs under
/// the core borrow before the selection is painted, so it calls no JS; returns whether the selection was cleared
/// by a screen switch, which the caller announces with [`screen_switched`] once the borrow is dropped.
pub(crate) fn follow(shared: &Shared, viewport: &Viewport) -> bool {
    let Ok(mut s) = shared.selection.try_borrow_mut() else {
        return false;
    };
    let alternate = viewport.modes & mode::ALT_SCREEN != 0;
    let (rebase, rebase_screen) = (
        std::mem::take(&mut s.rebase),
        std::mem::take(&mut s.rebase_screen),
    );
    let amount = viewport.trimmed_lines - s.trimmed_lines;
    let switched = alternate != s.alternate && !rebase_screen;
    s.trimmed_lines = viewport.trimmed_lines;
    s.alternate = alternate;
    // The frame paints the moved selection right after
    if amount != 0.0 && !rebase {
        s.model.handle_trim(amount);
    }
    if switched {
        s.model.clear();
    }
    switched
}

/// The screen switch [`follow`] saw: the rest of xterm.js `clearSelection` (listeners, `onSelectionChange`).
pub(crate) fn screen_switched(shared: &Rc<Shared>) {
    clear_selection(shared);
}

/// Full resets (the program's RIS, `Terminal.reset`): like xterm.js, each sets the buffers up again, which clears
/// the selection, and then resets the selection, which clears it once more.
pub(crate) fn handle_resets(shared: &Rc<Shared>, resets: u32) {
    if resets == 0 || !opened(shared) {
        return;
    }
    // A reset leaves the normal screen on; the next frame's trimmed line count is taken as is
    {
        let mut s = shared.selection.borrow_mut();
        s.rebase = true;
        s.alternate = false;
    }
    for _ in 0..resets {
        clear_selection(shared);
        clear_selection(shared);
    }
}

/// `Terminal.resize`: like xterm.js, a change of rows clears the selection.
pub(crate) fn after_resize(shared: &Rc<Shared>, rows_changed: bool) {
    if rows_changed && opened(shared) {
        clear_selection(shared);
    }
}

/// After an engine crash the terminal starts over empty: the renderer lost the selection, and it is cleared.
pub(crate) fn handle_crash(shared: &Rc<Shared>) {
    shared.selection.borrow_mut().drawn = None;
    handle_resets(shared, 1);
}

/// `open`: while the program already takes clicks the selection starts disabled, which clears it.
pub(crate) fn open(shared: &Rc<Shared>) {
    {
        let mut s = shared.selection.borrow_mut();
        s.rebase = true;
        s.rebase_screen = true;
    }
    if !mouse::selection_enabled(shared) {
        disable(shared);
    }
}

/// User input clears the selection (xterm.js `coreService.onUserInput`).
pub(crate) fn user_input(shared: &Rc<Shared>) {
    if has_selection(shared) {
        clear_selection(shared);
    }
}

pub(crate) fn dispose(shared: &Shared) {
    let timer = shared.selection.borrow_mut().drag_timer.take();
    drop(timer);
}

/// Hands the selection to the renderer in viewport cells when it or the view moved; runs on every frame before
/// the renderer draws.
pub(crate) fn paint(
    shared: &Shared,
    renderer: &mut dyn Renderer,
    viewport: &crate::engine::Viewport,
) {
    let Ok(mut s) = shared.selection.try_borrow_mut() else {
        return;
    };
    let (cols, rows) = (f64::from(viewport.cols), f64::from(viewport.rows));
    let ybase = f64::from(viewport.history_length);
    let start = s.model.final_start().map(|p| p.get());
    let end = s.model.final_end(cols, ybase, rows).map(|p| p.get());
    let range = match (start, end) {
        (Some(start), Some(end)) if start != end => {
            let top = ybase - f64::from(viewport.display_offset);
            let (start_y, end_y) = (start[1] - top, end[1] - top);
            let column_select = s.mode == Mode::Column;
            // The renderer takes unsigned cells: a start above the view starts at its top (column selections keep
            // their column), and columns round up to the cells they cover.
            let cell = |x: f64| x.max(0.0).ceil() as u32;
            (end_y >= 0.0 && start_y < rows && !start_y.is_nan() && !end_y.is_nan()).then(|| {
                SelectionRange {
                    start: if start_y < 0.0 {
                        (if column_select { cell(start[0]) } else { 0 }, 0)
                    } else {
                        (cell(start[0]), start_y as u32)
                    },
                    end: (cell(end[0]), end_y as u32),
                    column_select,
                }
            })
        }
        _ => None,
    };
    if range != s.drawn {
        s.drawn = range;
        renderer.set_selection(range);
    }
}

fn position(shared: &Rc<Shared>) -> Option<([f64; 2], [f64; 2])> {
    let (start, end) = final_points(shared);
    if !differ(&start, &end) {
        return None;
    }
    Some((start?.get(), end?.get()))
}

/// `getSelectionPosition()` for the search addon: buffer cells, end exclusive.
pub(crate) fn selection_position(shared: &Rc<Shared>) -> Option<([f64; 2], [f64; 2])> {
    position(shared)
}

/// xterm.js `setSelection` (`Terminal.select`).
pub(crate) fn set_selection(shared: &Rc<Shared>, column: f64, row: f64, length: f64) {
    {
        let mut s = shared.selection.borrow_mut();
        s.model.clear();
    }
    remove_mouse_down_listeners(shared);
    {
        let mut s = shared.selection.borrow_mut();
        s.model.start = Some(point(column, row));
        s.model.start_length = length;
    }
    refresh(shared, false);
    fire_event_if_selection_changed(shared);
}

/// xterm.js `selectAll`.
pub(crate) fn select_all(shared: &Rc<Shared>) {
    if !opened(shared) {
        return;
    }
    shared.selection.borrow_mut().model.select_all = true;
    refresh(shared, false);
    fire(shared);
}

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
/** xterm.js `IBufferCellPosition`: a buffer cell, `x` the column and `y` the buffer line. */
export interface IBufferCellPosition {
  x: number;
  y: number;
}
/** xterm.js `IBufferRange`. */
export interface IBufferRange {
  start: IBufferCellPosition;
  end: IBufferCellPosition;
}
"#;

#[wasm_bindgen]
impl Terminal {
    /// Whether some text is selected.
    #[wasm_bindgen(js_name = hasSelection)]
    pub fn has_selection(&self) -> bool {
        has_selection(&self.shared)
    }

    /// The selected text: wrapped rows joined, wide characters once, lines joined with `\n` (`\r\n` on Windows).
    #[wasm_bindgen(js_name = getSelection)]
    pub fn get_selection(&self) -> String {
        selection_text(&self.shared)
    }

    /// The selection in buffer cells, end exclusive, or `undefined` without one.
    #[wasm_bindgen(
        js_name = getSelectionPosition,
        unchecked_return_type = "IBufferRange | undefined"
    )]
    pub fn get_selection_position(&self) -> JsValue {
        let Some((start, end)) = position(&self.shared) else {
            return JsValue::UNDEFINED;
        };
        let cell = |[x, y]: [f64; 2]| {
            let o = Object::new();
            let _ = Reflect::set(&o, &"x".into(), &x.into());
            let _ = Reflect::set(&o, &"y".into(), &y.into());
            o
        };
        let range = Object::new();
        let _ = Reflect::set(&range, &"start".into(), &cell(start));
        let _ = Reflect::set(&range, &"end".into(), &cell(end));
        range.into()
    }

    #[wasm_bindgen(js_name = clearSelection)]
    pub fn clear_selection(&self) {
        if opened(&self.shared) {
            clear_selection(&self.shared);
        }
    }

    /// Selects `length` cells from `column` of buffer line `row`, running on into the next rows. Does nothing
    /// before `open()` (xterm.js throws).
    pub fn select(&self, column: f64, row: f64, length: f64) {
        if opened(&self.shared) {
            set_selection(&self.shared, column, row, length);
        }
    }

    /// Selects the whole buffer.
    #[wasm_bindgen(js_name = selectAll)]
    pub fn select_all(&self) {
        select_all(&self.shared);
    }

    /// Selects buffer lines `start` to `end`, both included.
    #[wasm_bindgen(js_name = selectLines)]
    pub fn select_lines(&self, start: f64, end: f64) {
        let shared = &self.shared;
        if !opened(shared) {
            return;
        }
        let Some(size) = size(shared) else {
            return;
        };
        let length = f64::from(buffer::active(shared).length());
        {
            let mut s = shared.selection.borrow_mut();
            s.model.clear();
            s.model.start = Some(point(0.0, js_max(start, 0.0)));
            s.model.end = Some(point(size.cols, js_min(end, length - 1.0)));
        }
        refresh(shared, false);
        fire(shared);
    }

    /// Fires when the selection changes: mouse selections when the button is released, the API, and clears by
    /// user input, a resize, a screen switch or a reset (not when trimming moves or drops it, like xterm.js).
    #[wasm_bindgen(getter = onSelectionChange, unchecked_return_type = "IEvent<void>")]
    pub fn on_selection_change(&self) -> js_sys::Function {
        crate::events::event(&self.shared, crate::events::Event::SelectionChange)
    }
}

/// JavaScript's `Math.min`: NaN wins.
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.min(b)
    }
}
