// The focus report xterm.js sends when the program sets focus events (`CSI ? 1004 h`).
use wongtty::{Engine, HookKind, hook_id};

#[test]
fn setting_focus_events_reports_the_focus_in_order_with_other_replies() {
    let mut t = Engine::new(10, 3, 0);
    // Without the host's focus nothing is reported, as in @xterm/headless.
    t.feed(b"\x1b[?1004h");
    assert_eq!(t.take_replies(), b"");
    t.set_focus(Some(false));
    t.feed(b"\x1b[6n\x1b[?1004h\x1b[5n");
    assert_eq!(t.take_replies(), b"\x1b[1;1R\x1b[O\x1b[0n");
    // Every set reports again, also among other modes; a reset does not.
    t.set_focus(Some(true));
    t.feed(b"\x1b[?25;1004h\x1b[?1004l");
    assert_eq!(t.take_replies(), b"\x1b[I");
}

#[test]
fn a_hook_that_takes_the_sequence_stops_the_report() {
    let mut t = Engine::new(10, 3, 0);
    t.set_focus(Some(true));
    t.set_hook(HookKind::Csi, hook_id(b"?", b'h'), true);
    assert_eq!(t.feed_until_hook(b"\x1b[?1004h"), 8);
    t.finish_hook(false);
    t.feed_until_hook(b"");
    assert_eq!(t.take_replies(), b"");
    t.feed(b"\x1b[?1004h");
    assert_eq!(t.take_replies(), b"\x1b[I");
}

#[test]
fn a_report_inside_a_synchronized_update_comes_at_once() {
    let mut t = Engine::new(10, 3, 0);
    t.set_focus(Some(false));
    t.feed(b"\x1b[?2026h\x1b[?1004h\x1b[6n");
    assert_eq!(
        t.take_reply_list(),
        [b"\x1b[O".to_vec(), b"\x1b[1;1R".to_vec()]
    );
}
