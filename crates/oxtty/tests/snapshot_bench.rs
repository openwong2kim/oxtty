//! Cost of an ANSI snapshot over a long history: allocations grow with the
//! rows and the attribute changes, not with every cell.
use std::alloc::{GlobalAlloc, Layout, System};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use oxtty::Engine;

/// Counts allocations.
struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
// Tests in this binary share the counter, so they run one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        unsafe { System.dealloc(p, layout) };
    }

    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(p, layout, size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// A 200-column engine with `history` lines of history: full rows of words,
/// a colour change every few words, some bold.
fn filled(history: usize) -> Engine {
    let mut t = Engine::new(200, 50, history);
    let mut out = String::new();
    for i in 0..history + 50 {
        for w in 0..20 {
            if w % 4 == 0 {
                out.push_str(&format!("\x1b[{}m", 31 + (i + w) % 7));
            }
            if w % 7 == 3 {
                out.push_str("\x1b[1m");
            }
            out.push_str("word_word ");
            if w % 4 == 3 {
                out.push_str("\x1b[0m");
            }
        }
        out.push_str("\r\n");
        if out.len() > 1 << 16 {
            t.feed(out.as_bytes());
            out.clear();
        }
    }
    t.feed(out.as_bytes());
    t
}

/// Payload hash, allocations and milliseconds of one snapshot.
fn measure(t: &Engine) -> (u64, usize, f64) {
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let start = Instant::now();
    let payload = t.ansi_snapshot().expect("snapshot");
    let ms = start.elapsed().as_secs_f64() * 1000.0;
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    let mut hash = DefaultHasher::new();
    payload.hash(&mut hash);
    (hash.finish(), allocations, ms)
}

#[test]
fn snapshot_allocations_do_not_grow_with_cells() {
    let _serial = SERIAL.lock().unwrap();
    let t = filled(1000);
    let (_, allocations, _) = measure(&t);
    // 1050 rows × 200 cells; a pen string per cell would be over 200k.
    assert!(allocations < 20_000, "{allocations} allocations");
}

/// `cargo test --release -p oxtty --test snapshot_bench -- --ignored --nocapture`
#[test]
#[ignore]
fn bench_snapshot_10k_history_200_cols() {
    let _serial = SERIAL.lock().unwrap();
    let t = filled(10_000);
    measure(&t);
    let runs: Vec<_> = (0..5).map(|_| measure(&t)).collect();
    let best = runs.iter().map(|r| r.2).fold(f64::MAX, f64::min);
    let (hash, allocations, _) = runs[0];
    println!(
        "snapshot 10k history x 200 cols: {best:.1} ms, {allocations} allocations, payload {hash:016x}"
    );
}
