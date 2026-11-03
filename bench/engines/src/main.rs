//! Feeds the throughput.mjs corpus (plus agent-emoji, long-lines and graphemes) to wongtty and other terminal
//! engines: MiB/s (median of 5 runs, engines in turn within each run) and the peak memory one engine takes to
//! swallow one scenario (a fresh process each).
//!
//!   node bench/engines/corpus.mjs <dir>
//!   cargo run --release --manifest-path bench/engines/Cargo.toml [--features ghostty,rio,libvterm] -- <dir> [out.json]
//!
//! Every engine gets the same 64 KiB chunks at 120×40 with 10,000 lines of scrollback, and its visible screen text
//! is compared with wongtty's at the end (with the first selected engine's when the filter leaves wongtty out).
//!
//! Single-engine modes (one process each, for /usr/bin/time -l and scripts/cyc.py):
//!   --one <engine> <case> <dir> <runs>        fresh engine per run after a warm-up: median MiB/s and every run
//!   --steady <engine> <case> <dir> <runs>     one engine with its scrollback full, fed again per run
//!   --loop <engine> <case> <dir> <reps>       fresh engine per feed; total, per-feed and thread-CPU MiB/s
//!   --same <engine> <case> <dir> <reps>       like --loop, one engine fed reps times
//!   --ab <case> <dir> <reps> <engine,...>     engines interleaved in one process
//!   --memory <engine> <case> <dir>            peak RSS and heap growth, physical footprint
//!   --lifecycle <engine> <case> <dir> <n>     n terminals: floor, fed, peak, cleared, reset footprint and heap
//!   --screen <engine> <case> <dir>            the visible rows after the case
//!
//! Environment: SB=<lines> overrides the scrollback; BENCH_CASES (or CASES) and BENCH_ENGINES (or ENGINES) filter
//! the default run by comma-separated names (an engine this build lacks exits 2; "null" is the no-op baseline);
//! BENCH_NOMEM skips its memory child processes.
//!
//! Ghostty is libghostty-vt from ghostty-vt-rs-sys: by default its pinned prebuilt (4a4fc017). Ghostty main is
//! selected at build time with GHOSTTY_VT_RS_SYS_PREBUILT_DIR=<dir with lib/libghostty-vt.a>; build it into its
//! own CARGO_TARGET_DIR so both binaries stay. The library comes from a ghostty checkout (main 0b1729e9 for the
//! phase-1 numbers) built with zig 0.17, which names the release modes debug/safe/fast/small:
//!   zig build -Demit-lib-vt=true -Demit-xcframework=false -Dapp-runtime=none -Doptimize=fast -Dcpu=apple_m1 \
//!     --prefix <dir>
//! These are the flags ghostty-vt-rs-sys's own source build passes (GHOSTTY_VT_RS_SYS_OPTIMIZE, _CPU).

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const COLS: u16 = 120;
const ROWS: u16 = 40;
/// Lines of scrollback for every engine: 10,000, or SB=<lines>.
fn scrollback() -> usize {
    static LINES: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *LINES.get_or_init(|| {
        std::env::var("SB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(10_000)
    })
}
const CHUNK: usize = 64 * 1024;
const RUNS: usize = 5;
const CASES: [&str; 9] = [
    "light-cells",
    "dense-cells",
    "unicode",
    "cursor-motion",
    "scrolling-region",
    "scroll-flood",
    "agent-emoji",
    "long-lines",
    "graphemes",
];

/// A comma-separated list from the first of `vars` that is set.
fn env_list(vars: &[&str]) -> Option<Vec<String>> {
    vars.iter()
        .find_map(|v| std::env::var(v).ok())
        .map(|v| v.split(',').map(str::to_string).collect())
}

/// Counts live heap bytes and their peak, for the engines that allocate through Rust.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            let live = LIVE.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(live, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, layout, size) };
        if !q.is_null() {
            if size > layout.size() {
                let live =
                    LIVE.fetch_add(size - layout.size(), Ordering::Relaxed) + size - layout.size();
                PEAK.fetch_max(live, Ordering::Relaxed);
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

trait Engine {
    fn feed(&mut self, bytes: &[u8]);
    /// Visible rows, wide-character spacers skipped, trailing blanks trimmed.
    fn screen(&self) -> Vec<String>;
    /// Lines of scrollback kept.
    fn history(&mut self) -> usize;
    /// Anything else worth printing after a memory run.
    fn note(&mut self) -> String {
        String::new()
    }
}

struct Wongtty(wongtty::Engine);

impl Engine for Wongtty {
    fn feed(&mut self, bytes: &[u8]) {
        self.0.feed(bytes);
    }
    fn screen(&self) -> Vec<String> {
        (0..ROWS as i32)
            .map(|i| self.0.row_text(i).unwrap_or_default())
            .collect()
    }
    fn history(&mut self) -> usize {
        self.0.history_len()
    }
}

mod alacritty {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::Line;
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::term::test::TermSize;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;
    use alacritty_terminal::Term;

    pub struct Alacritty {
        term: Term<VoidListener>,
        parser: Processor,
    }

    impl Alacritty {
        pub fn new() -> Self {
            let config = Config {
                scrolling_history: super::scrollback(),
                ..Config::default()
            };
            let size = TermSize::new(super::COLS as usize, super::ROWS as usize);
            Self {
                term: Term::new(config, &size, VoidListener),
                parser: Processor::new(),
            }
        }
    }

    impl super::Engine for Alacritty {
        fn feed(&mut self, bytes: &[u8]) {
            self.parser.advance(&mut self.term, bytes);
        }
        fn screen(&self) -> Vec<String> {
            let grid = self.term.grid();
            (0..super::ROWS as i32)
                .map(|i| {
                    let mut text = String::new();
                    for cell in &grid[Line(i)][..] {
                        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                            continue;
                        }
                        text.push(cell.c);
                        text.extend(cell.zerowidth().into_iter().flatten());
                    }
                    text.trim_end().to_string()
                })
                .collect()
        }
        fn history(&mut self) -> usize {
            self.term.grid().history_size()
        }
    }
}

struct Vt100(vt100::Parser);

impl Engine for Vt100 {
    fn feed(&mut self, bytes: &[u8]) {
        self.0.process(bytes);
    }
    fn screen(&self) -> Vec<String> {
        self.0
            .screen()
            .rows(0, COLS)
            .map(|r| r.trim_end().to_string())
            .collect()
    }
    fn history(&mut self) -> usize {
        // vt100 clamps the scrollback view to what it holds.
        self.0.screen_mut().set_scrollback(usize::MAX);
        self.0.screen().scrollback()
    }
}

/// avt takes text, not bytes: the UTF-8 decoding (with a sequence split across chunks carried over) is timed too.
struct Avt {
    term: avt::Terminal,
    carry: Vec<u8>,
}

impl Engine for Avt {
    fn feed(&mut self, bytes: &[u8]) {
        let joined;
        let bytes = if self.carry.is_empty() {
            bytes
        } else {
            self.carry.extend_from_slice(bytes);
            joined = std::mem::take(&mut self.carry);
            &joined[..]
        };
        let valid = match std::str::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => {
                self.carry = bytes[e.valid_up_to()..].to_vec();
                // The corpus is valid UTF-8, so only an incomplete sequence at the end is possible.
                std::str::from_utf8(&bytes[..e.valid_up_to()]).unwrap()
            }
        };
        self.term.feed(valid);
    }
    fn screen(&self) -> Vec<String> {
        self.term
            .screen()
            .visible_lines()
            .map(|l| l.text().trim_end().to_string())
            .collect()
    }
    fn history(&mut self) -> usize {
        self.term.primary_screen().all_lines().count() - ROWS as usize
    }
}

mod wezterm {
    use std::sync::Arc;
    use wezterm_term::color::ColorPalette;
    use wezterm_term::{Terminal, TerminalConfiguration, TerminalSize};

    #[derive(Debug)]
    struct Config;

    impl TerminalConfiguration for Config {
        fn scrollback_size(&self) -> usize {
            super::scrollback()
        }
        fn color_palette(&self) -> ColorPalette {
            ColorPalette::default()
        }
    }

    pub struct Wezterm(Terminal);

    impl Wezterm {
        pub fn new() -> Self {
            let size = TerminalSize {
                rows: super::ROWS as usize,
                cols: super::COLS as usize,
                pixel_width: 0,
                pixel_height: 0,
                dpi: 0,
            };
            Self(Terminal::new(
                size,
                Arc::new(Config),
                "bench",
                "0",
                Box::new(std::io::sink()),
            ))
        }
    }

    impl super::Engine for Wezterm {
        fn feed(&mut self, bytes: &[u8]) {
            self.0.advance_bytes(bytes);
        }
        fn screen(&self) -> Vec<String> {
            let screen = self.0.screen();
            let end = screen.scrollback_rows();
            screen
                .lines_in_phys_range(end - super::ROWS as usize..end)
                .iter()
                .map(|l| l.as_str().trim_end().to_string())
                .collect()
        }
        fn history(&mut self) -> usize {
            self.0.screen().scrollback_rows() - super::ROWS as usize
        }
    }
}

#[cfg(feature = "ghostty")]
mod ghostty {
    use ghostty_vt_rs_sys as g;
    use std::ffi::c_void;
    use std::ptr;

    pub struct Ghostty(g::Terminal);

    impl Ghostty {
        pub fn new() -> Self {
            let mut t: g::Terminal = ptr::null_mut();
            unsafe {
                assert_eq!(
                    g::ghostty_terminal_new(ptr::null(), &mut t, super::COLS, super::ROWS),
                    g::Result::SUCCESS
                );
                // The same line limit as the others, and no byte limit on top of it.
                let lines: usize = super::scrollback();
                g::ghostty_terminal_set(
                    t,
                    g::TerminalOption::SCROLLBACK_MAX_LINES,
                    &lines as *const usize as *const c_void,
                );
                g::ghostty_terminal_set(t, g::TerminalOption::SCROLLBACK_MAX_BYTES, ptr::null());
            }
            Self(t)
        }
    }

    impl Drop for Ghostty {
        fn drop(&mut self) {
            unsafe { g::ghostty_terminal_free(self.0) };
        }
    }

    impl super::Engine for Ghostty {
        fn feed(&mut self, bytes: &[u8]) {
            unsafe { g::ghostty_terminal_vt_write(self.0, bytes.as_ptr(), bytes.len()) };
        }
        fn screen(&self) -> Vec<String> {
            let mut rows = Vec::new();
            let mut graphemes = [0u32; 16];
            for y in 0..super::ROWS {
                let mut text = String::new();
                for x in 0..super::COLS {
                    let point = g::Point {
                        tag: g::PointTag::ACTIVE,
                        value: g::PointValue {
                            coordinate: g::PointCoordinate { x, y: y as u32 },
                        },
                    };
                    let mut at = g::sized!(g::GridRef);
                    let mut cell: g::Cell = 0;
                    let mut wide: g::CellWide::Type = 0;
                    let mut len = 0usize;
                    unsafe {
                        assert_eq!(
                            g::ghostty_terminal_grid_ref(self.0, point, &mut at),
                            g::Result::SUCCESS
                        );
                        g::ghostty_grid_ref_cell(&at, &mut cell);
                        g::ghostty_cell_get(
                            cell,
                            g::CellData::WIDE,
                            &mut wide as *mut _ as *mut c_void,
                        );
                        g::ghostty_grid_ref_graphemes(
                            &at,
                            graphemes.as_mut_ptr(),
                            graphemes.len(),
                            &mut len,
                        );
                    }
                    if wide == g::CellWide::SPACER_TAIL || wide == g::CellWide::SPACER_HEAD {
                        continue;
                    }
                    if len == 0 {
                        text.push(' ');
                    }
                    text.extend(graphemes[..len].iter().filter_map(|&c| char::from_u32(c)));
                }
                rows.push(text.trim_end().to_string());
            }
            rows
        }
        fn history(&mut self) -> usize {
            let mut rows = 0usize;
            unsafe {
                g::ghostty_terminal_get(
                    self.0,
                    g::TerminalData::SCROLLBACK_ROWS,
                    &mut rows as *mut usize as *mut c_void,
                )
            };
            rows
        }
        /// Ghostty compresses scrollback only when the host asks (on idle): its resident page bytes before and after
        /// a full pass.
        fn note(&mut self) -> String {
            let resident = |t: g::Terminal| {
                let mut usage = g::sized!(g::TerminalMemoryUsage);
                unsafe {
                    g::ghostty_terminal_get(
                        t,
                        g::TerminalData::MEMORY_USAGE,
                        &mut usage as *mut _ as *mut c_void,
                    )
                };
                usage.primary_resident_bytes as f64 / 1048576.0
            };
            let before = resident(self.0);
            let mut result: g::TerminalCompressionResult::Type = 0;
            unsafe {
                g::ghostty_terminal_compress(self.0, g::TerminalCompressionMode::FULL, &mut result)
            };
            format!(
                "pages {before:.1} MiB, after full compression {:.1} MiB",
                resident(self.0)
            )
        }
    }
}

/// Rio's core (rio-vt): 8-byte cells, grapheme clustering on (its default).
#[cfg(feature = "rio")]
mod rio {
    use rio_vt::ansi::CursorShape;
    use rio_vt::crosswords::grid::Dimensions;
    use rio_vt::crosswords::pos::{Column, Line, Pos};
    use rio_vt::crosswords::{Crosswords, CrosswordsSize};
    use rio_vt::event::{VoidListener, WindowId};
    use rio_vt::performer::handler::Processor;

    pub struct Rio {
        term: Crosswords<VoidListener>,
        parser: Processor,
    }

    impl Rio {
        pub fn new() -> Self {
            let size = CrosswordsSize::new(super::COLS as usize, super::ROWS as usize);
            Self {
                term: Crosswords::new(
                    size,
                    CursorShape::Block,
                    VoidListener,
                    WindowId::from(0),
                    0,
                    super::scrollback(),
                ),
                parser: Processor::default(),
            }
        }
    }

    impl super::Engine for Rio {
        fn feed(&mut self, bytes: &[u8]) {
            self.parser.advance(&mut self.term, bytes);
        }
        fn screen(&self) -> Vec<String> {
            let grid = &self.term.grid;
            (0..super::ROWS as i32)
                .map(|y| {
                    let mut text = String::new();
                    for x in 0..super::COLS as usize {
                        let pos = Pos {
                            row: Line(y),
                            col: Column(x),
                        };
                        if grid[pos].is_spacer() || grid[pos].is_leading_spacer() {
                            continue;
                        }
                        let c: Vec<char> = grid.cell_text(pos).collect();
                        if c.first() == Some(&'\0') {
                            text.push(' ');
                        } else {
                            text.extend(c);
                        }
                    }
                    text.trim_end().to_string()
                })
                .collect()
        }
        fn history(&mut self) -> usize {
            self.term.grid.history_size()
        }
    }
}

/// libvterm keeps no scrollback itself: like Neovim, the host copies each line that scrolls off (sb_pushline) into
/// a ring of 10,000 lines of libvterm cells, reusing the oldest line's buffer once the ring is full, and empties
/// the ring when libvterm asks (sb_clear, on ED 3).
#[cfg(feature = "libvterm")]
mod libvterm {
    use std::collections::VecDeque;
    use std::ffi::{c_char, c_int, c_void};

    #[repr(C)]
    struct VTerm(c_void);
    #[repr(C)]
    struct VTermScreen(c_void);
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Pos {
        row: c_int,
        col: c_int,
    }
    /// VTermScreenCell: chars[6], width, attrs (bit fields), fg, bg; 40 bytes on aarch64-apple-darwin.
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Cell {
        chars: [u32; 6],
        width: c_char,
        attrs: u32,
        fg: u32,
        bg: u32,
    }
    const _: () = assert!(std::mem::size_of::<Cell>() == 40);
    type Cb = Option<unsafe extern "C" fn()>;
    #[repr(C)]
    struct Callbacks {
        damage: Cb,
        moverect: Cb,
        movecursor: Cb,
        settermprop: Cb,
        bell: Cb,
        resize: Cb,
        sb_pushline: Option<unsafe extern "C" fn(c_int, *const Cell, *mut c_void) -> c_int>,
        sb_popline: Cb,
        sb_clear: Option<unsafe extern "C" fn(*mut c_void) -> c_int>,
    }
    unsafe extern "C" {
        fn vterm_new(rows: c_int, cols: c_int) -> *mut VTerm;
        fn vterm_free(vt: *mut VTerm);
        fn vterm_set_utf8(vt: *mut VTerm, on: c_int);
        fn vterm_input_write(vt: *mut VTerm, bytes: *const c_char, len: usize) -> usize;
        fn vterm_obtain_screen(vt: *mut VTerm) -> *mut VTermScreen;
        fn vterm_screen_set_callbacks(s: *mut VTermScreen, cb: *const Callbacks, user: *mut c_void);
        fn vterm_screen_enable_altscreen(s: *mut VTermScreen, on: c_int);
        fn vterm_screen_reset(s: *mut VTermScreen, hard: c_int);
        fn vterm_screen_get_cell(s: *const VTermScreen, pos: Pos, cell: *mut Cell) -> c_int;
    }

    struct Scrollback(VecDeque<Box<[Cell]>>);

    impl Scrollback {
        /// Keeps `src` as the newest line of at most `cap` lines; with `cap` 0 the line is dropped.
        fn keep(&mut self, src: &[Cell], cap: usize) {
            if cap == 0 {
                return;
            }
            if self.0.len() >= cap {
                if let Some(mut line) = self.0.pop_front() {
                    if line.len() == src.len() {
                        line.copy_from_slice(src);
                        self.0.push_back(line);
                        return;
                    }
                }
            }
            self.0.push_back(src.into());
        }
    }

    unsafe extern "C" fn push(cols: c_int, cells: *const Cell, user: *mut c_void) -> c_int {
        let sb = unsafe { &mut *(user as *mut Scrollback) };
        let src = unsafe { std::slice::from_raw_parts(cells, cols as usize) };
        sb.keep(src, super::scrollback());
        1
    }

    /// ED 3 (and Neovim's :clear of the scrollback): libvterm asks the host to drop its lines.
    unsafe extern "C" fn clear(user: *mut c_void) -> c_int {
        unsafe { (*(user as *mut Scrollback)).0 = VecDeque::new() };
        1
    }

    static CALLBACKS: Callbacks = Callbacks {
        damage: None,
        moverect: None,
        movecursor: None,
        settermprop: None,
        bell: None,
        resize: None,
        sb_pushline: Some(push),
        sb_popline: None,
        sb_clear: Some(clear),
    };

    pub struct Libvterm {
        vt: *mut VTerm,
        screen: *mut VTermScreen,
        sb: Box<Scrollback>,
    }

    impl Libvterm {
        pub fn new() -> Self {
            unsafe {
                let vt = vterm_new(super::ROWS as c_int, super::COLS as c_int);
                vterm_set_utf8(vt, 1);
                let screen = vterm_obtain_screen(vt);
                let mut sb = Box::new(Scrollback(VecDeque::new()));
                vterm_screen_set_callbacks(
                    screen,
                    &CALLBACKS,
                    &mut *sb as *mut Scrollback as *mut c_void,
                );
                vterm_screen_enable_altscreen(screen, 1);
                vterm_screen_reset(screen, 1);
                Self { vt, screen, sb }
            }
        }
    }

    impl Drop for Libvterm {
        fn drop(&mut self) {
            unsafe { vterm_free(self.vt) };
        }
    }

    impl super::Engine for Libvterm {
        fn feed(&mut self, bytes: &[u8]) {
            unsafe { vterm_input_write(self.vt, bytes.as_ptr() as *const c_char, bytes.len()) };
        }
        fn screen(&self) -> Vec<String> {
            (0..super::ROWS as c_int)
                .map(|row| {
                    let mut text = String::new();
                    let mut col = 0;
                    while col < super::COLS as c_int {
                        let mut cell: Cell = unsafe { std::mem::zeroed() };
                        unsafe { vterm_screen_get_cell(self.screen, Pos { row, col }, &mut cell) };
                        if cell.chars[0] == 0 {
                            text.push(' ');
                        } else {
                            text.extend(
                                cell.chars
                                    .iter()
                                    .take_while(|&&c| c != 0)
                                    .filter_map(|&c| char::from_u32(c)),
                            );
                        }
                        col += (cell.width as c_int).max(1);
                    }
                    text.trim_end().to_string()
                })
                .collect()
        }
        fn history(&mut self) -> usize {
            self.sb.0.len()
        }
    }

    #[cfg(test)]
    mod tests {
        use super::super::Engine;
        use super::*;

        #[test]
        fn ed3_empties_the_host_scrollback() {
            let mut t = Libvterm::new();
            for i in 0..100 {
                t.feed(format!("line {i}\r\n").as_bytes());
            }
            assert!(t.history() > 0);
            t.feed(b"\x1b[H\x1b[2J\x1b[3J");
            assert_eq!(t.history(), 0);
        }

        #[test]
        fn zero_capacity_keeps_no_lines() {
            let cell: Cell = unsafe { std::mem::zeroed() };
            let mut sb = Scrollback(VecDeque::new());
            sb.keep(&[cell; 3], 0);
            assert!(sb.0.is_empty());
            sb.keep(&[cell; 3], 2);
            sb.keep(&[cell; 3], 2);
            sb.keep(&[cell; 4], 2);
            assert_eq!(sb.0.len(), 2);
            assert_eq!(sb.0.back().unwrap().len(), 4);
        }
    }
}

/// Reads nothing: the process baseline for /usr/bin/time counts.
struct Null;

impl Engine for Null {
    fn feed(&mut self, bytes: &[u8]) {
        std::hint::black_box(bytes);
    }
    fn screen(&self) -> Vec<String> {
        vec![String::new(); ROWS as usize]
    }
    fn history(&mut self) -> usize {
        0
    }
}

fn engines() -> Vec<&'static str> {
    let mut all = vec![
        "wongtty",
        "wongtty-uncompressed",
        "alacritty",
        "vt100",
        "avt",
        "wezterm",
    ];
    if cfg!(feature = "rio") {
        all.push("rio");
    }
    if cfg!(feature = "libvterm") {
        all.push("libvterm");
    }
    if cfg!(feature = "ghostty") {
        all.push("ghostty");
    }
    match env_list(&["BENCH_ENGINES", "ENGINES"]) {
        Some(only) => select(&all, &only).unwrap_or_else(|unknown| {
            eprintln!(
                "BENCH_ENGINES/ENGINES: unknown engine(s) {} (this build has {}, null)",
                unknown.join(","),
                all.join(",")
            );
            std::process::exit(2)
        }),
        None => all,
    }
}

/// The engines of `all` (plus "null") named in `only`, in `all`'s order; Err with the names this build lacks.
fn select(all: &[&'static str], only: &[String]) -> Result<Vec<&'static str>, Vec<String>> {
    let unknown: Vec<String> = only
        .iter()
        .filter(|o| o.as_str() != "null" && !all.contains(&o.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(unknown);
    }
    let mut picked: Vec<&'static str> = all
        .iter()
        .copied()
        .filter(|e| only.iter().any(|o| o == e))
        .collect();
    if only.iter().any(|o| o == "null") {
        picked.push("null");
    }
    Ok(picked)
}

fn make(name: &str) -> Box<dyn Engine> {
    match name {
        "wongtty" => Box::new(Wongtty(wongtty::Engine::new(COLS, ROWS, scrollback()))),
        "wongtty-uncompressed" => Box::new(Wongtty(wongtty::Engine::new_uncompressed(
            COLS,
            ROWS,
            scrollback(),
        ))),
        "alacritty" => Box::new(alacritty::Alacritty::new()),
        "vt100" => Box::new(Vt100(vt100::Parser::new(
            ROWS.try_into().unwrap(),
            COLS.try_into().unwrap(),
            scrollback(),
        ))),
        "avt" => Box::new(Avt {
            term: avt::Terminal::with_scrollback(COLS as usize, ROWS as usize, Some(scrollback()))
                .unwrap(),
            carry: Vec::new(),
        }),
        "wezterm" => Box::new(wezterm::Wezterm::new()),
        #[cfg(feature = "rio")]
        "rio" => Box::new(rio::Rio::new()),
        #[cfg(feature = "libvterm")]
        "libvterm" => Box::new(libvterm::Libvterm::new()),
        "null" => Box::new(Null),
        #[cfg(feature = "ghostty")]
        "ghostty" => Box::new(ghostty::Ghostty::new()),
        _ => panic!("unknown engine {name}"),
    }
}

fn read_case(dir: &str, case: &str) -> Vec<u8> {
    std::fs::read(format!("{dir}/{case}.vt"))
        .unwrap_or_else(|e| panic!("{dir}/{case}.vt: {e} (run corpus.mjs first)"))
}

/// Largest resident set so far, in bytes.
fn max_rss() -> usize {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) };
    // Bytes on macOS, KiB on Linux.
    usage.ru_maxrss as usize * if cfg!(target_os = "macos") { 1 } else { 1024 }
}

/// Current physical footprint (what Activity Monitor shows), in bytes; 0 off macOS.
fn footprint() -> usize {
    #[cfg(target_os = "macos")]
    unsafe {
        let mut info: libc::rusage_info_v2 = std::mem::zeroed();
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V2,
            &mut info as *mut _ as *mut libc::rusage_info_t,
        );
        info.ri_phys_footprint as usize
    }
    #[cfg(not(target_os = "macos"))]
    0
}

/// Largest physical footprint since the previous call, in bytes (ri_interval_max_phys_footprint, which each
/// RUSAGE_INFO_V4 read resets); 0 off macOS.
fn interval_max_footprint() -> usize {
    #[cfg(target_os = "macos")]
    unsafe {
        let mut info: libc::rusage_info_v4 = std::mem::zeroed();
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            &mut info as *mut _ as *mut libc::rusage_info_t,
        );
        info.ri_interval_max_phys_footprint as usize
    }
    #[cfg(not(target_os = "macos"))]
    0
}

/// Child process: one engine, one scenario; prints the peak RSS growth and the peak heap growth in bytes.
fn memory(engine: &str, case: &str, dir: &str) {
    let bytes = read_case(dir, case);
    let rss = max_rss();
    let fp = footprint();
    let live = LIVE.load(Ordering::Relaxed);
    PEAK.store(live, Ordering::Relaxed);
    let mut t = make(engine);
    for chunk in bytes.chunks(CHUNK) {
        t.feed(chunk);
    }
    let heap = PEAK.load(Ordering::Relaxed) - live;
    let grew = max_rss() - rss;
    let fp_fed = footprint() as f64 - fp as f64;
    let note = t.note();
    // Footprint now (after the note, which for Ghostty is a full compression pass) against before the engine existed.
    let fp_after = footprint() as f64 - fp as f64;
    println!(
        "{grew} {heap} {} {note} footprint-fed {:.2} MiB, footprint-after-note {:.2} MiB",
        t.history(),
        fp_fed / 1048576.0,
        fp_after / 1048576.0
    );
}

/// One engine, one scenario, a fresh engine per run after one warm-up: prints the median MiB/s and every run.
fn one(engine: &str, case: &str, dir: &str, runs: usize) {
    let bytes = read_case(dir, case);
    let mib = bytes.len() as f64 / 1048576.0;
    let mut times = Vec::new();
    for run in 0..=runs {
        let mut t = make(engine);
        let start = Instant::now();
        for chunk in bytes.chunks(CHUNK) {
            t.feed(chunk);
        }
        let secs = start.elapsed().as_secs_f64();
        std::hint::black_box(t.history());
        if run > 0 {
            times.push(mib / secs);
        }
    }
    let mut s = times.clone();
    s.sort_by(f64::total_cmp);
    let all: Vec<String> = times.iter().map(|t| format!("{t:.1}")).collect();
    println!("{engine} {case} {:.1} [{}]", s[s.len() / 2], all.join(" "));
}

/// Steady state: one engine swallows the scenario once (scrollback full), then each timed run feeds it again into
/// the same engine. Prints the median MiB/s, every run, and the RSS growth over the whole process life.
fn steady(engine: &str, case: &str, dir: &str, runs: usize) {
    let bytes = read_case(dir, case);
    let mib = bytes.len() as f64 / 1048576.0;
    let rss = max_rss();
    let mut t = make(engine);
    for chunk in bytes.chunks(CHUNK) {
        t.feed(chunk);
    }
    let mut times = Vec::new();
    for _ in 0..runs {
        let start = Instant::now();
        for chunk in bytes.chunks(CHUNK) {
            t.feed(chunk);
        }
        times.push(mib / start.elapsed().as_secs_f64());
    }
    let mut s = times.clone();
    s.sort_by(f64::total_cmp);
    let all: Vec<String> = times.iter().map(|t| format!("{t:.1}")).collect();
    println!(
        "{engine} {case} {:.1} [{}] rss+{:.1}MiB",
        s[s.len() / 2],
        all.join(" "),
        (max_rss() - rss) as f64 / 1048576.0
    );
}

/// This thread's CPU time, in seconds.
fn thread_cpu() -> f64 {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    ts.tv_sec as f64 + ts.tv_nsec as f64 * 1e-9
}

struct Row {
    case: String,
    engine: &'static str,
    mib_s: f64,
    min: f64,
    max: f64,
    rss: usize,
    heap: usize,
    history: usize,
    screen: String,
}

/// Child process: `n` terminals of one engine through a life cycle: made (the empty floor), fed the scenario (after
/// 10,000+ lines), the peak on the way (footprint: the kernel's interval maximum; heap: the counter's peak), cleared (ED 2 + ED 3), reset (RIS). Footprint deltas over the start, per
/// terminal, and Rust heap live bytes per terminal (engines that allocate through Rust only).
fn lifecycle(engine: &str, case: &str, dir: &str, n: usize) {
    let bytes = read_case(dir, case);
    let mib = |b: isize| b as f64 / 1048576.0 / n as f64;
    let f0 = footprint() as isize;
    let h0 = LIVE.load(Ordering::Relaxed) as isize;
    let mut ts: Vec<Box<dyn Engine>> = (0..n).map(|_| make(engine)).collect();
    let floor = (
        footprint() as isize - f0,
        LIVE.load(Ordering::Relaxed) as isize - h0,
    );
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    // Starts a new footprint interval, so the read after the feeds is the peak during them.
    interval_max_footprint();
    for t in ts.iter_mut() {
        for chunk in bytes.chunks(CHUNK) {
            t.feed(chunk);
        }
    }
    let peak_fp = interval_max_footprint() as isize - f0;
    let fed = (
        footprint() as isize - f0,
        LIVE.load(Ordering::Relaxed) as isize - h0,
    );
    let peak_heap = PEAK.load(Ordering::Relaxed) as isize - h0;
    for t in ts.iter_mut() {
        t.feed(b"\x1b[H\x1b[2J\x1b[3J");
    }
    let cleared = (
        footprint() as isize - f0,
        LIVE.load(Ordering::Relaxed) as isize - h0,
    );
    for t in ts.iter_mut() {
        t.feed(b"\x1bc");
    }
    let reset = (
        footprint() as isize - f0,
        LIVE.load(Ordering::Relaxed) as isize - h0,
    );
    let history = ts[0].history();
    println!(
        "{engine:<19} {case:<17} n={n} per terminal MiB: floor fp {:>6.2} heap {:>6.2} | fed fp {:>6.2} heap {:>6.2} | peak fp {:>6.2} heap {:>6.2} | cleared fp {:>6.2} heap {:>6.2} | reset fp {:>6.2} heap {:>6.2} | history {history}",
        mib(floor.0), mib(floor.1), mib(fed.0), mib(fed.1), mib(peak_fp), mib(peak_heap), mib(cleared.0), mib(cleared.1), mib(reset.0), mib(reset.1)
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--lifecycle") {
        return lifecycle(&args[2], &args[3], &args[4], args[5].parse().unwrap());
    }
    // Analysis: --loop <engine> <case> <dir> <reps> feeds the case reps times, each into a fresh engine (construction
    // untimed per feed, timed in the total); --same feeds ONE engine reps times. Prints the total MiB/s, MiB/s per
    // feed, the history length, and the median and best MiB/s per feed in thread CPU time.
    if matches!(
        args.get(1).map(String::as_str),
        Some("--loop") | Some("--same")
    ) {
        let same = args[1] == "--same";
        let bytes = read_case(&args[4], &args[3]);
        let mib = bytes.len() as f64 / 1048576.0;
        let reps: usize = args[5].parse().unwrap();
        let mut t = make(&args[2]);
        let mut out = Vec::new();
        let mut cpu = Vec::new();
        let all = Instant::now();
        for i in 0..reps {
            if !same && i > 0 {
                t = make(&args[2]);
            }
            let start = Instant::now();
            let c0 = thread_cpu();
            for chunk in bytes.chunks(CHUNK) {
                t.feed(chunk);
            }
            cpu.push(mib / (thread_cpu() - c0));
            std::hint::black_box(&mut t);
            out.push(format!("{:.0}", mib / start.elapsed().as_secs_f64()));
        }
        let secs = all.elapsed().as_secs_f64();
        cpu.sort_by(f64::total_cmp);
        println!(
            "{} {} {:.1} MiB/s per-feed {} history {} cpu median {:.1} best {:.1}",
            args[2],
            args[3],
            mib * reps as f64 / secs,
            out.join(" "),
            t.history(),
            cpu[cpu.len() / 2],
            cpu[cpu.len() - 1]
        );
        return;
    }
    // Analysis: --ab <case> <dir> <reps> <engine,...>: engines interleaved in one process, construction untimed,
    // one warm-up pass each; prints median, best and worst MiB/s per engine.
    if args.get(1).map(String::as_str) == Some("--ab") {
        let bytes = read_case(&args[3], &args[2]);
        let reps: usize = args[4].parse().unwrap();
        let names: Vec<&str> = args[5].split(',').collect();
        let mut times: Vec<Vec<f64>> = vec![Vec::new(); names.len()];
        for rep in 0..=reps {
            for (i, n) in names.iter().enumerate() {
                let mut t = make(n);
                let start = Instant::now();
                for chunk in bytes.chunks(CHUNK) {
                    t.feed(chunk);
                }
                let s = start.elapsed().as_secs_f64();
                std::hint::black_box(&mut t);
                drop(t);
                if rep > 0 {
                    times[i].push(bytes.len() as f64 / 1048576.0 / s);
                }
            }
        }
        for (i, n) in names.iter().enumerate() {
            let v = &mut times[i];
            v.sort_by(f64::total_cmp);
            println!(
                "{:22} med {:7.1} best {:7.1} worst {:7.1} MiB/s",
                n,
                v[v.len() / 2],
                v[v.len() - 1],
                v[0]
            );
        }
        return;
    }
    if args.get(1).map(String::as_str) == Some("--memory") {
        return memory(&args[2], &args[3], &args[4]);
    }
    if args.get(1).map(String::as_str) == Some("--one") {
        return one(&args[2], &args[3], &args[4], args[5].parse().unwrap());
    }
    if args.get(1).map(String::as_str) == Some("--steady") {
        return steady(&args[2], &args[3], &args[4], args[5].parse().unwrap());
    }
    if args.get(1).map(String::as_str) == Some("--screen") {
        let mut t = make(&args[2]);
        for chunk in read_case(&args[4], &args[3]).chunks(CHUNK) {
            t.feed(chunk);
        }
        for row in t.screen() {
            println!("{row}");
        }
        return;
    }
    let dir = args
        .get(1)
        .expect("usage: wongtty-engine-bench <corpus-dir> [out.json]");
    let names = engines();
    let mut rows = Vec::new();
    let cases = env_list(&["BENCH_CASES", "CASES"])
        .unwrap_or_else(|| CASES.iter().map(|c| c.to_string()).collect());
    for case in &cases {
        let case = case.as_str();
        let bytes = read_case(dir, case);
        let mib = bytes.len() as f64 / 1048576.0;
        let mut times: Vec<Vec<f64>> = vec![Vec::new(); names.len()];
        let mut screens: Vec<Vec<String>> = vec![Vec::new(); names.len()];
        // Run 0 is warm-up. Each run starts with a different engine, so drift from other load spreads over all.
        for run in 0..=RUNS {
            for k in 0..names.len() {
                let e = (run + k) % names.len();
                let mut t = make(names[e]);
                let start = Instant::now();
                for chunk in bytes.chunks(CHUNK) {
                    t.feed(chunk);
                }
                let secs = start.elapsed().as_secs_f64();
                if run > 0 {
                    times[e].push(mib / secs);
                }
                screens[e] = t.screen();
            }
        }
        for (e, name) in names.iter().enumerate() {
            let mut s = times[e].clone();
            s.sort_by(f64::total_cmp);
            // Screens are compared with the first engine (wongtty unless the filter leaves it out).
            let differ: Vec<usize> = (0..ROWS as usize)
                .filter(|&i| screens[e][i] != screens[0][i])
                .collect();
            let screen = if e == 0 {
                "reference".to_string()
            } else if differ.is_empty() {
                format!("same as {}", names[0])
            } else {
                let i = differ[0];
                format!(
                    "{} rows differ; row {i}: {:?} vs {} {:?}",
                    differ.len(),
                    screens[e][i].chars().take(60).collect::<String>(),
                    names[0],
                    screens[0][i].chars().take(60).collect::<String>()
                )
            };
            // BENCH_NOMEM skips the memory child processes.
            let out = if std::env::var_os("BENCH_NOMEM").is_some() {
                String::new()
            } else {
                let child = Command::new(std::env::current_exe().unwrap())
                    .args(["--memory", name, case, dir])
                    .output()
                    .unwrap();
                String::from_utf8_lossy(&child.stdout).into_owned()
            };
            let mut words = out
                .split_whitespace()
                .map(|w| w.parse::<usize>().unwrap_or(0));
            let (rss, heap, history) = (
                words.next().unwrap_or(0),
                words.next().unwrap_or(0),
                words.next().unwrap_or(0),
            );
            let note: Vec<&str> = out.split_whitespace().skip(3).collect();
            let row = Row {
                case: case.to_string(),
                engine: name,
                mib_s: s[s.len() / 2],
                min: s[0],
                max: s[s.len() - 1],
                rss,
                heap,
                history,
                screen,
            };
            println!(
                "{:<17} {:<19} {:>7.1} MiB/s ({:>6.1}–{:>6.1})  rss +{:>6.1} MiB  heap peak +{:>6.1} MiB  history {:>5}  screen {} {}",
                row.case,
                row.engine,
                row.mib_s,
                row.min,
                row.max,
                row.rss as f64 / 1048576.0,
                row.heap as f64 / 1048576.0,
                row.history,
                row.screen,
                note.join(" ")
            );
            rows.push(row);
        }
    }
    if let Some(out) = args.get(2) {
        let mut json = String::from("[\n");
        for (i, r) in rows.iter().enumerate() {
            let _ = writeln!(
                json,
                "  {{\"case\":{:?},\"engine\":{:?},\"mibPerSec\":{:.1},\"min\":{:.1},\"max\":{:.1},\"rssBytes\":{},\"heapPeakBytes\":{},\"history\":{},\"screen\":{:?}}}{}",
                r.case,
                r.engine,
                r.mib_s,
                r.min,
                r.max,
                r.rss,
                r.heap,
                r.history,
                r.screen,
                if i + 1 < rows.len() { "," } else { "" }
            );
        }
        json.push_str("]\n");
        std::fs::write(out, json).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn select_keeps_build_order_and_rejects_unknown_names() {
        let all = ["wongtty", "alacritty", "libvterm"];
        assert_eq!(
            select(&all, &names(&["libvterm", "wongtty"])),
            Ok(vec!["wongtty", "libvterm"])
        );
        assert_eq!(
            select(&all, &names(&["null", "libvterm"])),
            Ok(vec!["libvterm", "null"])
        );
        assert_eq!(
            select(&all, &names(&["wongty", "wongtty-wasm"])),
            Err(names(&["wongty", "wongtty-wasm"]))
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn interval_max_footprint_sees_a_freed_spike() {
        interval_max_footprint();
        let before = footprint();
        // 64 MiB touched and freed between the two reads: gone from the footprint, kept in the interval maximum.
        let spike = vec![1u8; 64 << 20];
        std::hint::black_box(&spike);
        drop(spike);
        let peak = interval_max_footprint();
        assert!(footprint() < before + (32 << 20));
        assert!(peak >= before + (32 << 20), "peak {peak} before {before}");
    }
}
