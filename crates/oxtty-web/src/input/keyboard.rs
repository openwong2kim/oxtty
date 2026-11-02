//! The textarea's key handling: when keydown, keypress and input each send the text, around
//! `oxtty::input::encode_key`.
//!
//! Ported from xterm.js `src/browser/CoreBrowserTerminal.ts` @ 08829c7eb6279dda2a9e86df901353a1a89858ca (npm
//! `@xterm/xterm` 6.1.0-beta.336), Copyright (c) 2014 The xterm.js authors, MIT license
//! (`crates/oxtty/LICENSE-XTERM`): lines 846-1058 (`_keyDown`, `_keyUp`, `_keyPress`, `_inputEvent`) and 1168-1177
//! (`wasModifierKeyOnlyEvent`), with `src/browser/services/KeyboardService.ts` lines 39-66 (kitty only with
//! `vtExtensions.kittyKeyboard`). The rules and their order are unchanged; win32 input mode is not supported, and
//! `onKey` waits for its module.
use std::rc::Rc;

use js_sys::{JsString, Reflect};
use oxtty::input::{
    encode_key, is_third_level_shift, KeyAction, KeyEventType, KeyInput, Modifiers, Platform,
};
use wasm_bindgen::prelude::*;
use web_sys::{Event, InputEvent, KeyboardEvent};

use super::{composition, custom_handler_allows, key_context, send_data, terminal, Shared};

/// The flags xterm.js keeps between key events.
#[derive(Clone, Copy, Default)]
pub(super) struct Keys {
    /// The keydown sent its text: keypress must not send it again (screen reader mode only).
    key_down_handled: bool,
    /// A keydown came since the last keyup: an `insertText` input event is the key's own text.
    key_down_seen: bool,
    /// The keypress sent its text: the input event must not.
    key_press_handled: bool,
    /// A dead key waits for the character it composes; its keydown must not send.
    unprocessed_dead_key: bool,
}

fn update(shared: &Shared, f: impl FnOnce(&mut Keys)) {
    let mut keys = shared.input.keys.get();
    f(&mut keys);
    shared.input.keys.set(keys);
}

/// Modifier keys alone never produce text.
fn modifier_only(event: &KeyboardEvent) -> bool {
    matches!(event.key_code(), 16 | 17 | 18 | 91 | 92 | 93 | 224) || event.key() == "Meta"
}

fn key_input(event: &KeyboardEvent, event_type: KeyEventType) -> KeyInput {
    KeyInput {
        key: event.key(),
        code: event.code(),
        key_code: Some(event.key_code()),
        location: event.location() as u8,
        mods: Modifiers {
            shift: event.shift_key(),
            alt: event.alt_key(),
            ctrl: event.ctrl_key(),
            meta: event.meta_key(),
        },
        event_type,
        composing: event.is_composing() || event.key_code() == 229,
    }
}

fn cancel(event: &Event) {
    event.prevent_default();
    event.stop_propagation();
}

pub(super) fn key_down(shared: &Rc<Shared>, event: &KeyboardEvent) {
    update(shared, |k| {
        k.key_down_handled = false;
        // A modifier-only keydown must neither arm nor disarm the `_inputEvent` gate (xterm.js #5887, #6144).
        if !modifier_only(event) {
            k.key_down_seen = true;
        }
    });
    if !custom_handler_allows(&shared.input.custom_key_handler, event) {
        return;
    }
    let Some(context) = key_context(shared) else {
        return;
    };
    // Ignore composing with Alt key on Mac when macOptionIsMeta is enabled
    let ignore_composition = context.options.platform == Platform::Mac
        && context.options.mac_option_is_meta
        && event.alt_key();
    if !ignore_composition && !composition::key_down(shared, event) {
        scroll_on_user_input(shared);
        return;
    }
    if !ignore_composition && matches!(event.key().as_str(), "Dead" | "AltGraph") {
        update(shared, |k| k.unprocessed_dead_key = true);
    }
    // The composition may have sent text, and listeners may have changed the terminal: read the state again.
    let Some(context) = key_context(shared) else {
        return;
    };
    let event_type = if event.repeat() {
        KeyEventType::Repeat
    } else {
        KeyEventType::Press
    };
    let input = key_input(event, event_type);
    let result = encode_key(&input, context.modes, context.kitty_flags, &context.options);
    crate::selection::update_cursor_style(shared, event);
    match result.action {
        KeyAction::PageUp | KeyAction::PageDown => {
            let count = context.rows as i32 - 1;
            let amount = if result.action == KeyAction::PageUp {
                -count
            } else {
                count
            };
            let _ = terminal(shared).scroll_lines(amount);
            cancel(event);
            return;
        }
        KeyAction::SelectAll => crate::selection::select_all(shared),
        KeyAction::Send => {}
    }
    if is_third_level_shift(
        &input,
        false,
        event.get_modifier_state("AltGraph"),
        &context.options,
    ) {
        return;
    }
    if result.cancel {
        cancel(event);
    }
    let Some(key) = result.key else {
        return;
    };
    // Process A-Z in the keypress event so macOS IMEs can type lower case with caps lock on; not with the kitty
    // protocol, which sends proper sequences for every key.
    let text = event.key();
    let mut units = text.encode_utf16();
    if context.kitty_flags == 0
        && !event.ctrl_key()
        && !event.alt_key()
        && !event.meta_key()
        && matches!((units.next(), units.next()), (Some(65..=90), None))
    {
        return;
    }
    let mut dead_key = false;
    update(shared, |k| {
        dead_key = std::mem::take(&mut k.unprocessed_dead_key);
    });
    if dead_key {
        return;
    }
    // Clear the textarea on Ctrl+C or Enter so screen readers announce deleted characters.
    if key == "\x03" || key == "\r" {
        if let Some(textarea) = shared.input.textarea() {
            textarea.set_value("");
        }
    }
    send_data(shared, &key.into(), true);
    // Cancelled outside screen reader mode so other listeners do not handle it too; in screen reader mode the
    // textarea is updated, which the screen reader reads out.
    if !context.screen_reader || event.alt_key() || event.ctrl_key() {
        cancel(event);
        return;
    }
    update(shared, |k| k.key_down_handled = true);
}

/// The composition took the key: a scrolled-back view still goes to the bottom (`scrollOnUserInput`).
fn scroll_on_user_input(shared: &Rc<Shared>) {
    let scrolled = shared.core.try_borrow().is_ok_and(|core| {
        !core.disposed
            && crate::options::option(&core.options, "scrollOnUserInput").is_truthy()
            && core.view.display_offset() != 0
    });
    if scrolled {
        let _ = terminal(shared).scroll_to_bottom();
    }
}

pub(super) fn key_up(shared: &Rc<Shared>, event: &KeyboardEvent) {
    update(shared, |k| k.key_down_seen = false);
    if !custom_handler_allows(&shared.input.custom_key_handler, event) {
        return;
    }
    if !modifier_only(event) {
        terminal(shared).focus();
    }
    // Key releases for the kitty protocol
    if let Some(context) = key_context(shared) {
        let input = key_input(event, KeyEventType::Release);
        let result = encode_key(&input, context.modes, context.kitty_flags, &context.options);
        if let Some(key) = result.key {
            send_data(shared, &key.into(), true);
        }
    }
    crate::selection::update_cursor_style(shared, event);
    update(shared, |k| k.key_press_handled = false);
}

pub(super) fn key_press(shared: &Rc<Shared>, event: &KeyboardEvent) {
    let mut handled = false;
    update(shared, |k| {
        k.key_press_handled = false;
        handled = k.key_down_handled;
    });
    if handled || !custom_handler_allows(&shared.input.custom_key_handler, event) {
        return;
    }
    let char_code = event.char_code();
    let which = Reflect::get(event, &"which".into()).unwrap_or(JsValue::UNDEFINED);
    let key = if char_code != 0 {
        char_code
    } else if which.is_null() || which.is_undefined() {
        event.key_code()
    } else {
        return;
    };
    let Some(context) = key_context(shared) else {
        return;
    };
    let third_level = || {
        is_third_level_shift(
            &key_input(event, KeyEventType::Press),
            true,
            event.get_modifier_state("AltGraph"),
            &context.options,
        )
    };
    if key == 0 || ((event.alt_key() || event.ctrl_key() || event.meta_key()) && !third_level()) {
        return;
    }
    send_data(shared, &JsString::from_char_code1(key).into(), true);
    // The key was handled so clear the dead key state, otherwise certain keystrokes like arrow keys could be ignored
    update(shared, |k| {
        k.key_press_handled = true;
        k.unprocessed_dead_key = false;
    });
}

/// Text inserted without a key the terminal handled: emoji pickers, IMEs that commit directly, dictation.
pub(super) fn input(shared: &Rc<Shared>, event: &Event) {
    let data = Reflect::get(event, &"data".into()).unwrap_or(JsValue::UNDEFINED);
    let insert_text = event
        .dyn_ref::<InputEvent>()
        .map_or_else(|| get_string(event, "inputType"), |e| e.input_type())
        == "insertText";
    let keys = shared.input.keys.get();
    let screen_reader = shared.core.try_borrow().map_or(true, |core| {
        crate::options::option(&core.options, "screenReaderMode").is_truthy()
    });
    if data.is_truthy()
        && insert_text
        && (!event.composed() || !keys.key_down_seen)
        && !screen_reader
    {
        if keys.key_press_handled {
            return;
        }
        update(shared, |k| k.unprocessed_dead_key = false);
        send_data(shared, &data, true);
    }
}

fn get_string(target: &JsValue, key: &str) -> String {
    crate::engine::get(target, key)
        .as_string()
        .unwrap_or_default()
}
