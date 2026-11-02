//! The kitty keyboard protocol (<https://sw.kovidgoyal.net/kitty/keyboard-protocol/>).
//!
//! Ported from xterm.js `src/common/input/KittyKeyboard.ts` lines 12-526 (the flag, event type and modifier
//! constants and the `KittyKeyboard` class) @ 08829c7eb6279dda2a9e86df901353a1a89858ca (npm `@xterm/xterm`
//! 6.1.0-beta.336), Copyright (c) 2025 The xterm.js authors, MIT license (`crates/wongtty/LICENSE-XTERM`). The rules
//! and their order are unchanged; JavaScript string lengths are counted in UTF-16 units as there.

use super::{KeyInput, KeyResult, js_len};

/// Kitty keyboard enhancement flags, as [`crate::Engine::kitty_flags`] returns them.
pub mod flags {
    /// Disambiguate escape codes: fixes ambiguous legacy encodings.
    pub const DISAMBIGUATE_ESCAPE_CODES: u8 = 0b00001;
    /// Report event types: press, repeat and release.
    pub const REPORT_EVENT_TYPES: u8 = 0b00010;
    /// Report alternate keys: the shifted key and the base layout key.
    pub const REPORT_ALTERNATE_KEYS: u8 = 0b00100;
    /// Report all keys as escape codes: text-producing keys as `CSI u`.
    pub const REPORT_ALL_KEYS_AS_ESCAPE_CODES: u8 = 0b01000;
    /// Report associated text: the text code points in the escape code.
    pub const REPORT_ASSOCIATED_TEXT: u8 = 0b10000;
}

use flags::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum EventType {
    Press = 1,
    Repeat = 2,
    Release = 3,
}

/// Kitty modifier bits (not the xterm ones); the value sent is 1 + the bits.
const MOD_SHIFT: u32 = 0b0001;
const MOD_ALT: u32 = 0b0010;
const MOD_CTRL: u32 = 0b0100;
const MOD_SUPER: u32 = 0b1000;

const ESC: &str = "\x1b";

/// Key codes of the keys that produce no text, by DOM `key`.
fn functional_key_code(key: &str) -> Option<u32> {
    Some(match key {
        "Escape" => 27,
        "Enter" => 13,
        "Tab" => 9,
        "Backspace" => 127,
        "CapsLock" => 57358,
        "ScrollLock" => 57359,
        "NumLock" => 57360,
        "PrintScreen" => 57361,
        "Pause" => 57362,
        "ContextMenu" => 57363,
        // F13-F35 (F1-F12 use the legacy encoding)
        "F13" => 57376,
        "F14" => 57377,
        "F15" => 57378,
        "F16" => 57379,
        "F17" => 57380,
        "F18" => 57381,
        "F19" => 57382,
        "F20" => 57383,
        "F21" => 57384,
        "F22" => 57385,
        "F23" => 57386,
        "F24" => 57387,
        "F25" => 57388,
        // Keypad keys
        "KP_0" => 57399,
        "KP_1" => 57400,
        "KP_2" => 57401,
        "KP_3" => 57402,
        "KP_4" => 57403,
        "KP_5" => 57404,
        "KP_6" => 57405,
        "KP_7" => 57406,
        "KP_8" => 57407,
        "KP_9" => 57408,
        "KP_Decimal" => 57409,
        "KP_Divide" => 57410,
        "KP_Multiply" => 57411,
        "KP_Subtract" => 57412,
        "KP_Add" => 57413,
        "KP_Enter" => 57414,
        "KP_Equal" => 57415,
        // Modifier keys
        "ShiftLeft" => 57441,
        "ShiftRight" => 57447,
        "ControlLeft" => 57442,
        "ControlRight" => 57448,
        "AltLeft" => 57443,
        "AltRight" => 57449,
        "MetaLeft" => 57444,
        "MetaRight" => 57450,
        // Media keys
        "MediaPlayPause" => 57430,
        "MediaStop" => 57432,
        "MediaTrackNext" => 57435,
        "MediaTrackPrevious" => 57436,
        "AudioVolumeDown" => 57438,
        "AudioVolumeUp" => 57439,
        "AudioVolumeMute" => 57440,
        _ => return None,
    })
}

/// Keys sent as `CSI number ~`.
fn csi_tilde_key(key: &str) -> Option<u32> {
    Some(match key {
        "Insert" => 2,
        "Delete" => 3,
        "PageUp" => 5,
        "PageDown" => 6,
        "F5" => 15,
        "F6" => 17,
        "F7" => 18,
        "F8" => 19,
        "F9" => 20,
        "F10" => 21,
        "F11" => 23,
        "F12" => 24,
        _ => return None,
    })
}

/// Keys sent as `CSI letter` (arrows, Home, End).
fn csi_letter_key(key: &str) -> Option<char> {
    Some(match key {
        "ArrowUp" => 'A',
        "ArrowDown" => 'B',
        "ArrowRight" => 'C',
        "ArrowLeft" => 'D',
        "Home" => 'H',
        "End" => 'F',
        _ => return None,
    })
}

/// F1-F4, sent as SS3 without modifiers.
fn ss3_function_key(key: &str) -> Option<char> {
    Some(match key {
        "F1" => 'P',
        "F2" => 'Q',
        "F3" => 'R',
        "F4" => 'S',
        _ => return None,
    })
}

/// Kitty key codes of the numpad, by DOM `code`.
fn numpad_key_code(code: &str) -> Option<u32> {
    let suffix = code.strip_prefix("Numpad")?;
    // JavaScript compares the whole suffix as a string: `suffix >= '0' && suffix <= '9'`.
    if ("0"..="9").contains(&suffix) {
        // parseInt reads the leading digits; the comparison above lets only a digit first through.
        let n = suffix
            .bytes()
            .take_while(u8::is_ascii_digit)
            .fold(0u32, |n, d| {
                n.saturating_mul(10).saturating_add(u32::from(d - b'0'))
            });
        return Some(57399u32.saturating_add(n));
    }
    Some(match suffix {
        "Decimal" => 57409,
        "Divide" => 57410,
        "Multiply" => 57411,
        "Subtract" => 57412,
        "Add" => 57413,
        "Enter" => 57414,
        "Equal" => 57415,
        _ => return None,
    })
}

/// Kitty key codes of the modifier keys, by DOM `code`.
fn modifier_key_code(code: &str) -> Option<u32> {
    Some(match code {
        "ShiftLeft" => 57441,
        "ShiftRight" => 57447,
        "ControlLeft" => 57442,
        "ControlRight" => 57448,
        "AltLeft" => 57443,
        "AltRight" => 57449,
        "MetaLeft" => 57444,
        "MetaRight" => 57450,
        _ => return None,
    })
}

/// 1 + the modifier bits, or 0 without modifiers.
fn encode_modifiers(input: &KeyInput) -> u32 {
    let m = input.mods;
    let mods = if m.shift { MOD_SHIFT } else { 0 }
        | if m.alt { MOD_ALT } else { 0 }
        | if m.ctrl { MOD_CTRL } else { 0 }
        | if m.meta { MOD_SUPER } else { 0 };
    if mods > 0 { mods + 1 } else { 0 }
}

fn first_code_point(s: &str) -> Option<u32> {
    s.chars().next().map(u32::from)
}

/// The key code: the lowercase code point for letters, the base key (from `code`) for shifted keys.
fn get_key_code(input: &KeyInput, mac_option_as_alt: bool) -> Option<u32> {
    if let Some(code) = numpad_key_code(&input.code) {
        return Some(code);
    }
    if let Some(code) = modifier_key_code(&input.code) {
        return Some(code);
    }
    if let Some(code) = functional_key_code(&input.key) {
        return Some(code);
    }
    let code = input.code.as_str();
    if (input.mods.shift || (mac_option_as_alt && input.mods.alt)) && !code.is_empty() {
        if let Some(digit) = code.strip_prefix("Digit") {
            if js_len(code) == 6 {
                if let Some(d) = digit.chars().next().filter(char::is_ascii_digit) {
                    return Some(u32::from(d));
                }
            }
        }
        if let Some(letter) = code.strip_prefix("Key") {
            if js_len(code) == 4 {
                return letter
                    .chars()
                    .next()
                    .map(|c| c.to_lowercase().next().map_or(u32::from(c), u32::from));
            }
        }
    }
    if js_len(&input.key) == 1 {
        let code = first_code_point(&input.key)?;
        return Some(if (65..=90).contains(&code) {
            code + 32
        } else {
            code
        });
    }
    None
}

fn is_modifier_key(input: &KeyInput) -> bool {
    matches!(input.key.as_str(), "Shift" | "Control" | "Alt" | "Meta")
}

/// CapsLock, NumLock and ScrollLock count as modifier keys for suppressing presses, like kitty's
/// `is_modifier_key()` (kitty/keys.c).
fn is_lock_key(input: &KeyInput) -> bool {
    matches!(input.key.as_str(), "CapsLock" | "NumLock" | "ScrollLock")
}

/// `CSI 1 ; mod[:event] letter` with modifiers or a non-press event type, else `<plain><letter>`.
fn letter_sequence(
    plain: &str,
    letter: char,
    modifiers: u32,
    event_type: EventType,
    report_event_types: bool,
) -> String {
    let needs_event_type = report_event_types && event_type != EventType::Press;
    if modifiers > 0 || needs_event_type {
        let mut seq = format!("{ESC}[1;{}", modifiers.max(1));
        if needs_event_type {
            seq += &format!(":{}", event_type as u8);
        }
        seq.push(letter);
        seq
    } else {
        format!("{plain}{letter}")
    }
}

/// `CSI number [;mod[:event]] ~`.
fn tilde_sequence(
    number: u32,
    modifiers: u32,
    event_type: EventType,
    report_event_types: bool,
) -> String {
    let needs_event_type = report_event_types && event_type != EventType::Press;
    let mut seq = format!("{ESC}[{number}");
    if modifiers > 0 || needs_event_type {
        seq += &format!(";{}", modifiers.max(1));
        if needs_event_type {
            seq += &format!(":{}", event_type as u8);
        }
    }
    seq.push('~');
    seq
}

/// `CSI keycode[:shifted] [;mod[:event][;text]] u`.
fn csi_u_sequence(
    input: &KeyInput,
    key_code: u32,
    modifiers: u32,
    event_type: EventType,
    flags: u8,
    is_func: bool,
    is_mod: bool,
) -> String {
    let report_event_types = flags & REPORT_EVENT_TYPES != 0;
    let report_alternate_keys = flags & REPORT_ALTERNATE_KEYS != 0;
    let single = js_len(&input.key) == 1;

    let mut seq = format!("{ESC}[{key_code}");
    if report_alternate_keys && input.mods.shift && single && !is_func && !is_mod {
        if let Some(shifted) = first_code_point(&input.key) {
            seq += &format!(":{shifted}");
        }
    }

    let report_associated_text = flags & REPORT_ASSOCIATED_TEXT != 0
        && event_type != EventType::Release
        && single
        && !is_func
        && !is_mod
        && !input.mods.ctrl;
    let text_code = if report_associated_text {
        first_code_point(&input.key)
    } else {
        None
    };

    let needs_event_type = report_event_types
        && event_type != EventType::Press
        && (event_type == EventType::Release || text_code.is_none());

    if modifiers > 0 || needs_event_type || text_code.is_some() {
        seq.push(';');
        if modifiers > 0 {
            seq += &modifiers.to_string();
        } else if needs_event_type {
            seq.push('1');
        }
        if needs_event_type {
            seq += &format!(":{}", event_type as u8);
        }
    }
    if let Some(text) = text_code {
        seq += &format!(";{text}");
    }
    seq.push('u');
    seq
}

/// xterm.js `KittyKeyboard.evaluate`: one key event under the active kitty flags. `mac_option_as_alt` unwinds
/// macOS Option-composed keys through `code`.
pub(super) fn evaluate(
    input: &KeyInput,
    flags: u8,
    event_type: EventType,
    mac_option_as_alt: bool,
) -> KeyResult {
    let mut result = KeyResult::default();
    let modifiers = encode_modifiers(input);
    let is_mod = is_modifier_key(input);
    let report_event_types = flags & REPORT_EVENT_TYPES != 0;
    let report_all = flags & REPORT_ALL_KEYS_AS_ESCAPE_CODES != 0;

    if !report_event_types && event_type == EventType::Release {
        return result;
    }
    // Spec § "Report all keys as escape codes": "Additionally, with this mode, events for pressing modifier keys
    // are reported." Without it modifier and lock key events are suppressed.
    if (is_mod || is_lock_key(input)) && !report_all {
        return result;
    }

    if let Some(letter) = csi_letter_key(&input.key) {
        result.key = Some(letter_sequence(
            &format!("{ESC}["),
            letter,
            modifiers,
            event_type,
            report_event_types,
        ));
        result.cancel = true;
        return result;
    }
    if let Some(letter) = ss3_function_key(&input.key) {
        result.key = Some(letter_sequence(
            &format!("{ESC}O"),
            letter,
            modifiers,
            event_type,
            report_event_types,
        ));
        result.cancel = true;
        return result;
    }
    if let Some(number) = csi_tilde_key(&input.key) {
        result.key = Some(tilde_sequence(
            number,
            modifiers,
            event_type,
            report_event_types,
        ));
        result.cancel = true;
        return result;
    }

    let Some(key_code) = get_key_code(input, mac_option_as_alt) else {
        return result;
    };

    let special_key = key_code == 13 || key_code == 9 || key_code == 127;
    // Per spec, Enter, Tab and Backspace have no release events unless all keys are reported as escape codes.
    if special_key && event_type == EventType::Release && !report_all {
        return result;
    }

    let is_func =
        functional_key_code(&input.key).is_some() || numpad_key_code(&input.code).is_some();
    let single = js_len(&input.key) == 1;

    // Report event types without disambiguate makes little sense, so it implies disambiguate, as in xterm.js
    // (https://github.com/kovidgoyal/kitty/issues/9999). Per spec, Enter, Tab and Backspace "still generate the
    // same bytes as in legacy mode" and space is a text key, so these take CSI u only with modifiers.
    let use_csi_u = report_all
        || (report_event_types && event_type == EventType::Release)
        || ((flags & DISAMBIGUATE_ESCAPE_CODES != 0 || report_event_types)
            && ((is_func && !special_key)
                || (modifiers > 0 && !single)
                || (modifiers > 0 && modifiers - 1 > MOD_SHIFT)));

    if use_csi_u {
        result.key = Some(csi_u_sequence(
            input, key_code, modifiers, event_type, flags, is_func, is_mod,
        ));
        result.cancel = true;
    } else {
        let legacy = match key_code {
            13 => Some("\r"),
            9 => Some("\t"),
            127 => Some("\x7f"),
            _ => None,
        };
        if let Some(byte) = legacy {
            result.key = Some(byte.to_string());
        } else if single && !input.mods.ctrl && !input.mods.alt && !input.mods.meta {
            result.key = Some(input.key.clone());
        }
    }
    result
}
