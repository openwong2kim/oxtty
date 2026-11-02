//! The one boundary between the engine and everything drawn or typed on the page (PRD D7).
//!
//! The engine is the `oxtty` package's own WebAssembly instance, reached through its JS binding
//! (`packages/oxtty/src/index.js`): a crash there rebuilds only that instance and this terminal starts over
//! (`onCrash`), while the front end keeps running. The renderer, input and selection read only [`Events`],
//! [`Frame`] and [`Viewport`], never [`Engine`], so the engine can later move to a Web Worker and these become
//! messages. Only the buffer API (`buffer.active.getLine()`), search and serialize call [`Engine`] directly; a
//! Worker build needs a main-thread mirror for those.
//!
//! In viewport-only mode ([`crate::remote`]) the history above the engine's own lines lives in the host:
//! [`LocalEngineView`] counts it in `history_length`, `display_offset` and `trimmed_lines`, scrolls over it and fills
//! the rows it shows from the fetched rows, so frames and the viewport read as if the engine held everything.
use std::collections::HashMap;

use js_sys::{Array, Object, Reflect, Uint32Array, Uint8Array};
use wasm_bindgen::prelude::*;

use crate::remote::Remote;

#[wasm_bindgen(raw_module = "./index.js")]
extern "C" {
    /// `Terminal` of the engine binding. Every method that runs engine code is `catch`: a crash throws once from
    /// the call that hit it, and a JS exception must never unwind through Rust frames holding borrows.
    #[wasm_bindgen(js_name = Terminal)]
    pub type Engine;

    #[wasm_bindgen(constructor, js_class = "Terminal", catch)]
    pub fn new(options: &Object) -> Result<Engine, JsValue>;
    /// `data` is a string or a `Uint8Array`, passed through without copying it into this instance.
    #[wasm_bindgen(method, catch)]
    pub fn write(this: &Engine, data: &JsValue) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch)]
    pub fn resize(this: &Engine, cols: u32, rows: u32) -> Result<(), JsValue>;
    #[wasm_bindgen(method, getter)]
    pub fn cols(this: &Engine) -> u32;
    #[wasm_bindgen(method, getter)]
    pub fn rows(this: &Engine) -> u32;
    #[wasm_bindgen(method, getter, js_name = historyLength)]
    pub fn history_length(this: &Engine) -> u32;
    #[wasm_bindgen(method, getter, js_name = trimmedLines)]
    pub fn trimmed_lines(this: &Engine) -> f64;
    #[wasm_bindgen(method, getter, js_name = displayOffset)]
    pub fn display_offset(this: &Engine) -> u32;
    #[wasm_bindgen(method, catch, js_name = rowText)]
    pub fn row_text(this: &Engine, line: i32) -> Result<Option<String>, JsValue>;
    /// A view into the engine's memory, valid until the next call into any engine terminal: copy it at once.
    #[wasm_bindgen(method, catch, js_name = rowCellsView)]
    pub fn row_cells_view(this: &Engine, line: i32) -> Result<Option<Uint32Array>, JsValue>;
    #[wasm_bindgen(method, catch, js_name = rowWrapped)]
    pub fn row_wrapped(this: &Engine, line: i32) -> Result<bool, JsValue>;
    #[wasm_bindgen(method, catch, js_name = cellText)]
    pub fn cell_text(this: &Engine, line: i32, col: u32) -> Result<Option<String>, JsValue>;
    #[wasm_bindgen(method, catch, js_name = rowExtras)]
    pub fn row_extras(this: &Engine, line: i32) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(method, catch, js_name = cursor)]
    fn cursor_js(this: &Engine) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(method, catch, js_name = scrollDisplay)]
    pub fn scroll_display(this: &Engine, delta: i32) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch, js_name = scrollToBottom)]
    pub fn scroll_to_bottom(this: &Engine) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch)]
    pub fn modes(this: &Engine) -> Result<u32, JsValue>;
    #[wasm_bindgen(method, catch, js_name = setKittyKeyboard)]
    pub fn set_kitty_keyboard(this: &Engine, enabled: bool) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch, js_name = resetKittyKeyboard)]
    pub fn reset_kitty_keyboard(this: &Engine) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch, js_name = kittyFlags)]
    pub fn kitty_flags(this: &Engine) -> Result<u32, JsValue>;
    /// A synchronized update (`CSI ? 2026 h`) is open: its output is applied, frames wait until it ends.
    #[wasm_bindgen(method, catch, js_name = syncPending)]
    pub fn sync_pending(this: &Engine) -> Result<bool, JsValue>;
    #[wasm_bindgen(method, catch, js_name = takeDamage)]
    fn take_damage_js(this: &Engine) -> Result<JsValue, JsValue>;
    /// An array of `Uint8Array`, one per reply.
    #[wasm_bindgen(method, catch, js_name = takeReplyList)]
    pub fn take_reply_list(this: &Engine) -> Result<Array, JsValue>;
    #[wasm_bindgen(method, catch, js_name = takeTitle)]
    pub fn take_title(this: &Engine) -> Result<Option<String>, JsValue>;
    #[wasm_bindgen(method, catch, js_name = takeBells)]
    pub fn take_bells(this: &Engine) -> Result<u32, JsValue>;
    #[wasm_bindgen(method, catch, js_name = takeResets)]
    pub fn take_resets(this: &Engine) -> Result<u32, JsValue>;
    /// `{ read?: boolean, write?: boolean }`.
    #[wasm_bindgen(method, catch, js_name = setClipboardAccess)]
    pub fn set_clipboard_access(this: &Engine, access: &Object) -> Result<(), JsValue>;
    #[wasm_bindgen(method, catch, js_name = takeClipboardWrite)]
    fn take_clipboard_write_js(this: &Engine) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(method, catch, js_name = takeClipboardRead)]
    pub fn take_clipboard_read(this: &Engine) -> Result<Option<String>, JsValue>;
    #[wasm_bindgen(method, catch, js_name = replyClipboard)]
    pub fn reply_clipboard(this: &Engine, selection: &str, text: &str) -> Result<bool, JsValue>;
    /// 259 colours (0xRRGGBB): palette 0–255, foreground, background, cursor; `null` stops answering OSC 4/10/11/12.
    #[wasm_bindgen(method, catch, js_name = setColors)]
    pub fn set_colors(this: &Engine, colors: &JsValue) -> Result<(), JsValue>;
    /// `{ backend?: 'conpty' | 'winpty', buildNumber?: number }` or `null`: xterm.js 6.1's Windows PTY rules.
    #[wasm_bindgen(method, catch, js_name = setWindowsPty)]
    pub fn set_windows_pty(this: &Engine, pty: &JsValue) -> Result<(), JsValue>;
    /// `'block' | 'underline' | 'bar'` and whether it blinks: the `cursorStyle` and `cursorBlink` options.
    #[wasm_bindgen(method, catch, js_name = setCursorDefault)]
    pub fn set_cursor_default(this: &Engine, shape: &JsValue, blink: bool) -> Result<(), JsValue>;
    /// `options` is `{ scrollback?: number, excludeModes?: boolean }` or `undefined`.
    #[wasm_bindgen(method, catch, js_name = ansiSnapshot)]
    pub fn ansi_snapshot(this: &Engine, options: &JsValue) -> Result<JsValue, JsValue>;
    /// `{ first, rows, text, cells }`: the logical line row `line` is on, as search reads it; `null` outside the
    /// buffer. `cells` maps each UTF-16 unit of `text` to its cell (`row * cols + col` from `first`, bit 31 wide).
    #[wasm_bindgen(method, catch, js_name = searchLine)]
    pub fn search_line(this: &Engine, line: i32) -> Result<JsValue, JsValue>;
    /// Marker on buffer line `line` of the primary screen (0 the oldest history line); 0 when refused.
    #[wasm_bindgen(method, catch, js_name = addMarker)]
    pub fn add_marker(this: &Engine, line: i32) -> Result<u32, JsValue>;
    /// The marker's buffer line, -1 once it is gone.
    #[wasm_bindgen(method, catch, js_name = markerLine)]
    pub fn marker_line(this: &Engine, id: u32) -> Result<i32, JsValue>;
    #[wasm_bindgen(method, catch, js_name = removeMarker)]
    pub fn remove_marker(this: &Engine, id: u32) -> Result<(), JsValue>;
    /// An array of the ids the engine dropped since the last call.
    #[wasm_bindgen(method, catch, js_name = takeDisposedMarkers)]
    pub fn take_disposed_markers(this: &Engine) -> Result<JsValue, JsValue>;
    /// `{ tiles, covered }`: covers `cols` × `rows` cells with the tiles of image `id` (`options`: `{ scrolling,
    /// cursor: 'vt340' | 'iip' }`).
    #[wasm_bindgen(method, catch, js_name = placeImage)]
    pub fn place_image(
        this: &Engine,
        id: u32,
        cols: u32,
        rows: u32,
        options: &Object,
    ) -> Result<JsValue, JsValue>;
    /// `[col, id, tile]` per cell of the row an image tile covers; `null` outside the buffer.
    #[wasm_bindgen(method, catch, js_name = rowImages)]
    pub fn row_images(this: &Engine, line: i32) -> Result<Option<Uint32Array>, JsValue>;
    /// `[fg, bg, attrs]` text written now gets.
    #[wasm_bindgen(method, catch)]
    pub fn pen(this: &Engine) -> Result<Uint32Array, JsValue>;
    #[wasm_bindgen(method)]
    pub fn dispose(this: &Engine);
    /// Stops parsing at sequences of `kind` (`'csi'`, `'osc'` or `'dcs'`) with `id`, or no longer does.
    #[wasm_bindgen(method, catch, js_name = setHook)]
    pub fn set_hook(this: &Engine, kind: &str, id: u32, on: bool) -> Result<(), JsValue>;
    /// Like `write`, but false when it stopped right after a hooked sequence: `pendingHook`, `finishHook`, `resume`.
    #[wasm_bindgen(method, catch, js_name = writeUntilHook)]
    pub fn write_until_hook(this: &Engine, data: &JsValue) -> Result<bool, JsValue>;
    /// `{ kind, id, params: number[][], data: Uint8Array }` or `null`.
    #[wasm_bindgen(method, catch, js_name = pendingHook)]
    pub fn pending_hook(this: &Engine) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(method, catch, js_name = finishHook)]
    pub fn finish_hook(this: &Engine, run_default: bool) -> Result<(), JsValue>;
    /// Parses on after `finishHook`; false when it stopped at the next hook.
    #[wasm_bindgen(method, catch)]
    pub fn resume(this: &Engine) -> Result<bool, JsValue>;
}

/// `Mode` bits of the engine binding (`modes()`).
pub mod mode {
    pub const SHOW_CURSOR: u32 = 1;
    pub const APP_CURSOR: u32 = 1 << 1;
    pub const APP_KEYPAD: u32 = 1 << 2;
    pub const BRACKETED_PASTE: u32 = 1 << 3;
    pub const MOUSE_CLICK: u32 = 1 << 4;
    pub const MOUSE_DRAG: u32 = 1 << 5;
    pub const MOUSE_MOTION: u32 = 1 << 6;
    pub const SGR_MOUSE: u32 = 1 << 7;
    pub const FOCUS_EVENTS: u32 = 1 << 8;
    pub const ALT_SCREEN: u32 = 1 << 9;
    pub const ALTERNATE_SCROLL: u32 = 1 << 10;
    pub const LINE_WRAP: u32 = 1 << 11;
    pub const INSERT: u32 = 1 << 12;
    pub const ORIGIN: u32 = 1 << 13;
    pub const KITTY_KEYBOARD: u32 = 1 << 14;
    pub const MOUSE_X10: u32 = 1 << 15;
    pub const SGR_PIXELS_MOUSE: u32 = 1 << 16;
}

/// Packed colours of a cell's second and third words (the engine's `color` module).
pub mod color {
    pub const DEFAULT_FG: u32 = (1 << 24) | 256;
    pub const DEFAULT_BG: u32 = (1 << 24) | 257;
}

/// `Attr` bits of a cell's fourth word.
pub mod attr {
    pub const BOLD: u32 = 1;
    pub const DIM: u32 = 1 << 1;
    pub const ITALIC: u32 = 1 << 2;
    pub const UNDERLINE: u32 = 1 << 3;
    pub const DOUBLE_UNDERLINE: u32 = 1 << 4;
    pub const CURLY_UNDERLINE: u32 = 1 << 5;
    pub const DOTTED_UNDERLINE: u32 = 1 << 6;
    pub const DASHED_UNDERLINE: u32 = 1 << 7;
    pub const INVERSE: u32 = 1 << 8;
    pub const HIDDEN: u32 = 1 << 9;
    pub const STRIKEOUT: u32 = 1 << 10;
    pub const BLINK: u32 = 1 << 11;
    pub const WIDE: u32 = 1 << 12;
    pub const WIDE_SPACER: u32 = 1 << 13;
    pub const COMBINING: u32 = 1 << 14;
    pub const UNDERLINE_COLOR: u32 = 1 << 15;
    pub const HYPERLINK: u32 = 1 << 16;
    pub const LEADING_WIDE_SPACER: u32 = 1 << 17;
    /// A space the program printed; other blanks were erased or never written (xterm.js empty cells).
    pub const TYPED_SPACE: u32 = 1 << 18;
    /// An image tile covers the cell (`Engine::row_images`).
    pub const IMAGE: u32 = 1 << 19;
    pub const EXTRAS: u32 = COMBINING | UNDERLINE_COLOR | HYPERLINK;
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Cursor {
    /// Viewport row in a [`Frame`]; engine screen line from [`Engine::cursor`].
    pub line: i32,
    pub col: u32,
    pub visible: bool,
    pub shape: CursorShape,
    pub blink: bool,
    /// The program chose `shape` and `blink` (DECSCUSR); otherwise the host's own cursor style applies.
    pub styled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hyperlink {
    pub id: String,
    pub uri: String,
}

/// What a cell carries beyond its four words.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CellExtra {
    pub col: u32,
    /// Full cell text when it has combining marks.
    pub text: Option<String>,
    /// SGR 58, packed like the cell colours.
    pub underline_color: Option<u32>,
    pub hyperlink: Option<Hyperlink>,
    /// The image id and tile (`row * cols + col` in the image) drawn over the cell.
    pub image: Option<(u32, u32)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipboardWrite {
    /// `'c'` or `'p'`.
    pub selection: char,
    pub text: String,
}

/// What the program asked of the host since the last call, and the modes it left. Taken after every write drain,
/// not per frame, so replies flow while the page is hidden and input encodes with the modes at once.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Events {
    /// Bytes for the PTY (status reports, OSC 52 and colour query answers), one entry per reply: one `onData` each.
    pub replies: Vec<Vec<u8>>,
    /// New title, or `None` if unchanged: `onTitleChange`.
    pub title: Option<String>,
    pub bells: u32,
    /// Full resets (RIS): drop host state such as the selection.
    pub resets: u32,
    pub clipboard_writes: Vec<ClipboardWrite>,
    /// `'c'` or `'p'` per read, in order.
    pub clipboard_reads: Vec<char>,
    /// Bitwise OR of [`mode`] bits now. Input encoding reads them from here, as xterm.js encodes a key typed right
    /// after the write that changed them.
    pub modes: u32,
    /// Active kitty keyboard flags now, read by key encoding like `modes`.
    pub kitty_flags: u32,
}

/// Everything a renderer needs to bring its picture up to date.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub cols: u32,
    pub rows: u32,
    /// The picture must be rebuilt (resize, view scroll, engine crash): every row is in `lines`.
    pub full: bool,
    /// Viewport rows whose cells changed, ascending. Row `lines[k]` is `cells[k * cols * 4..]`.
    pub lines: Vec<u32>,
    /// `[codepoint, fg, bg, attrs]` per cell for `lines`.
    pub cells: Vec<u32>,
    /// Extras of the rows in `lines` that have any, by viewport row.
    pub extras: HashMap<u32, Vec<CellExtra>>,
    /// Cursor in viewport rows; `visible` is false while it is scrolled out of view.
    pub cursor: Cursor,
    pub display_offset: u32,
    pub history_length: u32,
    /// Lines dropped off the top of the scrollback so far: the selection keeps its text in place.
    pub trimmed_lines: f64,
    /// Bitwise OR of [`mode`] bits (input encoding takes them from [`Events`]).
    pub modes: u32,
    /// Active kitty keyboard flags.
    pub kitty_flags: u32,
}

pub trait EngineView {
    fn take_events(&mut self) -> Events;
    /// `None` while a synchronized update holds the picture, as xterm.js's `RenderService` holds its renders: the
    /// engine has applied the output (the buffer API reads it), the frame comes once the update ends.
    fn take_frame(&mut self) -> Option<Frame>;
}

/// [`EngineView`] over an engine on the same thread.
pub struct LocalEngineView {
    engine: Engine,
    full: bool,
    /// Viewport-only mode: the host's history above the engine's own (boxed: most terminals have none).
    remote: Option<Box<Remote>>,
}

impl LocalEngineView {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            full: true,
            remote: None,
        }
    }

    /// Viewport-only mode, while a history provider is set.
    pub(crate) fn remote(&self) -> Option<&Remote> {
        self.remote.as_deref()
    }

    pub(crate) fn remote_mut(&mut self) -> Option<&mut Remote> {
        self.remote.as_deref_mut()
    }

    /// Sets (or with `None` drops) the history provider; the view stays where it is in the engine's own lines.
    pub(crate) fn set_remote(&mut self, remote: Option<Remote>) {
        self.remote = remote.map(Box::new);
        if let Some(r) = self.remote.as_mut() {
            r.attach(&self.engine);
        } else {
            self.full = true;
        }
    }

    /// Host lines above the engine's own history (0 without a provider and on the alternate screen).
    pub fn remote_lines(&self) -> u32 {
        self.remote().map_or(0, Remote::visible_lines)
    }

    /// History lines, the host's included: `buffer.active.baseY`.
    pub fn history_length(&self) -> u32 {
        self.engine.history_length() + self.remote_lines()
    }

    /// Rows the view is scrolled back, into the host's lines too.
    pub fn display_offset(&self) -> u32 {
        self.engine.display_offset() + self.remote.as_ref().map_or(0, |r| r.extra.get())
    }

    /// Lines that left the top of the whole history so far (the host's own trimming in viewport-only mode).
    pub fn trimmed_lines(&self) -> f64 {
        match &self.remote {
            Some(r) => r.trimmed,
            None => self.engine.trimmed_lines(),
        }
    }

    /// Scrolls the view; positive `delta` goes back into history, past the engine's own into the host's.
    pub fn scroll_display(&self, delta: i32) -> Result<(), JsValue> {
        let Some(r) = self.remote().filter(|r| r.visible_lines() > 0) else {
            return self.engine.scroll_display(delta);
        };
        let (offset, local) = (self.engine.display_offset(), self.engine.history_length());
        let total = i64::from(local) + i64::from(r.lines);
        let wanted =
            (i64::from(offset) + i64::from(r.extra.get()) + i64::from(delta)).clamp(0, total);
        let engine = wanted.min(i64::from(local));
        self.engine
            .scroll_display((engine - i64::from(offset)) as i32)?;
        r.extra.set((wanted - engine) as u32);
        r.seen_offset.set(self.engine.display_offset());
        Ok(())
    }

    /// Back to the newest output.
    pub fn scroll_to_bottom(&self) -> Result<(), JsValue> {
        if let Some(r) = &self.remote {
            r.extra.set(0);
        }
        self.engine.scroll_to_bottom()?;
        if let Some(r) = &self.remote {
            r.seen_offset.set(0);
        }
        Ok(())
    }

    /// Follows the engine after it changed (output, a reset): counts the lines that went to the host and keeps a
    /// scrolled-back view on its text. `reset` is true after a full reset (RIS).
    pub fn sync(&mut self, reset: bool) {
        if let Some(r) = self.remote.as_mut() {
            if r.sync(&self.engine, reset) {
                self.full = true;
            }
        }
    }

    /// After an engine crash, which starts the engine over empty.
    pub fn restarted(&mut self) {
        if let Some(r) = self.remote.as_mut() {
            r.restarted(&self.engine);
        }
        self.full = true;
    }

    /// After a resize: rows fetched at the old width are dropped (the host calls `setHistoryProvider` again).
    pub fn resized(&mut self) {
        if let Some(r) = self.remote.as_mut() {
            r.resized(&self.engine);
            self.full = true;
        }
    }

    /// For the buffer API, search and serialize only (module docs).
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The next frame rebuilds everything (after an engine crash or a renderer restart).
    pub fn invalidate(&mut self) {
        self.full = true;
    }
}

impl EngineView for LocalEngineView {
    fn take_events(&mut self) -> Events {
        let e = &self.engine;
        let mut events = Events {
            replies: e
                .take_reply_list()
                .map(|list| {
                    list.iter()
                        .filter_map(|r| r.dyn_into::<Uint8Array>().ok())
                        .map(|r| r.to_vec())
                        .collect()
                })
                .unwrap_or_default(),
            title: e.take_title().ok().flatten(),
            bells: e.take_bells().unwrap_or(0),
            resets: e.take_resets().unwrap_or(0),
            modes: e.modes().unwrap_or(0),
            kitty_flags: e.kitty_flags().unwrap_or(0),
            ..Events::default()
        };
        while let Ok(write) = e.take_clipboard_write_js() {
            if write.is_null() || write.is_undefined() {
                break;
            }
            events.clipboard_writes.push(ClipboardWrite {
                selection: selection_char(&get(&write, "selection")),
                text: get(&write, "text").as_string().unwrap_or_default(),
            });
        }
        while let Ok(Some(selection)) = e.take_clipboard_read() {
            events
                .clipboard_reads
                .push(selection_char(&JsValue::from(selection)));
        }
        events
    }

    fn take_frame(&mut self) -> Option<Frame> {
        let e = &self.engine;
        if e.sync_pending().unwrap_or(false) {
            return None;
        }
        let (cols, rows) = (e.cols(), e.rows());
        let display_offset = self.display_offset();
        // Rows above this engine line are the host's (viewport-only mode).
        let local_top = -(e.history_length() as i32);
        let remote = self.remote.as_deref().filter(|r| r.visible_lines() > 0);
        let damage = e.take_damage_js().unwrap_or(JsValue::NULL);
        // Engine damage counts rows from the engine's own view, which stops at its oldest line.
        let full = self.full
            || remote.is_some_and(|r| r.extra.get() > 0)
            || damage.is_null()
            || get(&damage, "full").is_truthy()
            || get(&damage, "scroll").as_f64().unwrap_or(0.0) > 0.0;
        self.full = false;
        let lines: Vec<u32> = if full {
            (0..rows).collect()
        } else {
            Array::from(&get(&damage, "lines"))
                .iter()
                .filter_map(|y| y.as_f64())
                .map(|y| y as u32)
                .filter(|&y| y < rows)
                .collect()
        };
        let stride = cols as usize * 4;
        let mut cells = vec![0; lines.len() * stride];
        let mut extras = HashMap::new();
        if let Some(r) = remote {
            r.viewed(display_offset, e.history_length(), rows);
        }
        for (k, &y) in lines.iter().enumerate() {
            let line = y as i32 - display_offset as i32;
            let row = &mut cells[k * stride..(k + 1) * stride];
            if let Some(r) = remote.filter(|_| line < local_top) {
                // Host line `v`: the fetched row, or a blank placeholder until it comes.
                let v = (i64::from(r.lines) + i64::from(line) - i64::from(local_top)) as u32;
                match r.row(v) {
                    Some(fetched) => {
                        fetched.copy_cells(row);
                        if !fetched.extras.is_empty() {
                            extras.insert(y, fetched.extras.clone());
                        }
                    }
                    None => {
                        blank_row(row);
                        r.want(v, v + 1);
                    }
                }
                continue;
            }
            let Ok(Some(view)) = e.row_cells_view(line) else {
                continue;
            };
            let n = (view.length() as usize).min(stride);
            view.subarray(0, n as u32).copy_to(&mut row[..n]);
            if row.chunks_exact(4).any(|cell| cell[3] & attr::EXTRAS != 0) {
                extras.insert(
                    y,
                    parse_extras(&e.row_extras(line).unwrap_or(JsValue::NULL)),
                );
            }
            if row.chunks_exact(4).any(|cell| cell[3] & attr::IMAGE != 0) {
                if let Ok(Some(cells)) = e.row_images(line) {
                    add_images(extras.entry(y).or_default(), &cells.to_vec());
                }
            }
        }
        let mut cursor = self.cursor();
        cursor.line += display_offset as i32;
        if cursor.line >= rows as i32 {
            cursor.visible = false;
        }
        Some(Frame {
            cols,
            rows,
            full,
            lines,
            cells,
            extras,
            cursor,
            display_offset,
            history_length: self.history_length(),
            trimmed_lines: self.trimmed_lines(),
            modes: e.modes().unwrap_or(0),
            kitty_flags: e.kitty_flags().unwrap_or(0),
        })
    }
}

/// An erased cell in the default colours.
pub(crate) const BLANK_CELL: [u32; 4] = [b' ' as u32, color::DEFAULT_FG, color::DEFAULT_BG, 0];

/// Fills `row` (4 words per cell) with erased cells.
pub(crate) fn blank_row(row: &mut [u32]) {
    for cell in row.chunks_exact_mut(4) {
        cell.copy_from_slice(&BLANK_CELL);
    }
}

impl LocalEngineView {
    /// Cursor in engine screen lines.
    pub fn cursor(&self) -> Cursor {
        let Ok(c) = self.engine.cursor_js() else {
            return Cursor::default();
        };
        Cursor {
            line: get(&c, "line").as_f64().unwrap_or(0.0) as i32,
            col: get(&c, "col").as_f64().unwrap_or(0.0) as u32,
            visible: get(&c, "visible").is_truthy(),
            shape: match get(&c, "shape").as_string().as_deref() {
                Some("underline") => CursorShape::Underline,
                Some("bar") => CursorShape::Bar,
                _ => CursorShape::Block,
            },
            blink: get(&c, "blink").is_truthy(),
            styled: get(&c, "styled").is_truthy(),
        }
    }
}

/// The visible rows as of the last frame. The renderer redraws from here when the host changes something the
/// engine does not know about (selection, cursor blink, theme), and selection reads visible text from it.
#[derive(Clone, Debug, Default)]
pub struct Viewport {
    pub cols: u32,
    pub rows: u32,
    pub cells: Vec<u32>,
    pub extras: HashMap<u32, Vec<CellExtra>>,
    pub cursor: Cursor,
    pub display_offset: u32,
    pub history_length: u32,
    pub trimmed_lines: f64,
    pub modes: u32,
    pub kitty_flags: u32,
}

impl Viewport {
    /// Copies a frame in and returns the rows that changed (all of them when `frame.full`).
    pub fn apply(&mut self, frame: Frame) -> Vec<u32> {
        let stride = frame.cols as usize * 4;
        if frame.cols != self.cols || frame.rows != self.rows {
            self.cols = frame.cols;
            self.rows = frame.rows;
            self.cells = vec![0; frame.rows as usize * stride];
            self.extras.clear();
        }
        if frame.full {
            self.extras.clear();
        }
        let mut extras = frame.extras;
        for (k, &y) in frame.lines.iter().enumerate() {
            let at = y as usize * stride;
            self.cells[at..at + stride].copy_from_slice(&frame.cells[k * stride..(k + 1) * stride]);
            match extras.remove(&y) {
                Some(row) => self.extras.insert(y, row),
                None => self.extras.remove(&y),
            };
        }
        self.cursor = frame.cursor;
        self.display_offset = frame.display_offset;
        self.history_length = frame.history_length;
        self.trimmed_lines = frame.trimmed_lines;
        self.modes = frame.modes;
        self.kitty_flags = frame.kitty_flags;
        frame.lines
    }

    /// `[codepoint, fg, bg, attrs]` per cell of viewport row `y`.
    pub fn row(&self, y: u32) -> &[u32] {
        let stride = self.cols as usize * 4;
        &self.cells[y as usize * stride..(y as usize + 1) * stride]
    }
}

pub(crate) fn get(target: &JsValue, key: &str) -> JsValue {
    Reflect::get(target, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

fn selection_char(value: &JsValue) -> char {
    if value.as_string().as_deref() == Some("p") {
        'p'
    } else {
        'c'
    }
}

/// Merges `[col, id, tile]` triples into a row's extras.
fn add_images(extras: &mut Vec<CellExtra>, cells: &[u32]) {
    for cell in cells.chunks_exact(3) {
        let image = Some((cell[1], cell[2]));
        match extras.iter_mut().find(|e| e.col == cell[0]) {
            Some(extra) => extra.image = image,
            None => extras.push(CellExtra {
                col: cell[0],
                image,
                ..CellExtra::default()
            }),
        }
    }
}

pub(crate) fn parse_extras(value: &JsValue) -> Vec<CellExtra> {
    if !Array::is_array(value) {
        return Vec::new();
    }
    Array::from(value)
        .iter()
        .map(|extra| {
            let hyperlink = get(&extra, "hyperlink");
            CellExtra {
                col: get(&extra, "col").as_f64().unwrap_or(0.0) as u32,
                text: get(&extra, "text").as_string(),
                underline_color: get(&extra, "underlineColor").as_f64().map(|c| c as u32),
                hyperlink: hyperlink.is_object().then(|| Hyperlink {
                    id: get(&hyperlink, "id").as_string().unwrap_or_default(),
                    uri: get(&hyperlink, "uri").as_string().unwrap_or_default(),
                }),
                image: None,
            }
        })
        .collect()
}
