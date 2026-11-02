//! IME composition: the preedit shows in the composition view at the cursor, and only the committed text is sent,
//! read back from the textarea a task after `compositionend` (Korean moves a final consonant to the next syllable,
//! so the event data cannot be trusted).
//!
//! Ported from xterm.js `src/browser/input/CompositionHelper.ts` @ 08829c7eb6279dda2a9e86df901353a1a89858ca (npm
//! `@xterm/xterm` 6.1.0-beta.336), lines 21-284, Copyright (c) 2016 The xterm.js authors, MIT license
//! (`crates/wongtty/LICENSE-XTERM`). The rules, the `setTimeout(0)` re-reads and their order are unchanged; text
//! positions are UTF-16 indices into the textarea value, as in JavaScript.
use std::rc::Rc;

use js_sys::JsString;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Event, HtmlElement, HtmlTextAreaElement, KeyboardEvent};

use super::{buffer_cursor, dimensions, js_number, send_data, set_px, Shared};
use crate::options::option;
use crate::timer;

#[derive(Default)]
pub(super) struct Composition {
    /// A composition is in progress (IME, mobile keyboard, dictation): the view shows it.
    pub is_composing: bool,
    /// UTF-16 range of the composition in the textarea value.
    start: u32,
    end: u32,
    /// The value after the composition when it started, so existing text is not taken as new input.
    suffix: Option<JsString>,
    /// A commit waits for its timer; clearing it cancels that send.
    is_sending: bool,
    /// UTF-16 length of text a 229 keydown already sent during the composition (xterm.js #3191).
    data_already_sent: u32,
    /// A 229 keydown outside a composition waits for its timer.
    textarea_change_pending: bool,
}

fn value(textarea: &HtmlTextAreaElement) -> JsString {
    js_sys::Reflect::get(textarea, &"value".into())
        .ok()
        .and_then(|v| v.dyn_into().ok())
        .unwrap_or_else(|| JsString::from(""))
}

fn dom(shared: &Shared) -> Option<(HtmlTextAreaElement, HtmlElement)> {
    shared
        .input
        .dom(|dom| (dom.textarea.clone(), dom.composition_view.clone()))
}

/// Runs `f` with the shared state on a later task, unless the terminal is gone or closed by then.
fn later(shared: &Rc<Shared>, f: impl FnOnce(&Rc<Shared>) + 'static) {
    let weak = Rc::downgrade(shared);
    timer::queue(move || {
        if let Some(shared) = weak.upgrade() {
            if shared.input.dom(|_| ()).is_some() {
                f(&shared);
            }
        }
    });
}

pub(super) fn start(shared: &Rc<Shared>) {
    let Some((textarea, view)) = dom(shared) else {
        return;
    };
    let value = value(&textarea);
    // The selection, not the length, so screen reader mode's text does not get in the way
    let start = textarea
        .selection_start()
        .ok()
        .flatten()
        .unwrap_or(value.length());
    let end = textarea.selection_end().ok().flatten().unwrap_or(start);
    {
        let mut c = shared.input.composition.borrow_mut();
        c.is_composing = true;
        c.start = start.min(end);
        c.end = start.max(end);
        c.suffix = Some(value.substring(c.end, value.length()));
        c.data_already_sent = 0;
    }
    view.set_text_content(Some(""));
    let _ = view.class_list().add_1("active");
}

pub(super) fn update(shared: &Rc<Shared>, event: &Event) {
    let Some((_, view)) = dom(shared) else {
        return;
    };
    let data = crate::engine::get(event, "data")
        .as_string()
        .unwrap_or_default();
    // Marked LTR: the view is `direction: rtl` so a long composition shows its end
    view.set_text_content(Some(&format!("\u{200E}{data}\u{200E}")));
    update_elements(shared, false);
    later(shared, |shared| {
        let Some((textarea, _)) = dom(shared) else {
            return;
        };
        let end = textarea
            .selection_end()
            .ok()
            .flatten()
            .unwrap_or_else(|| value(&textarea).length());
        let mut c = shared.input.composition.borrow_mut();
        c.end = c.start.max(end);
    });
}

pub(super) fn end(shared: &Rc<Shared>) {
    finalize(shared, true);
}

/// Whether the terminal should go on handling this keydown.
pub(super) fn key_down(shared: &Rc<Shared>, event: &KeyboardEvent) -> bool {
    let key_code = event.key_code();
    let busy = shared
        .input
        .composition
        .try_borrow()
        .is_ok_and(|c| c.is_composing || c.is_sending);
    if busy {
        // 20 is CapsLock, 229 the "composition character": still composing. So are modifier keys.
        if matches!(key_code, 16 | 17 | 18 | 20 | 229) {
            return false;
        }
        // Finish composition immediately, so a key like Enter reaches the program after the text.
        finalize(shared, false);
    }
    if key_code == 229 {
        // The IME is on but this is no composition (digits, punctuation): send what it changes in the textarea.
        handle_any_textarea_changes(shared);
        return false;
    }
    true
}

/// Ends the composition. `wait` reads the committed text on the next task, after the browser has put it in the
/// textarea; without it (a key that ends the composition, like Enter) the text goes out now.
fn finalize(shared: &Rc<Shared>, wait: bool) {
    let Some((textarea, view)) = dom(shared) else {
        return;
    };
    let _ = view.class_list().remove_1("active");
    let (start, end, suffix) = {
        let mut c = shared.input.composition.borrow_mut();
        c.is_composing = false;
        c.is_sending = wait;
        (c.start, c.end, c.suffix.clone())
    };
    if !wait {
        // Cancels any delayed send of this composition.
        let text = value(&textarea).substring(start, end);
        send_data(shared, &text.into(), true);
        return;
    }
    // The positions are copied: a new compositionstart may come before the timer.
    later(shared, move |shared| {
        let Some((textarea, _)) = dom(shared) else {
            return;
        };
        let text = {
            let mut c = shared.input.composition.borrow_mut();
            // Ensure that the input has not already been sent
            if !c.is_sending {
                return;
            }
            c.is_sending = false;
            // Skip what a keydown already sent, or it would go out twice (xterm.js #3191)
            let start = start + c.data_already_sent;
            let value = value(&textarea);
            if c.is_composing {
                // A new composition started: the text ends where it begins
                value.substring(start, c.start)
            } else {
                // Keep text typed right after the composition, without the text that was after it already
                let suffix = suffix.filter(|s| s.length() > 0);
                let len = value.length();
                let value_end = match suffix {
                    Some(s) if value.ends_with(&String::from(&s), len as i32) => len - s.length(),
                    _ => len,
                };
                value.substring(start, start.max(value_end))
            }
        };
        if text.length() > 0 {
            send_data(shared, &text.into(), true);
        }
    });
}

/// Sends what changes in the textarea by the next task: a 229 keydown outside a composition.
fn handle_any_textarea_changes(shared: &Rc<Shared>) {
    let Some((textarea, _)) = dom(shared) else {
        return;
    };
    {
        let mut c = shared.input.composition.borrow_mut();
        if c.textarea_change_pending {
            return;
        }
        c.textarea_change_pending = true;
    }
    let old = value(&textarea);
    later(shared, move |shared| {
        let Some((textarea, _)) = dom(shared) else {
            return;
        };
        let data = {
            let mut c = shared.input.composition.borrow_mut();
            c.textarea_change_pending = false;
            // Ignore if a composition has started since
            if c.is_composing {
                return;
            }
            let new = value(&textarea);
            let diff = new.replace(&String::from(&old), "");
            c.data_already_sent = diff.length();
            if new.length() > old.length() {
                diff
            } else if new.length() < old.length() {
                JsString::from("\x7f")
            } else if new != old {
                new
            } else {
                return;
            }
        };
        send_data(shared, &data.into(), true);
    });
}

/// Puts the composition view on the cursor and the textarea under it, so the IME's window opens there. Browsers
/// do not fire the IME events consistently, so unless `dont_recurse` it does it again on the next task.
pub(super) fn update_elements(shared: &Rc<Shared>, dont_recurse: bool) {
    let composing = shared
        .input
        .composition
        .try_borrow()
        .is_ok_and(|c| c.is_composing);
    if !composing {
        return;
    }
    let Some((textarea, view)) = dom(shared) else {
        return;
    };
    let placed = shared.core.try_borrow().ok().and_then(|core| {
        let (x, y, in_viewport) = buffer_cursor(&core);
        let cell = dimensions(&core).css_cell;
        // Without a measured cell (no renderer) nothing is moved: a zero line height ends the composition.
        if !in_viewport || !(cell.width > 0.0 && cell.height > 0.0) {
            return None;
        }
        let cols = core.viewport.cols;
        let cursor_x = x.min(cols.saturating_sub(1));
        let left = f64::from(cursor_x) * cell.width;
        let top = f64::from(y) * cell.height;
        let font_family = option(&core.options, "fontFamily");
        let font_size = option(&core.options, "fontSize");
        Some((
            left,
            top,
            cell.height,
            f64::from(cols) * cell.width - left,
            font_family,
            font_size,
        ))
    });
    if let Some((left, top, cell_height, max_width, font_family, font_size)) = placed {
        set_px(
            &view,
            &[
                ("left", left),
                ("top", top),
                ("height", cell_height),
                ("line-height", cell_height),
            ],
        );
        let style = view.style();
        let _ = style.set_property("font-family", &js_text(&font_family));
        let _ = style.set_property("font-size", &format!("{}px", js_text(&font_size)));
        // Only up to the right edge of the terminal
        set_px(&view, &[("max-width", max_width)]);
        let _ = style.set_property("overflow", "hidden");
        let _ = style.set_property("direction", "rtl");
        let bounds = view.get_bounding_client_rect();
        set_px(textarea.unchecked_ref(), &[("left", left), ("top", top)]);
        // At least 1x1, or some IMEs break
        set_px(
            textarea.unchecked_ref(),
            &[
                ("width", bounds.width().max(1.0)),
                ("height", bounds.height().max(1.0)),
                ("line-height", bounds.height()),
            ],
        );
    }
    if !dont_recurse {
        later(shared, |shared| update_elements(shared, true));
    }
}

/// A number or string option as string concatenation prints it.
fn js_text(value: &JsValue) -> String {
    match value.as_f64() {
        Some(n) => js_number(n),
        None => value.as_string().unwrap_or_default(),
    }
}
