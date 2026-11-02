//! The default xterm key encoding.
//!
//! Ported from xterm.js `src/common/input/Keyboard.ts` lines 11-380 (`KEYCODE_KEY_MAPPINGS`,
//! `evaluateKeyboardEvent`) @ 08829c7eb6279dda2a9e86df901353a1a89858ca (npm `@xterm/xterm` 6.1.0-beta.336),
//! Copyright (c) 2014 The xterm.js authors, Copyright (c) 2012-2013 Christopher Jeffrey, MIT license
//! (`crates/wongtty/LICENSE-XTERM`). The rules and their order are unchanged; JavaScript string lengths are counted
//! in UTF-16 units as there.

use super::{KeyAction, KeyInput, KeyResult, js_len};

const ESC: &str = "\x1b";

/// Unshifted and shifted characters of the digit and punctuation keys on a US layout, by DOM `keyCode`.
fn keycode_key_mapping(key_code: u32) -> Option<[&'static str; 2]> {
    Some(match key_code {
        48 => ["0", ")"],
        49 => ["1", "!"],
        50 => ["2", "@"],
        51 => ["3", "#"],
        52 => ["4", "$"],
        53 => ["5", "%"],
        54 => ["6", "^"],
        55 => ["7", "&"],
        56 => ["8", "*"],
        57 => ["9", "("],
        186 => [";", ":"],
        187 => ["=", "+"],
        188 => [",", "<"],
        189 => ["-", "_"],
        190 => [".", ">"],
        191 => ["/", "?"],
        192 => ["`", "~"],
        219 => ["[", "{"],
        220 => ["\\", "|"],
        221 => ["]", "}"],
        222 => ["'", "\""],
        _ => return None,
    })
}

fn char_code(code: u32) -> String {
    char::from_u32(code).map(String::from).unwrap_or_default()
}

/// `CSI 1 ; mods+1 <letter>` with modifiers, else SS3 or CSI `<letter>` by the cursor key mode.
fn cursor_key(letter: char, modifiers: u32, app_cursor: bool) -> String {
    if modifiers != 0 {
        format!("{ESC}[1;{}{letter}", modifiers + 1)
    } else if app_cursor {
        format!("{ESC}O{letter}")
    } else {
        format!("{ESC}[{letter}")
    }
}

/// xterm.js `evaluateKeyboardEvent`: the legacy (non-kitty) encoding of one key press.
pub(super) fn evaluate(
    input: &KeyInput,
    key_code: u32,
    app_cursor: bool,
    mac: bool,
    mac_option_is_meta: bool,
) -> KeyResult {
    let mut result = KeyResult::default();
    let m = input.mods;
    let modifiers =
        m.shift as u32 | (m.alt as u32) << 1 | (m.ctrl as u32) << 2 | (m.meta as u32) << 3;
    let key = input.key.as_str();
    let mut out: Option<String> = None;
    let fkey = |n: u32| -> String {
        if modifiers != 0 {
            format!("{ESC}[{n};{}~", modifiers + 1)
        } else {
            format!("{ESC}[{n}~")
        }
    };
    let ss3 = |letter: char| -> String {
        if modifiers != 0 {
            format!("{ESC}[1;{}{letter}", modifiers + 1)
        } else {
            format!("{ESC}O{letter}")
        }
    };
    match key_code {
        0 => {
            let letter = match key {
                "UIKeyInputUpArrow" => Some('A'),
                "UIKeyInputLeftArrow" => Some('D'),
                "UIKeyInputRightArrow" => Some('C'),
                "UIKeyInputDownArrow" => Some('B'),
                _ => None,
            };
            if let Some(letter) = letter {
                out = Some(cursor_key(letter, 0, app_cursor));
            }
        }
        // Backspace: ^H or ^?
        8 => {
            let k = if m.ctrl { "\x08" } else { "\x7f" };
            out = Some(if m.alt {
                format!("{ESC}{k}")
            } else {
                k.to_string()
            });
        }
        9 => {
            if m.shift {
                out = Some(format!("{ESC}[Z"));
            } else {
                out = Some("\t".to_string());
                result.cancel = true;
            }
        }
        13 => {
            // Safari on iPad, iOS and Vision Pro sends keyCode 13 for Ctrl+C on a hardware keyboard.
            if key == "c" && m.ctrl {
                out = Some("\x03".to_string());
            } else {
                out = Some(if m.alt {
                    format!("{ESC}\r")
                } else {
                    "\r".to_string()
                });
            }
            result.cancel = true;
        }
        27 => {
            out = Some(if m.alt {
                format!("{ESC}{ESC}")
            } else {
                ESC.to_string()
            });
            result.cancel = true;
        }
        37..=40 => {
            if !m.meta {
                let letter = match key_code {
                    37 => 'D',
                    39 => 'C',
                    38 => 'A',
                    _ => 'B',
                };
                out = Some(cursor_key(letter, modifiers, app_cursor));
            }
        }
        // Insert: Ctrl or Shift + Insert copy and paste on some systems.
        45 => {
            if !m.shift && !m.ctrl {
                out = Some(format!("{ESC}[2~"));
            }
        }
        46 => out = Some(fkey(3)),
        36 => out = Some(cursor_key('H', modifiers, app_cursor)),
        35 => out = Some(cursor_key('F', modifiers, app_cursor)),
        33 | 34 => {
            let (n, action) = if key_code == 33 {
                (5, KeyAction::PageUp)
            } else {
                (6, KeyAction::PageDown)
            };
            if m.shift {
                result.action = action;
            } else if m.ctrl {
                out = Some(format!("{ESC}[{n};{}~", modifiers + 1));
            } else {
                out = Some(format!("{ESC}[{n}~"));
            }
        }
        112 => out = Some(ss3('P')),
        113 => out = Some(ss3('Q')),
        114 => out = Some(ss3('R')),
        115 => out = Some(ss3('S')),
        116 => out = Some(fkey(15)),
        117 => out = Some(fkey(17)),
        118 => out = Some(fkey(18)),
        119 => out = Some(fkey(19)),
        120 => out = Some(fkey(20)),
        121 => out = Some(fkey(21)),
        122 => out = Some(fkey(23)),
        123 => out = Some(fkey(24)),
        _ => {
            if m.ctrl && !m.shift && !m.alt && !m.meta {
                // a-z and space
                if (65..=90).contains(&key_code) {
                    out = Some(char_code(key_code - 64));
                } else if key_code == 32 {
                    out = Some("\0".to_string());
                } else if (51..=55).contains(&key_code) {
                    // Escape, file, group, record and unit separators
                    out = Some(char_code(key_code - 51 + 27));
                } else if key_code == 56 {
                    out = Some("\x7f".to_string());
                } else if key == "/" {
                    // https://github.com/xtermjs/xterm.js/issues/5457
                    out = Some("\x1f".to_string());
                } else if key_code == 219 {
                    out = Some(ESC.to_string());
                } else if key_code == 220 {
                    out = Some("\x1c".to_string());
                } else if key_code == 221 {
                    out = Some("\x1d".to_string());
                }
            } else if (!mac || mac_option_is_meta) && m.alt && !m.meta {
                // On macOS this is a third level shift when !macOptionIsMeta. Use <Esc> instead.
                if let Some(mapping) = keycode_key_mapping(key_code) {
                    out = Some(format!("{ESC}{}", mapping[m.shift as usize]));
                } else if (65..=90).contains(&key_code) {
                    let code = if m.ctrl { key_code - 64 } else { key_code + 32 };
                    let mut s = char_code(code);
                    if m.shift {
                        s = s.to_uppercase();
                    }
                    out = Some(format!("{ESC}{s}"));
                } else if key_code == 32 {
                    out = Some(format!("{ESC}{}", if m.ctrl { "\0" } else { " " }));
                } else if key == "Dead" && input.code.starts_with("Key") {
                    // Alt makes a dead key (starts a composition) of some letters on a US layout (N, E, U…):
                    // https://github.com/xtermjs/xterm.js/issues/3725
                    let mut s: String = input.code.chars().skip(3).take(1).collect();
                    if !m.shift {
                        s = s.to_lowercase();
                    }
                    out = Some(format!("{ESC}{s}"));
                    result.cancel = true;
                }
            } else if mac && !m.alt && !m.ctrl && !m.shift && m.meta {
                if key_code == 65 {
                    // Cmd+A
                    result.action = KeyAction::SelectAll;
                }
            } else if !key.is_empty()
                && !m.ctrl
                && !m.alt
                && !m.meta
                && key_code >= 48
                && js_len(key) == 1
            {
                // Only keys that produce a single character, not NumLock, volume up…
                out = Some(key.to_string());
            } else if !key.is_empty() && m.ctrl && m.shift {
                match input.code.as_str() {
                    // ^_ (Ctrl+Shift+-)
                    "Minus" => out = Some("\x1f".to_string()),
                    // ^@ (Ctrl+Shift+2)
                    "Digit2" => out = Some("\0".to_string()),
                    // ^^ (Ctrl+Shift+6)
                    "Digit6" => out = Some("\x1e".to_string()),
                    _ => {}
                }
            }
        }
    }
    result.key = out;
    result
}
