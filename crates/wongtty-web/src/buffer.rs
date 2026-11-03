//! xterm.js `term.buffer`, read straight from the engine (PRD D7 lets the buffer API do that). Lines are numbered
//! like xterm.js: 0 is the oldest scrollback line, `baseY` the top of the screen.
//!
//! Memory: nothing is cached on the terminal. The namespace, its buffers, lines and cells are small objects made
//! when read (so `buffer.active !== buffer.normal`, unlike xterm.js); `getLine` copies one row, a code point and
//! the attributes per cell, never the buffer. The engine keeps only the active screen readable: the other buffer
//! reads as empty (xterm.js keeps the normal buffer readable under the alternate one).
//!
//! In viewport-only mode ([`crate::remote`]) lines `0..H` are the host's: `getLine` returns the fetched row, or a
//! blank line of `cols` cells while it is not fetched (and asks for it), never `undefined` inside the buffer.
use std::rc::Rc;

use js_sys::{Array, Object};
use wasm_bindgen::prelude::*;

use crate::engine::{attr, get, mode, LocalEngineView};
use crate::events::{self, Event};
use crate::terminal::{Shared, Terminal};

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
export type IBufferNamespace = BufferNamespace;
export type IBuffer = BufferView;
export type IBufferLine = BufferLine;
export type IBufferCell = BufferCell;
"#;

/// xterm.js `IBufferNamespace`.
#[wasm_bindgen]
pub struct BufferNamespace {
    shared: Rc<Shared>,
}

#[wasm_bindgen]
impl BufferNamespace {
    /// The buffer on screen now; it keeps its type when the program switches screens.
    #[wasm_bindgen(getter)]
    pub fn active(&self) -> BufferView {
        active(&self.shared)
    }

    #[wasm_bindgen(getter)]
    pub fn normal(&self) -> BufferView {
        BufferView {
            shared: self.shared.clone(),
            alternate: false,
        }
    }

    #[wasm_bindgen(getter)]
    pub fn alternate(&self) -> BufferView {
        BufferView {
            shared: self.shared.clone(),
            alternate: true,
        }
    }

    /// Fires with the new active buffer when the program enters or leaves the alternate screen.
    #[wasm_bindgen(getter = onBufferChange, unchecked_return_type = "IEvent<IBuffer>")]
    pub fn on_buffer_change(&self) -> js_sys::Function {
        events::event(&self.shared, Event::BufferChange)
    }
}

pub(crate) fn active(shared: &Rc<Shared>) -> BufferView {
    let alternate = shared.core.try_borrow().is_ok_and(|core| {
        !core.disposed && core.view.engine().modes().unwrap_or(0) & mode::ALT_SCREEN != 0
    });
    BufferView {
        shared: shared.clone(),
        alternate,
    }
}

/// xterm.js `IBuffer`, one of the two screens.
#[wasm_bindgen]
pub struct BufferView {
    shared: Rc<Shared>,
    alternate: bool,
}

impl BufferView {
    /// `f` over the engine while this buffer is the one on screen; `None` otherwise or after dispose. `f` runs
    /// under the core borrow, so it may call the engine but nothing else.
    fn read<T>(&self, f: impl FnOnce(&LocalEngineView) -> T) -> Option<T> {
        // A resize waiting for the frame reflows first: the buffer has the rows `term.rows` reports.
        self.shared.apply_pending_resize_logged();
        let core = self.shared.core.try_borrow().ok()?;
        if core.disposed {
            return None;
        }
        let modes = core.view.engine().modes().ok()?;
        ((modes & mode::ALT_SCREEN != 0) == self.alternate).then(|| f(&core.view))
    }
}

#[wasm_bindgen]
impl BufferView {
    #[wasm_bindgen(getter = type, unchecked_return_type = "'normal' | 'alternate'")]
    pub fn kind(&self) -> String {
        if self.alternate {
            "alternate"
        } else {
            "normal"
        }
        .into()
    }

    /// Cursor column. At the right edge it stays on the last column where xterm.js reports `cols` (the engine
    /// keeps the pending wrap to itself).
    #[wasm_bindgen(getter = cursorX)]
    pub fn cursor_x(&self) -> u32 {
        self.read(|view| view.cursor().col).unwrap_or(0)
    }

    /// Cursor row on the screen (0 is the top screen row, whatever the scroll position).
    #[wasm_bindgen(getter = cursorY)]
    pub fn cursor_y(&self) -> u32 {
        self.read(|view| view.cursor().line.max(0) as u32)
            .unwrap_or(0)
    }

    /// Line shown at the top of the viewport.
    #[wasm_bindgen(getter = viewportY)]
    pub fn viewport_y(&self) -> u32 {
        self.read(|view| view.history_length().saturating_sub(view.display_offset()))
            .unwrap_or(0)
    }

    /// Line at the top of the screen: the scrollback length (the host's lines included in viewport-only mode).
    #[wasm_bindgen(getter = baseY)]
    pub fn base_y(&self) -> u32 {
        self.read(|view| view.history_length()).unwrap_or(0)
    }

    /// Scrollback plus screen lines.
    #[wasm_bindgen(getter)]
    pub fn length(&self) -> u32 {
        self.read(|view| view.history_length().saturating_add(view.engine().rows()))
            .unwrap_or(0)
    }

    /// A copy of line `y` taken now (xterm.js hands out a live view), or `undefined` outside `0..length`.
    #[wasm_bindgen(js_name = getLine)]
    pub fn get_line(&self, y: f64) -> Option<BufferLine> {
        let line = self.read(|view| read_line(view, y)).flatten();
        // A host line not fetched yet was asked for: the fetch calls the host, so it waits for the next task (the
        // caller may hold borrows of its own).
        crate::remote::fetch_soon(&self.shared);
        line
    }

    /// OSC 8 hyperlinks of line `y` as `(column, id, uri)`; empty outside the buffer. For links that reach past the
    /// viewport (rows in view come from the last frame).
    pub(crate) fn hyperlinks(&self, y: f64) -> Vec<(u32, String, String)> {
        self.read(|view| {
            let e = view.engine();
            let history = f64::from(view.history_length());
            if !(y >= 0.0 && y.fract() == 0.0 && y < history + f64::from(e.rows())) {
                return Vec::new();
            }
            if let Some(remote) = view.remote().filter(|_| y < f64::from(view.remote_lines())) {
                return remote.row(y as u32).map_or_else(Vec::new, |row| {
                    row.extras
                        .iter()
                        .filter_map(|extra| {
                            let link = extra.hyperlink.as_ref()?;
                            Some((extra.col, link.id.clone(), link.uri.clone()))
                        })
                        .collect()
                });
            }
            let extras = e.row_extras((y - history) as i32).unwrap_or(JsValue::NULL);
            if !Array::is_array(&extras) {
                return Vec::new();
            }
            Array::from(&extras)
                .iter()
                .filter_map(|extra| {
                    let link = get(&extra, "hyperlink");
                    link.is_object().then(|| {
                        (
                            get(&extra, "col").as_f64().unwrap_or(-1.0) as u32,
                            get(&link, "id").as_string().unwrap_or_default(),
                            get(&link, "uri").as_string().unwrap_or_default(),
                        )
                    })
                })
                .collect()
        })
        .unwrap_or_default()
    }

    /// An empty cell: no text, width 1.
    #[wasm_bindgen(js_name = getNullCell)]
    pub fn get_null_cell(&self) -> BufferCell {
        BufferCell {
            code: 0,
            attrs: 0,
            text: None,
        }
    }
}

fn read_line(view: &LocalEngineView, y: f64) -> Option<BufferLine> {
    let e = view.engine();
    let history = view.history_length();
    if !(y >= 0.0 && y.fract() == 0.0 && y < f64::from(history) + f64::from(e.rows())) {
        return None;
    }
    let remote_lines = view.remote_lines();
    if let Some(remote) = view.remote().filter(|_| y < f64::from(remote_lines)) {
        return Some(remote_line(remote, y as u32, e.cols()));
    }
    let line = (y - f64::from(history)) as i32;
    // A view into engine memory: copy it before the next engine call.
    let row = e.row_cells_view(line).ok()??;
    let mut cells = vec![0; row.length() as usize];
    row.copy_to(&mut cells);
    let count = cells.len() / 4;
    for x in 0..count {
        cells[2 * x] = cells[4 * x];
        cells[2 * x + 1] = cells[4 * x + 3];
    }
    cells.truncate(2 * count);
    cells.shrink_to_fit();
    let mut texts = Vec::new();
    if cells
        .chunks_exact(2)
        .any(|cell| cell[1] & attr::COMBINING != 0)
    {
        let extras = e.row_extras(line).unwrap_or(JsValue::NULL);
        if Array::is_array(&extras) {
            for extra in Array::from(&extras).iter() {
                if let (Some(col), Some(text)) =
                    (get(&extra, "col").as_f64(), get(&extra, "text").as_string())
                {
                    texts.push((col as u32, text));
                }
            }
        }
    }
    // Engine rows carry "wraps into the next row"; xterm.js lines carry "continues the previous one".
    let local = history - remote_lines;
    let wrapped = if line > -(local as i32) {
        e.row_wrapped(line - 1).unwrap_or(false)
    } else {
        // The engine's oldest line continues the host's newest one, if that is fetched.
        remote_lines > 0
            && view
                .remote()
                .and_then(|r| r.row(remote_lines - 1))
                .is_some_and(|row| row.wrapped)
    };
    Some(BufferLine {
        cells,
        texts,
        wrapped,
    })
}

/// Host line `y` in viewport-only mode: the fetched row, or a blank line of `cols` cells (asked for) until then.
fn remote_line(remote: &crate::remote::Remote, y: u32, cols: u32) -> BufferLine {
    let wrapped = y > 0 && remote.row(y - 1).is_some_and(|row| row.wrapped);
    let Some(row) = remote.row(y) else {
        remote.want(y, y + 1);
        return BufferLine {
            cells: vec![0; 2 * cols as usize],
            texts: Vec::new(),
            wrapped,
        };
    };
    let mut cells = vec![0; cols as usize * 4];
    row.copy_cells(&mut cells);
    let count = cells.len() / 4;
    for x in 0..count {
        cells[2 * x] = cells[4 * x];
        cells[2 * x + 1] = cells[4 * x + 3];
    }
    cells.truncate(2 * count);
    let texts = row
        .extras
        .iter()
        .filter_map(|extra| Some((extra.col, extra.text.clone()?)))
        .collect();
    BufferLine {
        cells,
        texts,
        wrapped,
    }
}

/// xterm.js `IBufferLine`: a copy of one row.
#[wasm_bindgen]
pub struct BufferLine {
    /// `[code point, attributes]` per cell.
    cells: Vec<u32>,
    /// Full text of the cells with combining marks, by column.
    texts: Vec<(u32, String)>,
    wrapped: bool,
}

impl BufferLine {
    fn len(&self) -> usize {
        self.cells.len() / 2
    }

    fn cell(&self, x: usize) -> BufferCell {
        BufferCell {
            code: self.cells[2 * x],
            attrs: self.cells[2 * x + 1],
            text: self
                .texts
                .iter()
                .find(|(col, _)| *col as usize == x)
                .map(|(_, text)| text.clone()),
        }
    }

    /// Columns up to the last one with text, like xterm.js `getTrimmedLength`.
    fn trimmed_len(&self) -> usize {
        (0..self.len())
            .rev()
            .find(|&x| !self.cell(x).is_blank())
            .map_or(0, |x| x + self.cell(x).width() as usize)
    }
}

#[wasm_bindgen]
impl BufferLine {
    /// Whether this line continues the previous one (soft wrap).
    #[wasm_bindgen(getter = isWrapped)]
    pub fn is_wrapped(&self) -> bool {
        self.wrapped
    }

    #[wasm_bindgen(getter)]
    pub fn length(&self) -> u32 {
        self.len() as u32
    }

    /// Cell `x`, or `undefined` outside the line. `cell` is accepted for xterm.js compatibility and not reused:
    /// use the returned cell.
    #[wasm_bindgen(js_name = getCell)]
    pub fn get_cell(
        &self,
        x: f64,
        #[wasm_bindgen(js_name = cell, unchecked_optional_param_type = "IBufferCell")]
        _cell: Option<Object>,
    ) -> Option<BufferCell> {
        (x >= 0.0 && x.fract() == 0.0 && x < self.len() as f64).then(|| self.cell(x as usize))
    }

    /// Like xterm.js: erased cells are spaces, `trimRight` drops trailing erased cells (not typed spaces), a wide
    /// character covers two columns, and a range that starts on its right half gets a space there. Unlike
    /// xterm.js, columns outside the line are clamped (xterm.js pads them).
    #[wasm_bindgen(js_name = translateToString)]
    pub fn translate_to_string(
        &self,
        #[wasm_bindgen(js_name = trimRight)] trim_right: Option<bool>,
        #[wasm_bindgen(js_name = startColumn)] start_column: Option<f64>,
        #[wasm_bindgen(js_name = endColumn)] end_column: Option<f64>,
    ) -> String {
        let len = self.len() as f64;
        let start = start_column.unwrap_or(0.0);
        let mut end = end_column.unwrap_or(len);
        if start.is_nan() || end.is_nan() {
            return String::new();
        }
        if trim_right == Some(true) {
            end = end.min(self.trimmed_len() as f64);
        }
        let (mut x, end) = (start.clamp(0.0, len) as usize, end.clamp(0.0, len) as usize);
        let mut text = String::new();
        while x < end {
            let cell = self.cell(x);
            match cell.text {
                Some(ref chars) => text.push_str(chars),
                None => text.push(cell.char().unwrap_or(' ')),
            }
            x += cell.width().max(1) as usize;
        }
        text
    }
}

/// xterm.js `IBufferCell`.
#[wasm_bindgen]
pub struct BufferCell {
    code: u32,
    attrs: u32,
    /// Full text when the cell has combining marks.
    text: Option<String>,
}

impl BufferCell {
    /// The cell's character; `None` for the right half of a wide character and blanks that are not text.
    fn char(&self) -> Option<char> {
        if self.attrs & (attr::WIDE_SPACER | attr::LEADING_WIDE_SPACER) != 0 {
            return None;
        }
        char::from_u32(self.code).filter(|&c| c != '\0')
    }

    /// Erased or never written: an xterm.js empty cell. A space the program printed is text.
    fn is_blank(&self) -> bool {
        self.text.is_none()
            && match self.char() {
                None => true,
                Some(' ') => self.attrs & attr::TYPED_SPACE == 0,
                Some(_) => false,
            }
    }
}

#[wasm_bindgen]
impl BufferCell {
    /// The cell text; `''` for an erased cell and the right half of a wide character, like xterm.js.
    #[wasm_bindgen(js_name = getChars)]
    pub fn get_chars(&self) -> String {
        match &self.text {
            Some(text) => text.clone(),
            None if self.is_blank() => String::new(),
            None => self.char().map(String::from).unwrap_or_default(),
        }
    }

    /// The code point (0 for an erased cell), or the last UTF-16 unit of a cell with combining marks, like xterm.js.
    #[wasm_bindgen(js_name = getCode)]
    pub fn get_code(&self) -> u32 {
        match &self.text {
            Some(text) => text.encode_utf16().last().map_or(0, u32::from),
            None if self.is_blank() => 0,
            None => self.char().map_or(0, u32::from),
        }
    }

    /// 2 for a wide character, 0 for its right half, else 1.
    #[wasm_bindgen(js_name = getWidth)]
    pub fn width(&self) -> u32 {
        if self.attrs & attr::WIDE != 0 {
            2
        } else if self.attrs & attr::WIDE_SPACER != 0 {
            0
        } else {
            1
        }
    }

    #[wasm_bindgen(js_name = isBold)]
    pub fn is_bold(&self) -> u32 {
        self.attrs & attr::BOLD
    }

    #[wasm_bindgen(js_name = isDim)]
    pub fn is_dim(&self) -> u32 {
        self.attrs & attr::DIM
    }

    #[wasm_bindgen(js_name = isItalic)]
    pub fn is_italic(&self) -> u32 {
        self.attrs & attr::ITALIC
    }

    /// Any underline style.
    #[wasm_bindgen(js_name = isUnderline)]
    pub fn is_underline(&self) -> u32 {
        self.attrs
            & (attr::UNDERLINE
                | attr::DOUBLE_UNDERLINE
                | attr::CURLY_UNDERLINE
                | attr::DOTTED_UNDERLINE
                | attr::DASHED_UNDERLINE)
    }

    #[wasm_bindgen(js_name = isBlink)]
    pub fn is_blink(&self) -> u32 {
        self.attrs & attr::BLINK
    }

    #[wasm_bindgen(js_name = isInverse)]
    pub fn is_inverse(&self) -> u32 {
        self.attrs & attr::INVERSE
    }

    #[wasm_bindgen(js_name = isInvisible)]
    pub fn is_invisible(&self) -> u32 {
        self.attrs & attr::HIDDEN
    }

    #[wasm_bindgen(js_name = isStrikethrough)]
    pub fn is_strikethrough(&self) -> u32 {
        self.attrs & attr::STRIKEOUT
    }
}

#[wasm_bindgen]
impl Terminal {
    /// xterm.js `term.buffer`; a new namespace object per read.
    #[wasm_bindgen(getter)]
    pub fn buffer(&self) -> BufferNamespace {
        BufferNamespace {
            shared: self.shared.clone(),
        }
    }
}
