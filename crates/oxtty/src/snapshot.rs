//! ANSI snapshot: turns the engine screen into "bytes that produce the same screen when written to an empty xterm.js".
//!
//! Origin: ported from `core/wmux-term/src/native_serialize.rs` in wmux (openwong2kim/wmux, MIT, written by the owner)
//! (the serializer), plus the corrections the host used to fill in, `src/daemon/RustSnapshot.ts` and
//! `src/daemon/util/ansiStreamScan.ts` (incomplete escape tail, UTF-8 carry, margin rejection).
//! The same author redistributes it under Apache-2.0 in oxtty.
//!
//! Output = serialize(grid) ‖ raw tail. During sync: serialize(grid) ‖ `CSI ?2026h` ‖ raw tail.
//! Cursor visibility carries the engine state (`SHOW_CURSOR`) as is — the vt option
//! `ris_keeps_cursor_visibility` aligns RIS behavior with xterm.js.
use crate::history::History;
use crate::vt::event::EventListener;
use crate::vt::grid::{Dimensions, Grid, Row};
use crate::vt::index::{Column, Line};
use crate::vt::term::cell::{Cell, Flags, Hyperlink};
use crate::vt::term::{Term, TermMode};
use crate::vt::vte::ansi::{Color, NamedColor};
use std::fmt::{self, Write};

/// Why [`crate::Engine::ansi_snapshot`] cannot rebuild the screen; replay the
/// raw output instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SnapshotRefusal {
    /// A fullwidth glyph is missing one of its halves: it starts in the last
    /// column or lost its right half to a resize, or an insert left its right
    /// half alone. Printing it again would wrap or fill both halves instead.
    WideEdge,
    /// Scroll margins (DECSTBM) narrower than the screen are set; the
    /// snapshot does not carry them.
    Margins,
    /// The output ends inside an escape sequence longer than 4096 bytes.
    TailOverflow,
}

impl SnapshotRefusal {
    /// The reason as wmux reports it: `"unsupported-wide-edge"`, `"margins"`
    /// or `"partial-tail-overflow"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WideEdge => "unsupported-wide-edge",
            Self::Margins => "margins",
            Self::TailOverflow => "partial-tail-overflow",
        }
    }
}

impl fmt::Display for SnapshotRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::error::Error for SnapshotRefusal {}

/// What [`crate::Engine::ansi_snapshot_with`] leaves out, like `@xterm/addon-serialize` options.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SnapshotOptions {
    /// Keep only the newest this many history lines (`Some(0)`: the screen alone); `None` keeps them all.
    pub scrollback: Option<usize>,
    /// Leave out the modes (cursor keys, keypad, mouse, focus reports, bracketed paste, insert, origin, autowrap
    /// off, cursor visibility) and what continues the output (the unfinished sequence at the end, a synchronized
    /// update in progress), so the bytes only paint the screen. Scroll margins are then not refused.
    pub exclude_modes: bool,
}

/// Maximum length of an incomplete sequence to carry over (wmux `MAX_PENDING_TAIL_CHARS`).
const MAX_PENDING_TAIL: usize = 4096;

/// The raw end of what `feed` received. vte does not give back the parser's incomplete state as raw bytes, so the engine keeps it.
///
/// ESC starts a new sequence from any state (common to vte and xterm), so the tail always starts at the
/// last ESC in the stream. So each feed scans only what follows the last ESC.
#[derive(Default)]
pub(crate) struct RawTail {
    state: Scan,
    /// Raw incomplete sequence (including ESC). On overflow it is cleared and `overflowed` is set.
    escape: Vec<u8>,
    overflowed: bool,
    /// Up to the last 3 bytes of the stream — for computing the incomplete UTF-8 suffix.
    last: Vec<u8>,
}

/// wmux `ScanState`. An ESC inside a string (OSC/DCS/SOS/PM/APC) proceeds the same as `Esc`, whether it is ST or an abort.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Scan {
    #[default]
    Ground,
    Esc,
    EscIntermediate,
    Csi,
    String,
}

impl RawTail {
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        let start = match bytes.iter().rposition(|&b| b == 0x1b) {
            Some(at) => {
                self.state = Scan::Ground;
                at
            }
            None if self.state == Scan::Ground => bytes.len(),
            None => 0,
        };
        for &b in &bytes[start..] {
            let was = self.state;
            self.state = step(was, b);
            if self.state == Scan::Ground {
                // This is after the last ESC, so the remaining bytes are plain text.
                self.escape.clear();
                self.overflowed = false;
                break;
            }
            if was == Scan::Ground {
                self.escape.clear();
                self.overflowed = false;
            }
            // A C0 inside a sequence (the LF in `\x1b[31\n`) has already been executed here. Dropping it so it does not run again leaves the parameter state the same.
            if b < 0x20 && b != 0x1b {
                continue;
            }
            if !self.overflowed {
                if self.escape.len() >= MAX_PENDING_TAIL {
                    self.overflowed = true;
                    self.escape.clear();
                } else {
                    self.escape.push(b);
                }
            }
        }
        let keep = self.last.len().min(3 - bytes.len().min(3));
        self.last.drain(..self.last.len() - keep);
        self.last
            .extend_from_slice(&bytes[bytes.len().saturating_sub(3)..]);
    }

    /// Raw bytes to append to the payload: the incomplete sequence (including any incomplete UTF-8 in it), or else the incomplete UTF-8 suffix.
    fn pending(&self) -> Result<&[u8], SnapshotRefusal> {
        if self.state != Scan::Ground {
            return if self.overflowed {
                Err(SnapshotRefusal::TailOverflow)
            } else {
                Ok(&self.escape)
            };
        }
        Ok(&self.last[self.last.len() - incomplete_utf8_suffix_len(&self.last)..])
    }
}

/// Byte version of wmux `PartialSequenceTracker.advance`.
fn step(state: Scan, b: u8) -> Scan {
    if b == 0x1b {
        return Scan::Esc;
    }
    match state {
        Scan::Ground => Scan::Ground,
        Scan::Esc => match b {
            b'[' => Scan::Csi,
            b']' | b'P' | b'X' | b'^' | b'_' => Scan::String,
            0x20..=0x2f => Scan::EscIntermediate,
            _ => Scan::Ground,
        },
        Scan::EscIntermediate => match b {
            0x20..=0x2f => Scan::EscIntermediate,
            _ => Scan::Ground,
        },
        Scan::Csi => match b {
            0x40..=0x7e | 0x18 | 0x1a => Scan::Ground,
            // Parameter and intermediate bytes, and C0s that do not end the sequence.
            _ => Scan::Csi,
        },
        Scan::String => match b {
            0x07 | 0x18 | 0x1a => Scan::Ground,
            _ => Scan::String,
        },
    }
}

/// Number of incomplete UTF-8 bytes at the end (0~3). wmux `incompleteUtf8SuffixLength`.
fn incomplete_utf8_suffix_len(buf: &[u8]) -> usize {
    // vte keeps exactly what `from_utf8` calls an unfinished character at the end; bytes that can
    // never start a character (E0 80, a lone continuation) were already handled as invalid.
    (1..=buf.len().min(3))
        .rev()
        .find(|&n| {
            std::str::from_utf8(&buf[buf.len() - n..])
                .is_err_and(|e| e.valid_up_to() == 0 && e.error_len().is_none())
        })
        .unwrap_or(0)
}

/// The body of [`crate::Engine::ansi_snapshot_with`]. The rejection order is the same as wmux: margins → tail overflow → wide edge.
pub(crate) fn ansi<T: EventListener>(
    term: &Term<T>,
    sync: bool,
    tail: &RawTail,
    older: Option<&History>,
    options: SnapshotOptions,
) -> Result<Vec<u8>, SnapshotRefusal> {
    let region = term.scroll_region();
    if !options.exclude_modes && (region.start.0 != 0 || region.end.0 != term.screen_lines() as i32)
    {
        return Err(SnapshotRefusal::Margins);
    }
    let tail = if options.exclude_modes {
        &[][..]
    } else {
        tail.pending()?
    };
    let mut out = serialize(term, older, options)?;
    if sync && !options.exclude_modes {
        out.push_str("\x1b[?2026h");
    }
    let mut bytes = out.into_bytes();
    bytes.extend_from_slice(tail);
    Ok(bytes)
}

// From here on, code ported from wmux native_serialize.rs. Changes: listener generic, rejection enum,
// tab cells as spaces, DECOM before the cursor.

fn color(out: &mut String, value: Color, foreground: bool) {
    let base = if foreground { 38 } else { 48 };
    match value {
        Color::Spec(rgb) => {
            let _ = write!(out, ";{base};2;{};{};{}", rgb.r, rgb.g, rgb.b);
        }
        Color::Indexed(index) => {
            let _ = write!(out, ";{base};5;{index}");
        }
        Color::Named(NamedColor::Foreground | NamedColor::Background) => {}
        Color::Named(value) => {
            let index = value as u32;
            if index < 8 {
                let _ = write!(out, ";{}", (if foreground { 30 } else { 40 }) + index);
            } else if index < 16 {
                let _ = write!(out, ";{}", (if foreground { 90 } else { 100 }) + index - 8);
            }
        }
    }
}

/// The attributes [`style`] writes. Cells with the same pen get the same SGR.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Pen {
    flags: Flags,
    fg: Color,
    bg: Color,
    underline: Option<Color>,
}

impl Pen {
    fn of(cell: &Cell) -> Self {
        let sgr = Flags::BOLD
            | Flags::DIM
            | Flags::ITALIC
            | Flags::BLINK
            | Flags::INVERSE
            | Flags::HIDDEN
            | Flags::STRIKEOUT
            | Flags::ALL_UNDERLINES;
        Self {
            flags: cell.flags & sgr,
            fg: cell.fg,
            bg: cell.bg,
            underline: cell.underline_color(),
        }
    }
}

fn style(out: &mut String, cell: &Cell) {
    out.push_str("\x1b[0");
    for (flag, code) in [
        (Flags::BOLD, 1),
        (Flags::DIM, 2),
        (Flags::ITALIC, 3),
        (Flags::BLINK, 5),
        (Flags::INVERSE, 7),
        (Flags::HIDDEN, 8),
        (Flags::STRIKEOUT, 9),
    ] {
        if cell.flags.contains(flag) {
            let _ = write!(out, ";{code}");
        }
    }
    let underline = if cell.flags.contains(Flags::DOUBLE_UNDERLINE) {
        Some(2)
    } else if cell.flags.contains(Flags::UNDERCURL) {
        Some(3)
    } else if cell.flags.contains(Flags::DOTTED_UNDERLINE) {
        Some(4)
    } else if cell.flags.contains(Flags::DASHED_UNDERLINE) {
        Some(5)
    } else if cell.flags.contains(Flags::UNDERLINE) {
        Some(1)
    } else {
        None
    };
    if let Some(kind) = underline {
        let _ = write!(out, ";4:{kind}");
    }
    color(out, cell.fg, true);
    color(out, cell.bg, false);
    if let Some(value) = cell.underline_color() {
        match value {
            Color::Spec(rgb) => {
                let _ = write!(out, ";58:2::{}:{}:{}", rgb.r, rgb.g, rgb.b);
            }
            Color::Indexed(index) => {
                let _ = write!(out, ";58:5:{index}");
            }
            Color::Named(_) => {}
        }
    }
    out.push('m');
}

/// Opens `link` or closes the open one. A program's own id is kept so separate runs of one
/// link stay one link; an id Alacritty generated is left out, and the receiver generates its
/// own (a new run starts wherever the link changes, so links stay apart).
fn link(out: &mut String, link: Option<&Hyperlink>) {
    match link {
        Some(link) if link.id().ends_with("_alacritty") => {
            let _ = write!(out, "\x1b]8;;{}\x1b\\", link.uri());
        }
        Some(link) => {
            let _ = write!(out, "\x1b]8;id={};{}\x1b\\", link.id(), link.uri());
        }
        None => out.push_str("\x1b]8;;\x1b\\"),
    }
}

fn glyph(out: &mut String, cell: &Cell) {
    // Writing the '\t' left in the first cell a tab passed over as is would make xterm execute it as a tab again.
    out.push(if cell.c == '\t' { ' ' } else { cell.c });
    if let Some(chars) = cell.zerowidth() {
        out.extend(chars);
    }
}

fn cursor(out: &mut String, grid: &Grid<Cell>) {
    let point = grid.cursor.point;
    let mut col = point.column.0;
    if grid.cursor.input_needs_wrap
        && grid[point.line][Column(col)]
            .flags
            .contains(Flags::WIDE_CHAR_SPACER)
    {
        col = col.saturating_sub(1);
    }
    let _ = write!(out, "\x1b[{};{}H", point.line.0 + 1, col + 1);
    // CUP clears the pending wrap at line end, so rewrite the edge cell to restore that state.
    if grid.cursor.input_needs_wrap {
        let cell = &grid[point.line][Column(col)];
        style(out, cell);
        let edge = cell.hyperlink();
        if edge.is_some() {
            link(out, edge.as_ref());
        }
        glyph(out, cell);
        if edge.is_some() {
            link(out, None);
        }
    }
    style(out, &grid.cursor.template);
    // The program may be in the middle of writing a link.
    if let Some(open) = grid.cursor.template.hyperlink() {
        link(out, Some(&open));
    }
}

/// A blank that was erased or never written, with at most a background: xterm.js holds nothing there, so it is not
/// printed (a printed space would be text). A blank with more attributes is printed, as an erase keeps only the
/// background.
fn erased(cell: &Cell) -> bool {
    cell.c == ' '
        && cell.fg == Color::Named(NamedColor::Foreground)
        && (cell.flags - Flags::WRAPLINE - Flags::LEADING_WIDE_CHAR_SPACER).is_empty()
        && cell.extra.is_none()
}

/// Writes rows one after another, keeping the pen and link between them.
struct Rows {
    /// The pen of the last SGR written; `None` writes the next one.
    previous_pen: Option<Pen>,
    previous_style: String,
    /// Where the next SGR is built, to compare with the last one.
    next_style: String,
    previous_link: Option<Hyperlink>,
    columns: usize,
}

impl Rows {
    fn pen(&mut self, out: &mut String, cell: &Cell) {
        let pen = Pen::of(cell);
        if self.previous_pen == Some(pen) {
            return;
        }
        self.previous_pen = Some(pen);
        // Pens can differ and still write the same SGR (colours it leaves out).
        self.next_style.clear();
        style(&mut self.next_style, cell);
        if self.next_style != self.previous_style {
            out.push_str(&self.next_style);
            std::mem::swap(&mut self.next_style, &mut self.previous_style);
        }
    }

    /// The blank pen of background `bg`, which an erase (ECH) fills cells with.
    fn blank_pen(&mut self, out: &mut String, bg: Color) {
        let blank = Cell {
            bg,
            ..Cell::default()
        };
        self.pen(out, &blank);
    }

    /// Moves over `n` erased cells of background `bg` without printing them, erasing them when coloured.
    fn skip(&mut self, out: &mut String, n: usize, bg: Color) {
        if bg != Color::Named(NamedColor::Background) {
            self.blank_pen(out, bg);
            let _ = write!(out, "\x1b[{n}X");
        }
        let _ = write!(out, "\x1b[{n}C");
    }

    /// `next` is the first cell of the row below, which a wrapped row runs on into.
    fn row(&mut self, out: &mut String, line: &Row<Cell>, next: Option<&Cell>, last: bool) {
        // Erased cells not written yet, all with the same background
        let (mut gap, mut gap_bg) = (0, Color::Named(NamedColor::Background));
        let mut used = 0;
        for cell in &line[..] {
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                used += 1;
                continue;
            }
            if erased(cell) {
                if gap > 0 && cell.bg != gap_bg {
                    self.skip(out, gap, gap_bg);
                    used += gap;
                    gap = 0;
                }
                gap += 1;
                gap_bg = cell.bg;
                continue;
            }
            if gap > 0 {
                self.skip(out, gap, gap_bg);
                used += gap;
                gap = 0;
            }
            self.pen(out, cell);
            let next_link = cell.hyperlink();
            if next_link != self.previous_link {
                link(out, next_link.as_ref());
                self.previous_link = next_link;
            }
            glyph(out, cell);
            used += 1;
        }
        let wrapped = line[Column(self.columns - 1)]
            .flags
            .contains(Flags::WRAPLINE);
        if last || !wrapped {
            // Erased cells at the end are only written for their colour
            if gap > 0 && gap_bg != Color::Named(NamedColor::Background) {
                self.blank_pen(out, gap_bg);
                let _ = write!(out, "\x1b[{gap}X");
            }
            if !last {
                // Clear attributes before scrolling. Otherwise a colored blank template
                // paints cells outside the serialized content.
                out.push_str("\x1b[0m\r\n");
                self.previous_pen = None;
                self.previous_style.clear();
            }
            return;
        }
        if gap == 0 && !next.is_some_and(erased) {
            // The next row's first character wraps on its own
            return;
        }
        // Only printing past the last column wraps, so print over the erased end and one more cell, then erase
        // what was printed (xterm.js SerializeAddon `_rowEnd` does the same)
        if self.previous_link.is_some() {
            link(out, None);
            self.previous_link = None;
        }
        self.pen(out, &Cell::default());
        out.push_str(&"-".repeat(gap + 1));
        out.push_str("\x1b[D");
        let next_bg = next.map_or(Color::Named(NamedColor::Background), |c| c.bg);
        self.blank_pen(out, next_bg);
        out.push_str("\x1b[X");
        if gap > 0 {
            let _ = write!(out, "\x1b[A\x1b[{}G", used + 1);
            self.blank_pen(out, gap_bg);
            let _ = write!(out, "\x1b[{gap}X\x1b[B\r");
        }
    }
}

/// `older` is the compressed history older than this grid (primary screen only); the oldest `skip` history rows
/// are left out.
fn screen(out: &mut String, grid: &Grid<Cell>, older: Option<&History>, skip: usize) {
    let mut rows = Rows {
        previous_pen: None,
        previous_style: String::new(),
        next_style: String::new(),
        previous_link: None,
        columns: grid.columns(),
    };
    let older_len = older.map_or(0, History::len);
    let total = older_len + grid.history_size() + grid.screen_lines();
    let fetch = |i: usize| match older {
        Some(older) if i < older_len => older.row(i),
        _ => grid[Line((i - older_len) as i32 - grid.history_size() as i32)].clone(),
    };
    // One row ahead: a wrapped row looks at how the next one starts
    let mut next = (skip < total).then(|| fetch(skip));
    for i in skip..total {
        let Some(line) = next.take() else { break };
        next = (i + 1 < total).then(|| fetch(i + 1));
        rows.row(
            out,
            &line,
            next.as_ref().map(|r| &r[Column(0)]),
            i + 1 == total,
        );
    }
    if rows.previous_link.is_some() {
        link(out, None);
    }
}

/// Whether a wide char in `row` lacks one of its halves.
fn split_wide(row: &Row<Cell>) -> bool {
    let mut leader = false;
    for cell in &row[..] {
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) != leader {
            return true;
        }
        leader = cell.flags.contains(Flags::WIDE_CHAR);
    }
    leader
}

fn serialize<T: EventListener>(
    term: &Term<T>,
    older: Option<&History>,
    options: SnapshotOptions,
) -> Result<String, SnapshotRefusal> {
    // When cutting the cursor paragraph, xterm can leave half of a wide character. Printing that character
    // wraps the line, so reject instead of corrupting it. The inactive grid is serialized only when on the
    // alternate screen, so check it only then (so leftover alternate screen content after leaving does not cause a rejection).
    // A resize also leaves a left half without its right half mid-row (cut, then widened), and
    // ICH in the middle of a wide char leaves a right half alone (xterm.js `insertCells`); no
    // printed text rebuilds either.
    let alt = term.mode().contains(TermMode::ALT_SCREEN);
    // History rows of the primary screen left out (`scrollback`), oldest first: compressed ones, then the grid's
    let primary = if alt {
        term.inactive_grid()
    } else {
        term.grid()
    };
    let older_len = older.map_or(0, History::len);
    let history = older_len + primary.history_size();
    let skip = options
        .scrollback
        .map_or(0, |keep| history.saturating_sub(keep));
    for grid in std::iter::once(term.grid()).chain(alt.then(|| term.inactive_grid())) {
        let skipped = if std::ptr::eq(grid, primary) {
            skip.saturating_sub(older_len)
        } else {
            0
        };
        for y in -(grid.history_size() as i32) + skipped as i32..grid.screen_lines() as i32 {
            if split_wide(&grid[Line(y)]) {
                return Err(SnapshotRefusal::WideEdge);
            }
        }
    }
    if let Some(older) = older {
        if (skip.min(older_len)..older_len).any(|i| split_wide(&older.row(i))) {
            return Err(SnapshotRefusal::WideEdge);
        }
    }
    let mut out = String::from("\x1b[0m\x1b[?6l\x1b[?7h\x1b[4l\x1b[H");
    let modes = term.mode();
    let alt = modes.contains(TermMode::ALT_SCREEN);
    if alt {
        screen(&mut out, term.inactive_grid(), older, skip);
        cursor(&mut out, term.inactive_grid());
        out.push_str("\x1b[?1049h\x1b[0m\x1b[H");
        // Entering clears the alternate screen with the primary pen's background; erased cells are not printed
        // over, so clear it again with the default one.
        if term.inactive_grid().cursor.template.bg != Color::Named(NamedColor::Background) {
            out.push_str("\x1b[2J");
        }
        // SGR 0 leaves an OSC 8 link open; the primary pen's link is saved with 1049 and must not spill.
        if term.inactive_grid().cursor.template.hyperlink().is_some() {
            link(&mut out, None);
        }
    }
    if alt {
        screen(&mut out, term.grid(), None, 0);
    } else {
        screen(&mut out, term.grid(), older, skip);
    }
    if options.exclude_modes {
        cursor(&mut out, term.grid());
        return Ok(out);
    }
    // Turning on DECOM sends the cursor home, so emit it before CUP (margins are rejected, so CUP is relative to the whole screen).
    if modes.contains(TermMode::ORIGIN) {
        out.push_str("\x1b[?6h");
    }
    cursor(&mut out, term.grid());
    for (mode, code) in [
        (TermMode::APP_CURSOR, 1),
        (TermMode::FOCUS_IN_OUT, 1004),
        (TermMode::SGR_MOUSE, 1006),
        (TermMode::BRACKETED_PASTE, 2004),
    ] {
        if modes.contains(mode) {
            let _ = write!(out, "\x1b[?{code}h");
        }
    }
    if modes.contains(TermMode::INSERT) {
        out.push_str("\x1b[4h");
    }
    if modes.contains(TermMode::APP_KEYPAD) {
        out.push_str("\x1b=");
    }
    if !modes.contains(TermMode::LINE_WRAP) {
        out.push_str("\x1b[?7l");
    }
    if modes.contains(TermMode::MOUSE_MOTION) {
        out.push_str("\x1b[?1003h");
    } else if modes.contains(TermMode::MOUSE_DRAG) {
        out.push_str("\x1b[?1002h");
    } else if modes.contains(TermMode::MOUSE_REPORT_CLICK) {
        out.push_str("\x1b[?1000h");
    }
    out.push_str(if modes.contains(TermMode::SHOW_CURSOR) {
        "\x1b[?25h"
    } else {
        "\x1b[?25l"
    });
    Ok(out)
}
