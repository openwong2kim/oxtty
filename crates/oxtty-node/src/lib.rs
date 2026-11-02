//! N-API binding: `new Terminal(cols, rows, scrollback?)` for Node and
//! Electron's main process. Same engine as the WebAssembly build.

use napi::bindgen_prelude::{Buffer, Either, Uint32Array};
use napi_derive::napi;
use oxtty::{Clipboard, HookKind, ImageCursor, WindowsPty, WindowsPtyBackend, COLOR_SLOTS};

#[napi(object)]
pub struct Cursor {
    pub line: u32,
    pub col: u32,
    pub visible: bool,
    /// `"block"`, `"underline"` or `"bar"`.
    pub shape: String,
    pub blink: bool,
    /// The program set `shape` and `blink` (DECSCUSR 1–6); otherwise they are defaults the host may replace.
    pub styled: bool,
}

#[napi(object)]
pub struct ClipboardWrite {
    /// `"c"` (clipboard) or `"p"` (primary selection).
    pub selection: String,
    pub text: String,
}

#[napi(object)]
pub struct Damage {
    /// Redraw everything; `scroll` is 0 and `lines` is empty.
    pub full: bool,
    /// Move drawn rows up by this many first.
    pub scroll: u32,
    pub lines: Vec<u32>,
}

#[napi(object)]
pub struct Hyperlink {
    pub id: String,
    pub uri: String,
}

#[napi(object)]
pub struct CellExtra {
    pub col: u32,
    pub text: Option<String>,
    pub underline_color: Option<u32>,
    pub hyperlink: Option<Hyperlink>,
}

/// What `placeImage` did: tiles written, and the image id of each tile it covered.
#[napi(object)]
pub struct ImagePlaced {
    pub tiles: u32,
    pub covered: Vec<u32>,
}

/// How `placeImage` moves the cursor.
#[napi(object)]
#[derive(Default)]
pub struct PlaceImageOptions {
    /// Default true; false places the image at the top left of the screen and keeps the cursor.
    pub scrolling: Option<bool>,
    /// `"vt340"` (default) or `"iip"`.
    pub cursor: Option<String>,
}

/// What `ansiSnapshot` leaves out, like `@xterm/addon-serialize` options.
#[napi(object)]
#[derive(Default)]
pub struct AnsiSnapshotOptions {
    /// Keep only the newest this many history lines (0: the screen alone).
    pub scrollback: Option<u32>,
    /// Leave out the modes and what continues the output (unfinished sequence, synchronized update).
    pub exclude_modes: Option<bool>,
}

/// `{ ok: true, payload }` or `{ ok: false, reason }`.
#[napi(object)]
pub struct AnsiSnapshot {
    pub ok: bool,
    pub payload: Option<Buffer>,
    /// `"unsupported-wide-edge"`, `"margins"` or `"partial-tail-overflow"`.
    pub reason: Option<String>,
}

/// A hooked sequence parsing stopped at, before its default action.
#[napi(object)]
pub struct Hook {
    /// `"csi"`, `"osc"` or `"dcs"`.
    pub kind: String,
    pub id: u32,
    /// Each parameter followed by its sub-parameters (an empty sub-parameter is -1).
    pub params: Vec<Vec<i32>>,
    /// OSC: everything after the first `;`. DCS: the payload.
    pub data: Buffer,
}

/// How the search methods compare text (no regex here: match `searchLine` text with your own).
#[napi(object)]
#[derive(Default)]
pub struct SearchOptions {
    pub case_sensitive: Option<bool>,
    pub whole_word: Option<bool>,
}

/// A match in cells; the end is exclusive and on `endLine`.
#[napi(object)]
pub struct SearchMatch {
    pub start_line: i32,
    pub start_col: u32,
    pub end_line: i32,
    pub end_col: u32,
}

/// A logical line as search reads it; `cells[i]` is the cell of the i-th character (not UTF-16 unit) of `text`.
#[napi(object)]
pub struct SearchLine {
    pub first: i32,
    pub rows: u32,
    pub text: String,
    pub cells: Uint32Array,
}

fn search_options(options: Option<SearchOptions>) -> oxtty::SearchOptions {
    let options = options.unwrap_or_default();
    oxtty::SearchOptions {
        case_sensitive: options.case_sensitive.unwrap_or(false),
        whole_word: options.whole_word.unwrap_or(false),
    }
}

fn search_match(m: oxtty::SearchMatch) -> SearchMatch {
    SearchMatch {
        start_line: m.start_line,
        start_col: m.start_col as u32,
        end_line: m.end_line,
        end_col: m.end_col as u32,
    }
}

#[napi]
pub struct Terminal {
    inner: oxtty::Engine,
}

#[napi]
impl Terminal {
    #[napi(constructor)]
    pub fn new(cols: u32, rows: u32, scrollback: Option<u32>) -> Self {
        Self {
            inner: oxtty::Engine::new(
                cols.min(u16::MAX as u32) as u16,
                rows.min(u16::MAX as u32) as u16,
                scrollback.unwrap_or(1000) as usize,
            ),
        }
    }

    /// Feeds program output (string as UTF-8, or raw bytes).
    #[napi]
    pub fn write(&mut self, data: Either<String, Buffer>) {
        match data {
            Either::A(s) => self.inner.feed(s.as_bytes()),
            Either::B(b) => self.inner.feed(&b),
        }
    }

    #[napi]
    pub fn resize(&mut self, cols: u32, rows: u32) {
        self.inner.resize(
            cols.min(u16::MAX as u32) as u16,
            rows.min(u16::MAX as u32) as u16,
        );
    }

    #[napi(getter)]
    pub fn cols(&self) -> u32 {
        self.inner.cols() as u32
    }

    #[napi(getter)]
    pub fn rows(&self) -> u32 {
        self.inner.rows() as u32
    }

    #[napi(getter)]
    pub fn history_length(&self) -> u32 {
        self.inner.history_len() as u32
    }

    /// Lines that have left the top of the scrollback so far (a full history dropping rows, ED3, RIS).
    #[napi(getter)]
    pub fn trimmed_lines(&self) -> f64 {
        self.inner.trimmed_lines() as f64
    }

    #[napi]
    pub fn row_text(&self, line: i32) -> Option<String> {
        self.inner.row_text(line)
    }

    /// `[ch, fg, bg, attrs]` per cell, or `null` if out of range.
    #[napi]
    pub fn row_cells(&self, line: i32) -> Option<Uint32Array> {
        let mut out = Vec::new();
        self.inner
            .row_packed(line, &mut out)
            .then(|| Uint32Array::new(out))
    }

    #[napi]
    pub fn row_wrapped(&self, line: i32) -> bool {
        self.inner.row_wrapped(line)
    }

    #[napi]
    pub fn cell_text(&self, line: i32, col: u32) -> Option<String> {
        self.inner.cell_text(line, col.min(u16::MAX as u32) as u16)
    }

    /// Cells with combining marks, an underline colour or a hyperlink, or
    /// `null` if out of range.
    #[napi]
    pub fn row_extras(&self, line: i32) -> Option<Vec<CellExtra>> {
        let mut extras = Vec::new();
        self.inner.row_extras(line, &mut extras).then(|| {
            extras
                .into_iter()
                .map(|e| CellExtra {
                    col: e.col as u32,
                    text: e.text,
                    underline_color: e.underline_color,
                    hyperlink: e.hyperlink.map(|l| Hyperlink {
                        id: l.id,
                        uri: l.uri,
                    }),
                })
                .collect()
        })
    }

    #[napi]
    pub fn cursor(&self) -> Cursor {
        let c = self.inner.cursor();
        let shape = match c.shape {
            oxtty::CursorShape::Block => "block",
            oxtty::CursorShape::Underline => "underline",
            oxtty::CursorShape::Bar => "bar",
        };
        Cursor {
            line: c.line as u32,
            col: c.col as u32,
            visible: c.visible,
            shape: shape.into(),
            blink: c.blinking,
            styled: c.styled,
        }
    }

    /// Rows the view is scrolled back into history (0 = following output).
    #[napi(getter)]
    pub fn display_offset(&self) -> u32 {
        self.inner.display_offset() as u32
    }

    /// Positive `delta` scrolls back into history.
    #[napi]
    pub fn scroll_display(&mut self, delta: i32) {
        self.inner.scroll_display(delta);
    }

    #[napi]
    pub fn scroll_to_bottom(&mut self) {
        self.inner.scroll_to_bottom();
    }

    #[napi]
    pub fn modes(&self) -> u32 {
        self.inner.modes()
    }

    /// Lets the program use the kitty keyboard protocol (off by default); switching drops its pushed flags.
    #[napi]
    pub fn set_kitty_keyboard(&mut self, enabled: bool) {
        self.inner.set_kitty_keyboard(enabled);
    }

    /// Drops the kitty keyboard flags the program pushed, keeping the protocol on or off.
    #[napi]
    pub fn reset_kitty_keyboard(&mut self) {
        self.inner.reset_kitty_keyboard();
    }

    /// Active kitty keyboard flags (1 disambiguate, 2 event types, 4 alternate keys, 8 all keys, 16 associated text).
    #[napi]
    pub fn kitty_flags(&self) -> u32 {
        self.inner.kitty_flags() as u32
    }

    /// The host's focus, for the report the engine sends when the program turns on focus events (`CSI ? 1004 h`);
    /// `null`, the default, sends none.
    #[napi]
    pub fn set_focus(&mut self, focused: Option<bool>) {
        self.inner.set_focus(focused);
    }

    /// The host's cursor style (`"block"`, `"underline"` or `"bar"`): `cursor()` reports it until the program sets
    /// one, and DECRQSS reports it.
    #[napi]
    pub fn set_cursor_default(&mut self, shape: String, blink: bool) {
        let shape = match shape.as_str() {
            "underline" => oxtty::CursorShape::Underline,
            "bar" => oxtty::CursorShape::Bar,
            _ => oxtty::CursorShape::Block,
        };
        self.inner.set_cursor_default(shape, blink);
    }

    #[napi]
    pub fn take_damage(&mut self) -> Damage {
        match self.inner.take_damage() {
            oxtty::Damage::Full => Damage {
                full: true,
                scroll: 0,
                lines: Vec::new(),
            },
            oxtty::Damage::Lines { scroll, lines } => Damage {
                full: false,
                scroll: scroll.into(),
                lines: lines.into_iter().map(u32::from).collect(),
            },
        }
    }

    #[napi]
    pub fn take_replies(&mut self) -> Buffer {
        self.inner.take_replies().into()
    }

    /// The same replies one by one, in order (xterm.js fires one `onData` per reply).
    #[napi]
    pub fn take_reply_list(&mut self) -> Vec<Buffer> {
        self.inner
            .take_reply_list()
            .into_iter()
            .map(Buffer::from)
            .collect()
    }

    #[napi]
    pub fn take_title(&mut self) -> Option<String> {
        self.inner.take_title()
    }

    #[napi]
    pub fn take_bells(&mut self) -> u32 {
        self.inner.take_bells()
    }

    /// Full resets (RIS) the program sent since the last call.
    #[napi]
    pub fn take_resets(&mut self) -> u32 {
        self.inner.take_resets()
    }

    /// Lets the program read and/or write the host clipboards with OSC 52 (both off by default).
    #[napi]
    pub fn set_clipboard_access(&mut self, read: bool, write: bool) {
        self.inner.set_clipboard_access(read, write);
    }

    /// Oldest text the program asked to copy.
    #[napi]
    pub fn take_clipboard_write(&mut self) -> Option<ClipboardWrite> {
        self.inner
            .take_clipboard_write()
            .map(|(clipboard, text)| ClipboardWrite {
                selection: selection_name(clipboard).to_owned(),
                text,
            })
    }

    /// Oldest clipboard (`"c"` or `"p"`) the program asked to read; answer with `replyClipboard`.
    #[napi]
    pub fn take_clipboard_read(&mut self) -> Option<String> {
        self.inner
            .take_clipboard_read()
            .map(|clipboard| selection_name(clipboard).to_owned())
    }

    /// Answers the oldest read of `selection`; the reply joins `takeReplies()`.
    #[napi]
    pub fn reply_clipboard(&mut self, selection: String, text: String) -> bool {
        let clipboard = match selection.as_str() {
            "c" => Clipboard::Clipboard,
            "p" => Clipboard::Selection,
            _ => return false,
        };
        self.inner.reply_clipboard(clipboard, &text)
    }

    /// Colours (`0xRRGGBB`, palette 0–255 then foreground, background, cursor) that answer
    /// OSC 4/10/11/12 queries; `null` stops answering.
    #[napi]
    pub fn set_colors(&mut self, colors: Option<Uint32Array>) -> napi::Result<()> {
        let colors = match colors {
            None => None,
            Some(c) => Some(<[u32; COLOR_SLOTS]>::try_from(&c[..]).map_err(|_| {
                napi::Error::from_reason(format!("setColors takes {COLOR_SLOTS} colours"))
            })?),
        };
        self.inner.set_colors(colors);
        Ok(())
    }

    #[napi]
    pub fn sync_pending(&self) -> bool {
        self.inner.sync_pending()
    }

    #[napi]
    pub fn flush_sync(&mut self) {
        self.inner.flush_sync();
    }

    /// Stops parsing at sequences of `kind` (`"csi"`, `"osc"`, `"dcs"`) with `id`, or no longer does.
    #[napi]
    pub fn set_hook(&mut self, kind: String, id: u32, on: bool) -> napi::Result<()> {
        let kind = match kind.as_str() {
            "csi" => HookKind::Csi,
            "osc" => HookKind::Osc,
            "dcs" => HookKind::Dcs,
            _ => {
                return Err(napi::Error::from_reason(format!(
                    "unknown hook kind {kind}"
                )))
            }
        };
        self.inner.set_hook(kind, id, on);
        Ok(())
    }

    /// Like `write`, but stops right after a hooked sequence; returns the bytes taken.
    #[napi]
    pub fn feed_until_hook(&mut self, data: Buffer) -> u32 {
        self.inner.feed_until_hook(&data) as u32
    }

    #[napi]
    pub fn pending_hook(&self) -> Option<Hook> {
        self.inner.pending_hook().map(|hook| Hook {
            kind: match hook.kind {
                HookKind::Csi => "csi",
                HookKind::Osc => "osc",
                HookKind::Dcs => "dcs",
            }
            .into(),
            id: hook.id,
            params: hook.params,
            data: hook.data.into(),
        })
    }

    /// Runs the pending hook's default action (`run_default`) or drops it. Parsing goes on with the next
    /// `feedUntilHook` (with the bytes it did not take, even none).
    #[napi]
    pub fn finish_hook(&mut self, run_default: bool) {
        self.inner.finish_hook(run_default);
    }

    /// xterm.js `windowsPty`: `backend` is `"conpty"` or `"winpty"`.
    #[napi]
    pub fn set_windows_pty(
        &mut self,
        backend: Option<String>,
        build_number: Option<u32>,
    ) -> napi::Result<()> {
        let backend = match backend.as_deref() {
            None => None,
            Some("conpty") => Some(WindowsPtyBackend::Conpty),
            Some("winpty") => Some(WindowsPtyBackend::Winpty),
            Some(other) => {
                return Err(napi::Error::from_reason(format!("unknown backend {other}")))
            }
        };
        self.inner.set_windows_pty(WindowsPty {
            backend,
            build_number,
        });
        Ok(())
    }

    /// First match starting at or after cell (`line`, `col`), down to the bottom of the screen.
    #[napi]
    pub fn search_next(
        &self,
        query: String,
        options: Option<SearchOptions>,
        line: i32,
        col: u32,
    ) -> Option<SearchMatch> {
        let col = col.min(u16::MAX as u32) as u16;
        self.inner
            .search_next(&query, search_options(options), line, col)
            .map(search_match)
    }

    /// Last match ending at or before cell (`line`, `col`), up to the top of the history.
    #[napi]
    pub fn search_previous(
        &self,
        query: String,
        options: Option<SearchOptions>,
        line: i32,
        col: u32,
    ) -> Option<SearchMatch> {
        let col = col.min(u16::MAX as u32) as u16;
        self.inner
            .search_previous(&query, search_options(options), line, col)
            .map(search_match)
    }

    /// Every match in the logical lines rows `first`…`last` belong to, keeping those that touch the rows.
    #[napi]
    pub fn search_rows(
        &self,
        query: String,
        options: Option<SearchOptions>,
        first: i32,
        last: i32,
    ) -> Vec<SearchMatch> {
        let mut found = Vec::new();
        self.inner
            .search_rows(&query, search_options(options), first, last, &mut found);
        found.into_iter().map(search_match).collect()
    }

    #[napi]
    pub fn search_line(&self, line: i32) -> Option<SearchLine> {
        self.inner.search_line(line).map(|l| SearchLine {
            first: l.first,
            rows: l.rows as u32,
            text: l.text,
            cells: l.cells.into(),
        })
    }

    /// Puts a marker on buffer line `line` of the screen that is up (0 is the oldest history line, or the alternate
    /// screen's top row); null outside the buffer. The alternate screen's markers go when it is left.
    #[napi]
    pub fn add_marker(&mut self, line: i32) -> Option<u32> {
        self.inner.add_marker(line as i64)
    }

    /// The marker's buffer line on its screen; null once it is gone.
    #[napi]
    pub fn marker_line(&self, id: u32) -> Option<i32> {
        self.inner.marker_line(id).map(|line| line as i32)
    }

    #[napi]
    pub fn remove_marker(&mut self, id: u32) {
        self.inner.remove_marker(id);
    }

    /// Markers the engine dropped since the last call (trimmed, cleared, reset).
    #[napi]
    pub fn take_disposed_markers(&mut self) -> Vec<u32> {
        self.inner.take_disposed_markers()
    }

    /// Covers `cols` × `rows` cells with the tiles of image `id`.
    #[napi]
    pub fn place_image(
        &mut self,
        id: u32,
        cols: u32,
        rows: u32,
        options: Option<PlaceImageOptions>,
    ) -> ImagePlaced {
        let options = options.unwrap_or_default();
        let cursor = match options.cursor.as_deref() {
            Some("iip") => ImageCursor::Iip,
            _ => ImageCursor::Vt340,
        };
        let clamp = |n: u32| n.min(u16::MAX as u32) as u16;
        let placed = self.inner.place_image(
            id,
            clamp(cols),
            clamp(rows),
            options.scrolling.unwrap_or(true),
            cursor,
        );
        ImagePlaced {
            tiles: placed.tiles,
            covered: placed.covered,
        }
    }

    /// `[col, id, tile]` per cell an image tile covers, or `null` if out of range.
    #[napi]
    pub fn row_images(&self, line: i32) -> Option<Uint32Array> {
        let mut cells = Vec::new();
        self.inner.row_images(line, &mut cells).then(|| {
            Uint32Array::new(
                cells
                    .iter()
                    .flat_map(|c| [c.col as u32, c.id, c.tile])
                    .collect(),
            )
        })
    }

    /// `[fg, bg, attrs]` that text written now gets.
    #[napi]
    pub fn pen(&self) -> Uint32Array {
        let pen = self.inner.pen();
        Uint32Array::new(vec![pen.fg, pen.bg, pen.attrs])
    }

    /// Bytes that rebuild this screen in a fresh xterm.js, or why they cannot.
    #[napi]
    pub fn ansi_snapshot(&self, options: Option<AnsiSnapshotOptions>) -> AnsiSnapshot {
        let options = options.unwrap_or_default();
        let options = oxtty::SnapshotOptions {
            scrollback: options.scrollback.map(|n| n as usize),
            exclude_modes: options.exclude_modes.unwrap_or(false),
        };
        match self.inner.ansi_snapshot_with(options) {
            Ok(bytes) => AnsiSnapshot {
                ok: true,
                payload: Some(bytes.into()),
                reason: None,
            },
            Err(why) => AnsiSnapshot {
                ok: false,
                payload: None,
                reason: Some(why.as_str().into()),
            },
        }
    }
}

fn selection_name(clipboard: Clipboard) -> &'static str {
    match clipboard {
        Clipboard::Clipboard => "c",
        Clipboard::Selection => "p",
    }
}
