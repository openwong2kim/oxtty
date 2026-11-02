use oxtty::{Cell, Damage, Engine, SnapshotOptions, SnapshotRefusal};

fn snapshot(t: &mut Engine) -> Vec<u8> {
    t.ansi_snapshot().expect("snapshot")
}

fn find(payload: &[u8], needle: &[u8]) -> usize {
    payload
        .windows(needle.len())
        .position(|w| w == needle)
        .unwrap_or_else(|| panic!("{needle:?} not in payload"))
}

/// Whether cells, cursor, modes and history match after feeding the snapshot to a new engine of the same size.
fn assert_round_trip(t: &mut Engine, scrollback: usize) -> Engine {
    let payload = snapshot(t);
    let mut copy = Engine::new(t.cols(), t.rows(), scrollback);
    copy.feed(&payload);
    assert_same_screen(t, &copy);
    copy
}

fn assert_same_screen(a: &Engine, b: &Engine) {
    assert_eq!(a.history_len(), b.history_len(), "history");
    assert_eq!(a.cursor(), b.cursor(), "cursor");
    assert_eq!(a.modes(), b.modes(), "modes");
    let (mut x, mut y): (Vec<Cell>, Vec<Cell>) = (Vec::new(), Vec::new());
    for line in -(a.history_len() as i32)..a.rows() as i32 {
        a.row_cells(line, &mut x);
        b.row_cells(line, &mut y);
        assert_eq!(x, y, "row {line}");
        assert_eq!(a.row_wrapped(line), b.row_wrapped(line), "wrap {line}");
    }
}

#[test]
fn refuses_wide_glyph_clipped_to_last_column() {
    let mut t = Engine::new(4, 2, 0);
    t.feed("a한".as_bytes());
    t.resize(2, 2);
    assert_eq!(t.ansi_snapshot(), Err(SnapshotRefusal::WideEdge));
    assert_eq!(SnapshotRefusal::WideEdge.as_str(), "unsupported-wide-edge");
}

#[test]
fn refuses_wide_glyph_missing_a_half_mid_row() {
    // Cut by a shrink, then widened: the left half is followed by a plain blank (fuzz seed 7660).
    let mut t = Engine::new(4, 1, 0);
    t.feed("🚀𘀄".as_bytes());
    t.resize(3, 1);
    t.resize(4, 1);
    assert_eq!(t.ansi_snapshot(), Err(SnapshotRefusal::WideEdge));
    // ICH at a right half moves it away from its blanked left half.
    let mut t = Engine::new(6, 2, 0);
    t.feed("a가b\x1b[1;3H\x1b[@".as_bytes());
    assert_eq!(t.ansi_snapshot(), Err(SnapshotRefusal::WideEdge));
}

#[test]
fn refuses_narrow_margins_but_not_full_screen_ones() {
    let mut t = Engine::new(10, 6, 0);
    t.feed(b"\x1b[2;4rhello");
    assert_eq!(t.ansi_snapshot(), Err(SnapshotRefusal::Margins));
    assert_eq!(SnapshotRefusal::Margins.as_str(), "margins");
    t.feed(b"\x1b[1;6r");
    assert!(t.ansi_snapshot().is_ok());
}

#[test]
fn refuses_overlong_unfinished_sequence_until_it_ends() {
    let mut t = Engine::new(10, 4, 0);
    t.feed(b"\x1b]0;");
    t.feed("a".repeat(5000).as_bytes());
    assert_eq!(t.ansi_snapshot(), Err(SnapshotRefusal::TailOverflow));
    assert_eq!(
        SnapshotRefusal::TailOverflow.as_str(),
        "partial-tail-overflow"
    );
    t.feed(b"\x07");
    assert!(t.ansi_snapshot().is_ok());
}

#[test]
fn keeps_unfinished_escape_raw_across_feeds() {
    let mut t = Engine::new(10, 4, 0);
    t.feed(b"hello\x1b[3");
    t.feed(b"1;");
    let payload = snapshot(&mut t);
    assert!(payload.ends_with(b"\x1b[31;"));
    t.feed(b"4mX");
    assert!(snapshot(&mut t).ends_with(b"\x1b[?25h"));
}

#[test]
fn keeps_split_utf8_raw() {
    let mut t = Engine::new(10, 4, 0);
    t.feed(b"ready\xf0");
    t.feed(b"\x9f");
    assert!(snapshot(&mut t).ends_with(b"\x1b[?25h\xf0\x9f"));
    let mut copy = assert_round_trip(&mut t, 0);
    t.feed(b"\x98\x80");
    copy.feed(b"\x98\x80");
    assert_eq!(copy.row_text(0).unwrap(), "ready😀");
    assert_same_screen(&t, &copy);
}

#[test]
fn open_sync_is_carried_and_repeatable() {
    let mut t = Engine::new(20, 4, 0);
    t.feed(b"\x1b[?2026hhello");
    let first = snapshot(&mut t);
    assert!(t.sync_pending());
    assert_eq!(snapshot(&mut t), first);
    assert!(first.ends_with(b"\x1b[?2026h"));
    let mut copy = Engine::new(20, 4, 0);
    copy.feed(&first);
    assert_eq!(
        (copy.sync_pending(), copy.row_text(0).unwrap()),
        (true, "hello".into())
    );
    t.feed(b" world\x1b[?2026l");
    assert!(!t.sync_pending());
    assert_eq!(t.row_text(0).unwrap(), "hello world");
}

#[test]
fn open_sync_is_judged_on_the_screen_it_has_applied() {
    let mut t = Engine::new(10, 4, 0);
    // The update's output is applied at once, so margins it sets are refused and margins it clears are not.
    t.feed(b"\x1b[?2026h\x1b[2;3r");
    assert_eq!(t.ansi_snapshot(), Err(SnapshotRefusal::Margins));
    t.feed(b"\x1b[rX\x1b[3");
    let first = snapshot(&mut t);
    assert_eq!(snapshot(&mut t), first);
    // The unfinished sequence comes after the update starts again.
    assert!(first.ends_with(b"\x1b[?2026h\x1b[3"));
    let mut copy = Engine::new(10, 4, 0);
    copy.feed(&first);
    for e in [&mut t, &mut copy] {
        e.feed(b"1mY\x1b[?2026l");
    }
    let mut cells = Vec::new();
    t.row_cells(0, &mut cells);
    assert_eq!((cells[1].ch, cells[1].fg), ('Y', oxtty::color::NAMED | 1));
    assert_same_screen(&t, &copy);
}

#[test]
fn snapshot_between_split_esu_lets_the_update_end() {
    let mut t = Engine::new(20, 4, 0);
    t.feed(b"\x1b[?2026hframe\x1b[?2026");
    snapshot(&mut t);
    t.feed(b"l");
    assert_eq!(
        (t.sync_pending(), t.row_text(0).unwrap()),
        (false, "frame".into())
    );
}

#[test]
fn snapshot_leaves_the_live_engine_untouched() {
    let mut t = Engine::new(20, 4, 0);
    t.feed(b"before\x1b[?2026h\x1b[2Hframe");
    t.take_damage();
    let first = snapshot(&mut t);
    let none = Damage::Lines {
        scroll: 0,
        lines: vec![],
    };
    assert_eq!(
        (t.take_damage(), t.row_text(1).unwrap(), t.sync_pending()),
        (none, "frame".into(), true)
    );
    assert_eq!(snapshot(&mut t), first);
}

#[test]
fn snapshot_during_sync_replays_into_the_same_update() {
    let mut t = Engine::new(20, 4, 0);
    t.feed(b"before\r\n\x1b[?2026hframe\x1b[31");
    let mut copy = Engine::new(20, 4, 0);
    copy.feed(&snapshot(&mut t));
    assert_eq!(
        (
            copy.sync_pending(),
            copy.row_text(0).unwrap(),
            copy.row_text(1).unwrap()
        ),
        (true, "before".into(), "frame".into())
    );
    for e in [&mut t, &mut copy] {
        e.feed(b"mX\x1b[?2026l");
    }
    assert!(!copy.sync_pending());
    assert_same_screen(&t, &copy);
}

fn rows(e: &Engine) -> Vec<String> {
    (0..e.rows() as i32)
        .map(|l| e.row_text(l).unwrap())
        .collect()
}

/// Taking a snapshot at every split point across sync boundaries leaves the engine unchanged, and the receiver is inside the same update.
#[test]
fn snapshot_at_every_split_of_synchronized_output() {
    // BSU right after ESU, a wide character and an incomplete CSI inside sync.
    let input =
        "pre\r\n\x1b[?2026hA\x1b[31mB\x1b[?2026l\x1b[?2026hC한\x1b[4\x1b[?2026lD\x1b[?2026hE"
            .as_bytes();
    for at in 0..=input.len() {
        for bytewise in [false, true] {
            let mut t = Engine::new(20, 4, 0);
            if bytewise {
                input[..at].iter().for_each(|b| t.feed(&[*b]));
            } else {
                t.feed(&input[..at]);
            }
            let mut untouched = Engine::new(20, 4, 0);
            untouched.feed(&input[..at]);
            let mut copy = Engine::new(20, 4, 0);
            copy.feed(&snapshot(&mut t));
            let at = (at, bytewise);
            assert_eq!(
                (rows(&t), t.sync_pending()),
                (rows(&untouched), untouched.sync_pending()),
                "{at:?}"
            );
            assert_eq!(
                (rows(&copy), copy.sync_pending()),
                (rows(&t), t.sync_pending()),
                "{at:?}"
            );
            assert_same_screen(&t, &copy);
            for e in [&mut t, &mut copy, &mut untouched] {
                e.feed(&input[at.0..]);
                e.flush_sync();
            }
            assert_eq!(rows(&t), rows(&untouched), "{at:?}");
            assert_same_screen(&t, &untouched);
            assert_same_screen(&t, &copy);
        }
    }
}

#[test]
fn primary_screen_comes_before_alternate() {
    let mut t = Engine::new(10, 4, 0);
    t.feed(b"normal\x1b[?1049halt");
    let payload = snapshot(&mut t);
    let normal = find(&payload, b"normal");
    let swap = find(&payload, b"\x1b[?1049h");
    assert!(normal < swap && swap < find(&payload, b"alt"));
    let mut copy = assert_round_trip(&mut t, 0);
    t.feed(b"\x1b[?1049l");
    copy.feed(b"\x1b[?1049l");
    assert_same_screen(&t, &copy);
}

#[test]
fn round_trip_in_oxtty() {
    let inputs: [&[u8]; 4] = [
        "\x1b[31mred\x1b[0m 한글 e\u{301}\r\n\x1b[1;3;4:3;7;58;5;196mX\x1b[0m\r\nabcdefghijklmn\r\n1\r\n2\r\n3"
            .as_bytes(),
        b"\x1b[44m\x1b[2J\x1b[2;3H\x1b[38;2;1;2;3mrgb\x1b[?1h\x1b=\x1b[?2004h\x1b[?1002h\x1b[?1006h\x1b[4h",
        b"abcdefghij",
        b"\x1b[?25l\x1b[?7lzz\x1b[?1004h",
    ];
    for input in inputs {
        let mut t = Engine::new(10, 4, 50);
        t.feed(input);
        assert_round_trip(&mut t, 50);
    }
}

#[test]
fn tab_cell_is_written_as_space() {
    let mut t = Engine::new(20, 4, 0);
    t.feed(b"a\tb");
    let payload = snapshot(&mut t);
    assert!(!payload.contains(&b'\t'));
    // The engine stores '\t' in tab cells (a known difference), so check character positions and the cursor instead of cells.
    let mut copy = Engine::new(20, 4, 0);
    copy.feed(&payload);
    assert_eq!(copy.row_text(0).unwrap(), "a       b");
    assert_eq!(copy.cursor(), t.cursor());
}

#[test]
fn origin_mode_comes_before_cursor() {
    let mut t = Engine::new(20, 6, 0);
    t.feed(b"\x1b[?6h\x1b[3;5HX");
    assert_round_trip(&mut t, 0);
}

#[test]
fn control_inside_unfinished_sequence_is_not_replayed() {
    let mut t = Engine::new(20, 4, 0);
    t.feed(b"ab\x1b[31\n");
    let mut copy = assert_round_trip(&mut t, 0);
    t.feed(b"mX");
    copy.feed(b"mX");
    assert_same_screen(&t, &copy);
}

#[test]
fn leftover_alt_screen_does_not_refuse_after_exit() {
    // Even if a cut wide character remains on the alternate screen, only the primary screen is serialized after leaving, so it must not be rejected.
    let mut t = Engine::new(4, 3, 10);
    t.feed("\x1b[?1049hab한".as_bytes());
    t.resize(3, 3);
    assert_eq!(t.ansi_snapshot(), Err(SnapshotRefusal::WideEdge));
    t.feed(b"\x1b[?1049lok");
    assert_round_trip(&mut t, 10);
}

#[test]
fn erased_cells_stay_empty_and_typed_spaces_stay_text() {
    let mut t = Engine::new(8, 4, 20);
    // Typed trailing spaces, a gap a cursor move left, a coloured erase, and wrapped rows ending and starting with
    // erased cells (the snapshot has to force those wraps)
    t.feed(b"ab  \r\nx\x1b[3Cy\r\n\x1b[41mq\x1b[K\x1b[0m\r\n");
    t.feed(b"12345678\x1b[3D\x1b[3X9z\x1b[1;3H\x1b[5;1H");
    t.feed(b"abcdefgh\x1b[2X\x1b[1;2H\x1b[5;1H\r\n\x1b[48;5;3m\x1b[K\x1b[0m");
    let copy = assert_round_trip(&mut t, 20);
    let mut cells = Vec::new();
    copy.row_cells(-(copy.history_len() as i32), &mut cells);
    let typed: Vec<bool> = cells
        .iter()
        .map(|c| c.attrs & oxtty::attr::TYPED_SPACE != 0)
        .collect();
    assert_eq!(
        typed,
        [false, false, true, true, false, false, false, false]
    );
}

#[test]
fn the_blank_a_wide_char_left_comes_back_empty() {
    let mut t = Engine::new(8, 3, 20);
    t.feed("abcdefg가".as_bytes());
    let mut copy = Engine::new(8, 3, 20);
    copy.feed(&snapshot(&mut t));
    let mut cells = Vec::new();
    copy.row_cells(0, &mut cells);
    // Not the LEADING_WIDE_SPACER mark (xterm.js has none either), but not a typed space
    assert_eq!(cells[7].attrs, 0);
    assert!(copy.row_wrapped(0));
    assert_eq!(copy.row_text(1).unwrap(), "가");
}

#[test]
fn alternate_screen_is_cleared_with_the_default_background() {
    // Entering it clears it with the pen's background (the primary pen is blue here); its erased cells, back to the
    // default background, are not printed over
    let mut t = Engine::new(6, 3, 20);
    t.feed(b"\x1b[44mblue\x1b[?1049h\x1b[0m\x1b[2J\x1b[Halt");
    assert_round_trip(&mut t, 20);
}

#[test]
fn scrollback_option_keeps_the_newest_history_lines() {
    let mut t = Engine::new(10, 3, 100);
    for i in 0..8 {
        t.feed(format!("line {i}\r\n").as_bytes());
    }
    let keep = |n| SnapshotOptions {
        scrollback: Some(n),
        exclude_modes: false,
    };
    let mut copy = Engine::new(10, 3, 100);
    copy.feed(&t.ansi_snapshot_with(keep(2)).unwrap());
    assert_eq!(copy.history_len(), 2);
    assert_eq!(copy.row_text(-2).unwrap(), "line 4");
    assert_eq!(copy.cursor(), t.cursor());
    let mut all = Engine::new(10, 3, 100);
    all.feed(&t.ansi_snapshot_with(keep(1000)).unwrap());
    assert_same_screen(&t, &all);
}

#[test]
fn exclude_modes_paints_only_the_screen() {
    let mut t = Engine::new(10, 3, 100);
    for i in 0..5 {
        t.feed(format!("line {i}\r\n").as_bytes());
    }
    // Modes, margins and an unfinished SGR at the end
    t.feed(b"\x1b[?1h\x1b[?1000h\x1b[?2004h\x1b[?25l\x1b[2;3r\x1b[1;1Hx\x1b[31");
    assert_eq!(t.ansi_snapshot(), Err(SnapshotRefusal::Margins));
    let cold = t
        .ansi_snapshot_with(SnapshotOptions {
            scrollback: Some(0),
            exclude_modes: true,
        })
        .unwrap();
    assert!(!cold.ends_with(b"\x1b[31"), "no unfinished sequence");
    let mut copy = Engine::new(10, 3, 100);
    copy.feed(&cold);
    assert_eq!(copy.history_len(), 0);
    for line in 0..3 {
        assert_eq!(copy.row_text(line), t.row_text(line));
    }
    assert_eq!(
        copy.modes(),
        Engine::new(10, 3, 100).modes(),
        "default modes"
    );
    assert!(copy.cursor().visible);
    // A synchronized update in progress is not carried
    t.feed(b"m\x1b[?2026hheld");
    let cold = t
        .ansi_snapshot_with(SnapshotOptions {
            scrollback: None,
            exclude_modes: true,
        })
        .unwrap();
    assert!(!String::from_utf8_lossy(&cold).contains("2026"));
}

/// Snapshots `input` at every split (fed whole and byte by byte), restores it into a new engine and feeds both the
/// rest: the screens match at the split and at the end.
fn resumes_at_every_split(input: &[u8]) {
    for at in 0..=input.len() {
        for bytewise in [false, true] {
            let mut t = Engine::new(20, 4, 0);
            if bytewise {
                input[..at].iter().for_each(|b| t.feed(&[*b]));
            } else {
                t.feed(&input[..at]);
            }
            let mut copy = Engine::new(20, 4, 0);
            copy.feed(&snapshot(&mut t));
            assert_same_screen(&t, &copy);
            t.feed(&input[at..]);
            copy.feed(&input[at..]);
            assert_eq!(rows(&copy), rows(&t), "{at} {bytewise}");
            assert_same_screen(&t, &copy);
        }
    }
}

#[test]
fn tail_keeps_escape_across_c0_and_del() {
    // vte executes C0s and ignores DEL after ESC (and after its intermediates) without leaving the sequence.
    resumes_at_every_split(b"ab\x1b\r[31mX\x1b\x7f[1mY\x1b(\n0qq\x1b(\x7fBq");
}

#[test]
fn tail_keeps_dcs_and_apc_across_bel() {
    // Only OSC ends on BEL; DCS passes it to its handler, SOS/PM/APC ignore it.
    resumes_at_every_split(
        b"\x1bPzz\x07ab\x1b\\cd\x1bP1<z\x07ab\x1b\\ef\x1b_x\x07ab\x1b\\gh\x1bXy\x07ab\x1b\\ij",
    );
}

#[test]
fn tail_ends_dcs_on_c1_st_byte() {
    // vte takes a 0x9c byte in a DCS string as ST, even inside a UTF-8 sequence.
    resumes_at_every_split(b"\x1bPzz\xe4\x9c\xadcd");
}

#[test]
fn tail_keeps_escape_across_utf8_right_after_it() {
    // vte ignores bytes from 0x80 after ESC, so a UTF-8 character there does not end the sequence.
    resumes_at_every_split("ab\x1b中M\x1b(\u{e9}0q".as_bytes());
}

/// Restores `input`'s snapshot into a new engine, then feeds both `follow`: the screens match before and after.
fn carries(input: &[u8], follow: &[u8]) {
    let mut t = Engine::new(20, 4, 10);
    t.feed(input);
    let mut copy = assert_round_trip(&mut t, 10);
    t.feed(follow);
    copy.feed(follow);
    assert_eq!(
        rows(&copy),
        rows(&t),
        "{:?}",
        String::from_utf8_lossy(input)
    );
    assert_same_screen(&t, &copy);
}

#[test]
fn charsets_are_carried() {
    let follow = b"q\x0fq\x0eq\x1b[?1049lq";
    for input in [
        &b"qq\x1b(0"[..],
        b"\x1b(0\x1b)0\x0e\x0f",
        // The edge cell a pending wrap rewrites is printed before the charsets come back.
        b"qqqqqqqqqqqqqqqqqqqq\x1b(0",
        // Each screen's cursor has its own charsets.
        b"\x1b(0\x1b[?1049hq\x1b(B\x1b)0q",
    ] {
        carries(input, follow);
    }
}

#[test]
fn saved_cursor_is_carried() {
    let follow = b"\x1b8qX\x1b[?1049h\x1b8Y\x1b[?1049l\x1b8Z";
    for input in [
        // Position, pen, link and charsets, saved apart from the cursor's own.
        &b"\x1b[3;5H\x1b[31;1m\x1b]8;;https://a\x1b\\\x1b(0\x1b7\x1b[H\x1b[0m\x1b]8;;\x1b\\\x1b(Bhome"[..],
        // Saved in a pending wrap.
        b"\x1b[1;20HW\x1b7\x1b[2;1H",
        // The alternate screen's own saved cursor, while on it and after leaving it.
        b"\x1b[2;2H\x1b7\x1b[3;3H\x1b[?1049h\x1b[4;4H\x1b[32m\x1b7\x1b[Halt",
        b"\x1b[?1049h\x1b[4;6H\x1b[44m\x1b7\x1b[?1049l\x1b[2;2Hmain",
    ] {
        carries(input, follow);
    }
}

#[test]
fn cursor_shape_is_carried() {
    for input in [
        &b"\x1b[5 q"[..],
        b"\x1b[4 q",
        b"\x1b[2 q",
        b"\x1b[?12h",
        b"\x1b[3 q\x1b[0 q",
    ] {
        carries(input, b"\x1b[?12lx");
    }
}

#[test]
fn exclude_modes_leaves_out_the_carried_state() {
    let mut t = Engine::new(20, 4, 0);
    t.feed(b"\x1b(0\x1b7\x1b[3g\x1b[5 qx");
    let cold = t
        .ansi_snapshot_with(SnapshotOptions {
            scrollback: None,
            exclude_modes: true,
        })
        .unwrap();
    let mut copy = Engine::new(20, 4, 0);
    copy.feed(&cold);
    copy.feed(b"\tq\x1b8");
    assert_eq!(copy.row_text(0).unwrap(), "\u{2502}       q");
    assert_eq!((copy.cursor().styled, copy.cursor().col), (false, 0));
}
