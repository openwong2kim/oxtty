//! Parity of the input encoders (`wongtty::input`) with the real xterm.js 6.1: every case of the fixtures that
//! `packages/wongtty/test/web/gen-input-fixtures.mjs` recorded from xterm.js in headless Chrome is replayed here and
//! must send the same bytes.

use serde_json::Value;
use wongtty::input::{
    KeyAction, KeyEventType, KeyInput, KeyOptions, KeyResult, Modifiers, MouseAction, MouseButton,
    MouseEncoder, MouseEncoding, MouseGrid, MouseInput, MouseOptions, MouseProtocol, MouseReport,
    Platform, WheelDeltaMode, WheelInput, WheelOutcome, encode_focus, encode_key, encode_paste,
    is_third_level_shift,
};
use wongtty::{Engine, mode};

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn str_of(v: &Value) -> String {
    v.as_str().unwrap().to_string()
}

fn int(v: &Value) -> u32 {
    v.as_u64().unwrap() as u32
}

/// Modifier bits as the fixtures record them: 1 Shift, 2 Alt, 4 Ctrl, 8 Meta.
fn mods(bits: u32) -> Modifiers {
    Modifiers {
        shift: bits & 1 != 0,
        alt: bits & 2 != 0,
        ctrl: bits & 4 != 0,
        meta: bits & 8 != 0,
    }
}

fn platform(name: &str) -> Platform {
    match name {
        "MacIntel" => Platform::Mac,
        "Win32" => Platform::Windows,
        _ => Platform::Other,
    }
}

/// [`mode`] bits of an xterm.js state object (`state()` in the fixture page).
fn key_modes(state: &Value) -> u32 {
    let mut modes = 0;
    if state["appCursor"].as_bool().unwrap() {
        modes |= mode::APP_CURSOR;
    }
    if state["appKeypad"].as_bool().unwrap() {
        modes |= mode::APP_KEYPAD;
    }
    if state["bracketedPaste"].as_bool().unwrap() {
        modes |= mode::BRACKETED_PASTE;
    }
    if state["sendFocus"].as_bool().unwrap() {
        modes |= mode::FOCUS_EVENTS;
    }
    modes
}

/// An xterm.js `IKeyboardResult` as recorded: `[type, cancel, key]`, or null for "no result".
fn key_result(v: &Value) -> KeyResult {
    if v.is_null() {
        return KeyResult::default();
    }
    let action = match int(&v[0]) {
        0 => KeyAction::Send,
        1 => KeyAction::SelectAll,
        2 => KeyAction::PageUp,
        _ => KeyAction::PageDown,
    };
    KeyResult {
        action,
        cancel: int(&v[1]) != 0,
        key: v[2].as_str().map(String::from),
    }
}

/// A recorded DOM key event: `[type, key, code, keyCode, charCode, location, mods, repeat, altGraph, …]`.
fn dom_key(e: &Value) -> KeyInput {
    let event_type = match (e[0].as_str().unwrap(), int(&e[7])) {
        ("keyup", _) => KeyEventType::Release,
        (_, 1) => KeyEventType::Repeat,
        _ => KeyEventType::Press,
    };
    KeyInput {
        key: str_of(&e[1]),
        code: str_of(&e[2]),
        key_code: Some(int(&e[3])),
        location: int(&e[5]) as u8,
        mods: mods(int(&e[6])),
        event_type,
        composing: false,
    }
}

/// The textarea handlers of xterm.js `CoreBrowserTerminal` (`_keyDown`, `_keyPress`, `_keyUp`, `_inputEvent`)
/// without composition and screen reader mode, around [`encode_key`]: what a DOM host does with its results.
struct Textarea {
    options: KeyOptions,
    kitty: bool,
    key_down_seen: bool,
    key_press_handled: bool,
    unprocessed_dead_key: bool,
    sent: String,
}

impl Textarea {
    fn modifier_only(e: &Value) -> bool {
        matches!(int(&e[3]), 16 | 17 | 18 | 91 | 92 | 93 | 224) || e[1] == "Meta"
    }

    fn key_down(&mut self, e: &Value, result: &KeyResult) {
        let input = dom_key(e);
        if !Self::modifier_only(e) {
            self.key_down_seen = true;
        }
        let ignore_composition = self.options.platform == Platform::Mac
            && self.options.mac_option_is_meta
            && input.mods.alt;
        if !ignore_composition && input.key_code == Some(229) {
            return;
        }
        if !ignore_composition && (input.key == "Dead" || input.key == "AltGraph") {
            self.unprocessed_dead_key = true;
        }
        if matches!(result.action, KeyAction::PageUp | KeyAction::PageDown) {
            return;
        }
        if is_third_level_shift(&input, false, int(&e[8]) != 0, &self.options) {
            return;
        }
        let Some(key) = &result.key else { return };
        // macOS IMEs: A-Z go through keypress so caps lock works (not with the kitty protocol).
        let m = input.mods;
        if !self.kitty && !m.ctrl && !m.alt && !m.meta && input.key.encode_utf16().count() == 1 {
            let c = input.key.encode_utf16().next().unwrap();
            if (65..=90).contains(&c) {
                return;
            }
        }
        if self.unprocessed_dead_key {
            self.unprocessed_dead_key = false;
            return;
        }
        self.sent += key;
    }

    fn key_press(&mut self, e: &Value) {
        self.key_press_handled = false;
        let input = dom_key(e);
        let char_code = int(&e[4]);
        let which = e[9].as_u64().map(|w| w as u32);
        let key = if char_code != 0 {
            char_code
        } else if let Some(w) = which.filter(|&w| w != 0) {
            w
        } else {
            return;
        };
        let m = input.mods;
        if (m.alt || m.ctrl || m.meta)
            && !is_third_level_shift(&input, true, int(&e[8]) != 0, &self.options)
        {
            return;
        }
        self.sent.push(char::from_u32(key).unwrap());
        self.key_press_handled = true;
        self.unprocessed_dead_key = false;
    }

    fn input(&mut self, e: &Value) {
        let data = e[1].as_str().unwrap_or("");
        if !data.is_empty() && e[2] == "insertText" && (int(&e[3]) == 0 || !self.key_down_seen) {
            if self.key_press_handled {
                return;
            }
            self.unprocessed_dead_key = false;
            self.sent += data;
        }
    }

    fn key_up(&mut self, result: &KeyResult) {
        self.key_down_seen = false;
        if let Some(key) = &result.key {
            self.sent += key;
        }
        self.key_press_handled = false;
    }
}

#[test]
fn key_presses_send_what_xterm_js_sends() {
    let f = fixture("input-keys-dom.json");
    let platform = platform(f["platform"].as_str().unwrap());
    let modes = f["modes"].as_array().unwrap();
    let cases = f["cases"].as_array().unwrap();
    let mut results = 0;
    let mut mismatches = Vec::new();
    for case in cases {
        let m = &modes[case[0].as_u64().unwrap() as usize];
        let options = KeyOptions {
            platform,
            mac_option_is_meta: m["options"]["macOptionIsMeta"].as_bool().unwrap_or(false),
        };
        let state = &m["state"];
        let kitty_flags = int(&state["kittyFlags"]) as u8;
        let modes = key_modes(state);
        let mut textarea = Textarea {
            options,
            kitty: kitty_flags != 0,
            key_down_seen: false,
            key_press_handled: false,
            unprocessed_dead_key: false,
            sent: String::new(),
        };
        for e in case[2].as_array().unwrap() {
            match e[0].as_str().unwrap() {
                "keydown" | "keyup" => {
                    let result = encode_key(&dom_key(e), modes, kitty_flags, &options);
                    results += 1;
                    let expected = key_result(&e[9]);
                    if result != expected {
                        mismatches.push(format!(
                            "{} {} {e}: {result:?}, xterm.js {expected:?}",
                            m["name"], case[1]
                        ));
                    }
                    if e[0] == "keydown" {
                        textarea.key_down(e, &result);
                    } else {
                        textarea.key_up(&result);
                    }
                }
                "keypress" => textarea.key_press(e),
                _ => textarea.input(e),
            }
        }
        if textarea.sent != case[3].as_str().unwrap() {
            mismatches.push(format!(
                "{} {}: sent {:?}, xterm.js {}",
                m["name"], case[1], textarea.sent, case[3]
            ));
        }
    }
    // The matrix of the old DOM test: 90 keys × 8 modifier sets × 6 modes.
    assert_eq!(cases.len(), 4320);
    assert!(results > 2 * cases.len());
    assert!(
        mismatches.is_empty(),
        "{} mismatches:\n{}",
        mismatches.len(),
        mismatches[..mismatches.len().min(20)].join("\n")
    );
    // Not vacuous: most presses send something.
    let empty = cases.iter().filter(|c| c[3] == "").count();
    assert!(empty < cases.len() / 2, "{empty} presses sent nothing");
}

#[test]
fn key_events_encode_like_xterm_js_evaluate() {
    let f = fixture("input-keys-eval.json");
    let keys = f["keys"].as_array().unwrap();
    let table: Vec<KeyResult> = f["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(key_result)
        .collect();
    let mut total = 0;
    let mut mismatches = Vec::new();
    for config in f["cases"].as_array().unwrap() {
        let options = KeyOptions {
            platform: platform(config["platform"].as_str().unwrap()),
            mac_option_is_meta: config["macOptionIsMeta"].as_bool().unwrap(),
        };
        let state = &config["state"];
        let kitty_flags = int(&state["kittyFlags"]) as u8;
        let modes = key_modes(state);
        let types = config["types"].as_array().unwrap();
        let mut results = config["results"].as_array().unwrap().iter();
        for k in keys {
            for bits in 0..16 {
                for t in types {
                    let shifted = bits & 1 != 0 && k.get(3).is_some_and(|s| s.is_string());
                    let input = KeyInput {
                        key: str_of(if shifted { &k[3] } else { &k[0] }),
                        code: str_of(&k[1]),
                        key_code: Some(int(&k[2])),
                        location: k.get(4).map_or(0, |l| int(l) as u8),
                        mods: mods(bits),
                        event_type: match t.as_str().unwrap() {
                            "press" => KeyEventType::Press,
                            "repeat" => KeyEventType::Repeat,
                            _ => KeyEventType::Release,
                        },
                        composing: false,
                    };
                    let expected = &table[results.next().unwrap().as_u64().unwrap() as usize];
                    let result = encode_key(&input, modes, kitty_flags, &options);
                    total += 1;
                    if &result != expected {
                        mismatches.push(format!(
                            "{} meta-is-option {} kitty {kitty_flags} modes {modes} {input:?}: {result:?}, xterm.js {expected:?}",
                            config["platform"], options.mac_option_is_meta
                        ));
                    }
                }
            }
        }
        assert!(results.next().is_none());
    }
    assert!(total > 100_000, "{total} cases");
    assert!(
        mismatches.is_empty(),
        "{} of {total} differ:\n{}",
        mismatches.len(),
        mismatches[..mismatches.len().min(20)].join("\n")
    );
}

/// [`mode`] bits of a recorded mouse state `[protocol, encoding, appCursor, alt, …]`.
fn mouse_modes(s: &Value) -> u32 {
    let protocol = match s[0].as_str().unwrap() {
        "X10" => mode::MOUSE_X10,
        "VT200" => mode::MOUSE_CLICK,
        "DRAG" => mode::MOUSE_DRAG,
        "ANY" => mode::MOUSE_MOTION,
        _ => 0,
    };
    let encoding = match s[1].as_str().unwrap() {
        "SGR" => mode::SGR_MOUSE,
        "SGR_PIXELS" => mode::SGR_PIXELS_MOUSE,
        _ => 0,
    };
    let mut modes = protocol | encoding;
    if int(&s[2]) != 0 {
        modes |= mode::APP_CURSOR;
    }
    if int(&s[3]) != 0 {
        modes |= mode::ALT_SCREEN;
    }
    modes
}

fn mouse_options(s: &Value, scrollback: u32) -> MouseOptions {
    MouseOptions {
        require_alt: int(&s[6]) != 0,
        scroll_sensitivity: s[4].as_f64().unwrap(),
        fast_scroll_sensitivity: s[5].as_f64().unwrap(),
        scrollback,
    }
}

/// Recorded output: `[["data" | "binary", text]…]`, binary text one byte per char.
fn reports(v: &Value) -> Vec<MouseReport> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|r| {
            let text = r[1].as_str().unwrap();
            match r[0].as_str().unwrap() {
                "binary" => MouseReport::Binary(text.chars().map(|c| c as u32 as u8).collect()),
                _ => MouseReport::Data(text.to_string()),
            }
        })
        .collect()
}

#[test]
fn mouse_and_wheel_reports_match_xterm_js() {
    let f = fixture("input-mouse.json");
    let (cols, rows, scrollback) = (
        int(&f["options"]["cols"]),
        int(&f["options"]["rows"]),
        int(&f["options"]["scrollback"]),
    );
    let cases = f["cases"].as_array().unwrap();
    // One encoder for the whole session, like the one xterm.js terminal: the last report carries over.
    let mut encoder = MouseEncoder::new();
    let (mut events, mut sent, mut wheels) = (0, 0, 0);
    let mut mismatches = Vec::new();
    for case in cases {
        for e in case[1].as_array().unwrap() {
            events += 1;
            let expected = reports(&e[7]);
            sent += expected.len();
            if e[0] == "mouse" {
                // [mouse, type, button, buttons, mods, [col, row, x, y] | null, state, bytes]
                let Some(at) = e[5].as_array() else {
                    assert!(expected.is_empty());
                    continue;
                };
                let dom_button = |b: u32| match b {
                    0 => MouseButton::Left,
                    1 => MouseButton::Middle,
                    2 => MouseButton::Right,
                    3 => MouseButton::Back,
                    4 => MouseButton::Forward,
                    _ => MouseButton::None,
                };
                let buttons = int(&e[3]);
                let (button, action) = match e[1].as_str().unwrap() {
                    "mousedown" => (dom_button(int(&e[2])), MouseAction::Down),
                    "mouseup" => (dom_button(int(&e[2])), MouseAction::Up),
                    _ => (
                        if buttons & 1 != 0 {
                            MouseButton::Left
                        } else if buttons & 4 != 0 {
                            MouseButton::Middle
                        } else if buttons & 2 != 0 {
                            MouseButton::Right
                        } else {
                            MouseButton::None
                        },
                        MouseAction::Move,
                    ),
                };
                let input = MouseInput {
                    col: int(&at[0]),
                    row: int(&at[1]),
                    x: int(&at[2]),
                    y: int(&at[3]),
                    button,
                    action,
                    mods: mods(int(&e[4])),
                };
                let grid = MouseGrid {
                    cols,
                    rows,
                    ..Default::default()
                };
                let report = encoder.encode(
                    &input,
                    mouse_modes(&e[6]),
                    &grid,
                    &mouse_options(&e[6], scrollback),
                );
                let got: Vec<MouseReport> = report.into_iter().collect();
                if got != expected {
                    mismatches.push(format!("{} {e}: {got:?}", case[0]));
                }
            } else {
                // [wheel, deltaY, deltaMode, mods, [col, row, x, y] | null, state, [cols, rows, cell height, dpr],
                //  bytes, defaultPrevented]
                wheels += 1;
                let at = e[4].as_array().unwrap();
                let g = &e[6];
                let input = WheelInput {
                    col: int(&at[0]),
                    row: int(&at[1]),
                    x: int(&at[2]),
                    y: int(&at[3]),
                    delta_y: e[1].as_f64().unwrap(),
                    delta_mode: match int(&e[2]) {
                        0 => WheelDeltaMode::Pixel,
                        1 => WheelDeltaMode::Line,
                        _ => WheelDeltaMode::Page,
                    },
                    mods: mods(int(&e[3])),
                };
                let grid = MouseGrid {
                    cols: int(&g[0]),
                    rows: int(&g[1]),
                    device_cell_height: g[2].as_f64().unwrap(),
                    device_pixel_ratio: g[3].as_f64().unwrap(),
                };
                let outcome = encoder.wheel(
                    &input,
                    mouse_modes(&e[5]),
                    &grid,
                    &mouse_options(&e[5], scrollback),
                );
                let (got, prevented) = match outcome {
                    WheelOutcome::Send(r) => (vec![r], true),
                    WheelOutcome::Consumed => (vec![], true),
                    WheelOutcome::Unhandled => (vec![], false),
                };
                if got != expected || prevented != (int(&e[8]) != 0) {
                    mismatches.push(format!("{} {e}: {got:?} prevented {prevented}", case[0]));
                }
            }
        }
    }
    // The button matrix of the old DOM test: 4 protocols × 3 encodings × 3 buttons × 5 modifier sets × 8 cells.
    assert_eq!(f["buttonCases"], 1440);
    assert!(
        sent > 1440 && wheels > 200,
        "{events} events, {sent} reports, {wheels} wheels"
    );
    assert!(
        mismatches.is_empty(),
        "{} of {events} events differ:\n{}",
        mismatches.len(),
        mismatches[..mismatches.len().min(20)].join("\n")
    );
}

#[test]
fn paste_and_focus_match_xterm_js() {
    let f = fixture("input-paste-focus.json");
    let mut checked = 0;
    for case in f["cases"].as_array().unwrap() {
        if case[0] == "paste" {
            let mut modes = key_modes(&case[3]);
            if case[2]["ignoreBracketedPasteMode"] == true {
                modes &= !mode::BRACKETED_PASTE;
            }
            assert_eq!(
                encode_paste(case[4].as_str().unwrap(), modes),
                case[5].as_str().unwrap(),
                "{case}"
            );
        } else {
            // The label says what happened; enabling the mode reports the current focus.
            let label = case[1].as_str().unwrap();
            let modes = key_modes(&case[2]);
            let focused = !label.starts_with("blur") && !label.contains("blurred");
            let sent = if label.starts_with("mode off") {
                None
            } else {
                encode_focus(focused, modes)
            };
            assert_eq!(sent.unwrap_or(""), case[3].as_str().unwrap(), "{case}");
        }
        checked += 1;
    }
    assert_eq!(checked, 54);
}

/// The engine's modes after the sequences the fixtures used pick the protocol, encoding and flags xterm.js had.
#[test]
fn engine_modes_drive_the_encoders_like_xterm_js_state() {
    let mut t = Engine::new(80, 24, 100);
    for (set, protocol) in [
        ("9", MouseProtocol::X10),
        ("1000", MouseProtocol::Vt200),
        ("1002", MouseProtocol::Drag),
        ("1003", MouseProtocol::Any),
    ] {
        for (extra, encoding) in [
            ("", MouseEncoding::Default),
            (";1006", MouseEncoding::Sgr),
            (";1016", MouseEncoding::SgrPixels),
        ] {
            t.feed(format!("\x1b[?1000l\x1b[?1006l\x1b[?{set}{extra}h").as_bytes());
            assert_eq!(
                MouseProtocol::from_modes(t.modes()),
                protocol,
                "?{set}{extra}"
            );
            assert_eq!(
                MouseEncoding::from_modes(t.modes()),
                encoding,
                "?{set}{extra}"
            );
        }
    }
    t.feed(b"\x1b[?1000l\x1b[?1006l");
    assert_eq!(MouseProtocol::from_modes(t.modes()), MouseProtocol::None);
    t.set_kitty_keyboard(true);
    t.feed(b"\x1b[>31u\x1b[?1h");
    assert_eq!(t.kitty_flags(), 31);
    let a = KeyInput {
        key: "a".into(),
        code: "KeyA".into(),
        key_code: Some(65),
        ..Default::default()
    };
    let options = KeyOptions::default();
    assert_eq!(
        encode_key(&a, t.modes(), t.kitty_flags(), &options)
            .key
            .as_deref(),
        Some("\x1b[97;;97u")
    );
    t.feed(b"\x1b[<u");
    let up = KeyInput {
        key: "ArrowUp".into(),
        code: "ArrowUp".into(),
        key_code: Some(38),
        ..Default::default()
    };
    assert_eq!(
        encode_key(&up, t.modes(), t.kitty_flags(), &options)
            .key
            .as_deref(),
        Some("\x1bOA")
    );
}

/// Hosts without DOM key codes (native) get them from `code`, else `key`.
#[test]
fn keys_without_key_codes_encode_from_code_or_key() {
    let options = KeyOptions::default();
    let key = |key: &str, code: &str, bits: u32| KeyInput {
        key: key.into(),
        code: code.into(),
        mods: mods(bits),
        ..Default::default()
    };
    let sent = |k: KeyInput| encode_key(&k, 0, 0, &options).key;
    assert_eq!(sent(key("c", "KeyC", 4)).as_deref(), Some("\x03"));
    assert_eq!(sent(key("ArrowLeft", "", 1)).as_deref(), Some("\x1b[1;2D"));
    assert_eq!(sent(key("F5", "F5", 0)).as_deref(), Some("\x1b[15~"));
    assert_eq!(sent(key("x", "", 2)).as_deref(), Some("\x1bx"));
    assert_eq!(sent(key("한", "", 0)), None);
    assert_eq!(sent(key("é", "Digit2", 0)).as_deref(), Some("é"));
}

/// Garbage in never panics.
#[test]
fn odd_input_does_not_panic() {
    let mut encoder = MouseEncoder::new();
    let options = MouseOptions {
        scroll_sensitivity: f64::NAN,
        ..Default::default()
    };
    for modes in [0, u32::MAX] {
        for (key, code) in [
            ("", ""),
            ("Numpad", "Numpad99999999999"),
            ("Dead", "Key"),
            ("\u{10FFFF}", "Digit"),
        ] {
            for bits in 0..16 {
                let k = KeyInput {
                    key: key.into(),
                    code: code.into(),
                    key_code: Some(u32::MAX),
                    mods: mods(bits),
                    ..Default::default()
                };
                for flags in [0, 31, 255] {
                    encode_key(
                        &k,
                        modes,
                        flags,
                        &KeyOptions {
                            platform: Platform::Mac,
                            mac_option_is_meta: true,
                        },
                    );
                }
            }
        }
        let grid = MouseGrid {
            cols: u32::MAX,
            rows: u32::MAX,
            device_cell_height: 0.0,
            device_pixel_ratio: f64::INFINITY,
        };
        let input = MouseInput {
            col: u32::MAX - 1,
            row: 0,
            x: u32::MAX,
            y: u32::MAX,
            button: MouseButton::WheelLeft,
            action: MouseAction::Wheel,
            mods: mods(15),
        };
        encoder.encode(&input, modes, &grid, &options);
        for delta_y in [f64::NAN, f64::INFINITY, -1e300, 1e-300] {
            let wheel = WheelInput {
                col: u32::MAX - 1,
                delta_y,
                ..Default::default()
            };
            encoder.wheel(
                &wheel,
                modes,
                &MouseGrid {
                    device_cell_height: 1e-300,
                    device_pixel_ratio: 1e300,
                    ..grid
                },
                &options,
            );
        }
    }
    assert_eq!(
        encode_paste("\r\n\x1b", mode::BRACKETED_PASTE),
        "\x1b[200~\r\u{241b}\x1b[201~"
    );
}
