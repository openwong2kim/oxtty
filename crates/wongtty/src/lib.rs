//! # wongtty
//!
//! A terminal emulator engine: feed it the bytes a program writes to its PTY,
//! and it keeps the screen — characters, colours, cursor, modes, scrollback.
//! It does not draw anything. A renderer (WebGL, DOM, wgpu, a test) asks which
//! rows changed since it last looked and reads only those.
//!
//! The same engine builds natively and for `wasm32-unknown-unknown`, so a
//! daemon, a desktop app and a browser can share one interpretation of the
//! byte stream.
//!
//! ```
//! let mut term = wongtty::Engine::new(80, 24, 1000);
//! term.feed("안녕 \x1b[31mred\x1b[0m\r\n".as_bytes());
//! assert_eq!(term.row_text(0).unwrap(), "안녕 red");
//! assert_eq!(term.cursor().line, 1);
//! ```
//!
//! Parsing and the screen model come from a lightly patched
//! [`alacritty_terminal`](https://crates.io/crates/alacritty_terminal) 0.26
//! built into this crate as [`vt`] (see `src/vt/PATCHES.md`).

use std::cell::{Ref, RefCell};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

// The built-in Alacritty fork. It keeps the upstream formatting so upgrade diffs stay small.
#[rustfmt::skip]
pub mod vt;

pub mod graphics;
mod history;
mod images;
pub mod input;
mod markers;
mod search;
mod snapshot;
pub use history::TermHistory;
pub use images::{ImageCell, ImageCursor, ImagePlaced};
pub use search::{SearchLine, SearchMatch, SearchOptions};
pub use snapshot::{SnapshotOptions, SnapshotRefusal};

use crate::history::History;
use crate::vt::event::{Event, EventListener};
use crate::vt::grid::{Dimensions, Grid, Row, Scroll};
use crate::vt::index::Line;
use crate::vt::term::cell::{Cell as AlacCell, Flags};
use crate::vt::term::{ClipboardType, Config, Osc52, Term, TermDamage, TermMode};
use crate::vt::vte::ansi::Hook as VtHook;
use crate::vt::vte::ansi::Rgb;
use crate::vt::vte::ansi::{
    Color, CursorShape as AlacCursorShape, CursorStyle, Handler, NamedPrivateMode, Processor,
};
pub use crate::vt::vte::ansi::{HookKind, hook_id};

/// Smallest and largest accepted dimensions.
pub const MIN_COLS: u16 = 2;
pub const MAX_COLS: u16 = 1000;
pub const MIN_ROWS: u16 = 1;
pub const MAX_ROWS: u16 = 1000;
/// Upper bound on retained scrollback lines.
pub const MAX_SCROLLBACK: usize = 100_000;

/// How long a synchronized update (`CSI ? 2026 h`) may hold the host's
/// frames back before it should call [`Engine::flush_sync`]. Matches
/// xterm.js 6.1 (`RenderService` `SYNCHRONIZED_OUTPUT_TIMEOUT_MS`).
pub const SYNC_TIMEOUT: Duration = Duration::from_millis(1000);

/// Cell attribute bits returned in [`Cell::attrs`]. Stable across versions.
pub mod attr {
    pub const BOLD: u32 = 1 << 0;
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
    /// First half of a double-width character.
    pub const WIDE: u32 = 1 << 12;
    /// Second half of a double-width character; renderers skip it.
    pub const WIDE_SPACER: u32 = 1 << 13;
    /// The cell carries combining marks; read them with [`crate::Engine::row_extras`].
    pub const COMBINING: u32 = 1 << 14;
    /// The cell has an underline colour (SGR 58); see [`crate::Engine::row_extras`].
    pub const UNDERLINE_COLOR: u32 = 1 << 15;
    /// The cell is part of an OSC 8 hyperlink; see [`crate::Engine::row_extras`].
    pub const HYPERLINK: u32 = 1 << 16;
    /// The blank last cell of a wrapped row whose wide character did not fit and moved to the next row (xterm.js
    /// leaves the same blank); it is not text, unlike a space the program wrote.
    pub const LEADING_WIDE_SPACER: u32 = 1 << 17;
    /// A space the program printed. Other blank cells were erased or never written: xterm.js reads them as empty,
    /// so trimming the end of a line drops them but keeps typed spaces.
    pub const TYPED_SPACE: u32 = 1 << 18;
    /// An image tile covers the cell; see [`crate::Engine::row_images`].
    pub const IMAGE: u32 = 1 << 19;
}

/// Terminal mode bits returned by [`Engine::modes`]. Stable across versions.
pub mod mode {
    pub const SHOW_CURSOR: u32 = 1 << 0;
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
    /// Any kitty keyboard protocol flag is active.
    pub const KITTY_KEYBOARD: u32 = 1 << 14;
    /// X10 mouse reporting (`CSI ? 9 h`): button presses only, no modifiers.
    pub const MOUSE_X10: u32 = 1 << 15;
    /// SGR mouse reports in pixels instead of cells (`CSI ? 1016 h`).
    pub const SGR_PIXELS_MOUSE: u32 = 1 << 16;
}

/// Packed colours in [`Cell::fg`] / [`Cell::bg`]: the top byte is the kind,
/// the low 24 bits the value.
pub mod color {
    /// `value` is a named colour index (0–15 ANSI, 256 = foreground,
    /// 257 = background, 258 = cursor, 259–266 dim variants …), as in Alacritty's `NamedColor`.
    pub const NAMED: u32 = 1 << 24;
    /// `value` is a 256-colour palette index.
    pub const INDEXED: u32 = 2 << 24;
    /// `value` is `0xRRGGBB`.
    pub const RGB: u32 = 3 << 24;
    pub const KIND_MASK: u32 = 0xff << 24;
    pub const VALUE_MASK: u32 = 0x00ff_ffff;
    /// Default foreground (named 256).
    pub const DEFAULT_FG: u32 = NAMED | 256;
    /// Default background (named 257).
    pub const DEFAULT_BG: u32 = NAMED | 257;
}

/// One screen cell, already flattened for a renderer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub fg: u32,
    pub bg: u32,
    pub attrs: u32,
}

impl Cell {
    /// Number of `u32` words per cell in [`Engine::row_packed`].
    pub const PACKED_WORDS: usize = 4;
}

/// Cursor shape set by the program (DECSCUSR, `CSI Ps SP q`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

/// Cursor position on the screen (0-based) and its style. While the view is
/// scrolled back ([`Engine::display_offset`]), it is drawn on viewport row
/// `line + display_offset`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub line: u16,
    pub col: u16,
    pub visible: bool,
    pub shape: CursorShape,
    pub blinking: bool,
    /// The program chose `shape` and `blinking` (DECSCUSR 1–6). When false they are the defaults
    /// (block, steady) and a host applies its own cursor style instead.
    pub styled: bool,
}

/// What changed in the viewport since the last [`Engine::take_damage`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Damage {
    /// Redraw everything (first frame, resize, view scroll, alternate screen …).
    Full,
    /// First move every drawn row up by `scroll` rows (the top `scroll` rows
    /// fall off), then redraw `lines`, top to bottom.
    Lines { scroll: u16, lines: Vec<u16> },
}

/// What a cell carries beyond [`Cell`]. Only cells with at least one of
/// these are listed by [`Engine::row_extras`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellExtra {
    pub col: u16,
    /// Full cell text when it has combining marks ([`attr::COMBINING`]).
    pub text: Option<String>,
    /// Underline colour (SGR 58) in the [`color`] format ([`attr::UNDERLINE_COLOR`]).
    pub underline_color: Option<u32>,
    /// OSC 8 hyperlink ([`attr::HYPERLINK`]).
    pub hyperlink: Option<Hyperlink>,
}

/// An OSC 8 hyperlink. Cells of one link share its `id` (generated when the
/// program gave none).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hyperlink {
    pub id: String,
    pub uri: String,
}

/// The clipboard an OSC 52 request names: `c`, or `p`/`s` (the primary selection).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clipboard {
    Clipboard,
    Selection,
}

impl From<ClipboardType> for Clipboard {
    fn from(ty: ClipboardType) -> Self {
        match ty {
            ClipboardType::Clipboard => Clipboard::Clipboard,
            ClipboardType::Selection => Clipboard::Selection,
        }
    }
}

/// A sequence the host hooked ([`Engine::set_hook`]) that parsing stopped at, before its default action
/// ([`Engine::finish_hook`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hook {
    pub kind: HookKind,
    /// [`hook_id`] for CSI and DCS, the number before the first `;` for OSC.
    pub id: u32,
    /// CSI and DCS parameters, each followed by its sub-parameters (an empty parameter is 0 and an empty
    /// sub-parameter -1, as in xterm.js; values stop at 65535).
    pub params: Vec<Vec<i32>>,
    /// OSC: everything after the first `;`. DCS: the payload.
    pub data: Vec<u8>,
}

/// The PTY backend of a Windows host (xterm.js `windowsPty.backend`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowsPtyBackend {
    Conpty,
    Winpty,
}

/// A PTY hosted on Windows (xterm.js `windowsPty`), for [`Engine::set_windows_pty`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WindowsPty {
    pub backend: Option<WindowsPtyBackend>,
    /// The Windows build, such as 19045.
    pub build_number: Option<u32>,
}

/// The conpty build from which xterm.js reflows and drops its wrapping heuristic.
const CONPTY_REFLOW_BUILD: u32 = 21376;

/// DECSTR (`CSI ! p`) as [`hook_id`] names it.
const DECSTR_HOOK: u32 = (b'!' as u32) << 8 | b'p' as u32;

/// Colours that answer OSC 4/10/11/12 queries: the 256 palette entries, then
/// foreground, background and cursor, as `0xRRGGBB`.
pub const COLOR_SLOTS: usize = 259;

/// A row [`Engine::with_row`] hands out: one of the grid's, a history row decoded for this read, or a cached one.
enum HeldRow<'a> {
    Grid(&'a Row<AlacCell>),
    Decoded(Row<AlacCell>),
    Cached(Ref<'a, Row<AlacCell>>),
}

impl std::ops::Deref for HeldRow<'_> {
    type Target = Row<AlacCell>;

    fn deref(&self) -> &Row<AlacCell> {
        match self {
            Self::Grid(row) => row,
            Self::Decoded(row) => row,
            Self::Cached(row) => row,
        }
    }
}

/// Compressed-history rows kept for repeated reads beyond the screen's
/// rows (a scrolled-back view redraws the same rows every frame). Nothing is
/// kept while the view is at the bottom.
const ROW_CACHE_SPARE: usize = 8;

/// A history row in the row cache: its cells as [`Engine::row_packed`] gives them (16 bytes a cell, not the 24 of a
/// decoded cell), and the decoded row only when a cell has an extra, as a renderer reads [`Engine::row_extras`] of
/// those rows alone.
struct CachedRow {
    id: u64,
    packed: Vec<u32>,
    row: Option<Row<AlacCell>>,
}

#[cfg(test)]
thread_local! {
    /// Compressed history rows [`Engine::held_row`] decoded on this thread.
    static DECODES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Row `i` of the compressed history, decoded.
fn decode(stored: &History, i: usize) -> Row<AlacCell> {
    #[cfg(test)]
    DECODES.with(|n| n.set(n.get() + 1));
    stored.row(i)
}

/// Unanswered clipboard reads and untaken writes kept at most (oldest dropped first).
const CLIPBOARD_QUEUE: usize = 16;

/// Cap on reply bytes the host has not taken. Past it, new replies are dropped
/// whole (a program sending `CSI 6n` forever to a host that never calls
/// [`Engine::take_replies`]).
pub const MAX_REPLIES: usize = 1 << 20;

type ClipboardFormatter = Arc<dyn Fn(&str) -> String + Sync + Send>;
type ColorFormatter = Arc<dyn Fn(Rgb) -> String + Sync + Send>;

#[derive(Default)]
struct Inbox {
    replies: Vec<u8>,
    // Where each reply in `replies` ends, for hosts that pass them on one by one as xterm.js does.
    reply_ends: Vec<usize>,
    title: Option<String>,
    bells: u32,
    clipboard_read: bool,
    clipboard_write: bool,
    clipboard_writes: VecDeque<(Clipboard, String)>,
    clipboard_reads: VecDeque<Clipboard>,
    // Reads waiting for an answer, with their reply format (it echoes the request's selection character).
    clipboard_pending: VecDeque<(Clipboard, ClipboardFormatter)>,
    // Colour queries: where in `replies` the answer goes, the colour index and the format. Answers are
    // inserted at that position to keep the order of replies.
    color_queries: Vec<(usize, usize, ColorFormatter)>,
}

fn push_bounded<T>(queue: &mut VecDeque<T>, item: T) {
    if queue.len() == CLIPBOARD_QUEUE {
        queue.pop_front();
    }
    queue.push_back(item);
}

/// Receives Alacritty's events and keeps what a host has to act on.
#[derive(Clone, Default)]
struct Listener(Arc<Mutex<Inbox>>);

impl Listener {
    // A poisoned lock (a panic elsewhere) keeps its contents usable, as TermHistory::lock does.
    fn inbox(&self) -> MutexGuard<'_, Inbox> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Inbox {
    fn reply(&mut self, bytes: &[u8]) {
        if self.replies.len() + bytes.len() <= MAX_REPLIES {
            self.replies.extend_from_slice(bytes);
            self.reply_ends.push(self.replies.len());
        }
    }

    // A reply that goes in at `at`, the end of an earlier reply (or 0).
    fn insert_reply(&mut self, at: usize, bytes: Vec<u8>) {
        if self.replies.len() + bytes.len() > MAX_REPLIES || at > self.replies.len() {
            return;
        }
        let len = bytes.len();
        self.replies.splice(at..at, bytes);
        let k = self.reply_ends.partition_point(|&end| end <= at);
        for end in &mut self.reply_ends[k..] {
            *end += len;
        }
        self.reply_ends.insert(k, at + len);
    }
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let mut inbox = self.inbox();
        match event {
            Event::PtyWrite(text) => inbox.reply(text.as_bytes()),
            Event::Title(title) => inbox.title = Some(title),
            Event::ResetTitle => inbox.title = Some(String::new()),
            Event::Bell => inbox.bells = inbox.bells.saturating_add(1),
            Event::ClipboardStore(ty, text) if inbox.clipboard_write => {
                push_bounded(&mut inbox.clipboard_writes, (ty.into(), text));
            }
            Event::ClipboardLoad(ty, format) if inbox.clipboard_read => {
                push_bounded(&mut inbox.clipboard_reads, ty.into());
                push_bounded(&mut inbox.clipboard_pending, (ty.into(), format));
            }
            Event::ColorRequest(index, format) => {
                let at = inbox.replies.len();
                inbox.color_queries.push((at, index, format));
            }
            // Size queries are host policy; not in 0.1.
            _ => {}
        }
    }
}

struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

fn size(cols: u16, rows: u16) -> Size {
    Size {
        cols: cols.clamp(MIN_COLS, MAX_COLS) as usize,
        rows: rows.clamp(MIN_ROWS, MAX_ROWS) as usize,
    }
}

/// The terminal engine.
pub struct Engine {
    term: Term<Listener>,
    parser: Processor,
    inbox: Listener,
    tail: snapshot::RawTail,
    // Compressed history. When on, Alacritty keeps only the newest TermHistory::RECENT rows and hands
    // older ones here.
    history: Option<TermHistory>,
    scrollback: usize,
    // With compressed history the engine owns the primary screen's view position (Alacritty's stays 0).
    offset: usize,
    // How many of the primary grid's scrolled lines the view has already followed.
    scrolled_seen: u64,
    // The view moved, so the next damage is full.
    view_moved: bool,
    // Full resets the host has been told about.
    resets_seen: u64,
    // Lines that left the top of the primary screen's scrollback, and what the count was taken from.
    trimmed: u64,
    entered_seen: u64,
    history_seen: usize,
    // Scanner state for a DECSTR (`CSI Ps ! p`) split across feeds.
    decstr: DecstrScan,
    // A DECSTR has been parsed, but a pending hook holds back its soft reset.
    decstr_owed: bool,
    // Host colours that answer colour queries; none means queries go unanswered.
    colors: Option<Box<[u32; COLOR_SLOTS]>>,
    // Recently read history rows by their History::id, most recently read last; empty while the view is at the bottom.
    row_cache: RefCell<VecDeque<CachedRow>>,
    markers: markers::Markers,
}

impl Engine {
    /// Creates a screen of `cols` × `rows` keeping up to `scrollback` lines of
    /// history. Dimensions are clamped to [`MIN_COLS`]..=[`MAX_COLS`] and
    /// [`MIN_ROWS`]..=[`MAX_ROWS`]; scrollback to [`MAX_SCROLLBACK`].
    ///
    /// History older than the newest few lines is kept compressed (text plus
    /// runs of equal attributes); reading it decodes one row at a time.
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
        Self::build(cols, rows, scrollback, true)
    }

    /// Like [`Engine::new`], but keeps the whole history uncompressed in the
    /// grid, as upstream Alacritty does. For comparison and fallback.
    pub fn new_uncompressed(cols: u16, rows: u16, scrollback: usize) -> Self {
        Self::build(cols, rows, scrollback, false)
    }

    fn build(cols: u16, rows: u16, scrollback: usize, compress: bool) -> Self {
        let scrollback = scrollback.min(MAX_SCROLLBACK);
        let compress = compress && scrollback > TermHistory::RECENT;
        let config = Config {
            scrolling_history: if compress {
                TermHistory::RECENT
            } else {
                scrollback
            },
            // Like xterm.js's default (reflowCursorLine: false), the cursor paragraph is not reflowed.
            reflow_cursor_line: false,
            // A scroll reports the scrolled row count and the new rows, not full damage.
            scroll_damage: true,
            // Like xterm.js, RIS keeps the cursor hidden.
            ris_keeps_cursor_visibility: true,
            // Like xterm.js, ED2 on the primary screen erases without pushing rows into history.
            scroll_on_erase_in_display: false,
            // Like xterm.js, line feeds and line/screen erases change line continuation.
            xterm_line_wrap: true,
            // Like xterm.js, the left half of an overwritten wide char becomes a blank in the current
            // pen, tabs leave no character in cells, and SGR 21 is a double underline.
            xterm_wide_leader: true,
            store_tabs: false,
            sgr21_double_underline: true,
            // Like xterm.js, device attributes report a VT100 with advanced video (DA1) and xterm 276 (DA2).
            xterm_device_attributes: true,
            // Like xterm.js, DECRQSS is answered.
            decrqss: true,
            // Like xterm.js, resetting any mouse tracking mode turns tracking off, and resetting an SGR encoding
            // returns to the default one.
            xterm_mouse_modes: true,
            // The engine takes OSC 52 and passes it on only with the host's consent (set_clipboard_access).
            osc52: Osc52::CopyPaste,
            ..Config::default()
        };
        let inbox = Listener::default();
        let mut term = Term::new(config, &size(cols, rows), inbox.clone());
        let history = compress.then(|| TermHistory::attach(&mut term, scrollback));
        Self {
            term,
            parser: Processor::new(),
            inbox,
            tail: Default::default(),
            history,
            scrollback,
            offset: 0,
            scrolled_seen: 0,
            resets_seen: 0,
            trimmed: 0,
            entered_seen: 0,
            history_seen: 0,
            view_moved: false,
            decstr: DecstrScan::default(),
            decstr_owed: false,
            colors: None,
            row_cache: RefCell::new(VecDeque::new()),
            markers: Default::default(),
        }
    }

    fn alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// The primary screen's grid, which holds the history (off screen while the alternate screen is up).
    fn primary(&self) -> &Grid<AlacCell> {
        if self.alt_screen() {
            self.term.inactive_grid()
        } else {
            self.term.grid()
        }
    }

    /// The compressed history; `None` on the alternate screen, where history is not visible.
    fn stored(&self) -> Option<&History> {
        let history = self.history.as_ref().filter(|_| !self.alt_screen())?;
        Some(history.stored(&self.term))
    }

    /// Moves a scrolled-back view up as far as the primary screen scrolled (as Alacritty's `display_offset` does).
    fn follow_scroll(&mut self) {
        if self.history.is_none() {
            return;
        }
        let scrolled = self.primary().scrolled_lines();
        let delta = (scrolled - self.scrolled_seen) as usize;
        self.scrolled_seen = scrolled;
        let cleared = self
            .history
            .as_ref()
            .is_some_and(|h| std::mem::take(&mut h.stored_mut(&mut self.term).cleared));
        // Alacritty returns the view to the bottom when history is cleared (without damage).
        if cleared {
            self.offset = 0;
        }
        // As in Alacritty, region scrolls move the view too, clamped to the limit rather than to the history.
        if self.offset > 0 && delta > 0 {
            self.offset = (self.offset + delta).min(self.scrollback);
            self.view_moved = true;
        }
        self.release_row_cache();
    }

    /// Frees the decoded history rows once the view is back at the bottom.
    fn release_row_cache(&mut self) {
        if self.display_offset() == 0 {
            *self.row_cache.get_mut() = VecDeque::new();
        }
    }

    /// Adds the lines that left the top of the primary scrollback since the last call: a full history dropping
    /// its oldest rows, or history cleared (ED3, RIS).
    fn count_trimmed(&mut self) {
        let entered = self.primary().history_entered();
        let now = self.primary_history_len();
        let added = (entered - self.entered_seen) as usize;
        self.trimmed += (self.history_seen + added).saturating_sub(now) as u64;
        self.entered_seen = entered;
        self.history_seen = now;
    }

    // Unlike `history_len`, also while the alternate screen is up.
    fn primary_history_len(&self) -> usize {
        self.primary().history_size()
            + self
                .history
                .as_ref()
                .map_or(0, |h| h.stored(&self.term).len())
    }

    /// Hands the row at `line` to `f`, decoding a compressed history row on the spot.
    fn with_row<R>(&self, line: i32, f: impl FnOnce(&Row<AlacCell>) -> R) -> Option<R> {
        let row = self.held_row(line)?;
        Some(f(&row))
    }

    /// The row at `line` for [`Engine::with_row`]; not generic, so its callers stay small.
    #[inline(never)]
    fn held_row(&self, line: i32) -> Option<HeldRow<'_>> {
        let grid = self.term.grid();
        if (-(grid.history_size() as i32)..self.rows() as i32).contains(&line) {
            return Some(HeldRow::Grid(&grid[Line(line)]));
        }
        let (stored, i) = self.history_row(line)?;
        // A scrolled-back view keeps the decoded rows that have extras (`cached`); the rest decode on the spot.
        if self.display_offset() > 0 {
            let id = stored.id(i);
            let mut cache = self.row_cache.borrow_mut();
            if let Some(at) = cache.iter().position(|c| c.id == id && c.row.is_some()) {
                let hit = cache.remove(at).expect("position is in range");
                cache.push_back(hit);
                drop(cache);
                return Ref::filter_map(self.row_cache.borrow(), |c| c.back()?.row.as_ref())
                    .ok()
                    .map(HeldRow::Cached);
            }
        }
        Some(HeldRow::Decoded(decode(stored, i)))
    }

    /// The compressed history and the index in it of row `line`; `None` for a grid row or past the history.
    fn history_row(&self, line: i32) -> Option<(&History, usize)> {
        let recent = self.term.grid().history_size() as i32;
        let stored = self.stored()?;
        let i = stored.len() as i32 + recent + line;
        (line < -recent && i >= 0).then_some((stored, i as usize))
    }

    /// Where history row `i` is in the row cache, which is put in it (decoded, packed) if it is not; at the back,
    /// as the least recently read goes first. A view scrolling back k lines a frame reads k new rows above rows it
    /// still shows, which must not push those out.
    fn cached(&self, stored: &History, i: usize) -> Ref<'_, CachedRow> {
        let id = stored.id(i);
        let mut cache = self.row_cache.borrow_mut();
        if let Some(at) = cache.iter().position(|c| c.id == id) {
            let hit = cache.remove(at).expect("position is in range");
            cache.push_back(hit);
        } else {
            let mut packed = Vec::new();
            while cache.len() >= self.rows() as usize + ROW_CACHE_SPARE {
                // The buffer of the row that goes takes the next one
                packed = cache.pop_front().expect("the cache is not empty").packed;
            }
            let row = decode(stored, i);
            pack_row(&row, &mut packed);
            let row = row[..].iter().any(|c| c.extra.is_some()).then_some(row);
            cache.push_back(CachedRow { id, packed, row });
        }
        drop(cache);
        Ref::map(self.row_cache.borrow(), |c| {
            c.back().expect("just put there")
        })
    }

    /// Interprets bytes written by the program (UTF-8 and escape sequences
    /// may be split across calls). Hooked sequences run their default action.
    pub fn feed(&mut self, bytes: &[u8]) {
        let mut fed = 0;
        loop {
            while self.parser.pending_hook().is_some() {
                self.finish_hook(true);
            }
            fed += self.feed_until_hook(&bytes[fed..]);
            if fed == bytes.len() && self.parser.pending_hook().is_none() {
                break;
            }
        }
    }

    /// Stops parsing at sequences of `kind` with `id` (`on`), or no longer does: [`hook_id`] of the intermediates (a
    /// private marker such as `?` first) and final byte for CSI and DCS, the number before the first `;` for OSC.
    /// Off for everything by default.
    pub fn set_hook(&mut self, kind: HookKind, id: u32, on: bool) {
        self.parser.set_hook(kind, id, on);
    }

    /// Like [`Engine::feed`], but stops right after a hooked sequence and returns how many bytes it took. The
    /// sequence's default action has not run: read it with [`Engine::pending_hook`], call [`Engine::finish_hook`],
    /// then call this again with the bytes it did not take — even none, as it first goes on with what parsing
    /// stopped in. Takes nothing while a hook is pending.
    pub fn feed_until_hook(&mut self, bytes: &[u8]) -> usize {
        if self.parser.pending_hook().is_some() {
            return 0;
        }
        // What parsing stopped in: a DECSTR's reset.
        self.settle();
        let mut fed = 0;
        while fed < bytes.len() && self.parser.pending_hook().is_none() {
            // vte drops DECSTR, so the input is split where it ends and the engine soft-resets.
            let rest = &bytes[fed..];
            let mut scan = self.decstr;
            let end = scan.find(rest);
            let head = &rest[..end.unwrap_or(rest.len())];
            let n = self.advance(head);
            fed += n;
            if n == head.len() {
                self.decstr = scan;
                self.decstr_owed = end.is_some();
                self.settle();
            } else {
                // Stopped before the DECSTR's end, so the scanner only moves over what was parsed.
                let found = self.decstr.find(&head[..n]);
                debug_assert!(found.is_none());
            }
        }
        fed
    }

    /// The hooked sequence parsing stopped at.
    pub fn pending_hook(&self) -> Option<Hook> {
        Some(match self.parser.pending_hook()? {
            VtHook::Csi { id, params } => Hook {
                kind: HookKind::Csi,
                id: *id,
                params: params.clone(),
                data: Vec::new(),
            },
            VtHook::Osc { id, params, .. } => Hook {
                kind: HookKind::Osc,
                id: *id,
                params: Vec::new(),
                data: params[1..].join(&b';'),
            },
            VtHook::Dcs { id, params, data } => Hook {
                kind: HookKind::Dcs,
                id: *id,
                params: params.clone(),
                data: data.clone(),
            },
        })
    }

    /// Runs the pending hook's default action (`run_default`) or drops it. Parsing goes on with the next
    /// [`Engine::feed_until_hook`].
    pub fn finish_hook(&mut self, run_default: bool) {
        let Some(hook) = self.parser.pending_hook() else {
            return;
        };
        if !run_default
            && matches!(
                hook,
                VtHook::Csi {
                    id: DECSTR_HOOK,
                    ..
                }
            )
        {
            self.decstr_owed = false;
        }
        self.parser.finish_hook(&mut self.term, run_default);
        self.after_parse();
    }

    /// Parses `bytes` up to a hooked sequence; returns how many it took.
    fn advance(&mut self, bytes: &[u8]) -> usize {
        let n = self.parser.advance_until_hook(&mut self.term, bytes);
        self.tail.push(&bytes[..n]);
        self.after_parse();
        n
    }

    fn after_parse(&mut self) {
        self.answer_color_queries();
        self.follow_scroll();
        self.count_trimmed();
        self.follow_markers();
    }

    /// The soft reset of a parsed DECSTR, once no hook holds it back. As in xterm.js, it also ends a
    /// synchronized update.
    fn settle(&mut self) {
        if !self.decstr_owed || self.parser.pending_hook().is_some() {
            return;
        }
        self.term.soft_reset();
        self.decstr_owed = false;
    }

    /// The host's focus, for the report xterm.js sends when the program turns on focus events (`CSI ? 1004 h`):
    /// each time the mode is set, `CSI I` (focused) or `CSI O` joins [`Engine::take_replies`] right there, in order
    /// with the other replies. `None`, the default, sends nothing, as `@xterm/headless` does. The host still sends
    /// the reports for focus changes itself.
    pub fn set_focus(&mut self, focused: Option<bool>) {
        self.term.set_focus_report(focused);
    }

    /// The host's cursor style (xterm.js `cursorStyle` and `cursorBlink` options; block, not blinking by default):
    /// what [`Engine::cursor`] reports until the program chooses one, and what DECRQSS reports for DECSCUSR, as
    /// xterm.js does even after the program chose one.
    pub fn set_cursor_default(&mut self, shape: CursorShape, blinking: bool) {
        let shape = match shape {
            CursorShape::Block => AlacCursorShape::Block,
            CursorShape::Underline => AlacCursorShape::Underline,
            CursorShape::Bar => AlacCursorShape::Beam,
        };
        self.term
            .set_default_cursor_style(CursorStyle { shape, blinking });
    }

    /// Behaves as xterm.js 6.1 does with `windowsPty` set to `pty` (the default is none): with a backend or build,
    /// more rows are added blank at the bottom instead of bringing scrollback back; with a build that is not
    /// conpty 21376 or later, resizing does not reflow; with conpty before 21376, a line feed or `CSI H` marks the
    /// cursor row as continuing the row above when that row ends in a non-blank cell.
    pub fn set_windows_pty(&mut self, pty: WindowsPty) {
        let conpty = pty.backend == Some(WindowsPtyBackend::Conpty);
        let reflow = match pty.build_number {
            // xterm.js tests the build for truth, so 0 is no build here.
            Some(build) if build != 0 => conpty && build >= CONPTY_REFLOW_BUILD,
            _ => true,
        };
        let grow_from_history = pty.backend.is_none() && pty.build_number.is_none();
        let heuristic = conpty
            && pty
                .build_number
                .is_some_and(|build| build < CONPTY_REFLOW_BUILD);
        self.term
            .set_windows_pty(reflow, grow_from_history, heuristic);
    }

    /// Changes the screen size. Damage becomes [`Damage::Full`].
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let old_cols = self.term.columns();
        let reflow = size(cols, rows).cols != old_cols;
        let anchors = self.anchor_markers(reflow);
        let alt_anchors = self.anchor_alt_markers();
        match &self.history {
            Some(history) => {
                self.offset = history.reflow(&mut self.term, size(cols, rows), self.offset);
                self.scrolled_seen = self.primary().scrolled_lines();
            }
            None => self.term.resize(size(cols, rows)),
        }
        // Reflow renumbers the history; it is not trimming.
        self.entered_seen = self.primary().history_entered();
        self.history_seen = self.primary_history_len();
        self.place_markers(anchors, reflow);
        self.place_alt_markers(alt_anchors);
        self.extend_images(old_cols);
        self.release_row_cache();
    }

    /// Lines that have left the top of the primary screen's scrollback so far (a full history dropping its oldest
    /// rows, ED3, RIS), like xterm.js `onTrim` summed up. History row `-k` is line `trimmed_lines() + history_len - k`
    /// counted from the first line ever kept, until a resize reflows the history.
    pub fn trimmed_lines(&self) -> u64 {
        self.trimmed
    }

    pub fn cols(&self) -> u16 {
        self.term.columns() as u16
    }

    pub fn rows(&self) -> u16 {
        self.term.screen_lines() as u16
    }

    /// Lines currently held in scrollback. History rows are addressed with
    /// negative line numbers: `-1` is the newest, `-(history_len)` the oldest.
    pub fn history_len(&self) -> usize {
        self.term.grid().history_size() + self.stored().map_or(0, |s| s.len())
    }

    /// Text of one row with trailing blanks removed; wide-character spacers
    /// skipped and combining marks kept. `None` if `line` is out of range.
    pub fn row_text(&self, line: i32) -> Option<String> {
        self.with_row(line, |row| {
            let mut text = String::with_capacity(row.len());
            for cell in &row[..] {
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                text.push(cell.c);
                if let Some(marks) = cell.zerowidth() {
                    text.extend(marks);
                }
            }
            text.truncate(text.trim_end().len());
            text
        })
    }

    /// Text of one row as xterm.js `translateToString` gives it: wide characters once, combining marks kept, empty
    /// cells (erased or never written) as spaces. With `trim_right` the empty cells at the end are dropped and typed
    /// spaces ([`attr::TYPED_SPACE`]) stay. `None` if `line` is out of range.
    pub fn row_string(&self, line: i32, trim_right: bool) -> Option<String> {
        self.with_row(line, |row| {
            let mut text = String::with_capacity(row.len());
            push_row_string(row, trim_right, &mut text);
            text
        })
    }

    /// [`Engine::row_string`] of rows `first..=last` (clamped to the buffer), each followed by `\n`, appended to
    /// `out` in one call. History rows are read straight from their compressed bytes, without a decode (but for
    /// rows with extras) or the row cache.
    pub fn text_rows(&self, first: i32, last: i32, trim_right: bool, out: &mut String) {
        let grid = self.term.grid();
        let recent = grid.history_size() as i32;
        let last = last.min(self.rows() as i32 - 1);
        let mut line = first.max(-recent);
        if let Some(stored) = self.stored() {
            let older = stored.len() as i32;
            line = first.max(-(recent + older));
            while line < -recent && line <= last {
                let i = (older + recent + line) as usize;
                if !stored.row_text_into(i, trim_right, out) {
                    push_row_string(&stored.row(i), trim_right, out);
                }
                out.push('\n');
                line += 1;
            }
        }
        while line <= last {
            push_row_string(&grid[Line(line)], trim_right, out);
            out.push('\n');
            line += 1;
        }
    }

    /// Whether `line` continues on the next line (soft wrap).
    pub fn row_wrapped(&self, line: i32) -> bool {
        // A history row tells from its bytes
        if let Some((stored, i)) = self.history_row(line) {
            return stored.wrapped(i);
        }
        self.with_row(line, |row| {
            row.last()
                .is_some_and(|cell| cell.flags.contains(Flags::WRAPLINE))
        })
        .unwrap_or(false)
    }

    /// Cells of one row, appended to `out` (cleared first). Returns `false`
    /// if `line` is out of range.
    pub fn row_cells(&self, line: i32, out: &mut Vec<Cell>) -> bool {
        out.clear();
        self.with_row(line, |row| out.extend(row[..].iter().map(flatten)))
            .is_some()
    }

    /// Cells of one row as `[ch, fg, bg, attrs]` words — the form handed to
    /// WebAssembly and N-API hosts. Returns `false` if `line` is out of range.
    pub fn row_packed(&self, line: i32, out: &mut Vec<u32>) -> bool {
        // A scrolled-back view redraws the same history rows every frame: they are copied from the row cache
        if let Some((stored, i)) = self.history_row(line).filter(|_| self.display_offset() > 0) {
            out.clear();
            out.extend_from_slice(&self.cached(stored, i).packed);
            return true;
        }
        let found = self.with_row(line, |row| pack_row(row, out)).is_some();
        if !found {
            out.clear();
        }
        found
    }

    /// Cells of one row that carry combining marks, an underline colour or a
    /// hyperlink, appended to `out` (cleared first). A wide character's right
    /// half lists only its hyperlink (the link covers both halves, as in
    /// xterm.js). Returns `false` if `line` is out of range.
    pub fn row_extras(&self, line: i32, out: &mut Vec<CellExtra>) -> bool {
        out.clear();
        self.with_row(line, |row| {
            for (col, cell) in row[..].iter().enumerate() {
                let spacer = cell.flags.contains(Flags::WIDE_CHAR_SPACER);
                let text = cell
                    .zerowidth()
                    .filter(|marks| !spacer && !marks.is_empty())
                    .map(|marks| {
                        std::iter::once(cell.c)
                            .chain(marks.iter().copied())
                            .collect()
                    });
                let underline_color = cell.underline_color().filter(|_| !spacer).map(pack_color);
                let hyperlink = cell.hyperlink().map(|link| Hyperlink {
                    id: link.id().to_owned(),
                    uri: link.uri().to_owned(),
                });
                if text.is_some() || underline_color.is_some() || hyperlink.is_some() {
                    out.push(CellExtra {
                        col: col as u16,
                        text,
                        underline_color,
                        hyperlink,
                    });
                }
            }
        })
        .is_some()
    }

    /// Full text of one cell including combining marks.
    pub fn cell_text(&self, line: i32, col: u16) -> Option<String> {
        self.with_row(line, |row| {
            let cell = row[..].get(col as usize)?;
            let mut text = String::from(cell.c);
            if let Some(marks) = cell.zerowidth() {
                text.extend(marks);
            }
            Some(text)
        })
        .flatten()
    }

    pub fn cursor(&self) -> Cursor {
        let point = self.term.grid().cursor.point;
        let style = self.term.cursor_style();
        let shape = match style.shape {
            AlacCursorShape::Underline => CursorShape::Underline,
            AlacCursorShape::Beam => CursorShape::Bar,
            AlacCursorShape::Block | AlacCursorShape::HollowBlock | AlacCursorShape::Hidden => {
                CursorShape::Block
            }
        };
        Cursor {
            line: point.line.0.max(0) as u16,
            col: point.column.0 as u16,
            visible: self.term.mode().contains(TermMode::SHOW_CURSOR)
                && style.shape != AlacCursorShape::Hidden,
            shape,
            blinking: style.blinking,
            styled: self.term.cursor_style_set(),
        }
    }

    /// Rows the view is scrolled back into history (0 = following output).
    /// Viewport row `y` shows line `y - display_offset` (see [`Engine::row_cells`]).
    pub fn display_offset(&self) -> usize {
        if self.history.is_some() && !self.alt_screen() {
            self.offset
        } else {
            self.term.grid().display_offset()
        }
    }

    /// Scrolls the view; positive `delta` goes back into history. Clamped to
    /// the history. Damage becomes [`Damage::Full`] when the view moves.
    pub fn scroll_display(&mut self, delta: i32) {
        if self.history.is_some() && !self.alt_screen() {
            let offset = (self.offset as i64 + delta as i64).clamp(0, self.history_len() as i64);
            self.view_moved |= offset as usize != self.offset;
            self.offset = offset as usize;
        } else {
            self.term.scroll_display(Scroll::Delta(delta));
        }
        self.release_row_cache();
    }

    /// Returns the view to the newest output.
    pub fn scroll_to_bottom(&mut self) {
        let offset = self.display_offset() as i32;
        self.scroll_display(-offset);
    }

    /// Active modes as [`mode`] bits.
    pub fn modes(&self) -> u32 {
        let m = *self.term.mode();
        let map = [
            (TermMode::SHOW_CURSOR, mode::SHOW_CURSOR),
            (TermMode::APP_CURSOR, mode::APP_CURSOR),
            (TermMode::APP_KEYPAD, mode::APP_KEYPAD),
            (TermMode::BRACKETED_PASTE, mode::BRACKETED_PASTE),
            (TermMode::MOUSE_REPORT_CLICK, mode::MOUSE_CLICK),
            (TermMode::MOUSE_DRAG, mode::MOUSE_DRAG),
            (TermMode::MOUSE_MOTION, mode::MOUSE_MOTION),
            (TermMode::SGR_MOUSE, mode::SGR_MOUSE),
            (TermMode::FOCUS_IN_OUT, mode::FOCUS_EVENTS),
            (TermMode::ALT_SCREEN, mode::ALT_SCREEN),
            (TermMode::ALTERNATE_SCROLL, mode::ALTERNATE_SCROLL),
            (TermMode::LINE_WRAP, mode::LINE_WRAP),
            (TermMode::INSERT, mode::INSERT),
            (TermMode::ORIGIN, mode::ORIGIN),
            (TermMode::MOUSE_X10, mode::MOUSE_X10),
            (TermMode::SGR_PIXELS_MOUSE, mode::SGR_PIXELS_MOUSE),
        ];
        let mut bits = map
            .iter()
            .filter(|(t, _)| m.contains(*t))
            .fold(0, |acc, (_, b)| acc | b);
        if m.intersects(TermMode::KITTY_KEYBOARD_PROTOCOL) {
            bits |= mode::KITTY_KEYBOARD;
        }
        bits
    }

    /// Viewport changes since the previous call, then resets tracking.
    pub fn take_damage(&mut self) -> Damage {
        let rows = self.term.screen_lines();
        let mut scroll = self.term.damage_scrolled() as u16;
        let moved = std::mem::take(&mut self.view_moved);
        // With compressed history Alacritty sees the view at the bottom, so lines move down by the scroll-back.
        let shift = if self.history.is_some() && !self.alt_screen() {
            self.offset
        } else {
            0
        };
        if shift > 0 {
            scroll = 0;
        }
        let damage = match self.term.damage() {
            TermDamage::Full => Damage::Full,
            _ if moved => Damage::Full,
            TermDamage::Partial(lines) => Damage::Lines {
                scroll,
                // With the view up in history, lines pushed off the screen are left out.
                lines: lines
                    .map(|b| b.line + shift)
                    .filter(|&l| l < rows)
                    .map(|l| l as u16)
                    .collect(),
            },
        };
        self.term.reset_damage();
        damage
    }

    /// Bytes the terminal wants written back to the program (device status
    /// and attribute reports, …). Send them to the PTY.
    pub fn take_replies(&mut self) -> Vec<u8> {
        let mut inbox = self.inbox.inbox();
        inbox.reply_ends.clear();
        std::mem::take(&mut inbox.replies)
    }

    /// [`Engine::take_replies`] one reply at a time, in order: xterm.js fires one `onData` per reply, and hosts
    /// that filter replies compare whole ones.
    pub fn take_reply_list(&mut self) -> Vec<Vec<u8>> {
        let mut inbox = self.inbox.inbox();
        let replies = std::mem::take(&mut inbox.replies);
        let mut start = 0;
        std::mem::take(&mut inbox.reply_ends)
            .into_iter()
            .map(|end| {
                let reply = replies.get(start..end).unwrap_or_default().to_vec();
                start = end;
                reply
            })
            .collect()
    }

    /// The new window title if it changed since the last call (`""` = reset).
    pub fn take_title(&mut self) -> Option<String> {
        self.inbox.inbox().title.take()
    }

    /// Lets the program turn on the kitty keyboard protocol (`CSI > u` and friends; off by default, so its
    /// sequences are ignored). Switching, either way, drops the flags the program pushed.
    pub fn set_kitty_keyboard(&mut self, enabled: bool) {
        self.term.set_kitty_keyboard(enabled);
    }

    /// Drops the kitty keyboard flags the program pushed, on both screens (for a host that resets them when a
    /// program exits without popping). The protocol stays enabled or disabled as it was.
    pub fn reset_kitty_keyboard(&mut self) {
        let enabled = self.term.kitty_keyboard_enabled();
        self.term.set_kitty_keyboard(enabled);
    }

    /// Active kitty keyboard flags as the protocol numbers them: 1 disambiguate escape codes, 2 report event
    /// types, 4 report alternate keys, 8 report all keys as escape codes, 16 report associated text.
    pub fn kitty_flags(&self) -> u8 {
        let m = *self.term.mode();
        [
            TermMode::DISAMBIGUATE_ESC_CODES,
            TermMode::REPORT_EVENT_TYPES,
            TermMode::REPORT_ALTERNATE_KEYS,
            TermMode::REPORT_ALL_KEYS_AS_ESC,
            TermMode::REPORT_ASSOCIATED_TEXT,
        ]
        .iter()
        .enumerate()
        .filter(|(_, f)| m.contains(**f))
        .fold(0, |acc, (i, _)| acc | 1 << i)
    }

    /// Lets the program read and/or write the host's clipboards with OSC 52.
    /// Both are off by default; requests that arrive while off are dropped.
    pub fn set_clipboard_access(&mut self, read: bool, write: bool) {
        let mut inbox = self.inbox.inbox();
        inbox.clipboard_read = read;
        inbox.clipboard_write = write;
        if !read {
            inbox.clipboard_reads.clear();
            inbox.clipboard_pending.clear();
        }
        if !write {
            inbox.clipboard_writes.clear();
        }
    }

    /// The oldest text the program asked to put on a clipboard (OSC 52) that
    /// the host has not taken yet. Invalid base64 is ignored.
    pub fn take_clipboard_write(&mut self) -> Option<(Clipboard, String)> {
        self.inbox.inbox().clipboard_writes.pop_front()
    }

    /// The oldest clipboard read (OSC 52 `?`) the host has not taken yet.
    /// Answer it with [`Engine::reply_clipboard`].
    pub fn take_clipboard_read(&mut self) -> Option<Clipboard> {
        self.inbox.inbox().clipboard_reads.pop_front()
    }

    /// Answers the oldest unanswered read of `clipboard` with `text`. The
    /// reply joins [`Engine::take_replies`] as xterm.js's clipboard addon
    /// sends it (`OSC 52 ; Pc ; base64 BEL`). Returns `false` when no read of
    /// that clipboard is waiting.
    pub fn reply_clipboard(&mut self, clipboard: Clipboard, text: &str) -> bool {
        let mut inbox = self.inbox.inbox();
        let at = inbox
            .clipboard_pending
            .iter()
            .position(|(c, _)| *c == clipboard);
        let Some((_, format)) = at.and_then(|at| inbox.clipboard_pending.remove(at)) else {
            return false;
        };
        let mut reply = format(text);
        if let Some(body) = reply.strip_suffix("\x1b\\") {
            reply = format!("{body}\x07");
        }
        inbox.reply(reply.as_bytes());
        true
    }

    /// Colours that answer OSC 4/10/11/12 queries: [`COLOR_SLOTS`] entries
    /// (palette 0–255, then foreground, background, cursor) as `0xRRGGBB`. A
    /// colour the program itself set with OSC 4/10/11/12 answers instead.
    /// `None`, the default, leaves the queries unanswered, as
    /// `@xterm/headless` does.
    pub fn set_colors(&mut self, colors: Option<[u32; COLOR_SLOTS]>) {
        self.colors = colors.map(Box::new);
    }

    // Inserts answers to the queued colour queries where each query arrived. Working from the back keeps
    // the earlier positions valid.
    fn answer_color_queries(&mut self) {
        let listener = self.inbox.clone();
        let mut inbox = listener.inbox();
        let queries = std::mem::take(&mut inbox.color_queries);
        let Some(colors) = &self.colors else { return };
        for (at, index, format) in queries.into_iter().rev() {
            let host = colors.get(index).map(|&c| Rgb {
                r: (c >> 16) as u8,
                g: (c >> 8) as u8,
                b: c as u8,
            });
            let Some(rgb) = self.term.colors()[index].or(host) else {
                continue;
            };
            let mut reply = format(rgb);
            // xterm.js answers with ST even when the query ended with BEL.
            if let Some(body) = reply.strip_suffix('\x07') {
                reply = format!("{body}\x1b\\");
            }
            inbox.insert_reply(at, reply.into_bytes());
        }
    }

    /// Full resets (RIS, `ESC c`) the program sent since the last call, so the host can drop what it keeps about the
    /// screen (selection).
    pub fn take_resets(&mut self) -> u32 {
        let now = self.term.resets();
        let n = now - self.resets_seen;
        self.resets_seen = now;
        n as u32
    }

    /// Bells (`BEL`) received since the last call.
    pub fn take_bells(&mut self) -> u32 {
        std::mem::take(&mut self.inbox.inbox().bells)
    }

    /// A synchronized update (`CSI ? 2026 h`) is open. Output is applied at
    /// once, as in xterm.js (the rows read the new content); a host that
    /// draws should not take frames meanwhile, and call [`Engine::flush_sync`]
    /// if the update is still open after [`SYNC_TIMEOUT`]. RIS and DECSTR end
    /// it too.
    pub fn sync_pending(&self) -> bool {
        self.term.mode().contains(TermMode::SYNC_UPDATE)
    }

    /// Ends an open synchronized update, as its timeout does in xterm.js.
    pub fn flush_sync(&mut self) {
        self.term
            .unset_private_mode(NamedPrivateMode::SyncUpdate.into());
    }

    /// Bytes that rebuild this screen in a fresh xterm.js terminal of the same
    /// size and scrollback: history, both buffers, cursor (pending wrap
    /// included), pen and modes, followed — raw — by the escape sequence or
    /// UTF-8 character the output ended in the middle of, so the program's
    /// next bytes continue there as they do here.
    ///
    /// During a synchronized update `CSI ? 2026 h` comes before that raw end,
    /// so the receiver is inside the same update and ends it on the same
    /// bytes.
    ///
    /// The engine is not changed (no damage, the update stays open), so it is
    /// safe on an engine being drawn, and calling this twice gives the same
    /// bytes.
    ///
    /// On a [`SnapshotRefusal`] these bytes could not reproduce the screen;
    /// replay the raw output instead.
    pub fn ansi_snapshot(&self) -> Result<Vec<u8>, SnapshotRefusal> {
        self.ansi_snapshot_with(SnapshotOptions::default())
    }

    /// [`Engine::ansi_snapshot`] with less history or without the modes, as `@xterm/addon-serialize` offers
    /// (`{ scrollback: 0, excludeModes: true }` paints just the screen the terminal shows).
    pub fn ansi_snapshot_with(&self, options: SnapshotOptions) -> Result<Vec<u8>, SnapshotRefusal> {
        let history = self.history.as_ref().map(|h| h.stored(&self.term));
        snapshot::ansi(
            &self.term,
            self.sync_pending(),
            &self.tail,
            history,
            options,
        )
    }
}

/// Byte-level scanner for `ESC [ Ps ! p` (DECSTR). Its state carries across calls.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum DecstrScan {
    #[default]
    Ground,
    Esc,
    Params,
    Bang,
}

impl DecstrScan {
    /// Where a DECSTR ends (just past its last byte), or `None` after advancing the state through `bytes`.
    ///
    /// Every DECSTR has a `!`, so from the ground state only the `!`s are looked at (a DECSTR is `ESC [`, then
    /// digits and `;`, `!` and `p`, with nothing in between); without one, the state is that after the last ESC.
    fn find(&mut self, bytes: &[u8]) -> Option<usize> {
        // A sequence carried in from the last call: step through it until it ends.
        let mut i = 0;
        while *self != DecstrScan::Ground {
            let b = *bytes.get(i)?;
            i += 1;
            if self.step(b) {
                return Some(i);
            }
        }
        let mut from = i;
        while let Some(at) = memchr::memchr(b'!', &bytes[from..]) {
            let bang = from + at;
            let params = bytes[i..bang]
                .iter()
                .rposition(|b| !matches!(b, b'0'..=b'9' | b';'))
                .map_or(i, |k| i + k + 1);
            if params >= i + 2 && bytes[params - 2..params] == [0x1b, b'['] {
                match bytes.get(bang + 1) {
                    Some(b'p') => return Some(bang + 2),
                    Some(_) => {}
                    None => {
                        *self = DecstrScan::Bang;
                        return None;
                    }
                }
            }
            from = bang + 1;
        }
        // No DECSTR here: only the bytes from the last ESC on decide the state.
        let esc = i + memchr::memrchr(0x1b, &bytes[i..])?;
        for &b in &bytes[esc..] {
            self.step(b);
            if *self == DecstrScan::Ground {
                break;
            }
        }
        None
    }

    /// Advances the state by one byte; true where a DECSTR ends.
    fn step(&mut self, b: u8) -> bool {
        *self = match (*self, b) {
            (_, 0x1b) => DecstrScan::Esc,
            (DecstrScan::Esc, b'[') => DecstrScan::Params,
            (DecstrScan::Params, b'0'..=b'9' | b';') => DecstrScan::Params,
            (DecstrScan::Params, b'!') => DecstrScan::Bang,
            (DecstrScan::Bang, b'p') => {
                *self = DecstrScan::Ground;
                return true;
            }
            _ => DecstrScan::Ground,
        };
        false
    }
}

/// `row`'s cells as [`Engine::row_packed`] gives them, into `out`.
fn pack_row(row: &Row<AlacCell>, out: &mut Vec<u32>) {
    // Every word is overwritten, so a buffer as long as the last row is not cleared first
    out.resize(row.len() * Cell::PACKED_WORDS, 0);
    for (cell, o) in row[..].iter().zip(out.chunks_exact_mut(Cell::PACKED_WORDS)) {
        o[0] = cell.c as u32;
        o[1] = pack_color(cell.fg);
        o[2] = pack_color(cell.bg);
        o[3] = attrs(cell);
    }
}

/// [`Engine::row_string`] of `row`, appended to `text`.
fn push_row_string(row: &Row<AlacCell>, trim_right: bool, text: &mut String) {
    let mut content = text.len();
    for cell in &row[..] {
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        text.push(if cell.c == '\t' { ' ' } else { cell.c });
        let marks = cell.zerowidth().unwrap_or_default();
        text.extend(marks);
        if cell.c != ' ' || cell.flags.contains(Flags::TYPED_SPACE) || !marks.is_empty() {
            content = text.len();
        }
    }
    if trim_right {
        text.truncate(content);
    }
}

/// The [`attr`] bits of the cell flags (`flag_attrs_match_each_flag` checks every flag): two table lookups, which
/// make a scrolled-back frame about 15% faster natively than moving the bits with shifts and masks, for 2 KiB.
fn flag_attrs(f: Flags) -> u32 {
    let b = f.bits();
    // TYPED_SPACE (bit 16) is the only flag above the two bytes
    FLAG_ATTRS[0][(b & 0xff) as usize]
        | FLAG_ATTRS[1][(b >> 8 & 0xff) as usize]
        | (b << 2 & attr::TYPED_SPACE)
}

/// [`flag_attrs`] of each value of the flags' low byte, then of their second byte.
static FLAG_ATTRS: [[u32; 256]; 2] = [flag_attr_table(0), flag_attr_table(8)];

const fn flag_attr_table(shift: u32) -> [u32; 256] {
    let pairs = [
        (Flags::BOLD, attr::BOLD),
        (Flags::DIM, attr::DIM),
        (Flags::ITALIC, attr::ITALIC),
        (Flags::UNDERLINE, attr::UNDERLINE),
        (Flags::DOUBLE_UNDERLINE, attr::DOUBLE_UNDERLINE),
        (Flags::UNDERCURL, attr::CURLY_UNDERLINE),
        (Flags::DOTTED_UNDERLINE, attr::DOTTED_UNDERLINE),
        (Flags::DASHED_UNDERLINE, attr::DASHED_UNDERLINE),
        (Flags::INVERSE, attr::INVERSE),
        (Flags::HIDDEN, attr::HIDDEN),
        (Flags::STRIKEOUT, attr::STRIKEOUT),
        (Flags::BLINK, attr::BLINK),
        (Flags::WIDE_CHAR, attr::WIDE),
        (Flags::WIDE_CHAR_SPACER, attr::WIDE_SPACER),
        (Flags::LEADING_WIDE_CHAR_SPACER, attr::LEADING_WIDE_SPACER),
    ];
    let mut table = [0; 256];
    let mut byte = 0;
    while byte < 256 {
        let bits = (byte as u32) << shift;
        let mut k = 0;
        while k < pairs.len() {
            if bits & pairs[k].0.bits() != 0 {
                table[byte] |= pairs[k].1;
            }
            k += 1;
        }
        byte += 1;
    }
    table
}

fn flatten(cell: &AlacCell) -> Cell {
    Cell {
        ch: cell.c,
        fg: pack_color(cell.fg),
        bg: pack_color(cell.bg),
        attrs: attrs(cell),
    }
}

/// The [`attr`] bits of a cell.
#[inline]
fn attrs(cell: &AlacCell) -> u32 {
    let mut attrs = flag_attrs(cell.flags);
    // Most cells have no extras
    if cell.extra.is_some() {
        if cell.zerowidth().is_some_and(|z| !z.is_empty()) {
            attrs |= attr::COMBINING;
        }
        if cell.underline_color().is_some() {
            attrs |= attr::UNDERLINE_COLOR;
        }
        if cell.hyperlink().is_some() {
            attrs |= attr::HYPERLINK;
        }
        if cell.image().is_some() {
            attrs |= attr::IMAGE;
        }
    }
    attrs
}

/// Packs an Alacritty colour into the [`color`] format.
pub fn pack_color(c: Color) -> u32 {
    match c {
        Color::Named(n) => color::NAMED | n as u32,
        Color::Indexed(i) => color::INDEXED | i as u32,
        Color::Spec(rgb) => color::RGB | (rgb.r as u32) << 16 | (rgb.g as u32) << 8 | rgb.b as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::DecstrScan;
    use crate::attr;
    use crate::vt::term::cell::Flags;

    #[test]
    fn flag_attrs_match_each_flag() {
        let pairs = [
            (Flags::BOLD, attr::BOLD),
            (Flags::DIM, attr::DIM),
            (Flags::ITALIC, attr::ITALIC),
            (Flags::UNDERLINE, attr::UNDERLINE),
            (Flags::DOUBLE_UNDERLINE, attr::DOUBLE_UNDERLINE),
            (Flags::UNDERCURL, attr::CURLY_UNDERLINE),
            (Flags::DOTTED_UNDERLINE, attr::DOTTED_UNDERLINE),
            (Flags::DASHED_UNDERLINE, attr::DASHED_UNDERLINE),
            (Flags::INVERSE, attr::INVERSE),
            (Flags::HIDDEN, attr::HIDDEN),
            (Flags::STRIKEOUT, attr::STRIKEOUT),
            (Flags::BLINK, attr::BLINK),
            (Flags::WIDE_CHAR, attr::WIDE),
            (Flags::WIDE_CHAR_SPACER, attr::WIDE_SPACER),
            (Flags::LEADING_WIDE_CHAR_SPACER, attr::LEADING_WIDE_SPACER),
            (Flags::TYPED_SPACE, attr::TYPED_SPACE),
        ];
        // Every combination of the 17 flag bits, WRAPLINE (no attribute) among them
        for bits in 0..1u32 << 17 {
            let flags = Flags::from_bits_retain(bits);
            let want = pairs
                .iter()
                .filter(|(f, _)| flags.contains(*f))
                .fold(0, |acc, (_, a)| acc | a);
            assert_eq!(super::flag_attrs(flags), want, "flags {bits:#x}");
        }
    }

    fn decodes() -> usize {
        super::DECODES.with(std::cell::Cell::get)
    }

    #[test]
    fn scrolling_back_decodes_each_row_that_comes_into_view_once() {
        let rows = 24;
        let mut t = super::Engine::new(80, rows, 3000);
        for i in 0..3000 {
            t.feed(format!("\x1b[3{}mline {i}\x1b[0m\r\n", i % 8).as_bytes());
        }
        let mut packed = Vec::new();
        let mut frame = |t: &super::Engine| {
            for y in 0..rows as i32 {
                assert!(t.row_packed(y - t.display_offset() as i32, &mut packed));
            }
        };
        // Up by lines (a wheel), down by lines, then up by pages
        for step in [1, 3, -3, rows as i32] {
            t.scroll_to_bottom();
            t.scroll_display(if step < 0 { 2500 } else { 100 });
            frame(&t);
            let before = decodes();
            for _ in 0..60 {
                t.scroll_display(step);
                frame(&t);
            }
            assert_eq!(
                decodes() - before,
                60 * step.unsigned_abs() as usize,
                "step {step}"
            );
        }
    }

    /// A scrolled-back renderer reads every row's cells and the extras of rows that have some: each history row is
    /// decoded once, as it comes into view, and its cached cells are the decoded row's.
    #[test]
    fn a_scrolled_back_frame_decodes_only_the_rows_that_come_into_view() {
        let rows = 10;
        let mut t = super::Engine::new(40, rows, 500);
        for i in 0..400 {
            let extra = ["", "e\u{301} ", "\x1b]8;;https://x\x07L\x1b]8;;\x07 "][i % 3];
            t.feed(format!("{extra}line {i}\r\n").as_bytes());
        }
        let (mut packed, mut extras) = (Vec::new(), Vec::new());
        let frame =
            |t: &super::Engine, packed: &mut Vec<u32>, extras: &mut Vec<crate::CellExtra>| {
                let mut seen = Vec::new();
                for y in 0..rows as i32 {
                    let line = y - t.display_offset() as i32;
                    assert!(t.row_packed(line, packed));
                    extras.clear();
                    if packed
                        .chunks(4)
                        .any(|c| c[3] & (crate::attr::COMBINING | crate::attr::HYPERLINK) != 0)
                    {
                        assert!(t.row_extras(line, extras) && !extras.is_empty());
                    }
                    seen.push((line, packed.clone(), extras.clone()));
                }
                seen
            };
        t.scroll_display(200);
        let mut seen = frame(&t, &mut packed, &mut extras);
        for _ in 0..30 {
            t.scroll_display(1);
            let before = decodes();
            seen = frame(&t, &mut packed, &mut extras);
            assert_eq!(decodes() - before, 1);
        }
        // At the bottom nothing is cached: these reads decode
        t.scroll_to_bottom();
        for (line, cells, cell_extras) in &seen {
            assert!(t.row_packed(*line, &mut packed) && t.row_extras(*line, &mut extras));
            assert_eq!((&packed, &extras), (cells, cell_extras), "line {line}");
        }
    }

    /// The byte-at-a-time scanner `DecstrScan::find` must agree with.
    fn find_bytewise(state: &mut DecstrScan, bytes: &[u8]) -> Option<usize> {
        for (i, &b) in bytes.iter().enumerate() {
            if state.step(b) {
                return Some(i + 1);
            }
        }
        None
    }

    #[test]
    fn decstr_scan_matches_bytewise_scan() {
        let pieces: [&[u8]; 12] = [
            b"\x1b",
            b"[",
            b"!",
            b"p",
            b"1",
            b";",
            b"x",
            b"\x1b[",
            b"!p",
            b"\x1b[2!p",
            b"ok ",
            b"[!",
        ];
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for _ in 0..20_000 {
            let mut bytes = Vec::new();
            for _ in 0..next(12) {
                bytes.extend_from_slice(pieces[next(pieces.len())]);
            }
            let start = [
                DecstrScan::Ground,
                DecstrScan::Esc,
                DecstrScan::Params,
                DecstrScan::Bang,
            ][next(4)];
            let (mut fast, mut slow) = (start, start);
            assert_eq!(
                fast.find(&bytes),
                find_bytewise(&mut slow, &bytes),
                "{start:?} {bytes:?}"
            );
            assert_eq!(fast, slow, "{start:?} {bytes:?}");
        }
    }
}
