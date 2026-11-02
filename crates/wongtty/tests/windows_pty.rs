// xterm.js `windowsPty` behaviour: the conpty wrapping heuristic, no reflow, and blank rows on growth.
use wongtty::{Engine, WindowsPty, WindowsPtyBackend};

fn windows(backend: Option<WindowsPtyBackend>, build_number: Option<u32>) -> WindowsPty {
    WindowsPty {
        backend,
        build_number,
    }
}

const OLD_CONPTY: WindowsPty = WindowsPty {
    backend: Some(WindowsPtyBackend::Conpty),
    build_number: Some(19045),
};

fn screen(t: &Engine) -> Vec<String> {
    (0..t.rows() as i32)
        .map(|y| t.row_text(y).unwrap())
        .collect()
}

#[test]
fn old_conpty_marks_a_line_wrapped_when_the_row_above_ends_in_text() {
    for compressed in [true, false] {
        let mut t = if compressed {
            Engine::new(10, 4, 100)
        } else {
            Engine::new_uncompressed(10, 4, 100)
        };
        t.set_windows_pty(OLD_CONPTY);
        // conpty prints a full row and moves on with CR LF instead of wrapping.
        t.feed(b"0123456789\r\nnext\r\nends here \r\n");
        assert!(t.row_wrapped(0));
        assert!(!t.row_wrapped(1), "the row ends in a blank");
        assert!(!t.row_wrapped(2), "the row ends in a space");
        // CSI H checks the row above the cursor before moving; CSI f does not.
        t.feed(b"\x1b[2;1Hxxxxxxxxxx\x1b[3f\x1b[3f");
        assert!(!t.row_wrapped(1), "CSI f left it");
        t.feed(b"\x1b[3H");
        assert!(t.row_wrapped(1));
        t.feed(b"\x1b[2;10H \x1b[3H");
        assert!(!t.row_wrapped(1), "now it ends in a space");
    }
}

#[test]
fn the_heuristic_is_only_for_conpty_before_21376() {
    for pty in [
        WindowsPty::default(),
        windows(Some(WindowsPtyBackend::Conpty), Some(21376)),
        windows(Some(WindowsPtyBackend::Winpty), Some(19045)),
        windows(Some(WindowsPtyBackend::Conpty), None),
        windows(None, Some(19045)),
    ] {
        let mut t = Engine::new(10, 4, 100);
        t.set_windows_pty(pty);
        t.feed(b"0123456789\r\n");
        assert!(!t.row_wrapped(0), "{pty:?}");
    }
}

#[test]
fn a_build_before_conpty_21376_does_not_reflow() {
    for (pty, reflows) in [
        (WindowsPty::default(), true),
        (OLD_CONPTY, false),
        (windows(Some(WindowsPtyBackend::Winpty), Some(22000)), false),
        (windows(None, Some(22000)), false),
        (windows(Some(WindowsPtyBackend::Conpty), Some(21376)), true),
        (windows(Some(WindowsPtyBackend::Winpty), None), true),
        (windows(Some(WindowsPtyBackend::Winpty), Some(0)), true),
    ] {
        for compressed in [true, false] {
            let mut t = if compressed {
                Engine::new(10, 4, 100)
            } else {
                Engine::new_uncompressed(10, 4, 100)
            };
            t.set_windows_pty(pty);
            t.feed(b"abcdefghijKL\r\n");
            t.resize(6, 4);
            let expected = if reflows {
                ["abcdef", "ghijKL", ""]
            } else {
                ["abcdef", "KL", ""]
            };
            assert_eq!(screen(&t)[..3], expected, "{pty:?}");
            // Not reflowed, the first row still continues on the next, as xterm.js keeps the line's flag.
            assert!(t.row_wrapped(0), "{pty:?}");
        }
    }
}

#[test]
fn a_windows_pty_grows_blank_rows_instead_of_pulling_back_history() {
    for compressed in [true, false] {
        let make = |pty| {
            let mut t = if compressed {
                Engine::new(10, 3, 100)
            } else {
                Engine::new_uncompressed(10, 3, 100)
            };
            t.set_windows_pty(pty);
            for i in 0..30 {
                t.feed(format!("line {i}\r\n").as_bytes());
            }
            t.feed(b"$ ");
            t.resize(10, 5);
            t
        };
        let t = make(windows(Some(WindowsPtyBackend::Conpty), None));
        assert_eq!(screen(&t), ["line 28", "line 29", "$", "", ""]);
        assert_eq!((t.history_len(), t.cursor().line), (28, 2));
        assert_eq!(t.row_text(-1).unwrap(), "line 27");
        // Without one, history comes back into view above the cursor.
        let t = make(WindowsPty::default());
        assert_eq!(
            screen(&t),
            ["line 26", "line 27", "line 28", "line 29", "$"]
        );
        assert_eq!((t.history_len(), t.cursor().line), (26, 4));
    }
}
