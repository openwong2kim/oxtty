//! Where a resize leaves the cursor.
use wongtty::Engine;
use wongtty::vt::event::VoidListener;
use wongtty::vt::index::Column;
use wongtty::vt::term::test::TermSize;
use wongtty::vt::term::{Config, Term};
use wongtty::vt::vte::ansi::Processor;

#[test]
fn widening_frees_a_pending_wrap_on_both_screens() {
    for enter in ["\x1b[?1049h", ""] {
        let mut t = Engine::new(5, 3, 100);
        t.feed(format!("{enter}abcde").as_bytes());
        t.resize(7, 3);
        // As xterm.js: past the old last column, which is now a real column.
        assert_eq!((t.cursor().line, t.cursor().col), (0, 5), "{enter:?}");
        t.feed(b"Z");
        assert_eq!(t.row_text(0).as_deref(), Some("abcdeZ"), "{enter:?}");
    }
}

#[test]
fn upstream_keeps_the_pending_wrap_on_the_alternate_screen() {
    let mut term = Term::new(Config::default(), &TermSize::new(5, 3), VoidListener);
    Processor::new().advance(&mut term, b"\x1b[?1049habcde");
    term.resize(TermSize::new(7, 3));
    assert_eq!(term.grid().cursor.point.column, Column(4));
    assert!(term.grid().cursor.input_needs_wrap);
}
