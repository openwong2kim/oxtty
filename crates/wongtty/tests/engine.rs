use wongtty::{
    COLOR_SLOTS, Cell, CellExtra, Clipboard, CursorShape, Damage, Engine, Hyperlink, attr, color,
    mode,
};

fn term() -> Engine {
    Engine::new(80, 24, 1000)
}

#[test]
fn korean_and_ascii_share_a_row() {
    let mut t = term();
    t.feed("안녕 hello\r\n".as_bytes());
    assert_eq!(t.row_text(0).unwrap(), "안녕 hello");
    let mut cells = Vec::new();
    assert!(t.row_cells(0, &mut cells));
    assert_eq!(cells[0].ch, '안');
    assert_ne!(cells[0].attrs & attr::WIDE, 0);
    assert_ne!(cells[1].attrs & attr::WIDE_SPACER, 0);
    assert_eq!(cells[4].ch, ' ');
    assert_eq!((t.cursor().line, t.cursor().col), (1, 0));
}

#[test]
fn split_utf8_and_escape_across_feeds() {
    let mut t = term();
    let bytes = "한\x1b[31mX".as_bytes();
    for b in bytes {
        t.feed(std::slice::from_ref(b));
    }
    assert_eq!(t.row_text(0).unwrap(), "한X");
    let mut cells = Vec::new();
    t.row_cells(0, &mut cells);
    assert_eq!(cells[2].fg, color::NAMED | 1);
}

#[test]
fn sgr_colors_and_attributes() {
    let mut t = term();
    t.feed(b"\x1b[31mR\x1b[38;5;208mO\x1b[38;2;10;20;30mT\x1b[1;3;4;7mS\x1b[0mN");
    let mut c: Vec<Cell> = Vec::new();
    t.row_cells(0, &mut c);
    assert_eq!(c[0].fg, color::NAMED | 1);
    assert_eq!(c[1].fg, color::INDEXED | 208);
    assert_eq!(c[2].fg, color::RGB | 0x0a141e);
    assert_eq!(
        c[3].attrs & (attr::BOLD | attr::ITALIC | attr::UNDERLINE | attr::INVERSE),
        attr::BOLD | attr::ITALIC | attr::UNDERLINE | attr::INVERSE
    );
    assert_eq!(
        (c[4].fg, c[4].bg, c[4].attrs),
        (color::DEFAULT_FG, color::DEFAULT_BG, 0)
    );
}

#[test]
fn cursor_addressing_and_wrap() {
    let mut t = term();
    t.feed(b"\x1b[5;10HX");
    assert_eq!(t.row_text(4).unwrap(), "         X");
    assert_eq!((t.cursor().line, t.cursor().col), (4, 10));
    t.feed(b"\x1b[7;1H");
    t.feed("a".repeat(85).as_bytes());
    assert_eq!(t.row_text(6).unwrap().len(), 80);
    assert!(t.row_wrapped(6));
    assert_eq!(t.row_text(7).unwrap(), "aaaaa");
}

#[test]
fn alternate_screen_round_trip() {
    let mut t = term();
    t.feed(b"main");
    t.feed(b"\x1b[?1049h\x1b[H\x1b[2Jalt");
    assert_ne!(t.modes() & mode::ALT_SCREEN, 0);
    assert_eq!(t.row_text(0).unwrap(), "alt");
    t.feed(b"\x1b[?1049l");
    assert_eq!(t.modes() & mode::ALT_SCREEN, 0);
    assert_eq!(t.row_text(0).unwrap(), "main");
}

#[test]
fn scrollback_is_addressed_with_negative_lines() {
    let mut t = Engine::new(20, 5, 100);
    for i in 0..12 {
        t.feed(format!("line {i}\r\n").as_bytes());
    }
    assert_eq!(t.history_len(), 8);
    assert_eq!(t.row_text(-8).unwrap(), "line 0");
    assert_eq!(t.row_text(-1).unwrap(), "line 7");
    assert_eq!(t.row_text(0).unwrap(), "line 8");
    assert!(t.row_text(-9).is_none());
    assert!(t.row_text(5).is_none());
}

#[test]
fn scrollback_limit_is_respected() {
    let mut t = Engine::new(20, 5, 3);
    for i in 0..50 {
        t.feed(format!("{i}\r\n").as_bytes());
    }
    assert_eq!(t.history_len(), 3);
}

#[test]
fn damage_reports_only_changed_lines() {
    let mut t = term();
    assert_eq!(t.take_damage(), Damage::Full);
    t.feed(b"\x1b[3;1Hhello");
    match t.take_damage() {
        Damage::Lines { lines, .. } => assert!(lines.contains(&2) && lines.len() <= 2, "{lines:?}"),
        Damage::Full => panic!("expected partial damage"),
    }
    // Nothing changed: no lines, not even the cursor's.
    assert_eq!(
        t.take_damage(),
        Damage::Lines {
            scroll: 0,
            lines: vec![]
        }
    );
    t.resize(100, 30);
    assert_eq!(t.take_damage(), Damage::Full);
}

#[test]
fn idle_frames_report_no_lines_but_writes_under_a_still_cursor_do() {
    let lines = |t: &mut Engine| match t.take_damage() {
        Damage::Lines { lines, .. } => lines,
        Damage::Full => panic!("expected partial damage"),
    };
    let mut t = Engine::new(10, 5, 100);
    t.feed(b"\x1b[2;1H");
    t.take_damage();
    t.feed(b"ab");
    assert_eq!(lines(&mut t), [1]);
    assert_eq!(lines(&mut t), [] as [u16; 0]);
    // A cursor move damages the rows it left and entered.
    t.feed(b"\x1b[4;3H");
    assert_eq!(lines(&mut t), [1, 3]);
    // The cursor does not move: the last column, a combining mark, a cursor style or visibility change.
    t.feed(b"\x1b[4;10H");
    lines(&mut t);
    t.feed(b"X");
    assert_eq!(lines(&mut t), [3]);
    t.feed(b"\x1b[5;2Ha");
    lines(&mut t);
    t.feed("\u{301}".as_bytes());
    assert_eq!(lines(&mut t), [4]);
    t.feed(b"\x1b[?25l\x1b[5 q");
    assert_eq!(lines(&mut t), [] as [u16; 0]);
    // Insert mode no longer damages everything on each frame, only the line written to.
    t.feed(b"\x1b[4h\x1b[1;1Hz");
    assert_eq!(lines(&mut t), [0, 4]);
    assert_eq!(lines(&mut t), [] as [u16; 0]);
}

#[test]
fn reply_list_keeps_each_reply_whole_and_in_order() {
    let mut t = term();
    let mut colors = [0u32; COLOR_SLOTS];
    colors[257] = 0x1e1e2e;
    t.set_colors(Some(colors));
    t.set_focus(Some(false));
    t.feed(b"out\x1b[c\x1b]11;?\x07\x1b[6n\x1b[?1004h\x1b]11;?\x1b\\\x1b[4$p\r\n");
    let bg = b"\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\".to_vec();
    assert_eq!(
        t.take_reply_list(),
        [
            b"\x1b[?1;2c".to_vec(),
            bg.clone(),
            b"\x1b[1;4R".to_vec(),
            b"\x1b[O".to_vec(),
            bg,
            b"\x1b[4;2$y".to_vec(),
        ]
    );
    assert!(t.take_reply_list().is_empty());
    // The concatenated form is the same bytes and drops the boundaries too.
    t.feed(b"\x1b[5n\x1b[6n");
    assert_eq!(t.take_replies(), b"\x1b[0n\x1b[2;1R");
    assert!(t.take_reply_list().is_empty());
}

#[test]
fn device_reports_are_returned_as_replies() {
    let mut t = term();
    t.feed(b"\x1b[3;7H\x1b[6n");
    assert_eq!(t.take_replies(), b"\x1b[3;7R");
    assert!(t.take_replies().is_empty());
    t.feed(b"\x1b[c");
    assert!(t.take_replies().starts_with(b"\x1b[?"));
}

#[test]
fn title_and_bell() {
    let mut t = term();
    t.feed(b"\x1b]0;my shell\x07\x07");
    assert_eq!(t.take_title().as_deref(), Some("my shell"));
    assert_eq!(t.take_title(), None);
    assert_eq!(t.take_bells(), 1);
}

#[test]
fn synchronized_output_is_applied_at_once_and_open_until_end_or_host_flush() {
    let mut t = term();
    // As in xterm.js, the buffer has the output inside the update; only the host's frames wait.
    t.feed(b"\x1b[?2026hSYNC");
    assert!(t.sync_pending());
    assert_eq!(t.row_text(0).unwrap(), "SYNC");
    t.feed(b"\x1b[?2026$p");
    assert_eq!(t.take_replies(), b"\x1b[?2026;1$y");
    t.feed(b"\x1b[?2026l");
    assert!(!t.sync_pending());
    t.feed(b"\x1b[?2026hLATE");
    t.flush_sync();
    assert!(!t.sync_pending());
    assert_eq!(t.row_text(0).unwrap(), "SYNCLATE");
    t.feed(b"\x1b[?2026$p\x1b[?2026h\x1bc");
    assert!(!t.sync_pending());
    assert_eq!(t.take_replies(), b"\x1b[?2026;2$y");
}

#[test]
fn reset_keeps_cursor_hidden_like_xterm() {
    let mut t = term();
    t.feed(b"\x1b[?25l\x1b[?2004hX\x1bc");
    assert!(!t.cursor().visible);
    assert_eq!(t.modes() & mode::BRACKETED_PASTE, 0);
    assert_eq!(t.row_text(0).unwrap(), "");
    t.feed(b"\x1b[?25h\x1bc");
    assert!(t.cursor().visible);
}

#[test]
fn combining_marks_are_kept() {
    let mut t = term();
    t.feed("e\u{301}x".as_bytes());
    assert_eq!(t.row_text(0).unwrap(), "e\u{301}x");
    let mut cells = Vec::new();
    t.row_cells(0, &mut cells);
    assert_ne!(cells[0].attrs & attr::COMBINING, 0);
    assert_eq!(t.cell_text(0, 0).unwrap(), "e\u{301}");
    assert!(t.cell_text(0, 80).is_none());
}

#[test]
fn modes_track_common_private_modes() {
    let mut t = term();
    assert_ne!(t.modes() & mode::SHOW_CURSOR, 0);
    t.feed(b"\x1b[?25l\x1b[?2004h\x1b[?1h\x1b[?1000h\x1b[?1006h");
    let m = t.modes();
    assert_eq!(m & mode::SHOW_CURSOR, 0);
    assert!(!t.cursor().visible);
    for bit in [
        mode::BRACKETED_PASTE,
        mode::APP_CURSOR,
        mode::MOUSE_CLICK,
        mode::SGR_MOUSE,
    ] {
        assert_ne!(m & bit, 0, "mode bit {bit:#x}");
    }
}

#[test]
fn resize_keeps_content_and_clamps() {
    let mut t = term();
    t.feed(b"keep me");
    t.resize(40, 10);
    assert_eq!((t.cols(), t.rows()), (40, 10));
    assert_eq!(t.row_text(0).unwrap(), "keep me");
    t.resize(0, 0);
    assert_eq!((t.cols(), t.rows()), (wongtty::MIN_COLS, wongtty::MIN_ROWS));
}

#[test]
fn resize_reflows_all_but_the_cursor_paragraph() {
    // xterm.js reflowCursorLine: false — only the paragraph holding the cursor is not re-wrapped.
    let mut t = Engine::new(10, 5, 100);
    t.feed(b"0123456789ab\r\nXXXXXXXXXXyy");
    t.resize(20, 5);
    let rows: Vec<String> = (0..3).map(|y| t.row_text(y).unwrap()).collect();
    assert_eq!(rows, ["0123456789ab", "XXXXXXXXXX", "yy"]);
}

#[test]
fn decrqss_answers_as_xterm_js() {
    let mut t = Engine::new(20, 6, 100);
    t.feed(b"\x1b[2;5r\x1b[6 q\x1bP$qr\x1b\\\x1bP$q q\x1b\\\x1bP$qm\x1b\\\x1bP$q\"p\x1b\\\x1bP$qx\x1b\\");
    // The cursor style is the host's, not what DECSCUSR set (xterm.js reports its options).
    assert_eq!(
        t.take_replies(),
        b"\x1bP1$r2;5r\x1b\\\x1bP1$r2 q\x1b\\\x1bP1$r0m\x1b\\\x1bP1$r61;1\"p\x1b\\\x1bP0$r\x1b\\"
    );
    t.set_cursor_default(CursorShape::Bar, true);
    // CAN aborts a request; ESC ends one.
    t.feed(b"\x1bP$q q\x18\x1bP$q q\x1b[A");
    assert_eq!(t.take_replies(), b"\x1bP1$r5 q\x1b\\");
}

#[test]
fn device_attributes_match_xterm_js() {
    // The replies @xterm/headless gives, so programs that probe the terminal see what they saw on xterm.js.
    let mut t = term();
    t.feed(b"\x1b[c\x1b[0c");
    assert_eq!(t.take_replies(), b"\x1b[?1;2c\x1b[?1;2c");
    t.feed(b"\x1b[>c");
    assert_eq!(t.take_replies(), b"\x1b[>0;276;0c");
}

#[test]
fn packed_rows_match_cells() {
    let mut t = term();
    t.feed("가\x1b[32mb".as_bytes());
    let (mut cells, mut packed) = (Vec::new(), Vec::new());
    t.row_cells(0, &mut cells);
    t.row_packed(0, &mut packed);
    assert_eq!(packed.len(), 80 * Cell::PACKED_WORDS);
    for (i, c) in cells.iter().enumerate() {
        assert_eq!(
            &packed[i * 4..i * 4 + 4],
            &[c.ch as u32, c.fg, c.bg, c.attrs]
        );
    }
}

#[test]
fn cursor_shape_follows_decscusr() {
    let mut t = term();
    assert_eq!(
        (t.cursor().shape, t.cursor().blinking),
        (CursorShape::Block, false)
    );
    t.feed(b"\x1b[5 q");
    assert_eq!(
        (t.cursor().shape, t.cursor().blinking),
        (CursorShape::Bar, true)
    );
    t.feed(b"\x1b[4 q");
    assert_eq!(
        (t.cursor().shape, t.cursor().blinking),
        (CursorShape::Underline, false)
    );
    t.feed(b"\x1b[0 q");
    assert_eq!(t.cursor().shape, CursorShape::Block);
}

#[test]
fn cursor_tells_a_program_chosen_steady_block_from_the_default() {
    // A host with its own cursor style keeps it until the program picks one, even a steady block.
    let mut t = term();
    assert!(!t.cursor().styled);
    t.feed(b"\x1b[2 q");
    let c = t.cursor();
    assert_eq!(
        (c.shape, c.blinking, c.styled),
        (CursorShape::Block, false, true)
    );
    t.feed(b"\x1b[0 q");
    assert!(!t.cursor().styled);
    t.feed(b"\x1b[6 q\x1bc");
    assert!(!t.cursor().styled, "RIS drops the program's style");
}

#[test]
fn row_extras_carry_combining_underline_colour_and_links() {
    let mut t = term();
    t.feed("e\u{301}\x1b[4:3;58;2;255;0;0mX\x1b[0m".as_bytes());
    t.feed(b"\x1b]8;id=a;https://wongtty.dev\x1b\\LN\x1b]8;;\x1b\\Z");
    let mut cells = Vec::new();
    t.row_cells(0, &mut cells);
    assert_ne!(cells[1].attrs & attr::UNDERLINE_COLOR, 0);
    assert_ne!(cells[2].attrs & attr::HYPERLINK, 0);
    assert_eq!(
        cells[4].attrs & (attr::HYPERLINK | attr::UNDERLINE_COLOR),
        0
    );

    let mut extras = Vec::new();
    assert!(t.row_extras(0, &mut extras));
    let link = Some(Hyperlink {
        id: "a".into(),
        uri: "https://wongtty.dev".into(),
    });
    assert_eq!(
        extras,
        [
            CellExtra {
                col: 0,
                text: Some("e\u{301}".into()),
                underline_color: None,
                hyperlink: None
            },
            CellExtra {
                col: 1,
                text: None,
                underline_color: Some(color::RGB | 0xff0000),
                hyperlink: None
            },
            CellExtra {
                col: 2,
                text: None,
                underline_color: None,
                hyperlink: link.clone()
            },
            CellExtra {
                col: 3,
                text: None,
                underline_color: None,
                hyperlink: link
            },
        ]
    );
    assert!(!t.row_extras(24, &mut extras));
}

#[test]
fn both_halves_of_a_wide_char_carry_its_link() {
    let mut t = term();
    t.feed("\x1b]8;id=w;https://wongtty.dev\x1b\\\x1b[58;5;1m漢\x1b]8;;\x1b\\".as_bytes());
    let mut cells = Vec::new();
    t.row_cells(0, &mut cells);
    assert_ne!(cells[1].attrs & attr::WIDE_SPACER, 0);
    assert_ne!(cells[1].attrs & attr::HYPERLINK, 0);
    let mut extras = Vec::new();
    assert!(t.row_extras(0, &mut extras));
    let link = Some(Hyperlink {
        id: "w".into(),
        uri: "https://wongtty.dev".into(),
    });
    // The underline colour stays with the left half, which draws the whole glyph.
    assert_eq!(
        extras,
        [
            CellExtra {
                col: 0,
                text: None,
                underline_color: Some(color::INDEXED | 1),
                hyperlink: link.clone()
            },
            CellExtra {
                col: 1,
                text: None,
                underline_color: None,
                hyperlink: link
            },
        ]
    );
}

#[test]
fn view_scrolls_into_history_and_back() {
    let mut t = Engine::new(20, 3, 100);
    for i in 0..10 {
        t.feed(format!("line{i}\r\n").as_bytes());
    }
    t.take_damage();
    t.scroll_display(2);
    assert_eq!(t.display_offset(), 2);
    assert_eq!(t.take_damage(), Damage::Full);
    // Viewport row 0 = line(0 - 2)
    assert_eq!(t.row_text(-2).unwrap(), "line6");
    t.scroll_display(1000);
    assert_eq!(t.display_offset(), t.history_len());
    t.scroll_to_bottom();
    assert_eq!(t.display_offset(), 0);
}

#[test]
fn scrolling_output_reports_scroll_and_new_lines() {
    let mut t = Engine::new(20, 5, 100);
    t.feed(b"a\r\nb\r\nc\r\nd\r\ne");
    t.take_damage();
    t.feed(b"\r\nf");
    // The screen moved up one line: redraw only the old cursor line (moved to 3) and the new line 4.
    assert_eq!(
        t.take_damage(),
        Damage::Lines {
            scroll: 1,
            lines: vec![3, 4]
        }
    );
    t.feed(b"\r\ng\r\nh");
    assert_eq!(
        t.take_damage(),
        Damage::Lines {
            scroll: 2,
            lines: vec![2, 3, 4]
        }
    );
    // Scrolling by the screen height or more redraws everything.
    t.feed(b"\r\n1\r\n2\r\n3\r\n4\r\n5");
    assert_eq!(t.take_damage(), Damage::Full);
    // If the scroll region (vim etc.) is part of the screen, treat it as full.
    t.feed(b"\x1b[2;4r\x1b[4;1H\n");
    assert_eq!(t.take_damage(), Damage::Full);
}

#[test]
fn erase_display_keeps_history_like_xterm() {
    // xterm.js scrollOnEraseInDisplay: false — ED2 on the primary screen only clears the screen and does not push into history.
    let mut t = Engine::new(20, 6, 100);
    t.feed(b"a\r\nb\r\nc\x1b[2J");
    assert_eq!(t.history_len(), 0);
    assert!((0..6).all(|y| t.row_text(y).unwrap().is_empty()));
    assert_eq!((t.cursor().line, t.cursor().col), (2, 1));
}

#[test]
fn legacy_alternate_screen_modes_match_xterm() {
    for enter in ["\x1b[?47h", "\x1b[?1047h"] {
        let mut t = Engine::new(20, 6, 100);
        t.feed(format!("main{enter}alt").as_bytes());
        assert_ne!(t.modes() & mode::ALT_SCREEN, 0, "{enter:?}");
        assert_eq!(t.row_text(0).unwrap(), "    alt");
        // xterm.js: on exit the primary screen cursor inherits the alternate screen cursor position (no restore, unlike 1049).
        t.feed(format!("\x1b[3;5H{}", enter.replace('h', "l")).as_bytes());
        assert_eq!(t.modes() & mode::ALT_SCREEN, 0);
        assert_eq!(t.row_text(0).unwrap(), "main");
        assert_eq!((t.cursor().line, t.cursor().col), (2, 4));
    }
    // 47/1047 do not overwrite the saved cursor (DECSC).
    let mut t = Engine::new(20, 6, 100);
    t.feed(b"\x1b[2;2H\x1b7\x1b[H\x1b[?47h\x1b[?47l\x1b8");
    assert_eq!((t.cursor().line, t.cursor().col), (1, 1));
    // Entering with 47 and leaving with ?1049l, xterm.js goes to the primary screen saved cursor (home here, since nothing was saved).
    let mut t = Engine::new(20, 6, 100);
    t.feed(b"main\x1b[?47halt\x1b[?1049lX");
    assert_eq!(t.row_text(0).unwrap(), "Xain");
}

#[test]
fn mode_1048_saves_and_restores_cursor() {
    let mut t = Engine::new(20, 6, 100);
    t.feed(b"\x1b[3;3H\x1b[?1048h\x1b[H\x1b[?1048lX");
    assert_eq!(t.row_text(2).unwrap(), "  X");
    assert_eq!((t.cursor().line, t.cursor().col), (2, 3));
}

#[test]
fn narrowing_fills_empty_rows_before_history_like_xterm() {
    // xterm.js _reflowSmaller: if the history is empty, drop as many lines below the cursor as were added and move the cursor down.
    let rows = |t: &Engine| {
        (0..t.rows() as i32)
            .map(|y| t.row_text(y).unwrap())
            .collect::<Vec<_>>()
    };
    let mut t = Engine::new(16, 4, 100);
    t.feed("abc한글0123456789\r\nlast".as_bytes());
    t.resize(8, 4);
    assert_eq!(t.history_len(), 0);
    assert_eq!(rows(&t), ["abc한글0", "12345678", "9", "last"]);
    assert_eq!((t.cursor().line, t.cursor().col), (3, 4));
    // Once it reaches the end of the screen, the rest goes to history.
    let mut t = Engine::new(16, 6, 100);
    t.feed(b"abcdefghijklmnopqrstuvwxyz0123456\r\nlast");
    t.resize(4, 6);
    assert_eq!(t.history_len(), 4);
    assert_eq!(rows(&t), ["qrst", "uvwx", "yz01", "2345", "6", "last"]);
    assert_eq!((t.cursor().line, t.cursor().col), (5, 3));
    // Even when the paragraph below the cursor grows, xterm.js moves the cursor down by the number of added lines.
    let mut t = Engine::new(16, 4, 100);
    t.feed("top\r\nabc한글0123456789\x1b[1;4H".as_bytes());
    t.resize(8, 4);
    assert_eq!(t.history_len(), 0);
    assert_eq!(rows(&t), ["top", "abc한글0", "12345678", "9"]);
    assert_eq!((t.cursor().line, t.cursor().col), (1, 3));
}

#[test]
fn decstr_split_across_feeds_resets_modes_and_ends_a_synchronized_update() {
    let mut t = term();
    t.feed(b"\x1b[?25l\x1b[4h\x1b[?2004h\x1b[?2026hA");
    assert!(t.sync_pending());
    for chunk in [&b"\x1b"[..], b"[", b"!", b"pB"] {
        t.feed(chunk);
    }
    assert!(!t.sync_pending());
    assert!(t.cursor().visible);
    assert_eq!(t.modes() & (mode::INSERT | mode::BRACKETED_PASTE), 0);
    assert_eq!(t.row_text(0).unwrap(), "AB");
    // Sequences with parameters or other intermediate characters are not DECSTR.
    t.feed(b"\x1b[4h\x1b[?!p\x1b[!q");
    assert_ne!(t.modes() & mode::INSERT, 0);
    t.feed(b"\x1b[0!p");
    assert_eq!(t.modes() & mode::INSERT, 0);
}

#[test]
fn color_queries_answer_only_with_host_colors_in_xterm_format_and_order() {
    let mut t = term();
    t.feed(b"\x1b]11;?\x07");
    assert!(t.take_replies().is_empty());
    let mut colors = [0u32; COLOR_SLOTS];
    colors[1] = 0xcd3131;
    colors[256] = 0xcccccc;
    colors[257] = 0x1e1e2e;
    t.set_colors(Some(colors));
    // xterm.js: 16-bit channels (rrrr), replies with ST even when the query ends with BEL, and keeps query order with other replies.
    t.feed(b"\x1b]11;?\x07\x1b[6n\x1b]4;1;?\x1b\\");
    assert_eq!(
        String::from_utf8(t.take_replies()).unwrap(),
        "\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[1;1R\x1b]4;1;rgb:cdcd/3131/3131\x1b\\"
    );
    // Colors the program changed via OSC 4 take precedence over host colors.
    t.feed(b"\x1b]4;1;rgb:00/80/ff\x07\x1b]4;1;?\x07");
    assert_eq!(
        String::from_utf8(t.take_replies()).unwrap(),
        "\x1b]4;1;rgb:0000/8080/ffff\x1b\\"
    );
}

#[test]
fn clipboard_requests_wait_for_host_access_and_replies_use_the_xterm_addon_format() {
    let mut t = term();
    t.feed(b"\x1b]52;c;aGk=\x07\x1b]52;c;?\x07");
    assert_eq!(
        (t.take_clipboard_write(), t.take_clipboard_read()),
        (None, None)
    );
    t.set_clipboard_access(true, true);
    t.feed(b"\x1b]52;c;aGk=\x07\x1b]52;p;?\x1b\\");
    assert_eq!(
        t.take_clipboard_write(),
        Some((Clipboard::Clipboard, "hi".to_owned()))
    );
    assert_eq!(t.take_clipboard_read(), Some(Clipboard::Selection));
    assert!(!t.reply_clipboard(Clipboard::Clipboard, "nope"));
    assert!(t.reply_clipboard(Clipboard::Selection, "한글"));
    assert_eq!(
        String::from_utf8(t.take_replies()).unwrap(),
        "\x1b]52;p;7ZWc6riA\x07"
    );
}

#[test]
fn replies_stop_growing_when_the_host_never_takes_them() {
    let mut t = term();
    let flood = b"\x1b[6n".repeat(200_000);
    t.feed(&flood);
    let replies = t.take_replies();
    assert!(replies.len() <= wongtty::MAX_REPLIES);
    assert!(replies.ends_with(b"R"), "only whole replies are kept");
    t.feed(b"\x1b[6n");
    assert_eq!(t.take_replies(), b"\x1b[1;1R");
}

#[test]
fn cached_history_rows_follow_a_reflow() {
    let mut packed = Engine::new(40, 5, 500);
    let mut plain = Engine::new_uncompressed(40, 5, 500);
    for i in 0..200 {
        let line = format!("{i:03} {}\r\n", "word ".repeat(i % 13));
        packed.feed(line.as_bytes());
        plain.feed(line.as_bytes());
    }
    let rows = |t: &Engine| {
        (1..=t.history_len() as i32)
            .map(|n| t.row_text(-n))
            .collect::<Vec<_>>()
    };
    // Reading fills the row cache; the reflow must not serve those rows again.
    assert_eq!(rows(&packed), rows(&plain));
    for (cols, lines) in [(23, 5), (61, 7), (40, 5)] {
        packed.resize(cols, lines);
        plain.resize(cols, lines);
        assert_eq!(rows(&packed), rows(&plain), "{cols}x{lines}");
    }
}

#[test]
fn history_rows_read_at_the_bottom_or_after_scrolling_back_match() {
    let mut packed = Engine::new(30, 4, 300);
    let mut plain = Engine::new_uncompressed(30, 4, 300);
    for i in 0..120 {
        let line = format!("\x1b[3{}m{i:03}\x1b[0m {}\r\n", i % 8, "ab ".repeat(i % 9));
        packed.feed(line.as_bytes());
        plain.feed(line.as_bytes());
    }
    let (mut a, mut b) = (Vec::new(), Vec::new());
    // At the bottom (no row cache), scrolled back (cached), then at the bottom again
    for offset in [0, 37, 0] {
        packed.scroll_to_bottom();
        packed.scroll_display(offset);
        for line in -(packed.history_len() as i32)..4 {
            assert!(packed.row_packed(line, &mut a));
            assert!(plain.row_packed(line, &mut b));
            assert_eq!(a, b, "line {line} at offset {offset}");
        }
    }
    assert!(!packed.row_packed(4, &mut a));
    assert!(a.is_empty());
}

#[test]
fn a_wide_char_over_a_wide_spacer_leaves_a_narrow_blank() {
    let mut t = term();
    t.feed("漢字\x08🚀".as_bytes());
    let mut cells = Vec::new();
    t.row_cells(0, &mut cells);
    assert_eq!(
        (
            cells[2].ch,
            cells[2].attrs & (attr::WIDE | attr::WIDE_SPACER)
        ),
        (' ', 0)
    );
    assert_eq!(cells[3].ch, '🚀');
    assert_ne!(cells[3].attrs & attr::WIDE, 0);
}

/// Row 0 as text with a `|` for each cell that is the right half of a wide char.
fn cells_text(t: &Engine) -> String {
    let mut cells = Vec::new();
    t.row_cells(0, &mut cells);
    cells
        .iter()
        .map(|c| {
            if c.attrs & attr::WIDE_SPACER != 0 {
                '|'
            } else {
                c.ch
            }
        })
        .collect()
}

#[test]
fn edits_that_cut_a_wide_char_erase_it_whole() {
    // xterm.js BufferLine: an erase, insert or delete that takes one half of a wide char blanks
    // the other half too (fuzz seeds 258, 296, 2684, 4172, 5786).
    for (input, row) in [
        ("a가b\x1b[1;3H\x1b[X", "a  b  "),
        ("a가b\x1b[1;2H\x1b[X", "a  b  "),
        ("a가b\x1b[1;3H\x1b[K", "a     "),
        ("a가b\x1b[1;2H\x1b[1K", "   b  "),
        ("a가b\x1b[1;2H\x1b[1J", "   b  "),
        ("a가b\x1b[1;3H\x1b[J", "a     "),
        ("a가b\x1b[1;3H\x1b[P", "a b   "),
        ("a가b\x1b[1;2H\x1b[P", "a b   "),
        ("abcd가\x1b[1;1H\x1b[@", " abcd "),
        ("a가b\x1b[1;3H\x1b[4hZ", "a Z b "),
        ("abcd가\x1b[1;1H\x1b[4hZ", "Zabcd "),
        ("abc가\x1b[1;1H\x1b[4h가", "가|abc "),
    ] {
        let mut t = Engine::new(6, 2, 0);
        t.feed(input.as_bytes());
        assert_eq!(cells_text(&t), row, "{input:?}");
    }
    // ICH at a right half leaves that half alone, as in xterm.js `insertCells`.
    let mut t = Engine::new(6, 2, 0);
    t.feed("a가b\x1b[1;3H\x1b[@".as_bytes());
    assert_eq!(cells_text(&t), "a  |b ");
    // Writing over that lone half leaves the cell to its left alone.
    t.feed("Z한".as_bytes());
    assert_eq!(cells_text(&t), "a Z한| ");
}

#[test]
fn writing_over_a_clipped_wide_char_keeps_its_neighbour() {
    // A shrink leaves 🚀 in the last column without a right half; writing there must not blank
    // the 😀 spacer to its left (fuzz seed 1912).
    let mut t = Engine::new(5, 1, 0);
    t.feed("\\😀🚀".as_bytes());
    t.resize(4, 1);
    t.feed(b"x");
    assert_eq!(cells_text(&t), "\\😀|x");
}

#[test]
fn a_restored_cursor_drops_its_pending_wrap() {
    // xterm.js `restoreCursor` clamps the saved column, one past the edge, to the screen.
    for (save, restore) in [
        ("\x1b7", "\x1b8"),
        ("\x1b[s", "\x1b[u"),
        ("\x1b[?1048h", "\x1b[?1048l"),
        ("\x1b[?1049h", "\x1b[?1049l"),
    ] {
        let mut t = Engine::new(5, 2, 0);
        t.feed(format!("12345{save}{restore}Z").as_bytes());
        assert_eq!(t.row_text(0).as_deref(), Some("1234Z"), "{save:?}");
    }
    // After widening, that column is a real one (fuzz seeds 22754, 82656).
    let mut t = Engine::new(2, 1, 0);
    t.feed(b"H \x1b[s");
    t.resize(3, 1);
    t.feed(b"\x1b[uZ");
    assert_eq!(t.row_text(0).as_deref(), Some("H Z"));
}

#[test]
fn widening_the_alternate_screen_frees_a_pending_wrap() {
    let mut t = Engine::new(5, 3, 100);
    t.feed(b"\x1b[?1049habcde");
    t.resize(7, 3);
    // xterm keeps the cursor one past the old edge, which is now a column.
    assert_eq!((t.cursor().line, t.cursor().col), (0, 5));
    t.feed(b"Z");
    assert_eq!(t.row_text(0).as_deref(), Some("abcdeZ"));
}

#[test]
fn ech_dch_and_ich_end_a_pending_wrap() {
    // xterm.js clamps its cursor, one past the edge, back onto the last column first.
    for op in ["\x1b[X", "\x1b[P", "\x1b[@"] {
        let mut t = Engine::new(5, 2, 0);
        t.feed(format!("12345{op}Z").as_bytes());
        assert_eq!(t.row_text(0).as_deref(), Some("1234Z"), "{op:?}");
        assert_eq!(t.row_text(1).as_deref(), Some(""), "{op:?}");
    }
}

#[test]
fn ed0_in_a_pending_wrap_keeps_the_last_column() {
    let mut t = Engine::new(5, 2, 0);
    t.feed(b"12345\r\nabc\x1b[1;1H12345\x1b[J");
    assert_eq!(t.row_text(0).as_deref(), Some("12345"));
    assert_eq!(t.row_text(1).as_deref(), Some(""));
    // Not in a pending wrap, it erases from the cursor as before.
    t.feed(b"\x1b[1;5H\x1b[J");
    assert_eq!(t.row_text(0).as_deref(), Some("1234"));
}

#[test]
fn ed1_from_line_1_erases_line_0() {
    let mut t = Engine::new(5, 3, 0);
    t.feed(b"aaa\r\nbbb\x1b[1J");
    assert_eq!(t.row_text(0).as_deref(), Some(""));
    assert_eq!(t.row_text(1).as_deref(), Some(""));
    t.feed(b"\x1b[2;1Hb\x1b[1;1Ha\x1b[2;1H\x1b[1J");
    assert_eq!(t.row_text(0).as_deref(), Some(""));
}

#[test]
fn x10_mouse_mode_is_exclusive_with_the_other_mouse_modes() {
    let mut t = term();
    t.feed(b"\x1b[?9h");
    assert_eq!(
        t.modes() & (mode::MOUSE_X10 | mode::MOUSE_CLICK),
        mode::MOUSE_X10
    );
    t.feed(b"\x1b[?9$p");
    assert_eq!(t.take_replies(), b"\x1b[?9;1$y");
    t.feed(b"\x1b[?1000h");
    assert_eq!(
        t.modes() & (mode::MOUSE_X10 | mode::MOUSE_CLICK),
        mode::MOUSE_CLICK
    );
    t.feed(b"\x1b[?9h\x1b[?9l");
    assert_eq!(t.modes() & (mode::MOUSE_X10 | mode::MOUSE_CLICK), 0);
}

#[test]
fn kitty_keyboard_flags_follow_the_program_only_when_enabled() {
    let mut t = term();
    t.feed(b"\x1b[>1u");
    assert_eq!(t.kitty_flags(), 0, "off by default: the push is ignored");
    t.set_kitty_keyboard(true);
    t.feed(b"\x1b[>1u\x1b[>3u");
    assert_eq!(t.kitty_flags(), 3);
    assert_ne!(t.modes() & mode::KITTY_KEYBOARD, 0);
    t.feed(b"\x1b[<u");
    assert_eq!(t.kitty_flags(), 1);
    t.reset_kitty_keyboard();
    assert_eq!(t.kitty_flags(), 0);
    t.feed(b"\x1b[>8u");
    assert_eq!(t.kitty_flags(), 8, "still enabled after a reset");
    t.set_kitty_keyboard(false);
    assert_eq!(t.kitty_flags(), 0);
}

#[test]
fn mouse_mode_resets_follow_xterm_js() {
    let mut t = term();
    let tracking = mode::MOUSE_X10 | mode::MOUSE_CLICK | mode::MOUSE_DRAG | mode::MOUSE_MOTION;
    t.feed(b"\x1b[?1003h\x1b[?1000l");
    assert_eq!(
        t.modes() & tracking,
        0,
        "any tracking reset turns tracking off"
    );
    t.feed(b"\x1b[?1016h");
    assert_eq!(
        t.modes() & (mode::SGR_MOUSE | mode::SGR_PIXELS_MOUSE),
        mode::SGR_PIXELS_MOUSE
    );
    t.feed(b"\x1b[?1006h");
    assert_eq!(
        t.modes() & (mode::SGR_MOUSE | mode::SGR_PIXELS_MOUSE),
        mode::SGR_MOUSE
    );
    t.feed(b"\x1b[?1016h\x1b[?1006l");
    assert_eq!(
        t.modes() & (mode::SGR_MOUSE | mode::SGR_PIXELS_MOUSE),
        0,
        "either reset returns to the default encoding"
    );
}

#[test]
fn trimmed_lines_count_what_leaves_the_top_of_the_scrollback() {
    for mut t in [Engine::new(10, 2, 3), Engine::new_uncompressed(10, 2, 3)] {
        // 10 lines on a 2-row screen: 8 enter the history, which keeps 3
        t.feed(b"0\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6\r\n7\r\n8\r\n9");
        assert_eq!((t.history_len(), t.trimmed_lines()), (3, 5));
        assert_eq!(t.row_text(-3).as_deref(), Some("5"));
        // The alternate screen neither keeps nor drops history
        t.feed(b"\x1b[?1049ha\r\nb\r\nc\x1b[?1049l");
        assert_eq!((t.history_len(), t.trimmed_lines()), (3, 5));
        // A resize reflows; it does not trim
        t.resize(5, 2);
        let reflowed = t.history_len();
        assert_eq!(t.trimmed_lines(), 5);
        t.feed(b"\x1b[3J");
        assert_eq!(
            (t.history_len(), t.trimmed_lines()),
            (0, 5 + reflowed as u64)
        );
    }
}

#[test]
fn full_resets_are_reported_once() {
    let mut t = term();
    t.feed(b"a\x1bcb\x1bc");
    assert_eq!(t.take_resets(), 2);
    assert_eq!(t.take_resets(), 0);
}

#[test]
fn a_wide_char_pushed_to_the_next_row_leaves_a_marked_blank() {
    let mut t = Engine::new(5, 3, 10);
    t.feed("abcd한".as_bytes());
    let mut cells = Vec::new();
    t.row_cells(0, &mut cells);
    assert_eq!(cells[4].ch, ' ');
    assert_ne!(cells[4].attrs & attr::LEADING_WIDE_SPACER, 0);
    assert_eq!(cells[3].attrs & attr::LEADING_WIDE_SPACER, 0);
    t.feed(b"\r\n    x");
    t.row_cells(2, &mut cells);
    assert_eq!(
        cells[0].attrs & attr::LEADING_WIDE_SPACER,
        0,
        "written spaces are text"
    );
}

#[test]
fn typed_spaces_are_text_and_erased_cells_are_not() {
    // xterm.js translateToString(true) of the same output (checked against @xterm/headless): typed trailing spaces
    // stay; cells erased by EL and ECH, skipped by a cursor move or left by a wide char moved to the next row go
    let mut t = Engine::new(10, 7, 100);
    t.feed("typed   \r\nab\x1b[5Ccd\r\nxyz\x1b[2D\x1b[K\r\n12345\x1b[3D\x1b[2X\r\n\x1b[44m  \x1b[0m\x1b[44m\x1b[K\x1b[0m\r\n123456789가".as_bytes());
    let rows: Vec<String> = (0..6).map(|y| t.row_string(y, true).unwrap()).collect();
    assert_eq!(
        rows,
        ["typed   ", "ab     cd", "x", "12  5", "  ", "123456789"]
    );
    assert_eq!(t.row_string(1, false).unwrap(), "ab     cd ");
    assert_eq!(
        t.row_text(0).unwrap(),
        "typed",
        "row_text still drops every trailing blank"
    );
    let mut cells = Vec::new();
    t.row_cells(0, &mut cells);
    let typed: Vec<bool> = cells
        .iter()
        .map(|c| c.attrs & attr::TYPED_SPACE != 0)
        .collect();
    assert_eq!(
        typed,
        [
            false, false, false, false, false, true, true, true, false, false
        ]
    );
    // A tab writes nothing, and the blank a wide char moved off is empty too
    t.feed(b"\x1b[2J\x1b[Ha\tb");
    assert_eq!(t.row_string(0, true).unwrap(), "a       b");
    t.row_cells(0, &mut cells);
    assert!(cells.iter().all(|c| c.attrs & attr::TYPED_SPACE == 0));
}
