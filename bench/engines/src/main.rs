//! Feeds the throughput.mjs corpus to oxtty and other Rust terminal engines: MiB/s (median of 5 runs, engines in
//! turn within each run) and the peak memory one engine takes to swallow one scenario (a fresh process each).
//!
//!   node bench/engines/corpus.mjs <dir>
//!   cargo run --release --manifest-path bench/engines/Cargo.toml [--features ghostty] -- <dir> [out.json]
//!
//! Every engine gets the same 64 KiB chunks at 120×40 with 10,000 lines of scrollback, and its visible screen text
//! is compared with oxtty's at the end.

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const COLS: u16 = 120;
const ROWS: u16 = 40;
const SCROLLBACK: usize = 10_000;
const CHUNK: usize = 64 * 1024;
const RUNS: usize = 5;
const CASES: [&str; 6] = [
    "light-cells",
    "dense-cells",
    "unicode",
    "cursor-motion",
    "scrolling-region",
    "scroll-flood",
];

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

struct Oxtty(oxtty::Engine);

impl Engine for Oxtty {
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
                scrolling_history: super::SCROLLBACK,
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
            super::SCROLLBACK
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
                let lines: usize = super::SCROLLBACK;
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

fn engines() -> Vec<&'static str> {
    let mut all = vec![
        "oxtty",
        "oxtty-uncompressed",
        "alacritty",
        "vt100",
        "avt",
        "wezterm",
    ];
    if cfg!(feature = "ghostty") {
        all.push("ghostty");
    }
    all
}

fn make(name: &str) -> Box<dyn Engine> {
    match name {
        "oxtty" => Box::new(Oxtty(oxtty::Engine::new(COLS, ROWS, SCROLLBACK))),
        "oxtty-uncompressed" => Box::new(Oxtty(oxtty::Engine::new_uncompressed(
            COLS, ROWS, SCROLLBACK,
        ))),
        "alacritty" => Box::new(alacritty::Alacritty::new()),
        "vt100" => Box::new(Vt100(vt100::Parser::new(
            ROWS.try_into().unwrap(),
            COLS.try_into().unwrap(),
            SCROLLBACK,
        ))),
        "avt" => Box::new(Avt {
            term: avt::Terminal::with_scrollback(COLS as usize, ROWS as usize, Some(SCROLLBACK))
                .unwrap(),
            carry: Vec::new(),
        }),
        "wezterm" => Box::new(wezterm::Wezterm::new()),
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

/// Child process: one engine, one scenario; prints the peak RSS growth and the peak heap growth in bytes.
fn memory(engine: &str, case: &str, dir: &str) {
    let bytes = read_case(dir, case);
    let rss = max_rss();
    let live = LIVE.load(Ordering::Relaxed);
    PEAK.store(live, Ordering::Relaxed);
    let mut t = make(engine);
    for chunk in bytes.chunks(CHUNK) {
        t.feed(chunk);
    }
    let heap = PEAK.load(Ordering::Relaxed) - live;
    let grew = max_rss() - rss;
    println!("{grew} {heap} {} {}", t.history(), t.note());
}

struct Row {
    case: &'static str,
    engine: &'static str,
    mib_s: f64,
    min: f64,
    max: f64,
    rss: usize,
    heap: usize,
    history: usize,
    screen: String,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--memory") {
        return memory(&args[2], &args[3], &args[4]);
    }
    let dir = args
        .get(1)
        .expect("usage: oxtty-engine-bench <corpus-dir> [out.json]");
    let names = engines();
    let mut rows = Vec::new();
    for case in CASES {
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
            let differ: Vec<usize> = (0..ROWS as usize)
                .filter(|&i| screens[e][i] != screens[0][i])
                .collect();
            let screen = if differ.is_empty() {
                "same".to_string()
            } else {
                let i = differ[0];
                format!(
                    "{} rows differ; row {i}: {:?} vs oxtty {:?}",
                    differ.len(),
                    screens[e][i].chars().take(60).collect::<String>(),
                    screens[0][i].chars().take(60).collect::<String>()
                )
            };
            let child = Command::new(std::env::current_exe().unwrap())
                .args(["--memory", name, case, dir])
                .output()
                .unwrap();
            let out = String::from_utf8_lossy(&child.stdout);
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
                case,
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
