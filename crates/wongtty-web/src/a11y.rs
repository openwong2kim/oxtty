//! `options.screenReaderMode`: the xterm.js accessibility tree (one `role="listitem"` element per row with
//! `aria-posinset`/`aria-setsize`, focus at the first or last row scrolling the view) and the `aria-live` region
//! that announces new output, refreshed at most once a second like xterm.js.
//!
//! It reads only the [`Viewport`] of each frame (PRD D7). xterm.js announces every character the parser prints; the
//! engine reports no such stream, so the new output is the text between where the cursor was at the last frame and
//! where it is now (rows soft-wrapped into the next one, by a full last cell, join without a line break). Lines
//! that scrolled out of the viewport between two frames count towards the line limit but cannot be read. `onData` stands in for `onKey`: a
//! one-character input is not announced again when the program echoes it.
//!
//! Memory: nothing while the option is off (the terminal holds `None`); on, the row elements, one closure and the
//! announcement text.
//!
//! Ported from xterm.js 6.1.0-beta.336 (commit 08829c7eb6279dda2a9e86df901353a1a89858ca, MIT):
//! - `src/browser/AccessibilityManager.ts` lines 15–273 and 372–439: the tree, the live region and its limits,
//!   boundary focus, resizing and row alignment (not the `selectionchange` mapping, lines 275–370).
//! - `src/browser/TimeBasedDebouncer.ts` lines 8–91: one refresh a second, leading and trailing.
//! - `src/browser/LocalizableStrings.ts` line 14: the "too much output" text.
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::{Rc, Weak};

use js_sys::Function;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Event, FocusEvent, HtmlElement};

use crate::emitter::Disposable;
use crate::engine::{attr, mode, Viewport};
use crate::options::option;
use crate::render::Dimensions;
use crate::terminal::Shared;
use crate::timer::{self, Timeout};

const MAX_ROWS_TO_READ: u32 = 20;
const TOO_MUCH_OUTPUT: &str = "Too much output to announce, navigate to rows manually to read";
const DEBOUNCE_MS: f64 = 1000.0;

/// Where the cursor was at the last frame, in lines since the first line the terminal ever had.
#[derive(Clone, Copy, PartialEq)]
struct Mark {
    alternate: bool,
    cols: u32,
    rows: u32,
    line: f64,
    col: u32,
}

/// `TimeBasedDebouncer`.
#[derive(Default)]
struct Debouncer {
    start: Option<u32>,
    end: Option<u32>,
    count: u32,
    last_ms: f64,
    trailing: Option<Timeout>,
}

struct Inner {
    element: HtmlElement,
    container: HtmlElement,
    tree: HtmlElement,
    live: HtmlElement,
    /// Row elements with the column after their text (`_rowColumns`' last entry, for the width alignment).
    rows: Vec<(HtmlElement, u32)>,
    /// The one listener (focus on the boundary rows, `focusout` on the element) and the same as a function.
    _closure: Closure<dyn FnMut(Event)>,
    listener: Function,
    live_lines: u32,
    consume: VecDeque<char>,
    announce: String,
    debouncer: Debouncer,
    mark: Option<Mark>,
    /// Cell height, canvas width and font size the rows were last sized for.
    sized: (f64, f64, f64),
    cell_width: f64,
    set_size: u32,
    /// Rows the boundary focus scrolled by, applied by the next frame.
    scroll: i32,
    wake: Weak<Shared>,
}

/// The accessibility tree of one opened terminal; dropping it removes its DOM and listeners.
pub(crate) struct A11y {
    inner: Rc<RefCell<Inner>>,
    /// The `onData` subscription standing in for `onKey`.
    data: Disposable,
}

impl A11y {
    fn new(shared: &Rc<Shared>, element: &HtmlElement) -> Result<Self, JsValue> {
        let document = element
            .owner_document()
            .ok_or("wongtty/xterm: the element is not in a document")?;
        let create = |class: &str| -> Result<HtmlElement, JsValue> {
            let element: HtmlElement = document.create_element("div")?.unchecked_into();
            element.class_list().add_1(class)?;
            Ok(element)
        };
        let container = create("xterm-accessibility")?;
        let tree = create("xterm-accessibility-tree")?;
        tree.set_attribute("role", "list")?;
        container.append_child(&tree)?;
        let live = create("live-region")?;
        live.set_attribute("aria-live", "assertive")?;
        container.append_child(&live)?;
        element.insert_adjacent_element("afterbegin", &container)?;

        let inner = Rc::new_cyclic(|this: &Weak<RefCell<Inner>>| {
            let weak = this.clone();
            let closure = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
                let Some(inner) = weak.upgrade() else { return };
                // Busy only while our own `focus()` dispatches into a boundary row: nothing to do then.
                let Ok(mut inner) = inner.try_borrow_mut() else {
                    return;
                };
                inner.handle(&event);
            });
            let listener: Function = closure.as_ref().unchecked_ref::<Function>().clone();
            RefCell::new(Inner {
                element: element.clone(),
                container,
                tree,
                live,
                rows: Vec::new(),
                _closure: closure,
                listener,
                live_lines: 0,
                consume: VecDeque::new(),
                announce: String::new(),
                debouncer: Debouncer::default(),
                mark: None,
                sized: (0.0, 0.0, 0.0),
                cell_width: 0.0,
                set_size: 0,
                scroll: 0,
                wake: Rc::downgrade(shared),
            })
        });
        {
            let i = inner.borrow();
            let _ = element.add_event_listener_with_callback("focusout", &i.listener);
        }
        // `onKey` → `_handleKey`, from the data a key sends (replies start with ESC and are skipped). The emitter
        // may call it after this terminal dropped the tree, so the GC owns it and it holds the tree weakly.
        let weak = Rc::downgrade(&inner);
        let on_data: Function = Closure::<dyn Fn(JsValue)>::new(move |data: JsValue| {
            let Some(inner) = weak.upgrade() else { return };
            let (Some(data), Ok(mut inner)) = (data.as_string(), inner.try_borrow_mut()) else {
                return;
            };
            if !data.starts_with('\x1b') {
                inner.handle_key(&data);
            }
        })
        .into_js_value()
        .unchecked_into();
        let data = shared.events.data.event(on_data);
        Ok(Self { inner, data })
    }

    /// After each frame: size the rows, collect new output, refresh the rows the frame drew (`range`).
    pub(crate) fn frame(
        &self,
        viewport: &Viewport,
        range: Option<(u32, u32)>,
        dimensions: &Dimensions,
        font_size: f64,
    ) {
        let Ok(mut inner) = self.inner.try_borrow_mut() else {
            return;
        };
        inner.set_size = set_size(viewport);
        inner.refresh_dimensions(viewport.rows, dimensions, font_size);
        inner.collect(viewport);
        if let Some((start, end)) = range {
            inner.refresh(viewport, Some(start), Some(end));
        }
    }

    /// Rows the boundary focus scrolled by since the last call.
    pub(crate) fn take_scroll(&self) -> i32 {
        self.inner
            .try_borrow_mut()
            .map_or(0, |mut inner| std::mem::take(&mut inner.scroll))
    }

    /// The debouncer's trailing refresh.
    fn flush(&self, viewport: &Viewport) {
        if let Ok(mut inner) = self.inner.try_borrow_mut() {
            inner.debouncer.last_ms = timer::now();
            inner.debouncer.trailing = None;
            inner.render_debounced(viewport);
        }
    }
}

impl Drop for A11y {
    fn drop(&mut self) {
        self.data.dispose();
        let Ok(mut inner) = self.inner.try_borrow_mut() else {
            return;
        };
        let _ = inner
            .element
            .remove_event_listener_with_callback("focusout", &inner.listener);
        inner.unlisten_boundaries();
        inner.debouncer.trailing = None;
        inner.container.remove();
        inner.rows.clear();
    }
}

impl Inner {
    fn handle(&mut self, event: &Event) {
        match event.type_().as_str() {
            "focus" => {
                let Some(e) = event.dyn_ref::<FocusEvent>() else {
                    return;
                };
                let target = event.target();
                let top = self
                    .rows
                    .first()
                    .is_some_and(|(row, _)| target.as_ref() == Some(row.unchecked_ref()));
                self.handle_boundary_focus(e, top);
            }
            // `onBlur` of the textarea.
            "focusout" => {
                let textarea = event
                    .target()
                    .and_then(|t| t.dyn_into::<web_sys::Element>().ok())
                    .is_some_and(|t| t.class_list().contains("xterm-helper-textarea"));
                if textarea {
                    self.clear_live_region();
                }
            }
            _ => {}
        }
    }

    fn handle_char(&mut self, c: char) {
        if self.live_lines > MAX_ROWS_TO_READ {
            return;
        }
        match self.consume.pop_front() {
            // The screen reader already read the key that typed it.
            Some(typed) if typed == c => {}
            _ => self.announce.push(c),
        }
        if c == '\n' {
            self.count_line();
        }
    }

    fn count_line(&mut self) {
        if self.live_lines > MAX_ROWS_TO_READ {
            return;
        }
        self.live_lines += 1;
        if self.live_lines == MAX_ROWS_TO_READ + 1 {
            self.live.set_text_content(Some(TOO_MUCH_OUTPUT));
        }
    }

    fn clear_live_region(&mut self) {
        self.live.set_text_content(Some(""));
        self.live_lines = 0;
    }

    fn handle_key(&mut self, key: &str) {
        self.clear_live_region();
        let mut chars = key.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            if !c.is_control() {
                self.consume.push_back(c);
            }
        }
    }

    /// The text the cursor moved over since the last frame, fed to `handle_char` as xterm.js feeds printed
    /// characters and line feeds. A jump back, a resize or a buffer switch only moves the mark.
    fn collect(&mut self, viewport: &Viewport) {
        let alternate = viewport.modes & mode::ALT_SCREEN != 0;
        let top = viewport.trimmed_lines + f64::from(ydisp(viewport));
        let now = Mark {
            alternate,
            cols: viewport.cols,
            rows: viewport.rows,
            line: top + f64::from(viewport.cursor.line.max(0) as u32),
            col: viewport.cursor.col,
        };
        let Some(was) = self.mark.replace(now) else {
            return;
        };
        let same = (was.alternate, was.cols, was.rows) == (now.alternate, now.cols, now.rows);
        if !same || (now.line, now.col) <= (was.line, was.col) {
            return;
        }
        let mut line = was.line;
        while line <= now.line && self.live_lines <= MAX_ROWS_TO_READ {
            let start = if line == was.line { was.col } else { 0 };
            let end = (line == now.line).then_some(now.col);
            let row = line - top;
            if row >= 0.0 && row < f64::from(viewport.rows) {
                let (text, _) = row_text(viewport, row as u32, start, end);
                for c in text.chars() {
                    self.handle_char(c);
                }
                if line < now.line && !full(viewport, row as u32) {
                    self.handle_char('\n');
                }
            } else if line < now.line {
                // Scrolled out before this frame: it counts towards the limit, with nothing to read.
                self.count_line();
            }
            line += 1.0;
        }
    }

    /// `TimeBasedDebouncer.refresh`.
    fn refresh(&mut self, viewport: &Viewport, start: Option<u32>, end: Option<u32>) {
        let count = viewport.rows;
        let d = &mut self.debouncer;
        d.count = count;
        let start = start.unwrap_or(0);
        let end = end.unwrap_or(count.saturating_sub(1));
        d.start = Some(d.start.map_or(start, |s| s.min(start)));
        d.end = Some(d.end.map_or(end, |e| e.max(end)));
        let now = timer::now();
        if now - d.last_ms >= DEBOUNCE_MS {
            d.trailing = None;
            d.last_ms = now;
            self.render_debounced(viewport);
        } else if d.trailing.is_none() {
            let wake = self.wake.clone();
            d.trailing = Some(Timeout::new(
                (DEBOUNCE_MS - (now - d.last_ms)) as i32,
                move || {
                    if let Some(shared) = wake.upgrade() {
                        if let Ok(core) = shared.core.try_borrow() {
                            if let Some(a11y) = &core.a11y {
                                a11y.flush(&core.viewport);
                            }
                        }
                    }
                },
            ));
        }
    }

    /// `TimeBasedDebouncer._innerRefresh`.
    fn render_debounced(&mut self, viewport: &Viewport) {
        let d = &mut self.debouncer;
        let (Some(start), Some(end)) = (d.start.take(), d.end.take()) else {
            return;
        };
        let end = end.min(d.count.saturating_sub(1));
        self.render_rows(viewport, start, end);
    }

    fn render_rows(&mut self, viewport: &Viewport, start: u32, end: u32) {
        let set_size = set_size(viewport).to_string();
        let ydisp = ydisp(viewport);
        for i in start..=end.min(viewport.rows.saturating_sub(1)) {
            let (text, mut column) = row_text(viewport, i, 0, None);
            let Some((element, last)) = self.rows.get_mut(i as usize) else {
                continue;
            };
            if text.is_empty() {
                element.set_text_content(Some("\u{a0}"));
                column = 1;
            } else {
                element.set_text_content(Some(&text));
            }
            *last = column;
            let _ = element.set_attribute("aria-posinset", &(ydisp + i + 1).to_string());
            let _ = element.set_attribute("aria-setsize", &set_size);
            align_row_width(element, column, self.cell_width);
        }
        self.announce_characters();
    }

    fn announce_characters(&mut self) {
        if self.announce.is_empty() {
            return;
        }
        let mut text = self.live.text_content().unwrap_or_default();
        if text == TOO_MUCH_OUTPUT {
            self.clear_live_region();
            text.clear();
        }
        text.push_str(&self.announce);
        self.live.set_text_content(Some(&text));
        self.announce.clear();
    }

    /// `_handleBoundaryFocus`: focus moving past the first or last row scrolls the view by one row and keeps
    /// focus on the row next to the boundary.
    fn handle_boundary_focus(&mut self, e: &FocusEvent, top: bool) {
        let n = self.rows.len();
        if n < 2 {
            return;
        }
        let boundary = if top {
            &self.rows[0].0
        } else {
            &self.rows[n - 1].0
        };
        let before = if top {
            &self.rows[1].0
        } else {
            &self.rows[n - 2].0
        };
        let last = if top {
            "1".to_string()
        } else {
            self.set_size.to_string()
        };
        if boundary.get_attribute("aria-posinset").as_deref() == Some(last.as_str()) {
            return;
        }
        if e.related_target().as_ref() != Some(before.unchecked_ref()) {
            return;
        }
        self.unlisten_boundaries();
        let Ok(element) = self.create_row() else {
            return;
        };
        if top {
            if let Some((bottom, _)) = self.rows.pop() {
                bottom.remove();
            }
            let _ = self.tree.insert_adjacent_element("afterbegin", &element);
            self.rows.insert(0, (element, 0));
        } else {
            let (old_top, _) = self.rows.remove(0);
            old_top.remove();
            let _ = self.tree.append_child(&element);
            self.rows.push((element, 0));
        }
        self.listen_boundaries();
        self.scroll += if top { -1 } else { 1 };
        if let Some(shared) = self.wake.upgrade() {
            shared.schedule_frame();
        }
        let next = if top { 1 } else { self.rows.len() - 2 };
        let _ = self.rows[next].0.focus();
        e.prevent_default();
        e.stop_immediate_propagation();
    }

    fn listen_boundaries(&self) {
        for (row, _) in [self.rows.first(), self.rows.last()].into_iter().flatten() {
            let _ = row.add_event_listener_with_callback("focus", &self.listener);
        }
    }

    fn unlisten_boundaries(&self) {
        for (row, _) in [self.rows.first(), self.rows.last()].into_iter().flatten() {
            let _ = row.remove_event_listener_with_callback("focus", &self.listener);
        }
    }

    fn create_row(&self) -> Result<HtmlElement, JsValue> {
        let document = self
            .tree
            .owner_document()
            .ok_or("wongtty/xterm: the tree is not in a document")?;
        let element: HtmlElement = document.create_element("div")?.unchecked_into();
        element.set_attribute("role", "listitem")?;
        element.set_tab_index(-1);
        element.style().set_property("height", &px(self.sized.0))?;
        Ok(element)
    }

    /// `_handleResize` and `_refreshRowsDimensions`, when the row count or the cell size changed.
    fn refresh_dimensions(&mut self, rows: u32, dimensions: &Dimensions, font_size: f64) {
        let sized = (
            dimensions.css_cell.height,
            dimensions.css_canvas.width,
            font_size,
        );
        self.cell_width = dimensions.css_cell.width;
        if self.rows.len() == rows as usize && self.sized == sized {
            return;
        }
        self.sized = sized;
        if self.rows.len() != rows as usize {
            self.unlisten_boundaries();
            while self.rows.len() < rows as usize {
                let Ok(element) = self.create_row() else {
                    break;
                };
                let _ = self.tree.append_child(&element);
                self.rows.push((element, 0));
            }
            while self.rows.len() > rows as usize {
                if let Some((element, _)) = self.rows.pop() {
                    element.remove();
                }
            }
            self.listen_boundaries();
        }
        if sized.0 <= 0.0 {
            return;
        }
        let style = self.container.style();
        let _ = style.set_property("width", &px(sized.1));
        let _ = style.set_property("font-size", &px(font_size));
        for (element, column) in &self.rows {
            let _ = element.style().set_property("height", &px(sized.0));
            align_row_width(element, *column, self.cell_width);
        }
    }
}

/// `_alignRowWidth`: scales the row so its characters line up with the cells (fonts without CJK, …).
fn align_row_width(element: &HtmlElement, last_column: u32, cell_width: f64) {
    let style = element.style();
    let _ = style.set_property("transform", "");
    if last_column == 0 {
        return;
    }
    let width = element.get_bounding_client_rect().width();
    if width <= 0.0 {
        return;
    }
    let target = f64::from(last_column) * cell_width;
    let _ = style.set_property("transform", &format!("scaleX({})", target / width));
}

/// `buffer.ydisp`: the buffer line at the top of the view.
fn ydisp(viewport: &Viewport) -> u32 {
    if viewport.modes & mode::ALT_SCREEN != 0 {
        return 0;
    }
    viewport
        .history_length
        .saturating_sub(viewport.display_offset)
}

/// `buffer.lines.length`.
fn set_size(viewport: &Viewport) -> u32 {
    if viewport.modes & mode::ALT_SCREEN != 0 {
        return viewport.rows;
    }
    viewport.history_length + viewport.rows
}

/// Whether row `y`'s last cell has text, taken as a soft wrap into the next row.
fn full(viewport: &Viewport, y: u32) -> bool {
    let cells = viewport.row(y);
    cells.len() >= 4 && !blank(&cells[cells.len() - 4..], false)
}

/// An xterm.js empty cell: erased or never written (a space the program printed is text).
fn blank(cell: &[u32], combined: bool) -> bool {
    if combined {
        return false;
    }
    let (code, attrs) = (cell[0], cell[3]);
    if attrs & (attr::WIDE_SPACER | attr::LEADING_WIDE_SPACER) != 0 || code == 0 {
        return true;
    }
    code == u32::from(' ') && attrs & attr::TYPED_SPACE == 0
}

/// `translateToString(true, start, end)` of viewport row `y` (end `None`: the trimmed length) and the column
/// after the text, like the last entry of its `outColumns`.
fn row_text(viewport: &Viewport, y: u32, start: u32, end: Option<u32>) -> (String, u32) {
    let cells = viewport.row(y);
    let cols = (cells.len() / 4) as u32;
    let extras = viewport.extras.get(&y);
    let combined = |x: u32| {
        extras.and_then(|row| {
            row.iter()
                .find(|e| e.col == x)
                .and_then(|e| e.text.as_deref())
        })
    };
    let cell = |x: u32| &cells[x as usize * 4..x as usize * 4 + 4];
    let width = |x: u32| if cell(x)[3] & attr::WIDE != 0 { 2 } else { 1 };
    let trimmed = (0..cols)
        .rev()
        .find(|&x| !blank(cell(x), combined(x).is_some()))
        .map_or(0, |x| x + width(x));
    let end = end.unwrap_or(trimmed).min(trimmed).min(cols);
    let mut x = start.min(cols);
    let mut text = String::new();
    while x < end {
        match combined(x) {
            Some(chars) => text.push_str(chars),
            None => {
                let c = cell(x);
                let ch = (c[3] & (attr::WIDE_SPACER | attr::LEADING_WIDE_SPACER) == 0)
                    .then(|| char::from_u32(c[0]))
                    .flatten()
                    .filter(|&ch| ch != '\0');
                text.push(ch.unwrap_or(' '));
            }
        }
        x += width(x);
    }
    (text, x)
}

fn px(value: f64) -> String {
    format!("{value}px")
}

impl Shared {
    /// Builds or removes the accessibility tree for `options.screenReaderMode` (after `open`, like xterm.js).
    pub(crate) fn apply_screen_reader_mode(self: &Rc<Self>) {
        let mut core = self.core.borrow_mut();
        let Some(element) = core.element.clone() else {
            return;
        };
        let on = option(&core.options, "screenReaderMode").is_truthy();
        match (core.a11y.is_some(), on) {
            (false, true) => {
                if let Ok(a11y) = A11y::new(self, &element) {
                    // `_refreshRowsDimensions` and `_refreshRows` of the constructor.
                    let font_size = option(&core.options, "fontSize").as_f64().unwrap_or(15.0);
                    let dimensions = core
                        .renderer
                        .as_ref()
                        .map(|r| r.dimensions())
                        .unwrap_or_default();
                    let rows = core.size().1;
                    if let Ok(mut inner) = a11y.inner.try_borrow_mut() {
                        inner.set_size = set_size(&core.viewport);
                        inner.refresh_dimensions(rows, &dimensions, font_size);
                        inner.refresh(&core.viewport, None, None);
                    }
                    core.a11y = Some(a11y);
                }
            }
            (true, false) => core.a11y = None,
            _ => {}
        }
    }
}
