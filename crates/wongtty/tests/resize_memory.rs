//! Memory a resize takes with a long compressed history: the reflow decodes
//! a screen or two and one logical line at a time, not the whole history.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use wongtty::Engine;

/// Counts the bytes allocated and the most allocated at once.
struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
// Tests in this binary share the counters, so they run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

fn grew(n: usize) {
    let live = LIVE.fetch_add(n, Ordering::Relaxed) + n;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(layout) };
        if !p.is_null() {
            grew(layout.size());
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
                grew(size - layout.size());
            } else {
                LIVE.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A 120-column engine with `rows` lines of history: build log lines, some
/// coloured, some wrapping over two or three rows.
fn filled(rows: usize) -> Engine {
    fill(Engine::new(120, 40, rows), rows)
}

fn fill(mut t: Engine, rows: usize) -> Engine {
    let mut out = String::new();
    let mut n = 0u64;
    while t.history_len() < rows {
        out.clear();
        for _ in 0..1000 {
            n += 1;
            let tail = "x".repeat((n * 7919 % 300) as usize);
            if n % 5 == 0 {
                out.push_str("\x1b[32m");
            }
            out.push_str(&format!(
                "[{n}] compiling module_{}.rs ok {tail}\x1b[0m\r\n",
                n % 9999
            ));
        }
        t.feed(out.as_bytes());
    }
    t
}

/// Most bytes allocated at once beyond the start, over `resizes` column
/// changes (120 and 100 in turn), and the time they took.
fn resize_peak(t: &mut Engine, resizes: usize) -> (usize, f64) {
    PEAK.store(LIVE.load(Ordering::Relaxed), Ordering::Relaxed);
    let base = LIVE.load(Ordering::Relaxed);
    let start = Instant::now();
    for i in 0..resizes {
        t.resize(if i % 2 == 0 { 100 } else { 120 }, 40);
    }
    let ms = start.elapsed().as_secs_f64() * 1000.0 / resizes as f64;
    (PEAK.load(Ordering::Relaxed) - base, ms)
}

#[test]
fn resize_does_not_decode_the_history() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut t = filled(20_000);
    t.scroll_display(15_000);
    let (peak, _) = resize_peak(&mut t, 4);
    // The whole history decoded is 20,000 rows × 120 cells × 24 bytes ≈ 55 MiB.
    assert!(peak < 4 << 20, "{peak} bytes");

    // The same as with the whole history uncompressed in the grid.
    let mut plain = fill(Engine::new_uncompressed(120, 40, 20_000), 20_000);
    plain.scroll_display(15_000);
    for i in 0..4 {
        plain.resize(if i % 2 == 0 { 100 } else { 120 }, 40);
    }
    assert_eq!(t.history_len(), plain.history_len());
    assert_eq!(t.display_offset(), plain.display_offset());
    for line in -(t.history_len() as i32)..40 {
        assert_eq!(t.row_text(line), plain.row_text(line), "line {line}");
    }
}

/// `cargo test --release -p wongtty --test resize_memory -- --ignored --nocapture`
#[test]
#[ignore]
fn report_resize_memory() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    for rows in [10_000, 100_000] {
        let mut t = filled(rows);
        let (peak, ms) = resize_peak(&mut t, 20);
        println!(
            "{rows} rows: {:.1} MiB peak extra, {ms:.1} ms per resize",
            peak as f64 / 1048576.0
        );
    }
}
