// Parser hooks: parsing stops at a hooked sequence before its default action, and the host runs or skips it.
use wongtty::{Engine, Hook, HookKind, hook_id, mode};

fn term() -> Engine {
    Engine::new(20, 4, 100)
}

/// Feeds `bytes`, deciding each hook with `handled` (true skips the default), and returns the hooks met.
fn feed_with(t: &mut Engine, bytes: &[u8], mut handled: impl FnMut(&Hook) -> bool) -> Vec<Hook> {
    let mut hooks = Vec::new();
    let mut fed = 0;
    loop {
        while let Some(hook) = t.pending_hook() {
            let skip = handled(&hook);
            hooks.push(hook);
            t.finish_hook(!skip);
        }
        // Called once more after the last hook (even with nothing left) to go on where parsing stopped.
        fed += t.feed_until_hook(&bytes[fed..]);
        if fed == bytes.len() && t.pending_hook().is_none() {
            return hooks;
        }
    }
}

#[test]
fn csi_hook_stops_right_after_the_sequence() {
    let mut t = term();
    t.set_hook(HookKind::Csi, hook_id(b"?", b'h'), true);
    let bytes = b"ab\x1b[?1004;25hcd";
    assert_eq!(t.feed_until_hook(bytes), 13);
    assert_eq!(t.row_text(0).unwrap(), "ab");
    let hook = t.pending_hook().unwrap();
    assert_eq!(
        hook,
        Hook {
            kind: HookKind::Csi,
            id: hook_id(b"?", b'h'),
            params: vec![vec![1004], vec![25]],
            data: vec![]
        }
    );
    // The default has not run yet, and nothing more is taken while the hook is pending.
    assert_eq!(t.modes() & mode::FOCUS_EVENTS, 0);
    assert_eq!(t.feed_until_hook(b"cd"), 0);
    t.finish_hook(true);
    assert_ne!(t.modes() & mode::FOCUS_EVENTS, 0);
    assert_eq!(t.feed_until_hook(&bytes[13..]), 2);
    assert_eq!(t.row_text(0).unwrap(), "abcd");
}

#[test]
fn empty_sub_parameters_are_minus_one_and_the_default_still_reads_them() {
    let mut t = term();
    t.set_hook(HookKind::Csi, hook_id(b"", b'm'), true);
    t.set_hook(HookKind::Dcs, hook_id(b"$", b'q'), true);
    let hooks = feed_with(
        &mut t,
        b"\x1b[1;38:2::10:20:30;:5mA\x1bP1:;2::$qm\x1b\\",
        |h| h.kind == HookKind::Dcs,
    );
    // As xterm.js `Params`: an empty sub-parameter is -1, an empty parameter (also before a `:`) is 0.
    assert_eq!(
        hooks[0].params,
        [vec![1], vec![38, 2, -1, 10, 20, 30], vec![0, 5]]
    );
    assert_eq!(hooks[1].params, [vec![1, -1], vec![2, -1, -1]]);
    let mut cells = Vec::new();
    t.row_cells(0, &mut cells);
    assert_eq!(cells[0].fg, wongtty::color::RGB | 0x0a141e);
    assert_ne!(cells[0].attrs & wongtty::attr::BOLD, 0);
}

#[test]
fn handled_hook_skips_the_default_action() {
    let mut t = term();
    t.set_hook(HookKind::Csi, hook_id(b"", b'n'), true);
    t.set_hook(HookKind::Csi, hook_id(b"?", b'l'), true);
    let hooks = feed_with(&mut t, b"\x1b[6n\x1b[?25l\x1b[5n", |h| {
        h.params == [vec![6]] || h.params == [vec![25]]
    });
    assert_eq!(hooks.len(), 3);
    // CPR and hiding the cursor were skipped; DSR 5 ran its default.
    assert_eq!(t.take_replies(), b"\x1b[0n");
    assert!(t.cursor().visible);
}

#[test]
fn default_keeps_sub_parameters_and_intermediates() {
    let mut t = term();
    t.set_hook(HookKind::Csi, hook_id(b"", b'm'), true);
    t.set_hook(HookKind::Csi, hook_id(b" ", b'q'), true);
    let hooks = feed_with(&mut t, b"\x1b[1;38:2:1:2:3mX\x1b[5 q", |_| false);
    assert_eq!(hooks[0].params, vec![vec![1], vec![38, 2, 1, 2, 3]]);
    assert_eq!(hooks[1].params, vec![vec![5]]);
    let mut cells = Vec::new();
    t.row_cells(0, &mut cells);
    assert_eq!(cells[0].fg, wongtty::color::RGB | 0x010203);
    assert_ne!(cells[0].attrs & wongtty::attr::BOLD, 0);
    assert_eq!(t.cursor().shape, wongtty::CursorShape::Bar);
}

#[test]
fn osc_hook_gets_the_data_after_the_number() {
    let mut t = term();
    t.set_hook(HookKind::Osc, 133, true);
    t.set_hook(HookKind::Osc, 2, true);
    let hooks = feed_with(
        &mut t,
        b"\x1b]133;A;cl=m\x07\x1b]2;one;two\x1b\\\x1b]2;kept\x07",
        |h| h.data == b"one;two",
    );
    let data: Vec<_> = hooks
        .iter()
        .map(|h| (h.id, String::from_utf8(h.data.clone()).unwrap()))
        .collect();
    assert_eq!(
        data,
        [
            (133, "A;cl=m".into()),
            (2, "one;two".into()),
            (2, "kept".into())
        ]
    );
    // The handled title was skipped, the last one ran its default.
    assert_eq!(t.take_title().as_deref(), Some("kept"));
}

#[test]
fn osc_aborted_by_can_is_not_stopped_at() {
    let mut t = term();
    t.set_hook(HookKind::Osc, 2, true);
    t.set_hook(HookKind::Dcs, hook_id(b"$", b'q'), true);
    let hooks = feed_with(&mut t, b"\x1b]2;kept\x18X\x1bP$qm\x1aY", |_| true);
    assert!(hooks.is_empty());
    // The aborted title applies as without a hook (vte applies it, unlike xterm.js).
    assert_eq!(t.take_title().as_deref(), Some("kept"));
    assert_eq!(t.row_text(0).unwrap(), "XY");
}

#[test]
fn dcs_hook_gets_the_whole_payload() {
    let mut t = term();
    t.set_hook(HookKind::Dcs, hook_id(b"$", b'q'), true);
    let bytes = b"\x1bP1$qm\x1b\\Z";
    // Split anywhere, the payload arrives whole when the sequence ends.
    let mut hooks = Vec::new();
    for b in bytes {
        hooks.extend(feed_with(&mut t, std::slice::from_ref(b), |_| true));
    }
    assert_eq!(
        hooks,
        [Hook {
            kind: HookKind::Dcs,
            id: hook_id(b"$", b'q'),
            params: vec![vec![1]],
            data: b"m".to_vec()
        }]
    );
    assert_eq!(t.row_text(0).unwrap(), "Z");
}

#[test]
fn unregistered_hook_no_longer_stops() {
    let mut t = term();
    let id = hook_id(b"", b'n');
    t.set_hook(HookKind::Csi, id, true);
    t.set_hook(HookKind::Csi, id, false);
    assert_eq!(t.feed_until_hook(b"\x1b[6n"), 4);
    assert_eq!(t.pending_hook(), None);
    assert_eq!(t.take_replies(), b"\x1b[1;1R");
}

#[test]
fn feed_runs_hook_defaults() {
    let mut t = term();
    t.set_hook(HookKind::Csi, hook_id(b"", b'n'), true);
    t.feed(b"\x1b[6nA");
    assert_eq!(t.take_replies(), b"\x1b[1;1R");
    assert_eq!(t.row_text(0).unwrap(), "A");
}

#[test]
fn hooks_in_a_synchronized_update_stop_where_they_are() {
    let mut t = term();
    t.set_hook(HookKind::Csi, hook_id(b"", b'n'), true);
    let bytes = b"\x1b[?2026hA\x1b[6nB\x1b[?2026lC";
    // As in xterm.js, the update's bytes are parsed at once, so its hooks are met as they come.
    assert_eq!(t.feed_until_hook(bytes), 13);
    assert_eq!(t.pending_hook().unwrap().params, [vec![6]]);
    assert_eq!(t.row_text(0).unwrap(), "A");
    assert!(t.sync_pending());
    // A timed-out update ends without parsing anything.
    t.flush_sync();
    assert!(!t.sync_pending());
    assert!(t.pending_hook().is_some());
    t.finish_hook(false);
    assert_eq!(t.feed_until_hook(&bytes[13..]), bytes.len() - 13);
    assert_eq!(t.row_text(0).unwrap(), "ABC");
    assert_eq!(t.take_replies(), b"");
}

#[test]
fn hooked_decstr_skips_the_soft_reset() {
    let mut t = term();
    t.set_hook(HookKind::Csi, hook_id(b"!", b'p'), true);
    feed_with(&mut t, b"\x1b[?25l\x1b[!p", |_| true);
    assert!(!t.cursor().visible);
    feed_with(&mut t, b"\x1b[!p", |_| false);
    assert!(t.cursor().visible);
}

#[test]
fn split_feeds_match_one_feed() {
    let bytes = b"x\x1b[?1049h\x1b]133;A\x07y\x1b[6n\x1b[?2026hz\x1b[!p\x1bP$qm\x1b\\\x1b[?2026l\x1b[?1049lw";
    let run = |chunks: &[&[u8]]| {
        let mut t = term();
        for (kind, id) in [
            (HookKind::Csi, hook_id(b"?", b'h')),
            (HookKind::Osc, 133),
            (HookKind::Csi, hook_id(b"", b'n')),
            (HookKind::Dcs, hook_id(b"$", b'q')),
        ] {
            t.set_hook(kind, id, true);
        }
        let mut hooks = Vec::new();
        for chunk in chunks {
            hooks.extend(feed_with(&mut t, chunk, |h| {
                h.kind == HookKind::Csi && h.id == hook_id(b"", b'n')
            }));
        }
        (hooks, t.row_text(0), t.take_replies(), t.modes())
    };
    let whole = run(&[bytes]);
    assert_eq!(whole.0.len(), 5);
    for at in 1..bytes.len() {
        assert_eq!(run(&[&bytes[..at], &bytes[at..]]), whole, "split at {at}");
    }
}

#[test]
fn hooks_that_run_their_defaults_change_nothing() {
    // Seeded random output of hooked sequences, text and synchronized updates, fed in random pieces.
    const PIECES: &[&[u8]] = &[
        b"text ",
        "한글".as_bytes(),
        b"\r\n",
        b"\x1b[6n",
        b"\x1b[?25l",
        b"\x1b[?25h",
        b"\x1b[31;1m",
        b"\x1b[38:5:3m",
        b"\x1b[2;5H",
        b"\x1b[?2026h",
        b"\x1b[?2026l",
        b"\x1b]2;title\x07",
        b"\x1b]133;A\x1b\\",
        b"\x1b]10;?\x07",
        b"\x1bP$qm\x1b\\",
        b"\x1b[!p",
        b"\x1b[5 q",
        b"\x1b[?1049h",
        b"\x1b[?1049l",
        b"\x1b]2;cut\x18",
        b"\x1b[",
    ];
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move |n: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % n as u64) as usize
    };
    for _ in 0..200 {
        let bytes: Vec<u8> = (0..40)
            .flat_map(|_| PIECES[next(PIECES.len())].iter().copied())
            .collect();
        let (mut plain, mut hooked) = (term(), term());
        for (kind, id) in [
            (HookKind::Csi, hook_id(b"", b'n')),
            (HookKind::Csi, hook_id(b"?", b'h')),
            (HookKind::Csi, hook_id(b"?", b'l')),
            (HookKind::Csi, hook_id(b"", b'm')),
            (HookKind::Csi, hook_id(b"!", b'p')),
            (HookKind::Csi, hook_id(b"", b'H')),
            (HookKind::Osc, 2),
            (HookKind::Osc, 133),
            (HookKind::Osc, 10),
            (HookKind::Dcs, hook_id(b"$", b'q')),
        ] {
            hooked.set_hook(kind, id, true);
        }
        plain.set_colors(Some([0x123456; wongtty::COLOR_SLOTS]));
        hooked.set_colors(Some([0x123456; wongtty::COLOR_SLOTS]));
        plain.feed(&bytes);
        let mut at = 0;
        while at < bytes.len() {
            let end = (at + 1 + next(24)).min(bytes.len());
            feed_with(&mut hooked, &bytes[at..end], |_| false);
            at = end;
        }
        hooked.flush_sync();
        feed_with(&mut hooked, b"", |_| false);
        plain.flush_sync();
        let screen = |t: &Engine| (0..4).map(|y| t.row_text(y)).collect::<Vec<_>>();
        assert_eq!(screen(&hooked), screen(&plain));
        assert_eq!(
            (hooked.cursor(), hooked.modes()),
            (plain.cursor(), plain.modes())
        );
        assert_eq!(hooked.take_replies(), plain.take_replies());
        assert_eq!(hooked.take_title(), plain.take_title());
    }
}
