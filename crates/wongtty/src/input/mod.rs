//! Platform-neutral input that the encoders turn into PTY bytes (PRD M3), shared by the browser front end
//! (`crates/wongtty-web`) and native hosts. Fields follow DOM `KeyboardEvent`/`MouseEvent`, the richest source;
//! a native host fills what it has. Encoders read terminal modes from the host's last frame, not the live
//! engine (PRD D7).
//!
//! The encoders are ports of xterm.js 6.1.0-beta.336 (commit 08829c7eb6279dda2a9e86df901353a1a89858ca, MIT license,
//! `crates/wongtty/LICENSE-XTERM`) and send the bytes it sends; each file names its source:
//! - keys: [`encode_key`] (the default xterm encoding in `keyboard.rs`, the kitty keyboard protocol in `kitty.rs`)
//! - mouse and wheel: [`MouseEncoder`] (`mouse.rs`)
//! - paste: [`encode_paste`]; focus: [`encode_focus`]
//!
//! What stays with the host: IME composition, the DOM `keypress`/`input` fallbacks for text (see
//! [`is_third_level_shift`]), which listeners are on (see [`MouseProtocol::events`]), cell coordinates and view
//! scrolling.

mod keyboard;
pub mod kitty;
mod mouse;

pub use mouse::{
    MouseEncoder, MouseEncoding, MouseGrid, MouseOptions, MouseProtocol, MouseReport,
    WheelDeltaMode, WheelInput, WheelOutcome, events as mouse_event, mouse_events,
};

use crate::mode;

/// Modifier keys held during an event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
    /// Command on macOS, the Windows key elsewhere.
    pub meta: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyEventType {
    #[default]
    Press,
    Repeat,
    Release,
}

/// One key event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyInput {
    /// DOM `KeyboardEvent.key`: the character produced (`"a"`, `"한"`) or a key name (`"Enter"`, `"ArrowUp"`).
    pub key: String,
    /// DOM `KeyboardEvent.code`: the physical key (`"KeyA"`, `"Numpad1"`); empty when unknown.
    pub code: String,
    /// DOM `KeyboardEvent.keyCode`, which the xterm.js rules still rely on; `None` on hosts without it, which
    /// [`encode_key`] then derives from `code` (or `key`) as on a US layout.
    pub key_code: Option<u32>,
    /// DOM `KeyboardEvent.location`: 0 standard, 1 left, 2 right, 3 numpad.
    pub location: u8,
    pub mods: Modifiers,
    pub event_type: KeyEventType,
    /// An IME composition is in progress (DOM `isComposing` or keyCode 229).
    pub composing: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseButton {
    #[default]
    Left,
    Middle,
    Right,
    /// No button (motion).
    None,
    WheelUp,
    WheelDown,
    WheelLeft,
    WheelRight,
    /// Back and forward buttons (8, 9).
    Back,
    Forward,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseAction {
    #[default]
    Down,
    Up,
    Move,
    Wheel,
}

/// One mouse event at a cell, with the pixel position SGR-pixels mode (`?1016`) reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MouseInput {
    /// 0-based viewport cell.
    pub col: u32,
    pub row: u32,
    /// 0-based pixels from the top left of the screen.
    pub x: u32,
    pub y: u32,
    pub button: MouseButton,
    pub action: MouseAction,
    pub mods: Modifiers,
}

/// The platform, for the rules that differ by it (xterm.js `isMac`, `isWindows`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Platform {
    #[default]
    Other,
    Mac,
    Windows,
}

/// Terminal options the key encoding reads.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyOptions {
    pub platform: Platform,
    /// xterm.js `macOptionIsMeta`: Option sends ESC-prefixed keys on macOS instead of composing characters.
    pub mac_option_is_meta: bool,
}

/// What a key asks the terminal to do (xterm.js `KeyboardResultType`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeyAction {
    /// Send [`KeyResult::key`], if any.
    #[default]
    Send,
    /// Cmd+A on macOS: select all.
    SelectAll,
    /// Shift+PageUp: scroll the view up a page (rows - 1), send nothing.
    PageUp,
    /// Shift+PageDown: scroll the view down a page.
    PageDown,
}

/// The result of [`encode_key`] (xterm.js `IKeyboardResult`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KeyResult {
    pub action: KeyAction,
    /// Stop the event from reaching the page (`preventDefault` and `stopPropagation`).
    pub cancel: bool,
    /// The bytes for the PTY (user input, `onData`); `None` sends nothing.
    pub key: Option<String>,
}

/// JavaScript `String.length`: UTF-16 code units, which the xterm.js rules count.
fn js_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Encodes one key event, like xterm.js `KeyboardService.evaluateKeyDown` for presses and repeats and
/// `evaluateKeyUp` for releases. `modes` are [`mode`] bits and `kitty_flags` the active kitty keyboard flags
/// ([`crate::Engine::kitty_flags`], 0 when the protocol is off or nothing is pushed): with any flag the kitty
/// protocol encodes, else the default xterm scheme, where releases send nothing.
///
/// Like xterm.js, application keypad mode changes nothing (numpad keys send their characters) and win32 input
/// mode is not supported. The host skips keys that belong to an IME composition (`composing`).
pub fn encode_key(
    input: &KeyInput,
    modes: u32,
    kitty_flags: u8,
    options: &KeyOptions,
) -> KeyResult {
    let mac = options.platform == Platform::Mac;
    let kitty_type = match input.event_type {
        KeyEventType::Press => kitty::EventType::Press,
        KeyEventType::Repeat => kitty::EventType::Repeat,
        KeyEventType::Release => kitty::EventType::Release,
    };
    let mac_option_as_alt = mac && options.mac_option_is_meta;
    if input.event_type == KeyEventType::Release {
        if kitty_flags & kitty::flags::REPORT_EVENT_TYPES != 0 {
            return kitty::evaluate(input, kitty_flags, kitty_type, mac_option_as_alt);
        }
        return KeyResult::default();
    }
    if kitty_flags != 0 {
        return kitty::evaluate(input, kitty_flags, kitty_type, mac_option_as_alt);
    }
    let key_code = input
        .key_code
        .unwrap_or_else(|| derive_key_code(&input.code, &input.key));
    keyboard::evaluate(
        input,
        key_code,
        modes & mode::APP_CURSOR != 0,
        mac,
        options.mac_option_is_meta,
    )
}

/// xterm.js `CoreBrowserTerminal._isThirdLevelShift` (`src/browser/CoreBrowserTerminal.ts` lines 947-959 @
/// 08829c7eb6279dda2a9e86df901353a1a89858ca, MIT): the modifiers pick a third-level character (macOS Option without
/// `macOptionIsMeta`, Windows Ctrl+Alt or AltGr), so the host lets the browser or OS produce the text instead of
/// sending the key. `keypress` is true for a DOM `keypress` event; for other events keys below 48 (arrows, Home,
/// Backspace…) are never third-level. `alt_graph` is DOM `getModifierState("AltGraph")`.
pub fn is_third_level_shift(
    input: &KeyInput,
    keypress: bool,
    alt_graph: bool,
    options: &KeyOptions,
) -> bool {
    let m = input.mods;
    let windows = options.platform == Platform::Windows;
    let third_level = (options.platform == Platform::Mac
        && !options.mac_option_is_meta
        && m.alt
        && !m.ctrl
        && !m.meta)
        || (windows && m.alt && m.ctrl && !m.meta)
        || (windows && alt_graph);
    if keypress {
        return third_level;
    }
    let key_code = input
        .key_code
        .unwrap_or_else(|| derive_key_code(&input.code, &input.key));
    third_level && (key_code == 0 || key_code > 47)
}

/// The DOM `keyCode` a US layout gives, from `code`, else from `key`; 0 when neither is known.
fn derive_key_code(code: &str, key: &str) -> u32 {
    let from_code = match code {
        "Backspace" => 8,
        "Tab" => 9,
        "Enter" | "NumpadEnter" => 13,
        "ShiftLeft" | "ShiftRight" => 16,
        "ControlLeft" | "ControlRight" => 17,
        "AltLeft" | "AltRight" => 18,
        "Pause" => 19,
        "CapsLock" => 20,
        "Escape" => 27,
        "Space" => 32,
        "PageUp" => 33,
        "PageDown" => 34,
        "End" => 35,
        "Home" => 36,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "Insert" => 45,
        "Delete" => 46,
        "MetaLeft" => 91,
        "MetaRight" => 93,
        "NumpadMultiply" => 106,
        "NumpadAdd" => 107,
        "NumpadSubtract" => 109,
        "NumpadDecimal" => 110,
        "NumpadDivide" => 111,
        "NumLock" => 144,
        "ScrollLock" => 145,
        "Semicolon" => 186,
        "Equal" => 187,
        "Comma" => 188,
        "Minus" => 189,
        "Period" => 190,
        "Slash" => 191,
        "Backquote" => 192,
        "BracketLeft" => 219,
        "Backslash" => 220,
        "BracketRight" => 221,
        "Quote" => 222,
        _ => {
            let digit = |s: &str| -> Option<u32> {
                let mut chars = s.chars();
                let d = chars.next()?.to_digit(10)?;
                chars.next().is_none().then_some(d)
            };
            if let Some(d) = code.strip_prefix("Digit").and_then(digit) {
                48 + d
            } else if let Some(d) = code.strip_prefix("Numpad").and_then(digit) {
                96 + d
            } else if let Some(c) = code
                .strip_prefix("Key")
                .filter(|l| l.len() == 1 && l.as_bytes()[0].is_ascii_uppercase())
            {
                u32::from(c.as_bytes()[0])
            } else if let Some(n) = code
                .strip_prefix('F')
                .and_then(|n| n.parse::<u32>().ok())
                .filter(|n| (1..=24).contains(n))
            {
                111 + n
            } else {
                0
            }
        }
    };
    if from_code != 0 {
        return from_code;
    }
    match key {
        "Backspace" => 8,
        "Tab" => 9,
        "Enter" => 13,
        "Escape" => 27,
        "PageUp" => 33,
        "PageDown" => 34,
        "End" => 35,
        "Home" => 36,
        "ArrowLeft" => 37,
        "ArrowUp" => 38,
        "ArrowRight" => 39,
        "ArrowDown" => 40,
        "Insert" => 45,
        "Delete" => 46,
        _ => {
            let mut chars = key.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphabetic() => u32::from(c.to_ascii_uppercase()),
                (Some(c), None) if c.is_ascii_digit() || c == ' ' => u32::from(c),
                _ => 0,
            }
        }
    }
}

/// Pasted text for the PTY, like xterm.js `paste` (`src/browser/Clipboard.ts` lines 13-29 and 54-59 @
/// 08829c7eb6279dda2a9e86df901353a1a89858ca, MIT): line endings (`\r\n`, `\n`) become `\r`, and in bracketed paste
/// mode ([`mode::BRACKETED_PASTE`] in `modes`) the text is wrapped in `CSI 200~` … `CSI 201~` with every ESC
/// replaced by U+241B (␛) so the text cannot end the paste early. A host with xterm.js
/// `ignoreBracketedPasteMode` clears that bit. User input (`onData`).
pub fn encode_paste(text: &str, modes: u32) -> String {
    let text = text.replace("\r\n", "\r").replace('\n', "\r");
    if modes & mode::BRACKETED_PASTE == 0 {
        return text;
    }
    format!("\x1b[200~{}\x1b[201~", text.replace('\x1b', "\u{241b}"))
}

/// The focus report for gaining (`CSI I`) or losing (`CSI O`) focus while focus events ([`mode::FOCUS_EVENTS`]) are
/// on, like xterm.js `_handleTextAreaFocus`, `_handleTextAreaBlur` and `_reportFocus`
/// (`src/browser/CoreBrowserTerminal.ts` lines 305-335 and 1135-1141 @ 08829c7eb6279dda2a9e86df901353a1a89858ca,
/// MIT). xterm.js also reports the current focus when the program turns the mode on; the host does that on the
/// frame where the bit appears. Not user input: it goes to `onData` without scrolling the view.
pub fn encode_focus(focused: bool, modes: u32) -> Option<&'static str> {
    if modes & mode::FOCUS_EVENTS == 0 {
        return None;
    }
    Some(if focused { "\x1b[I" } else { "\x1b[O" })
}
