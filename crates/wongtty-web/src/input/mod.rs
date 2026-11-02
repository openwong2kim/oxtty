//! Keyboard, IME, paste, focus and mouse input: the hidden textarea and the DOM listeners of xterm.js 6.1
//! (`CoreBrowserTerminal`, `CompositionHelper`, `MouseService`), ported to Rust around the encoders of
//! `wongtty::input`. Encoding reads modes and kitty flags as of the last parsed write ([`crate::engine::Events`],
//! PRD D7), so a key right after the write that changed them is encoded as in xterm.js; the engine is only called
//! for terminal actions (scrolling, the kitty switch, the host focus it reports when the program asks) and the
//! `kittyKeyboardFlags` getter (public API, like `modes`), and the scroll position and the lines Alt+click walks
//! come from `buffer.active`.
//!
//! Memory: one DOM listener closure per terminal. Every listener the terminal registers (textarea, element,
//! screen, document, window) is that closure, which dispatches on the event's type and current target; the
//! dynamic ones follow the mouse state ([`Dom::set_dynamic`]). Where xterm.js registers two listeners for one
//! type on one target (mousedown, wheel and mousemove on the element), the one registration runs both handlers in
//! xterm.js's order.
//!
//! The textarea, focus and paste parts are ported from xterm.js `src/browser/CoreBrowserTerminal.ts` @
//! 08829c7eb6279dda2a9e86df901353a1a89858ca (npm `@xterm/xterm` 6.1.0-beta.336), Copyright (c) 2014 The xterm.js
//! authors, MIT license (`crates/wongtty/LICENSE-XTERM`): lines 286-363 (focus, blur, `_syncTextArea`), 377-379 and
//! 413-431 (listeners), 487-514 (the textarea) and 735-741 (`paste`, `attachCustomKeyEventHandler`); each function
//! from another file names it.
mod composition;
mod keyboard;
pub(crate) mod mouse;
mod move_to_cell;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use js_sys::{Function, JsString};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    AddEventListenerOptions, ClipboardEvent, Document, Event, EventTarget, FocusOptions,
    HtmlElement, HtmlTextAreaElement, Window,
};
use wongtty::input::{encode_focus, encode_paste, KeyOptions, Platform};

use crate::engine::{get, mode, Engine};
use crate::options::option;
use crate::render::Dimensions;
use crate::terminal::{Core, Shared, Terminal};

#[wasm_bindgen(raw_module = "./index.js")]
extern "C" {
    /// The host's focus, which the engine reports in place when the program turns on focus events (`CSI ? 1004 h`)
    /// instead of a frame later.
    #[wasm_bindgen(method, catch, js_class = "Terminal", js_name = setFocus)]
    fn set_focus(this: &Engine, focused: bool) -> Result<(), JsValue>;
}

const STYLE_ID: &str = "wongtty-xterm-input-style";
/// The xterm.css rules of the textarea, the composition view and the mouse cursor while the program takes clicks.
const STYLE: &str = "
.xterm.enable-mouse-events { cursor: default; }
.xterm.xterm-cursor-pointer, .xterm .xterm-cursor-pointer { cursor: pointer; }
.xterm.column-select.focus { cursor: crosshair; }
.xterm .xterm-helper-textarea { padding: 0; border: 0; margin: 0; position: absolute; opacity: 0; left: -9999em; top: 0; width: 0; height: 0; z-index: -5; white-space: nowrap; overflow: hidden; resize: none; }
.xterm .composition-view { background: #000; color: #FFF; display: none; position: absolute; white-space: nowrap; z-index: 1; }
.xterm .composition-view.active { display: block; }
";

/// xterm.js `common/Platform.ts`, read once per page.
#[derive(Clone, Copy)]
struct Browser {
    mac: bool,
    windows: bool,
    linux: bool,
    firefox: bool,
    /// The Chrome major version in the user agent.
    chrome: Option<u32>,
}

thread_local! {
    static BROWSER: Browser = {
        let navigator = get(&js_sys::global(), "navigator");
        let platform = get(&navigator, "platform").as_string().unwrap_or_default();
        let agent = get(&navigator, "userAgent").as_string().unwrap_or_default();
        Browser {
            mac: matches!(platform.as_str(), "Macintosh" | "MacIntel" | "MacPPC" | "Mac68K"),
            windows: matches!(platform.as_str(), "Windows" | "Win16" | "Win32" | "WinCE"),
            linux: platform.contains("Linux"),
            firefox: agent.contains("Firefox"),
            chrome: agent.split_once("Chrome/").and_then(|(_, v)| {
                let digits: String = v.chars().take_while(char::is_ascii_digit).collect();
                digits.parse().ok()
            }),
        }
    };
}

fn browser() -> Browser {
    BROWSER.with(|b| *b)
}

fn platform() -> Platform {
    let b = browser();
    if b.mac {
        Platform::Mac
    } else if b.windows {
        Platform::Windows
    } else {
        Platform::Other
    }
}

/// What a listener is on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Target {
    Textarea,
    Element,
    Screen,
    Document,
    Window,
}

/// Listeners on from `open` to `dispose`: target, type, capture, and whether it may cancel (`passive: false`).
const STATIC: [(Target, &str, bool, bool); 21] = [
    (Target::Textarea, "focus", false, false),
    (Target::Textarea, "blur", false, false),
    (Target::Textarea, "keyup", true, false),
    (Target::Textarea, "keydown", true, false),
    (Target::Textarea, "keypress", true, false),
    (Target::Textarea, "compositionstart", false, false),
    (Target::Textarea, "compositionupdate", false, false),
    (Target::Textarea, "compositionend", false, false),
    (Target::Textarea, "input", true, false),
    (Target::Textarea, "paste", false, false),
    (Target::Element, "paste", false, false),
    // The selection's mousedown, then MouseService's.
    (Target::Element, "mousedown", false, false),
    // MouseService's passive wheel (arrows without scrollback), then its report wheel while the program takes them.
    (Target::Element, "wheel", false, true),
    // View scrolling, which sees wheels before the element (xterm.js's scrollable element is inside it).
    (Target::Screen, "wheel", false, true),
    // The selection's copy and context menu (contextmenu off Firefox, auxclick on Linux, as the handlers check).
    (Target::Element, "copy", false, false),
    (Target::Element, "contextmenu", false, false),
    (Target::Element, "auxclick", false, false),
    // The column-select cursor, then the link under the mouse (xterm.js `Linkifier`).
    (Target::Screen, "mousemove", false, false),
    (Target::Screen, "mouseleave", false, false),
    (Target::Screen, "mousedown", false, false),
    (Target::Screen, "mouseup", false, false),
];

/// Listeners that come and go with the mouse state, as bits.
mod dynamic {
    /// Motion reports (`?1003`) and the Alt cursor of `mouseEventsRequireAlt`.
    pub const ELEMENT_MOUSEMOVE: u8 = 1;
    /// Releases after a reported press, and the selection's mouseup (Alt+click).
    pub const DOCUMENT_MOUSEUP: u8 = 1 << 1;
    /// Drags after a reported press.
    pub const DOCUMENT_MOUSEMOVE: u8 = 1 << 2;
    /// The Alt cursor of `mouseEventsRequireAlt`.
    pub const DOCUMENT_KEYDOWN: u8 = 1 << 3;
    pub const DOCUMENT_KEYUP: u8 = 1 << 4;
    pub const WINDOW_BLUR: u8 = 1 << 5;
}

const DYNAMIC: [(u8, Target, &str); 6] = [
    (dynamic::ELEMENT_MOUSEMOVE, Target::Element, "mousemove"),
    (dynamic::DOCUMENT_MOUSEUP, Target::Document, "mouseup"),
    (dynamic::DOCUMENT_MOUSEMOVE, Target::Document, "mousemove"),
    (dynamic::DOCUMENT_KEYDOWN, Target::Document, "keydown"),
    (dynamic::DOCUMENT_KEYUP, Target::Document, "keyup"),
    (dynamic::WINDOW_BLUR, Target::Window, "blur"),
];

/// The DOM input works on, from `open` to `dispose`.
struct Dom {
    textarea: HtmlTextAreaElement,
    composition_view: HtmlElement,
    element: HtmlElement,
    screen: HtmlElement,
    /// The element's document when opened, where the document listeners go.
    document: Document,
    window: Option<Window>,
    /// `Fn`, not `FnMut`: handlers dispatch events into it again (`focus()` on mousedown fires `focus` at once).
    listener: Closure<dyn Fn(Event)>,
    /// [`dynamic`] listeners that are on.
    dynamic: u8,
    /// The viewport cursor the textarea was last placed at.
    cursor: Option<(i32, u32)>,
}

impl Dom {
    fn target(&self, target: Target) -> Option<&EventTarget> {
        match target {
            Target::Textarea => Some(self.textarea.as_ref()),
            Target::Element => Some(self.element.as_ref()),
            Target::Screen => Some(self.screen.as_ref()),
            Target::Document => Some(self.document.as_ref()),
            Target::Window => self.window.as_ref().map(AsRef::as_ref),
        }
    }

    fn which(&self, target: &EventTarget) -> Option<Target> {
        [
            Target::Textarea,
            Target::Element,
            Target::Screen,
            Target::Document,
            Target::Window,
        ]
        .into_iter()
        .find(|&t| self.target(t).is_some_and(|x| x == target))
    }

    fn listen(&self, target: Target, kind: &str, capture: bool, active: bool) {
        let Some(target) = self.target(target) else {
            return;
        };
        let options = AddEventListenerOptions::new();
        options.set_capture(capture);
        if active {
            options.set_passive(false);
        }
        let _ = target.add_event_listener_with_callback_and_add_event_listener_options(
            kind,
            self.listener.as_ref().unchecked_ref(),
            &options,
        );
    }

    fn unlisten(&self, target: Target, kind: &str, capture: bool) {
        if let Some(target) = self.target(target) {
            let _ = target.remove_event_listener_with_callback_and_bool(
                kind,
                self.listener.as_ref().unchecked_ref(),
                capture,
            );
        }
    }

    /// Adds and removes [`dynamic`] listeners so that exactly `wanted` are on.
    fn set_dynamic(&mut self, wanted: u8) {
        for (bit, target, kind) in DYNAMIC {
            if wanted & bit != 0 && self.dynamic & bit == 0 {
                self.listen(target, kind, false, false);
            } else if wanted & bit == 0 && self.dynamic & bit != 0 {
                self.unlisten(target, kind, false);
            }
        }
        self.dynamic = wanted;
    }
}

/// A terminal's input state. Everything in here is borrowed only briefly, never while JS runs (crate docs).
#[derive(Default)]
pub(crate) struct Input {
    dom: RefCell<Option<Dom>>,
    custom_key_handler: RefCell<Option<Function>>,
    custom_wheel_handler: RefCell<Option<Function>>,
    keys: Cell<keyboard::Keys>,
    composition: RefCell<composition::Composition>,
    mouse: RefCell<mouse::Mouse>,
    /// `vtExtensions.kittyKeyboard` as last given to the engine.
    kitty_keyboard: Cell<bool>,
    /// The engine's mode bits and kitty keyboard flags as of the last parsed write, which the encoders use.
    modes: Cell<u32>,
    kitty_flags: Cell<u32>,
}

impl Input {
    /// `f` over the DOM while the terminal is open.
    fn dom<T>(&self, f: impl FnOnce(&Dom) -> T) -> Option<T> {
        self.dom.try_borrow().ok()?.as_ref().map(f)
    }

    fn dom_mut<T>(&self, f: impl FnOnce(&mut Dom) -> T) -> Option<T> {
        self.dom.try_borrow_mut().ok()?.as_mut().map(f)
    }

    fn textarea(&self) -> Option<HtmlTextAreaElement> {
        self.dom(|dom| dom.textarea.clone())
    }

    fn element(&self) -> Option<HtmlElement> {
        self.dom(|dom| dom.element.clone())
    }
}

/// `event` as `T`: script-made events must be instances of it; trusted ones (dispatched by the browser) are taken
/// as they come, so a terminal moved to another window (another realm's constructors) still works.
fn cast<T: JsCast>(event: &Event) -> Option<&T> {
    event
        .dyn_ref::<T>()
        .or_else(|| event.is_trusted().then(|| event.unchecked_ref()))
}

fn handle_event(shared: &Rc<Shared>, event: Event) {
    let Some(target) = event.current_target() else {
        return;
    };
    let Some(target) = shared.input.dom(|dom| dom.which(&target)).flatten() else {
        return;
    };
    match (target, event.type_().as_str()) {
        (Target::Textarea, "keydown") => {
            if let Some(e) = cast(&event) {
                keyboard::key_down(shared, e);
            }
        }
        (Target::Textarea, "keyup") => {
            if let Some(e) = cast(&event) {
                keyboard::key_up(shared, e);
            }
        }
        (Target::Textarea, "keypress") => {
            if let Some(e) = cast(&event) {
                keyboard::key_press(shared, e);
            }
        }
        (Target::Textarea, "input") => keyboard::input(shared, &event),
        (Target::Textarea, "compositionstart") => {
            sync_textarea(shared);
            composition::start(shared);
            composition::update_elements(shared, false);
        }
        (Target::Textarea, "compositionupdate") => composition::update(shared, &event),
        (Target::Textarea, "compositionend") => composition::end(shared),
        (Target::Textarea, "focus") => handle_focus(shared, true),
        (Target::Textarea, "blur") => handle_focus(shared, false),
        (Target::Textarea | Target::Element, "paste") => {
            if let Some(e) = cast::<ClipboardEvent>(&event) {
                handle_paste_event(shared, e);
            }
        }
        (Target::Element, "mousedown") => {
            if let Some(e) = cast(&event) {
                mouse::mouse_down(shared, e);
            }
        }
        (Target::Element, "wheel") => {
            if let Some(e) = cast(&event) {
                mouse::wheel(shared, e);
            }
        }
        (Target::Element, "mousemove") => {
            if let Some(e) = cast(&event) {
                mouse::element_mouse_move(shared, e);
            }
        }
        (Target::Screen, "wheel") => {
            if let Some(e) = cast(&event) {
                mouse::scroll_view(shared, e);
            }
        }
        (Target::Element, "copy") => {
            if let Some(e) = cast::<ClipboardEvent>(&event) {
                crate::selection::copy(shared, e);
            }
        }
        (Target::Element, "contextmenu") if !browser().firefox => {
            if let Some(e) = cast(&event) {
                crate::selection::right_click(shared, e);
            }
        }
        (Target::Element, "auxclick") if browser().linux => {
            if let Some(e) = cast::<web_sys::MouseEvent>(&event) {
                if e.button() == 1 {
                    crate::selection::move_textarea_under_mouse(shared, e);
                }
            }
        }
        (Target::Screen, "mousemove") => {
            crate::selection::update_cursor_style(shared, &event);
            if let Some(e) = cast(&event) {
                crate::links::mouse_move(shared, e);
            }
        }
        (Target::Screen, "mouseleave") => crate::links::mouse_leave(shared),
        (Target::Screen, "mousedown") => crate::links::mouse_down(shared),
        (Target::Screen, "mouseup") => {
            if let Some(e) = cast(&event) {
                crate::links::mouse_up(shared, e);
            }
        }
        (Target::Document, "mouseup") => {
            if let Some(e) = cast(&event) {
                mouse::document_mouse_up(shared, e);
            }
        }
        (Target::Document, "mousemove") => {
            if let Some(e) = cast(&event) {
                mouse::document_mouse_move(shared, e);
            }
        }
        (Target::Document, "keydown" | "keyup") => mouse::alt_cursor_from_modifier(shared, &event),
        (Target::Window, "blur") => mouse::alt_cursor_reset(shared),
        _ => {}
    }
}

/// Builds the textarea and the composition view in `helpers` like xterm.js `open` and binds every listener. Runs
/// under the core borrow `open` holds: DOM calls that dispatch no events, and the engine.
pub(crate) fn open(
    shared: &Rc<Shared>,
    core: &Core,
    helpers: &web_sys::Element,
) -> Result<(), JsValue> {
    let (Some(element), Some(screen)) = (core.element.clone(), core.screen.clone()) else {
        return Ok(());
    };
    let document = element
        .owner_document()
        .ok_or("wongtty/xterm: the element is not in a document")?;
    if document.get_element_by_id(STYLE_ID).is_none() {
        let style = document.create_element("style")?;
        style.set_id(STYLE_ID);
        style.set_text_content(Some(STYLE));
        if let Some(head) = document.head() {
            head.append_child(&style)?;
        }
    }
    let textarea: HtmlTextAreaElement = document.create_element("textarea")?.unchecked_into();
    textarea.class_list().add_1("xterm-helper-textarea")?;
    for (name, value) in [
        ("aria-label", "Terminal input"),
        ("aria-multiline", "false"),
        ("autocomplete", "off"),
        ("autocorrect", "off"),
        ("autocapitalize", "off"),
        ("spellcheck", "false"),
    ] {
        textarea.set_attribute(name, value)?;
    }
    textarea.set_tab_index(0);
    textarea.set_read_only(option(&core.options, "disableStdin").is_truthy());
    helpers.append_child(&textarea)?;
    let composition_view: HtmlElement = document.create_element("div")?.unchecked_into();
    composition_view.class_list().add_1("composition-view")?;
    helpers.append_child(&composition_view)?;
    let weak = Rc::downgrade(shared);
    let listener = Closure::<dyn Fn(Event)>::new(move |event: Event| {
        if let Some(shared) = weak.upgrade() {
            handle_event(&shared, event);
        }
    });
    let dom = Dom {
        textarea,
        composition_view,
        element,
        screen,
        window: document.default_view(),
        document,
        listener,
        dynamic: 0,
        cursor: None,
    };
    for (target, kind, capture, active) in STATIC {
        dom.listen(target, kind, capture, active);
    }
    *shared.input.dom.borrow_mut() = Some(dom);
    mouse::bind(shared, core);
    Ok(())
}

/// Removes every listener; the element (with the textarea) is removed by the terminal.
pub(crate) fn dispose(shared: &Shared) {
    let Some(mut dom) = shared.input.dom.borrow_mut().take() else {
        return;
    };
    dom.set_dynamic(0);
    for (target, kind, capture, _) in STATIC {
        dom.unlisten(target, kind, capture);
    }
}

/// At construction: a browser terminal always knows its focus (xterm.js reports `CSI O` before `open` too).
pub(crate) fn init(shared: &Rc<Shared>) {
    let _ = shared.core.borrow().view.engine().set_focus(false);
    apply_options(shared);
}

/// Engine settings the input options carry.
fn apply_options(shared: &Rc<Shared>) {
    let core = shared.core.borrow();
    if core.disposed {
        return;
    }
    let kitty = get(&option(&core.options, "vtExtensions"), "kittyKeyboard").is_truthy();
    if kitty != shared.input.kitty_keyboard.get() {
        shared.input.kitty_keyboard.set(kitty);
        let _ = core.view.engine().set_kitty_keyboard(kitty);
    }
}

/// After `options[key]` changed.
pub(crate) fn set_option(shared: &Rc<Shared>, key: &str) {
    match key {
        "vtExtensions" => {
            apply_options(shared);
            // Key encoding sees the dropped flags from the next frame.
            shared.schedule_frame();
        }
        "disableStdin" => {
            let disabled = option(&shared.core.borrow().options, "disableStdin").is_truthy();
            if let Some(textarea) = shared.input.textarea() {
                textarea.set_read_only(disabled);
            }
        }
        "mouseEventsRequireAlt" => mouse::require_alt_changed(shared),
        _ => {}
    }
}

/// The modes and kitty flags after a parse (or of a frame): the encoders use them from now on, and the mouse
/// listeners follow the protocol (xterm.js `onProtocolChange`).
pub(crate) fn follow_modes(shared: &Rc<Shared>, modes: u32, kitty_flags: u32) {
    shared.input.modes.set(modes);
    shared.input.kitty_flags.set(kitty_flags);
    mouse::sync_modes(shared, false);
}

/// After a frame was applied to the viewport: its modes are the newest (an engine that restarted has the defaults),
/// the textarea follows the cursor, and an open composition view is placed again (xterm.js does that on every
/// render).
pub(crate) fn after_frame(shared: &Rc<Shared>) {
    let Some((modes, kitty_flags, cursor)) = shared.core.try_borrow().ok().map(|core| {
        let v = &core.viewport;
        (v.modes, v.kitty_flags, (v.cursor.line, v.cursor.col))
    }) else {
        return;
    };
    follow_modes(shared, modes, kitty_flags);
    let cursor = Some(cursor);
    let moved = shared
        .input
        .dom_mut(|dom| std::mem::replace(&mut dom.cursor, cursor) != cursor)
        .unwrap_or(false);
    if moved {
        sync_textarea(shared);
    }
    composition::update_elements(shared, false);
}

/// After a resize (xterm.js `onResize`).
pub(crate) fn after_resize(shared: &Rc<Shared>) {
    sync_textarea(shared);
}

/// `Terminal.reset`: the mouse forgets its last report and partial wheel scrolls.
pub(crate) fn reset(shared: &Shared) {
    if let Ok(mut mouse) = shared.input.mouse.try_borrow_mut() {
        mouse.reset();
    }
}

/// xterm.js `CoreService.triggerDataEvent`: everything for the program goes through here, user input and the
/// engine's replies alike, so `disableStdin` silences both. User input scrolls a scrolled-back view to the bottom
/// (`scrollOnUserInput`) and fires `onUserInput` just before `onData`.
pub(crate) fn send_data(shared: &Rc<Shared>, data: &JsValue, user_input: bool) {
    let scroll = {
        let Ok(core) = shared.core.try_borrow() else {
            return;
        };
        if core.disposed || option(&core.options, "disableStdin").is_truthy() {
            return;
        }
        user_input
            && option(&core.options, "scrollOnUserInput").is_truthy()
            && core.view.display_offset() != 0
    };
    if scroll {
        let _ = terminal(shared).scroll_to_bottom();
    }
    if user_input {
        crate::selection::user_input(shared);
        shared.events.user_input.fire(&[]);
    }
    shared.events.data.fire(&[data]);
}

/// xterm.js `CoreService.triggerBinaryEvent`: default-encoding mouse reports.
fn send_binary(shared: &Rc<Shared>, data: &str) {
    let disabled = match shared.core.try_borrow() {
        Ok(core) => core.disposed || option(&core.options, "disableStdin").is_truthy(),
        Err(_) => true,
    };
    if !disabled {
        shared.events.binary.fire(&[&data.into()]);
    }
}

fn terminal(shared: &Rc<Shared>) -> Terminal {
    Terminal {
        shared: shared.clone(),
    }
}

fn is_disposed(shared: &Shared) -> bool {
    shared.core.try_borrow().map_or(true, |core| core.disposed)
}

/// Whether the custom key handler lets the terminal handle `event` (it returns anything but `false`).
fn custom_handler_allows(handler: &RefCell<Option<Function>>, event: &JsValue) -> bool {
    let handler = handler.borrow().clone();
    let Some(handler) = handler else {
        return true;
    };
    match handler.call1(&JsValue::UNDEFINED, event) {
        Ok(result) => result != JsValue::FALSE,
        Err(error) => {
            web_sys::console::error_1(&error);
            true
        }
    }
}

/// The key options and the modes of the last parsed write, read together.
struct KeyContext {
    options: KeyOptions,
    modes: u32,
    /// 0 unless `vtExtensions.kittyKeyboard` is on (xterm.js `useKitty`).
    kitty_flags: u8,
    screen_reader: bool,
    rows: u32,
}

fn key_context(shared: &Shared) -> Option<KeyContext> {
    let core = shared.core.try_borrow().ok()?;
    if core.disposed {
        return None;
    }
    Some(KeyContext {
        options: KeyOptions {
            platform: platform(),
            mac_option_is_meta: option(&core.options, "macOptionIsMeta").is_truthy(),
        },
        modes: shared.input.modes.get(),
        kitty_flags: if shared.input.kitty_keyboard.get() {
            shared.input.kitty_flags.get() as u8
        } else {
            0
        },
        screen_reader: option(&core.options, "screenReaderMode").is_truthy(),
        rows: core.size().1,
    })
}

/// xterm.js `_handleTextAreaFocus` and `_handleTextAreaBlur`.
fn handle_focus(shared: &Rc<Shared>, focused: bool) {
    let report = {
        let Ok(core) = shared.core.try_borrow() else {
            return;
        };
        if core.disposed {
            return;
        }
        let _ = core.view.engine().set_focus(focused);
        encode_focus(focused, shared.input.modes.get())
    };
    if !focused {
        // Text can safely be removed on blur. Doing it earlier could interfere with screen readers reading it out.
        if let Some(textarea) = shared.input.textarea() {
            textarea.set_value("");
        }
    }
    if let Some(report) = report {
        send_data(shared, &report.into(), false);
    }
    if is_disposed(shared) {
        return;
    }
    if let Some(element) = shared.input.element() {
        let classes = element.class_list();
        let _ = if focused {
            classes.add_1("focus")
        } else {
            classes.remove_1("focus")
        };
    }
    if let Ok(mut core) = shared.core.try_borrow_mut() {
        if let Some(renderer) = core.renderer.as_mut() {
            renderer.set_focused(focused);
        }
    }
}

/// xterm.js `handlePasteEvent` (`src/browser/Clipboard.ts` lines 46-52 @ 08829c7eb6279dda2a9e86df901353a1a89858ca,
/// MIT).
fn handle_paste_event(shared: &Rc<Shared>, event: &ClipboardEvent) {
    event.stop_propagation();
    if let Some(data) = event.clipboard_data() {
        let text = data.get_data("text/plain").unwrap_or_default();
        paste(shared, &text);
    }
}

/// xterm.js `paste` (`src/browser/Clipboard.ts` lines 54-59 @ 08829c7eb6279dda2a9e86df901353a1a89858ca, MIT):
/// bracketed unless `ignoreBracketedPasteMode` is `true`.
fn paste(shared: &Rc<Shared>, text: &str) {
    let modes = {
        let Ok(core) = shared.core.try_borrow() else {
            return;
        };
        let ignore = option(&core.options, "ignoreBracketedPasteMode") == JsValue::TRUE;
        shared.input.modes.get() & if ignore { !mode::BRACKETED_PASTE } else { !0 }
    };
    send_data(shared, &encode_paste(text, modes).into(), true);
    if let Some(textarea) = shared.input.textarea() {
        textarea.set_value("");
    }
}

/// The cell and canvas size, zero without a renderer.
fn dimensions(core: &Core) -> Dimensions {
    core.renderer
        .as_ref()
        .map(|r| r.dimensions())
        .unwrap_or_default()
}

/// The cursor as xterm.js `buffer` gives it: column, screen row, and whether it is in the viewport.
fn buffer_cursor(core: &Core) -> (u32, i32, bool) {
    let v = &core.viewport;
    let line = v.cursor.line;
    (
        v.cursor.col,
        line - v.display_offset as i32,
        line >= 0 && line < v.rows as i32,
    )
}

/// A number as JavaScript prints it (`n + 'px'` in xterm.js), without Rust's float formatting in the wasm.
pub(crate) fn js_number(n: f64) -> String {
    js_sys::Number::from(n)
        .to_string_with_radix(10)
        .map(String::from)
        .unwrap_or_default()
}

/// Sets `px` style properties.
fn set_px(element: &HtmlElement, properties: &[(&str, f64)]) {
    let style = element.style();
    for (name, value) in properties {
        let _ = style.set_property(name, &(js_number(*value) + "px"));
    }
}

/// xterm.js `_syncTextArea` (`src/browser/CoreBrowserTerminal.ts` lines 337-363): the textarea sits on the cursor
/// cell so the IME opens there.
fn sync_textarea(shared: &Rc<Shared>) {
    let composing = shared
        .input
        .composition
        .try_borrow()
        .map_or(true, |c| c.is_composing);
    let Some(textarea) = shared.input.textarea() else {
        return;
    };
    let Ok(core) = shared.core.try_borrow() else {
        return;
    };
    let (x, y, in_viewport) = buffer_cursor(&core);
    if !in_viewport || composing || core.renderer.is_none() {
        return;
    }
    let v = &core.viewport;
    let line = v.cursor.line as u32;
    if v.cols == 0 || line >= v.rows {
        return;
    }
    let cursor_x = x.min(v.cols - 1);
    let cell = dimensions(&core).css_cell;
    // Not before the renderer measured a cell: a zero line height ends IME compositions.
    if !(cell.width > 0.0 && cell.height > 0.0) {
        return;
    }
    let attrs = v
        .row(line)
        .get(cursor_x as usize * 4 + 3)
        .copied()
        .unwrap_or(0);
    let width = if attrs & crate::engine::attr::WIDE != 0 {
        2.0
    } else if attrs & crate::engine::attr::WIDE_SPACER != 0 {
        0.0
    } else {
        1.0
    };
    set_px(
        textarea.unchecked_ref(),
        &[
            ("left", f64::from(cursor_x) * cell.width),
            ("top", f64::from(y) * cell.height),
            ("width", cell.width * width),
            ("height", cell.height),
            ("line-height", cell.height),
        ],
    );
    let _ = textarea.style().set_property("z-index", "-5");
}

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
/** xterm.js `CustomKeyEventHandler`: return false to stop the terminal from handling the key. */
export type CustomKeyEventHandler = (event: KeyboardEvent) => boolean;
/** xterm.js `CustomWheelEventHandler`: return false to stop the terminal from handling the wheel. */
export type CustomWheelEventHandler = (event: WheelEvent) => boolean;
"#;

#[wasm_bindgen]
impl Terminal {
    /// The hidden textarea that takes keyboard and IME input, set by `open`.
    #[wasm_bindgen(getter)]
    pub fn textarea(&self) -> Option<HtmlTextAreaElement> {
        self.shared.input.textarea()
    }

    pub fn focus(&self) {
        if let Some(textarea) = self.shared.input.textarea() {
            let options = FocusOptions::new();
            options.set_prevent_scroll(true);
            let _ = textarea.focus_with_options(&options);
        }
    }

    pub fn blur(&self) {
        if let Some(textarea) = self.shared.input.textarea() {
            let _ = textarea.blur();
        }
    }

    /// Sends `data` as if pasted: newlines become CR, bracketed when the program asked for it.
    pub fn paste(&self, data: &str) {
        paste(&self.shared, data);
    }

    /// xterm.js `input` (`CoreTerminal.input`): `data` goes to `onData` as is. With `wasUserInput` (the default) it
    /// counts as typed: it scrolls to the bottom (`scrollOnUserInput`), fires `onUserInput` and clears the selection.
    /// `disableStdin` drops it either way.
    pub fn input(
        &self,
        data: JsString,
        #[wasm_bindgen(js_name = wasUserInput, unchecked_optional_param_type = "boolean")]
        was_user_input: Option<bool>,
    ) {
        send_data(&self.shared, &data.into(), was_user_input.unwrap_or(true));
    }

    /// `handler` sees every keydown, keypress and keyup first; returning false stops the terminal from handling it.
    #[wasm_bindgen(js_name = attachCustomKeyEventHandler)]
    pub fn attach_custom_key_event_handler(
        &self,
        #[wasm_bindgen(unchecked_param_type = "CustomKeyEventHandler")] handler: JsValue,
    ) {
        *self.shared.input.custom_key_handler.borrow_mut() = handler.dyn_into().ok();
    }

    /// `handler` sees every wheel event the mouse handles first; returning false keeps it from being reported or
    /// sent as arrow keys.
    #[wasm_bindgen(js_name = attachCustomWheelEventHandler)]
    pub fn attach_custom_wheel_event_handler(
        &self,
        #[wasm_bindgen(unchecked_param_type = "CustomWheelEventHandler")] handler: JsValue,
    ) {
        *self.shared.input.custom_wheel_handler.borrow_mut() = handler.dyn_into().ok();
    }

    /// wongtty API: the kitty keyboard flags the program pushed, read from the engine like `modes` so they are current
    /// right after a write (0 while it pushed none or `vtExtensions.kittyKeyboard` is off).
    #[wasm_bindgen(getter = kittyKeyboardFlags)]
    pub fn kitty_keyboard_flags(&self) -> u32 {
        match self.shared.core.try_borrow() {
            Ok(core) if !core.disposed && self.shared.input.kitty_keyboard.get() => {
                core.view.engine().kitty_flags().unwrap_or(0)
            }
            _ => 0,
        }
    }

    /// wongtty API: drops the kitty keyboard flags the program pushed, on both screens; keys are encoded the legacy
    /// way at once.
    #[wasm_bindgen(js_name = resetKittyKeyboard)]
    pub fn reset_kitty_keyboard(&self) {
        if let Ok(mut core) = self.shared.core.try_borrow_mut() {
            if !core.disposed {
                let _ = core.view.engine().reset_kitty_keyboard();
                // The next parse and frame bring the same 0 from the engine.
                core.viewport.kitty_flags = 0;
                self.shared.input.kitty_flags.set(0);
            }
        }
    }
}
