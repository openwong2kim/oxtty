//! Seeded random terminal streams, run on every `cargo test`.
//!
//! Each case drives a compressed-history engine and an uncompressed one with
//! the same random output, resizes, view scrolls and host calls, and checks:
//! nothing panics, the cursor and row widths stay inside the screen, the two
//! engines hold the same screen and history (so compressed rows round-trip),
//! and an accepted ANSI snapshot rebuilds the same screen in a fresh engine.
//!
//! `WONGTTY_FUZZ_CASES=5000 cargo test -p wongtty --test fuzz` runs longer; a
//! failing case prints its seed, and `WONGTTY_FUZZ_SEED=<seed>` reruns just it.

use wongtty::{Cell, CellExtra, Engine};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len() as u64) as usize]
    }
}

const TEXT: &[&str] = &[
    "hello",
    " ",
    "가",
    "한글",
    "漢字",
    "😀",
    "🚀",
    "e\u{301}",
    "\u{200d}",
    "x\u{fe0f}",
    "\t",
    "\r",
    "\n",
    "\r\n",
    "\x08",
    "\x07",
    "wrap me please across the edge of a narrow screen ",
];

/// One random chunk of program output: text, controls, escape sequences or raw bytes.
fn chunk(r: &mut Rng, out: &mut Vec<u8>) {
    let n = |r: &mut Rng, max: u64| r.below(max).to_string();
    match r.below(16) {
        0..=4 => out.extend_from_slice(r.pick(TEXT).as_bytes()),
        5 => out.extend(format!("\x1b[{};{}H", n(r, 70), n(r, 220)).bytes()),
        6 => {
            let p = r.pick(&["", "0", "1", "2", "3"]).to_owned();
            let f = *r.pick(&['J', 'K', 'X', '@', 'P', 'L', 'M', 'S', 'T']);
            out.extend(format!("\x1b[{p}{f}").bytes());
        }
        7 => {
            let sgr = r.pick(&[
                "0",
                "1",
                "4",
                "4:3",
                "7",
                "21",
                "22",
                "31",
                "38;5;208",
                "48;2;10;20;30",
                "58;2;255;0;0",
                "5",
                "9",
            ]);
            out.extend(format!("\x1b[{sgr}m").bytes());
        }
        8 => {
            let m = r.pick(&[
                "?1049", "?47", "?1047", "?25", "?7", "?6", "4", "?2004", "?2026", "?1000", "?1006",
            ]);
            let hl = if r.below(2) == 0 { 'h' } else { 'l' };
            out.extend(format!("\x1b[{m}{hl}").bytes());
        }
        9 => out.extend(format!("\x1b[{};{}r", n(r, 40), n(r, 70)).bytes()),
        10 => out.extend_from_slice(r.pick(&[
            &b"\x1b7"[..],
            b"\x1b8",
            b"\x1bD",
            b"\x1bE",
            b"\x1bM",
            b"\x1bc",
            b"\x1b[!p",
            b"\x1b[3J",
            b"\x1b[6n",
            b"\x1b[s",
            b"\x1b[u",
        ])),
        11 => {
            let osc = r.pick(&[
                "0;title",
                "8;;https://wongtty.dev",
                "8;;",
                "52;c;aGk=",
                "52;c;?",
                "4;1;?",
                "11;?",
                "4;3;rgb:10/20/30",
                "133;A",
                "7;file://h/p",
            ]);
            let st = if r.below(2) == 0 { "\x07" } else { "\x1b\\" };
            out.extend(format!("\x1b]{osc}{st}").bytes());
        }
        12 => out.extend(format!("\x1bP{}\x1b\\", n(r, 1000)).bytes()),
        13 => {
            // A long line that wraps several times.
            let len = r.below(600) as usize;
            out.extend(std::iter::repeat_n(b'a' + r.below(26) as u8, len));
        }
        _ => {
            for _ in 0..r.below(12) {
                out.push(r.next() as u8);
            }
        }
    }
}

fn screen(t: &Engine) -> Vec<(Vec<Cell>, Vec<CellExtra>)> {
    let (mut cells, mut extras) = (Vec::new(), Vec::new());
    let first = -(t.history_len() as i32);
    (first..t.rows() as i32)
        .map(|line| {
            assert!(t.row_cells(line, &mut cells), "row {line} readable");
            assert!(t.row_extras(line, &mut extras));
            // Links without an id get one from a counter shared by every terminal in the
            // process, so the two engines number them differently.
            for link in extras.iter_mut().filter_map(|e| e.hyperlink.as_mut()) {
                if link.id.ends_with("_alacritty") {
                    link.id.clear();
                }
            }
            (cells.clone(), extras.clone())
        })
        .collect()
}

fn check_bounds(t: &Engine) {
    let mut row = Vec::new();
    for line in 0..t.rows() as i32 {
        t.row_cells(line, &mut row);
        if let Some(col) = row
            .iter()
            .position(|c| c.attrs & wongtty::attr::WIDE != 0 && c.ch == ' ')
        {
            panic!("row {line} col {col}: a wide cell holds a space");
        }
    }
    let c = t.cursor();
    assert!(
        c.line < t.rows() && c.col < t.cols(),
        "cursor {c:?} inside {}x{}",
        t.cols(),
        t.rows()
    );
    let mut cells = Vec::new();
    for line in [-(t.history_len() as i32), -1, 0, t.rows() as i32 - 1] {
        if t.row_cells(line, &mut cells) {
            assert_eq!(cells.len(), t.cols() as usize, "row {line} width");
        }
    }
}

/// A blank with more than a background is printed by a snapshot (an erase keeps only the background), so it comes
/// back typed.
fn snapshot_blank(a: &mut Cell, b: &mut Cell) {
    use wongtty::attr;
    let printed = a.ch == ' '
        && a.attrs & (attr::TYPED_SPACE | attr::WIDE_SPACER) == 0
        && (a.fg != wongtty::color::DEFAULT_FG || a.attrs & !attr::LEADING_WIDE_SPACER != 0);
    if printed {
        b.attrs &= !attr::TYPED_SPACE;
    }
}

fn check_snapshot(t: &Engine, scrollback: usize) {
    let Ok(payload) = t.ansi_snapshot() else {
        return;
    };
    let mut copy = Engine::new(t.cols(), t.rows(), scrollback);
    copy.feed(&payload);
    let (mut a, mut b) = (Vec::new(), Vec::new());
    // A snapshot writes the blank a wrapped wide char left as an ordinary blank (xterm.js has no such mark either).
    let unmark = |cells: &mut Vec<Cell>| {
        cells
            .iter_mut()
            .for_each(|c| c.attrs &= !wongtty::attr::LEADING_WIDE_SPACER)
    };
    for line in 0..t.rows() as i32 {
        t.row_cells(line, &mut a);
        copy.row_cells(line, &mut b);
        unmark(&mut a);
        for (a, b) in a.iter_mut().zip(b.iter_mut()) {
            snapshot_blank(a, b);
        }
        if a != b {
            let col = (0..a.len()).find(|&c| a.get(c) != b.get(c)).unwrap_or(0);
            let tail = &payload[payload.len().saturating_sub(48)..];
            panic!(
                "snapshot row {line} col {col}: {:?} vs {:?}; payload ends {:?}",
                a.get(col),
                b.get(col),
                String::from_utf8_lossy(tail)
            );
        }
    }
    assert_eq!(t.cursor(), copy.cursor(), "snapshot cursor");
}

fn run(seed: u64) {
    let mut r = Rng(seed | 1);
    let scrollback = [0, 20, 200, 1000][r.below(4) as usize];
    let (cols, rows) = (1 + r.below(120) as u16, 1 + r.below(40) as u16);
    let mut packed = Engine::new(cols, rows, scrollback);
    let mut plain = Engine::new_uncompressed(cols, rows, scrollback);
    let trace = std::env::var_os("WONGTTY_FUZZ_TRACE").is_some();
    if trace {
        eprintln!("new {cols}x{rows} scrollback {scrollback}");
    }
    let mut bytes = Vec::new();
    for _ in 0..r.below(80) + 20 {
        match r.below(12) {
            0 => {
                let (c, l) = (1 + r.below(160) as u16, 1 + r.below(60) as u16);
                if trace {
                    eprintln!("resize {c}x{l}");
                }
                packed.resize(c, l);
                plain.resize(c, l);
            }
            1 => {
                let d = r.below(400) as i32 - 200;
                packed.scroll_display(d);
                plain.scroll_display(d);
            }
            2 => {
                for t in [&mut packed, &mut plain] {
                    t.take_replies();
                    t.take_damage();
                    t.flush_sync();
                }
            }
            _ => {
                bytes.clear();
                for _ in 0..r.below(40) + 1 {
                    chunk(&mut r, &mut bytes);
                }
                // Split the same bytes at a random point to cross feed boundaries.
                let cut = r.below(bytes.len() as u64 + 1) as usize;
                if trace {
                    eprintln!(
                        "feed {:?}",
                        String::from_utf8_lossy(&bytes).escape_debug().to_string()
                    );
                }
                for t in [&mut packed, &mut plain] {
                    t.feed(&bytes[..cut]);
                    t.feed(&bytes[cut..]);
                }
            }
        }
        check_bounds(&packed);
        assert_eq!(packed.history_len(), plain.history_len(), "history length");
        assert_eq!(
            packed.trimmed_lines(),
            plain.trimmed_lines(),
            "trimmed lines"
        );
        assert_eq!(packed.cursor(), plain.cursor(), "cursor");
    }
    let (a, b) = (screen(&packed), screen(&plain));
    if let Some(i) = (0..a.len()).find(|&i| a[i] != b[i]) {
        let col = (0..a[i].0.len()).find(|&c| a[i].0.get(c) != b[i].0.get(c));
        panic!(
            "compressed and plain engines differ at row {} of {} (line {}), first column {col:?}:\n  compressed {:?}\n  plain      {:?}\n  extras {:?} vs {:?}",
            i,
            a.len(),
            i as i32 - packed.history_len() as i32,
            col.map(|c| a[i].0[c]),
            col.map(|c| b[i].0[c]),
            a[i].1,
            b[i].1,
        );
    }
    check_snapshot(&packed, scrollback);
}

#[test]
fn random_streams() {
    if let Some(seed) = std::env::var("WONGTTY_FUZZ_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        run(seed);
        return;
    }
    let cases: u64 = std::env::var("WONGTTY_FUZZ_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(150);
    for seed in 1..=cases {
        let result = std::panic::catch_unwind(|| run(seed));
        if let Err(err) = result {
            eprintln!("fuzz case failed: WONGTTY_FUZZ_SEED={seed}");
            std::panic::resume_unwind(err);
        }
    }
}
