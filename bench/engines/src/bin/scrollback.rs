//! Operations on a full scrollback: each engine swallows one corpus scenario (10,000 lines of history at 120x40), then
//! one operation runs in a fresh process: reflowing resizes, scrolling the view through the history while reading every
//! visible row as a renderer does, search over the whole buffer, plain text and serialized output of the whole
//! buffer, and clearing the history. Reported: median latency, instructions retired, peak footprint growth during the
//! operation, footprint growth that stays after it.
//!
//!   cargo run --release --features ghostty --bin scrollback -- <corpus-dir> [runs]
//!   ... --bin scrollback -- --one <engine> <case> <op> <corpus-dir>

use std::alloc::{GlobalAlloc, Layout, System};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

const COLS: u16 = 120;
const ROWS: u16 = 40;
const NARROW: u16 = 100;
const SCROLLBACK: usize = 10_000;
const CHUNK: usize = 64 * 1024;

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

/// (phys_footprint, interval max phys_footprint since the previous call, instructions, cycles)
fn usage() -> (u64, u64, u64, u64) {
    let mut ri: libc::rusage_info_v4 = unsafe { std::mem::zeroed() };
    unsafe {
        libc::proc_pid_rusage(
            libc::getpid(),
            libc::RUSAGE_INFO_V4,
            &mut ri as *mut _ as *mut libc::rusage_info_t,
        )
    };
    (
        ri.ri_phys_footprint,
        ri.ri_interval_max_phys_footprint,
        ri.ri_instructions,
        ri.ri_cycles,
    )
}

trait Engine {
    fn feed(&mut self, bytes: &[u8]);
    fn resize(&mut self, cols: u16, rows: u16);
    fn history(&mut self) -> usize;
    /// Scrolls the view `delta` rows back (negative: forward) and reads every visible cell; returns a checksum.
    fn frame(&mut self, delta: i32) -> u64;
    /// Puts the view at `offset` rows above the bottom and reads every visible cell.
    fn frame_at(&mut self, offset: usize) -> u64;
    /// Matches of `needle` in the whole buffer (case-insensitive).
    fn count(&mut self, needle: &str) -> usize;
    /// Plain text of the whole buffer.
    fn text(&mut self) -> usize;
    /// Bytes that rebuild the buffer (the engine's serializer); None if it has none.
    fn serialize(&mut self) -> Option<usize>;
    fn snapshot(&mut self) -> Option<usize> {
        None
    }
    fn after_fill(&mut self) {}
}

struct Wongtty {
    t: wongtty::Engine,
    buf: Vec<u32>,
}

impl Engine for Wongtty {
    fn feed(&mut self, bytes: &[u8]) {
        self.t.feed(bytes);
    }
    fn resize(&mut self, cols: u16, rows: u16) {
        self.t.resize(cols, rows);
    }
    fn history(&mut self) -> usize {
        self.t.history_len()
    }
    fn frame(&mut self, delta: i32) -> u64 {
        self.t.scroll_display(delta);
        self.read()
    }
    fn frame_at(&mut self, offset: usize) -> u64 {
        let d = offset as i32 - self.t.display_offset() as i32;
        self.t.scroll_display(d);
        self.read()
    }
    fn count(&mut self, needle: &str) -> usize {
        let mut out = Vec::new();
        let top = -(self.t.history_len() as i32);
        self.t.search_rows(
            needle,
            wongtty::SearchOptions::default(),
            top,
            self.t.rows() as i32 - 1,
            &mut out,
        );
        out.len()
    }
    fn text(&mut self) -> usize {
        let mut s = String::new();
        for line in -(self.t.history_len() as i32)..self.t.rows() as i32 {
            s.push_str(&self.t.row_string(line, true).unwrap_or_default());
            s.push('\n');
        }
        s.len()
    }
    fn serialize(&mut self) -> Option<usize> {
        self.t.ansi_snapshot().ok().map(|b| b.len())
    }
}

impl Wongtty {
    fn read(&mut self) -> u64 {
        let _ = self.t.take_damage();
        let off = self.t.display_offset() as i32;
        let mut sum = 0u64;
        for y in 0..self.t.rows() as i32 {
            self.t.row_packed(y - off, &mut self.buf);
            sum = sum.wrapping_add(self.buf.iter().map(|&w| w as u64).sum::<u64>());
        }
        sum
    }
}

mod alacritty {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::{Dimensions, Scroll};
    use alacritty_terminal::index::{Boundary, Column, Direction, Point, Side};
    use alacritty_terminal::term::search::RegexSearch;
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
        fn read(&mut self) -> u64 {
            let mut sum = 0u64;
            for cell in self.term.renderable_content().display_iter {
                sum = sum.wrapping_add(cell.c as u64 + cell.flags.bits() as u64);
            }
            sum
        }
    }

    impl super::Engine for Alacritty {
        fn feed(&mut self, bytes: &[u8]) {
            self.parser.advance(&mut self.term, bytes);
        }
        fn resize(&mut self, cols: u16, rows: u16) {
            self.term
                .resize(TermSize::new(cols as usize, rows as usize));
        }
        fn history(&mut self) -> usize {
            self.term.grid().history_size()
        }
        fn frame(&mut self, delta: i32) -> u64 {
            self.term.scroll_display(Scroll::Delta(delta));
            self.read()
        }
        fn frame_at(&mut self, offset: usize) -> u64 {
            let d = offset as i32 - self.term.grid().display_offset() as i32;
            self.term.scroll_display(Scroll::Delta(d));
            self.read()
        }
        fn count(&mut self, needle: &str) -> usize {
            let mut regex = RegexSearch::new(&regex_escape(needle)).unwrap();
            let start = Point::new(self.term.topmost_line(), Column(0));
            let mut origin = start;
            let mut n = 0;
            let mut last: Option<Point> = None;
            while let Some(m) =
                self.term
                    .search_next(&mut regex, origin, Direction::Right, Side::Left, None)
            {
                // The search wraps around: stop at the first match seen again.
                if last.is_some_and(|l| *m.start() <= l) || n > 10_000_000 {
                    break;
                }
                n += 1;
                last = Some(*m.start());
                origin = m.end().add(&self.term, Boundary::None, 1);
                if *m.end() >= Point::new(self.term.bottommost_line(), self.term.last_column()) {
                    break;
                }
            }
            n
        }
        fn text(&mut self) -> usize {
            let start = Point::new(self.term.topmost_line(), Column(0));
            let end = Point::new(self.term.bottommost_line(), self.term.last_column());
            self.term.bounds_to_string(start, end).len()
        }
        fn serialize(&mut self) -> Option<usize> {
            None
        }
    }

    fn regex_escape(s: &str) -> String {
        let mut out = String::new();
        for c in s.chars() {
            if "\\.+*?()|[]{}^$#&-~".contains(c) {
                out.push('\\');
            }
            out.push(c);
        }
        out
    }
}

#[cfg(feature = "ghostty")]
mod ghostty {
    use ghostty_vt_rs_sys as g;
    use std::ffi::c_void;
    use std::ptr;

    pub struct Ghostty {
        t: g::Terminal,
        rs: g::RenderState,
        rows: g::RenderStateRowIterator,
        offset: usize,
        pub compressed: bool,
    }

    impl Ghostty {
        pub fn new(compressed: bool) -> Self {
            let mut t: g::Terminal = ptr::null_mut();
            let mut rs: g::RenderState = ptr::null_mut();
            let mut rows: g::RenderStateRowIterator = ptr::null_mut();
            unsafe {
                assert_eq!(
                    g::ghostty_terminal_new(ptr::null(), &mut t, super::COLS, super::ROWS),
                    g::Result::SUCCESS
                );
                let lines: usize = super::SCROLLBACK;
                g::ghostty_terminal_set(
                    t,
                    g::TerminalOption::SCROLLBACK_MAX_LINES,
                    &lines as *const usize as *const c_void,
                );
                g::ghostty_terminal_set(t, g::TerminalOption::SCROLLBACK_MAX_BYTES, ptr::null());
                assert_eq!(
                    g::ghostty_render_state_new(ptr::null(), &mut rs),
                    g::Result::SUCCESS
                );
                assert_eq!(
                    g::ghostty_render_state_row_iterator_new(ptr::null(), &mut rows),
                    g::Result::SUCCESS
                );
            }
            Self {
                t,
                rs,
                rows,
                offset: 0,
                compressed,
            }
        }
        pub fn compress(&mut self) {
            let mut result: g::TerminalCompressionResult::Type = 0;
            unsafe {
                g::ghostty_terminal_compress(self.t, g::TerminalCompressionMode::FULL, &mut result)
            };
        }
        fn scroll(&mut self, delta: isize) {
            let mut v = g::TerminalScrollViewport::default();
            v.tag = g::TerminalScrollViewportTag::DELTA;
            v.value.delta = delta;
            unsafe { g::ghostty_terminal_scroll_viewport(self.t, v) };
        }
        fn read(&mut self) -> u64 {
            let mut sum = 0u64;
            unsafe {
                g::ghostty_render_state_update(self.rs, self.t);
                g::ghostty_render_state_get(
                    self.rs,
                    g::RenderStateData::ROW_ITERATOR,
                    &mut self.rows as *mut _ as *mut c_void,
                );
                while g::ghostty_render_state_row_iterator_next(self.rows) {
                    let mut view = g::CellsView::default();
                    g::ghostty_render_state_row_get(
                        self.rows,
                        g::RenderStateRowData::CELLS_RAW,
                        &mut view as *mut _ as *mut c_void,
                    );
                    let cells = std::slice::from_raw_parts(view.ptr, view.len);
                    sum = sum.wrapping_add(cells.iter().copied().fold(0u64, u64::wrapping_add));
                }
            }
            sum
        }
        fn format(&mut self, emit: g::FormatterFormat::Type) -> usize {
            let mut opts = g::FormatterTerminalOptions::default();
            opts.size = std::mem::size_of::<g::FormatterTerminalOptions>();
            opts.emit = emit;
            opts.unwrap = false;
            opts.trim = true;
            opts.extra.size = std::mem::size_of::<g::FormatterTerminalExtra>();
            opts.extra.screen.size = std::mem::size_of::<g::FormatterScreenExtra>();
            if emit == g::FormatterFormat::VT {
                opts.extra.modes = true;
                opts.extra.scrolling_region = true;
                opts.extra.tabstops = true;
                opts.extra.keyboard = true;
                opts.extra.screen.cursor = true;
                opts.extra.screen.style = true;
                opts.extra.screen.hyperlink = true;
                opts.extra.screen.charsets = true;
                opts.extra.screen.kitty_keyboard = true;
            }
            let mut f: g::Formatter = ptr::null_mut();
            let mut out: *mut u8 = ptr::null_mut();
            let mut len = 0usize;
            unsafe {
                assert_eq!(
                    g::ghostty_formatter_terminal_new(ptr::null(), &mut f, self.t, opts),
                    g::Result::SUCCESS
                );
                assert_eq!(
                    g::ghostty_formatter_format_alloc(f, ptr::null(), &mut out, &mut len),
                    g::Result::SUCCESS
                );
                g::ghostty_free(ptr::null(), out, len);
                g::ghostty_formatter_free(f);
            }
            len
        }
    }

    impl Drop for Ghostty {
        fn drop(&mut self) {
            unsafe {
                g::ghostty_render_state_row_iterator_free(self.rows);
                g::ghostty_render_state_free(self.rs);
                g::ghostty_terminal_free(self.t)
            };
        }
    }

    impl super::Engine for Ghostty {
        fn feed(&mut self, bytes: &[u8]) {
            unsafe { g::ghostty_terminal_vt_write(self.t, bytes.as_ptr(), bytes.len()) };
        }
        fn resize(&mut self, cols: u16, rows: u16) {
            unsafe { g::ghostty_terminal_resize(self.t, cols, rows, 8, 16) };
        }
        fn history(&mut self) -> usize {
            let mut rows = 0usize;
            unsafe {
                g::ghostty_terminal_get(
                    self.t,
                    g::TerminalData::SCROLLBACK_ROWS,
                    &mut rows as *mut usize as *mut c_void,
                )
            };
            rows
        }
        fn frame(&mut self, delta: i32) -> u64 {
            self.scroll(-(delta as isize));
            self.offset = (self.offset as isize + delta as isize).max(0) as usize;
            self.read()
        }
        fn frame_at(&mut self, offset: usize) -> u64 {
            let d = offset as isize - self.offset as isize;
            self.scroll(-d);
            self.offset = offset;
            self.read()
        }
        fn count(&mut self, needle: &str) -> usize {
            let mut s: g::Search = ptr::null_mut();
            let mut n = 0usize;
            unsafe {
                assert_eq!(
                    g::ghostty_search_new(ptr::null(), &mut s, self.t),
                    g::Result::SUCCESS
                );
                let needle = g::String::from(needle);
                g::ghostty_search_set(
                    s,
                    g::SearchOption::GHOSTTY_SEARCH_OPT_NEEDLE,
                    &needle as *const _ as *const c_void,
                );
                g::ghostty_search_run(s);
                g::ghostty_search_get(
                    s,
                    g::SearchData::TOTAL_MATCHES,
                    &mut n as *mut usize as *mut c_void,
                );
                g::ghostty_search_free(s);
            }
            n
        }
        fn text(&mut self) -> usize {
            self.format(g::FormatterFormat::PLAIN)
        }
        fn serialize(&mut self) -> Option<usize> {
            Some(self.format(g::FormatterFormat::VT))
        }
        fn after_fill(&mut self) {
            if self.compressed {
                self.compress();
            }
        }
        fn snapshot(&mut self) -> Option<usize> {
            let mut out: *mut u8 = ptr::null_mut();
            let mut len = 0usize;
            unsafe {
                if g::ghostty_snapshot_encode_alloc(self.t, ptr::null(), &mut out, &mut len)
                    != g::Result::SUCCESS
                {
                    return None;
                }
                g::ghostty_free(ptr::null(), out, len);
            }
            Some(len)
        }
    }
}

fn make(name: &str) -> Box<dyn Engine> {
    match name {
        "wongtty" => Box::new(Wongtty {
            t: wongtty::Engine::new(COLS, ROWS, SCROLLBACK),
            buf: Vec::new(),
        }),
        "wongtty-uncompressed" => Box::new(Wongtty {
            t: wongtty::Engine::new_uncompressed(COLS, ROWS, SCROLLBACK),
            buf: Vec::new(),
        }),
        "alacritty" => Box::new(alacritty::Alacritty::new()),
        #[cfg(feature = "ghostty")]
        "ghostty" => Box::new(ghostty::Ghostty::new(false)),
        #[cfg(feature = "ghostty")]
        "ghostty-z" => Box::new(ghostty::Ghostty::new(true)),
        _ => panic!("unknown engine {name}"),
    }
}

const OPS: [&str; 13] = [
    "fill",
    "resize-shrink",
    "resize-grow",
    "resize-rows",
    "scroll-page",
    "scroll-wheel",
    "scroll-jump",
    "search-absent",
    "search-common",
    "text",
    "serialize",
    "snapshot",
    "clear",
];

fn needle(case: &str) -> &'static str {
    match case {
        "light-cells" => "module_44",
        "unicode" => "漢",
        _ => "R7",
    }
}

/// One measured step: (seconds, instructions, cycles, peak footprint growth, heap peak growth).
struct Step {
    secs: f64,
    instr: u64,
    cycles: u64,
    peak: i64,
    heap: i64,
}

fn step(f: impl FnOnce()) -> Step {
    let live = LIVE.load(Ordering::Relaxed);
    PEAK.store(live, Ordering::Relaxed);
    let (fp0, _, i0, c0) = usage();
    let start = Instant::now();
    f();
    let secs = start.elapsed().as_secs_f64();
    let (_, max, i1, c1) = usage();
    Step {
        secs,
        instr: i1 - i0,
        cycles: c1 - c0,
        peak: max as i64 - fp0 as i64,
        heap: PEAK.load(Ordering::Relaxed) as i64 - live as i64,
    }
}

fn one(engine: &str, case: &str, op: &str, dir: &str) {
    let bytes = std::fs::read(format!("{dir}/{case}.vt")).unwrap();
    let (base_fp, _, _, _) = usage();
    let mut t = make(engine);
    let fill = step(|| {
        for chunk in bytes.chunks(CHUNK) {
            t.feed(chunk);
        }
    });
    // Ghostty compresses on idle; ghostty-z runs the whole pass before the operation.
    t.after_fill();
    let history = t.history();
    let (after_fill_fp, _, _, _) = usage();
    let live_after_fill = LIVE.load(Ordering::Relaxed) as i64;
    // Several timed repetitions of a repeatable operation, else one.
    let mut steps: Vec<Step> = Vec::new();
    let mut result = 0u64;
    let frames;
    match op {
        "fill" => {
            steps.push(fill);
            frames = 1;
        }
        "resize-shrink" | "resize-grow" => {
            let cycles: usize = std::env::var("RESIZE_CYCLES")
                .ok()
                .and_then(|c| c.parse().ok())
                .unwrap_or(3);
            for _ in 0..cycles {
                let s = step(|| t.resize(NARROW, ROWS));
                let g = step(|| t.resize(COLS, ROWS));
                steps.push(if op == "resize-shrink" { s } else { g });
            }
            frames = 1;
        }
        "resize-rows" => {
            for _ in 0..3 {
                steps.push(step(|| t.resize(COLS, ROWS - 10)));
                steps.push(step(|| t.resize(COLS, ROWS)));
            }
            frames = 1;
        }
        "scroll-page" => {
            // Bottom to top a page at a time, then back down: frames per pass.
            let pages = history / ROWS as usize;
            frames = 2 * pages;
            steps.push(step(|| {
                for _ in 0..pages {
                    result = result.wrapping_add(t.frame(ROWS as i32));
                }
                for _ in 0..pages {
                    result = result.wrapping_add(t.frame(-(ROWS as i32)));
                }
            }));
        }
        "scroll-wheel" => {
            // Three rows per frame, 2,000 frames up from the bottom.
            frames = 2000;
            steps.push(step(|| {
                for _ in 0..frames {
                    result = result.wrapping_add(t.frame(3));
                }
            }));
        }
        "scroll-jump" => {
            // A scrollbar drag far through the history: every frame lands somewhere new.
            frames = 500;
            let mut x: u64 = 0x9E3779B97F4A7C15;
            steps.push(step(|| {
                for _ in 0..frames {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    result = result.wrapping_add(t.frame_at((x % history as u64) as usize));
                }
            }));
        }
        "search-absent" => {
            frames = 1;
            for _ in 0..3 {
                steps.push(step(|| result = t.count("qqzzxx") as u64));
            }
        }
        "search-common" => {
            frames = 1;
            for _ in 0..3 {
                steps.push(step(|| result = t.count(needle(case)) as u64));
            }
        }
        "text" => {
            frames = 1;
            for _ in 0..3 {
                steps.push(step(|| result = t.text() as u64));
            }
        }
        "serialize" => {
            frames = 1;
            for _ in 0..3 {
                steps.push(step(|| result = t.serialize().map_or(0, |n| n as u64)));
            }
        }
        "snapshot" => {
            frames = 1;
            for _ in 0..3 {
                steps.push(step(|| result = t.snapshot().map_or(0, |n| n as u64)));
            }
        }
        "clear" => {
            frames = 1;
            steps.push(step(|| t.feed(b"\x1b[3J")));
            result = t.history() as u64;
        }
        _ => panic!("unknown op {op}"),
    }
    let (end_fp, _, _, _) = usage();
    let live_end = LIVE.load(Ordering::Relaxed) as i64;
    steps.sort_by(|a, b| a.secs.total_cmp(&b.secs));
    let s = &steps[steps.len() / 2];
    // engine case op secs instr cycles peak heap frames result history fill_fp retained_fp
    println!(
        "{} {} {} {} {} {} {} {} {} {} {}",
        s.secs,
        s.instr,
        s.cycles,
        s.peak,
        s.heap,
        frames,
        result,
        history,
        after_fill_fp as i64 - base_fp as i64,
        end_fp as i64 - after_fill_fp as i64,
        live_end - live_after_fill,
    );
}

fn median(v: &mut [f64]) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--one") {
        return one(&args[2], &args[3], &args[4], &args[5]);
    }
    let dir = &args[1];
    let runs: usize = args.get(2).map_or(3, |r| r.parse().unwrap());
    let cases: Vec<String> = std::env::var("CASES")
        .map(|c| c.split(',').map(String::from).collect())
        .unwrap_or_else(|_| vec!["light-cells".into(), "dense-cells".into(), "unicode".into()]);
    let ops: Vec<String> = std::env::var("OPS")
        .map(|c| c.split(',').map(String::from).collect())
        .unwrap_or_else(|_| OPS.iter().map(|s| s.to_string()).collect());
    // ENGINES=<name,...> replaces the engine list.
    let listed = std::env::var("ENGINES").ok();
    let engines: Vec<&str> = match &listed {
        Some(e) => e.split(',').collect(),
        None if cfg!(feature = "ghostty") => {
            vec![
                "wongtty",
                "wongtty-uncompressed",
                "alacritty",
                "ghostty",
                "ghostty-z",
            ]
        }
        None => vec!["wongtty", "wongtty-uncompressed", "alacritty"],
    };
    println!("case op engine | median ms (per frame for scroll) [min-max] | Minstr | peak MiB | heap MiB | retained MiB | heap kept MiB | result | history | fill footprint MiB");
    for case in &cases {
        for op in &ops {
            let mut rows: Vec<Vec<Vec<f64>>> = vec![Vec::new(); engines.len()];
            let mut meta: Vec<(u64, u64)> = vec![(0, 0); engines.len()];
            for run in 0..runs {
                for k in 0..engines.len() {
                    let e = (run + k) % engines.len();
                    let out = Command::new(std::env::current_exe().unwrap())
                        .args(["--one", engines[e], case, op, dir])
                        .output()
                        .unwrap();
                    let text = String::from_utf8_lossy(&out.stdout);
                    let w: Vec<f64> = text
                        .split_whitespace()
                        .map(|x| x.parse::<f64>().unwrap_or(f64::NAN))
                        .collect();
                    if w.len() < 11 {
                        eprintln!(
                            "{} {} {}: failed {}",
                            engines[e],
                            case,
                            op,
                            String::from_utf8_lossy(&out.stderr)
                        );
                        continue;
                    }
                    meta[e] = (w[6] as u64, w[7] as u64);
                    rows[e].push(w);
                }
            }
            for (e, name) in engines.iter().enumerate() {
                let r = &rows[e];
                if r.is_empty() {
                    continue;
                }
                let col = |i: usize| -> Vec<f64> { r.iter().map(|w| w[i]).collect() };
                let frames = r[0][5];
                let mut ms: Vec<f64> = col(0).iter().map(|s| s * 1000.0 / frames).collect();
                let (lo, hi) = (
                    ms.iter().cloned().fold(f64::MAX, f64::min),
                    ms.iter().cloned().fold(0.0, f64::max),
                );
                let mib = 1048576.0;
                println!(
                    "{:<12} {:<14} {:<19} | {:>9.3} [{:>8.3}-{:>8.3}] | {:>9.2} | {:>7.2} | {:>7.2} | {:>7.2} | {:>7.2} | {:>9} | {:>5} | {:>6.2}",
                    case,
                    op,
                    name,
                    median(&mut ms),
                    lo,
                    hi,
                    median(&mut col(1)) / 1e6 / frames,
                    median(&mut col(3)) / mib,
                    median(&mut col(4)) / mib,
                    median(&mut col(9)) / mib,
                    median(&mut col(10)) / mib,
                    meta[e].0,
                    meta[e].1,
                    median(&mut col(8)) / mib,
                );
            }
        }
    }
}
