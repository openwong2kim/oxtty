//! The xterm.js-compatible `Terminal`: owns the engine view, the write queue, the frame loop and the DOM element.
//! Other modules add their members in their own `#[wasm_bindgen] impl Terminal` blocks and hook in where marked
//! "Hook:" below.
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::{Rc, Weak};

use js_sys::{Function, JsString, Object, Promise, Proxy, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlElement;

use crate::emitter::Emitter;
use crate::engine::{get, mode, Engine, EngineView, LocalEngineView, Viewport};
use crate::parser::Parsed;
use crate::render::{Dirty, Renderer};
use crate::timer;

/// Bytes or UTF-16 units fed per engine call.
const SLICE: u32 = 16 * 1024;
/// Options that only the constructor takes, like xterm.js.
const INIT_ONLY: [&str; 3] = ["cols", "rows", "scrollback"];

const STYLE_ID: &str = "wongtty-xterm-style";
/// The accessibility and scrollbar rules are xterm.js 6.1.0-beta.336 `css/xterm.css` lines 133–166 and 218–262
/// (commit 08829c7eb6279dda2a9e86df901353a1a89858ca, MIT), with the default theme's slider colours.
const STYLE: &str = "
.xterm { position: relative; cursor: text; user-select: none; -webkit-user-select: none; }
.xterm .xterm-screen { position: relative; }
.xterm .xterm-screen canvas { position: absolute; left: 0; top: 0; }
.xterm .xterm-helpers { position: absolute; top: 0; z-index: 5; }
.xterm.focus, .xterm:focus { outline: none; }
.xterm .xterm-accessibility { position: absolute; left: 0; top: 0; bottom: 0; right: 0; z-index: 10; color: transparent; pointer-events: none; }
.xterm .xterm-accessibility-tree *::selection { color: transparent; }
.xterm .xterm-accessibility-tree { font-family: monospace; user-select: text; white-space: pre; }
.xterm .xterm-accessibility-tree > div { transform-origin: left; width: fit-content; }
.xterm .live-region { position: absolute; left: -9999px; width: 1px; height: 1px; overflow: hidden; }
.xterm .xterm-scrollable-element > .xterm-scrollbar { cursor: default; }
.xterm .xterm-scrollable-element > .xterm-visible { opacity: 1; background: rgba(0,0,0,0); transition: opacity 100ms linear; z-index: 11; }
.xterm .xterm-scrollable-element > .xterm-invisible { opacity: 0; pointer-events: none; }
.xterm .xterm-scrollable-element > .xterm-invisible.xterm-fade { transition: opacity 800ms linear; }
.xterm .xterm-scrollable-element > .xterm-scrollbar > .xterm-slider { background: #ffffff33; }
.xterm .xterm-scrollable-element > .xterm-scrollbar > .xterm-slider:hover { background: #ffffff66; }
.xterm .xterm-scrollable-element > .xterm-scrollbar > .xterm-slider.xterm-active { background: #ffffff80; }
";

/// One `write()` waiting for the engine. `data` is a string or a `Uint8Array`.
struct Chunk {
    data: JsValue,
    callback: Option<Function>,
}

/// State behind one `RefCell`. Never call JS (listeners, write callbacks, host handlers) while it is borrowed:
/// collect what to call, drop the borrow, then call (crate docs, "Re-entrancy").
pub(crate) struct Core {
    pub view: LocalEngineView,
    pub viewport: Viewport,
    /// The user's options with the defaults filled in; `term.options` is a proxy over it.
    pub options: Object,
    pub element: Option<HtmlElement>,
    pub screen: Option<HtmlElement>,
    /// Hook: the renderer module creates it in `open`.
    pub renderer: Option<Box<dyn Renderer>>,
    /// While `options.screenReaderMode` is on.
    pub a11y: Option<crate::a11y::A11y>,
    /// While open (hidden with `options.scrollbar.showScrollbar: false`).
    pub scrollbar: Option<crate::scrollbar::Scrollbar>,
    pub dirty: Dirty,
    queue: VecDeque<Chunk>,
    /// A high surrogate a string `write` ended with, joined to the next one.
    pub high_surrogate: Option<u16>,
    pub disposed: bool,
    /// The size when disposed: xterm.js keeps answering `cols`/`rows` after `dispose`.
    disposed_size: (u32, u32),
}

impl Core {
    pub fn size(&self) -> (u32, u32) {
        if self.disposed {
            return self.disposed_size;
        }
        let engine = self.view.engine();
        (engine.cols(), engine.rows())
    }
}

/// Every xterm.js event the terminal fires. Members are added by the modules that fire them.
#[derive(Default)]
pub(crate) struct Emitters {
    pub data: Rc<Emitter>,
    pub binary: Rc<Emitter>,
    pub resize: Rc<Emitter>,
    pub title_change: Rc<Emitter>,
    pub bell: Rc<Emitter>,
    pub scroll: Rc<Emitter>,
    pub render: Rc<Emitter>,
    pub write_parsed: Rc<Emitter>,
    pub cursor_move: Rc<Emitter>,
    pub buffer_change: Rc<Emitter>,
    pub user_input: Rc<Emitter>,
    pub dimensions_change: Rc<Emitter>,
    pub selection_change: Rc<Emitter>,
}

/// What the engine looked like after the last change, to fire `onScroll`/`onCursorMove` on differences.
#[derive(Clone, Copy, Default, PartialEq)]
struct State {
    cursor: (i32, u32),
    viewport_y: u32,
    alternate: bool,
}

type ProxySetTrap = Closure<dyn FnMut(JsValue, JsValue, JsValue) -> bool>;
type CrashCallback = Closure<dyn FnMut(JsValue)>;

pub(crate) struct Shared {
    pub core: RefCell<Core>,
    pub events: Emitters,
    pub input: crate::input::Input,
    pub markers: RefCell<crate::markers::Markers>,
    pub selection: RefCell<crate::selection::Selection>,
    pub links: crate::links::Links,
    pub parser: crate::parser::ParserState,
    /// Hook: the image addon's state, while one is loaded.
    pub images: RefCell<Option<Rc<crate::image::State>>>,
    state: Cell<State>,
    /// Serialises OSC 52 reads so each answers the oldest pending read.
    clipboard_reads: RefCell<Promise>,
    /// Waiting for a turn of the write scheduler, or in one.
    drain_queued: Cell<bool>,
    frame: RefCell<Option<timer::AnimationFrame>>,
    options_proxy: RefCell<Option<(Proxy, ProxySetTrap)>>,
    /// `onCrash` and `onSyncFlush` the engine holds; they live as long as the terminal.
    #[allow(dead_code)]
    engine_callbacks: (CrashCallback, Closure<dyn FnMut()>),
}

#[wasm_bindgen]
pub struct Terminal {
    pub(crate) shared: Rc<Shared>,
}

#[wasm_bindgen]
impl Terminal {
    /// Throws unless `await init()` has finished.
    #[wasm_bindgen(constructor)]
    pub fn new(options: Option<Object>) -> Result<Terminal, JsValue> {
        crate::set_panic_hook();
        let options = default_options(options.unwrap_or_default());
        let engine_options = Object::new();
        for key in INIT_ONLY {
            Reflect::set(&engine_options, &key.into(), &get(&options, key))?;
        }
        // The engine calls these from inside a call this terminal made: only note it, handle it on the next task.
        let slot: Rc<RefCell<Weak<Shared>>> = Rc::default();
        let on_crash = {
            let slot = slot.clone();
            CrashCallback::new(move |error: JsValue| {
                web_sys::console::warn_1(&error);
                let weak = slot.borrow().clone();
                timer::queue(move || {
                    if let Some(shared) = weak.upgrade() {
                        shared.handle_crash();
                    }
                });
            })
        };
        let on_sync_flush = {
            let slot = slot.clone();
            Closure::<dyn FnMut()>::new(move || {
                let weak = slot.borrow().clone();
                timer::queue(move || {
                    if let Some(shared) = weak.upgrade() {
                        shared.after_change();
                    }
                });
            })
        };
        Reflect::set(&engine_options, &"onCrash".into(), on_crash.as_ref())?;
        Reflect::set(
            &engine_options,
            &"onSyncFlush".into(),
            on_sync_flush.as_ref(),
        )?;
        let engine = Engine::new(&engine_options)?;
        let shared = Rc::new(Shared {
            core: RefCell::new(Core {
                view: LocalEngineView::new(engine),
                viewport: Viewport::default(),
                options,
                element: None,
                screen: None,
                renderer: None,
                a11y: None,
                scrollbar: None,
                dirty: Dirty::All,
                queue: VecDeque::new(),
                high_surrogate: None,
                disposed: false,
                disposed_size: (0, 0),
            }),
            events: Emitters::default(),
            input: Default::default(),
            markers: RefCell::default(),
            selection: RefCell::default(),
            links: Default::default(),
            parser: Default::default(),
            images: RefCell::new(None),
            state: Cell::new(State::default()),
            clipboard_reads: RefCell::new(Promise::resolve(&JsValue::UNDEFINED)),
            drain_queued: Cell::new(false),
            frame: RefCell::new(None),
            options_proxy: RefCell::new(None),
            engine_callbacks: (on_crash, on_sync_flush),
        });
        *slot.borrow_mut() = Rc::downgrade(&shared);
        shared.apply_clipboard();
        shared.apply_windows_pty();
        shared.apply_cursor_default();
        crate::input::init(&shared);
        shared.apply_theme();
        shared.state.set(shared.read_state());
        Ok(Terminal { shared })
    }

    #[wasm_bindgen(getter)]
    pub fn cols(&self) -> u32 {
        self.shared.core.borrow().size().0
    }

    #[wasm_bindgen(getter)]
    pub fn rows(&self) -> u32 {
        self.shared.core.borrow().size().1
    }

    #[wasm_bindgen(getter)]
    pub fn element(&self) -> Option<HtmlElement> {
        self.shared.core.borrow().element.clone()
    }

    /// `term.options.x = y` and `term.options = { … }` both go through `setOption`.
    #[wasm_bindgen(getter)]
    pub fn options(&self) -> Result<Proxy, JsValue> {
        if let Some((proxy, _)) = &*self.shared.options_proxy.borrow() {
            return Ok(proxy.clone());
        }
        let weak = Rc::downgrade(&self.shared);
        let set = ProxySetTrap::new(move |_target, key: JsValue, value: JsValue| {
            // Throw only after every Rust value is dropped: a JS exception skips destructors.
            let error = match (weak.upgrade(), key.as_string()) {
                (Some(shared), Some(key)) => shared.set_option(&key, value).err(),
                _ => None,
            };
            if let Some(error) = error {
                wasm_bindgen::throw_val(error);
            }
            true
        });
        let handler = Object::new();
        Reflect::set(&handler, &"set".into(), set.as_ref())?;
        Reflect::set(&handler, &"get".into(), &crate::options::get_trap())?;
        let proxy = Proxy::new(&self.shared.core.borrow().options, &handler);
        *self.shared.options_proxy.borrow_mut() = Some((proxy.clone(), set));
        Ok(proxy)
    }

    #[wasm_bindgen(setter)]
    pub fn set_options(&self, options: Object) -> Result<(), JsValue> {
        for entry in Object::entries(&options).iter() {
            let entry: js_sys::Array = entry.unchecked_into();
            self.shared
                .set_option(&entry.get(0).as_string().unwrap_or_default(), entry.get(1))?;
        }
        Ok(())
    }

    pub fn open(&self, parent: HtmlElement) -> Result<(), JsValue> {
        let mut core = self.shared.core.borrow_mut();
        if let Some(element) = &core.element {
            // xterm.js keeps its DOM when opened again; a parked element is simply moved.
            if element.parent_element().as_deref() != Some(&parent) {
                parent.append_child(element)?;
            }
            return Ok(());
        }
        let document = parent
            .owner_document()
            .ok_or("wongtty/xterm: the parent is not in a document")?;
        if document.get_element_by_id(STYLE_ID).is_none() {
            let style = document.create_element("style")?;
            style.set_id(STYLE_ID);
            style.set_text_content(Some(STYLE));
            document
                .head()
                .ok_or("wongtty/xterm: the document has no head")?
                .append_child(&style)?;
        }
        let element: HtmlElement = document.create_element("div")?.unchecked_into();
        element.class_list().add_2("terminal", "xterm")?;
        element.set_dir("ltr");
        let screen: HtmlElement = document.create_element("div")?.unchecked_into();
        screen.class_list().add_1("xterm-screen")?;
        // Input puts the hidden textarea and the IME composition view in here.
        let helpers = document.create_element("div")?;
        helpers.class_list().add_1("xterm-helpers")?;
        screen.append_child(&helpers)?;
        element.append_child(&screen)?;
        parent.append_child(&element)?;
        core.element = Some(element);
        core.screen = Some(screen);
        // Hook: renderer, input, mouse, selection and accessibility attach here.
        crate::input::open(&self.shared, &core, &helpers)?;
        core.view.invalidate();
        core.dirty = Dirty::All;
        drop(core);
        crate::selection::open(&self.shared);
        self.shared.apply_scrollbar();
        self.shared.create_renderer()?;
        self.shared.apply_screen_reader_mode();
        crate::image::after_open(&self.shared);
        self.shared.schedule_frame();
        Ok(())
    }

    /// `callback` runs once the engine has parsed `data`; `write('', callback)` waits for earlier writes.
    pub fn write(&self, data: JsValue, callback: Option<Function>) {
        let mut core = self.shared.core.borrow_mut();
        if core.disposed {
            return;
        }
        let data = match data.dyn_into::<JsString>() {
            Ok(mut text) => {
                if let Some(high) = core.high_surrogate.take() {
                    text = JsString::from_char_code1(high.into()).concat(&text);
                }
                let len = text.length();
                if len > 0 && is_high_surrogate(text.char_code_at(len - 1) as u16) {
                    core.high_surrogate = Some(text.char_code_at(len - 1) as u16);
                    text = text.slice(0, len - 1);
                }
                text.into()
            }
            Err(data) => data,
        };
        core.queue.push_back(Chunk { data, callback });
        drop(core);
        self.shared.schedule_drain();
    }

    pub fn writeln(&self, data: JsValue, callback: Option<Function>) {
        self.write(data, None);
        self.write("\r\n".into(), callback);
    }

    pub fn resize(&self, cols: u32, rows: u32) -> Result<(), JsValue> {
        let rows_changed = {
            let mut core = self.shared.core.borrow_mut();
            let engine = core.view.engine();
            if core.disposed || (cols == engine.cols() && rows == engine.rows()) {
                return Ok(());
            }
            let old_rows = engine.rows();
            engine.resize(cols, rows)?;
            let (cols, rows) = (engine.cols(), engine.rows());
            core.view.resized();
            Reflect::set(&core.options, &"cols".into(), &cols.into())?;
            Reflect::set(&core.options, &"rows".into(), &rows.into())?;
            if let Some(renderer) = core.renderer.as_mut() {
                renderer.resize(cols, rows);
            }
            rows != old_rows
        };
        // The selection drops on row changes and the hovered link is let go, before `onResize` (xterm.js).
        crate::selection::after_resize(&self.shared, rows_changed);
        crate::links::after_resize(&self.shared);
        crate::input::after_resize(&self.shared);
        self.shared.sync_screen_size();
        let size = Object::new();
        Reflect::set(&size, &"cols".into(), &self.cols().into())?;
        Reflect::set(&size, &"rows".into(), &self.rows().into())?;
        self.shared.events.resize.fire(&[&size]);
        self.shared.after_change();
        Ok(())
    }

    /// Positive `amount` scrolls down towards the newest output.
    #[wasm_bindgen(js_name = scrollLines)]
    pub fn scroll_lines(&self, amount: i32) -> Result<(), JsValue> {
        self.shared.core.borrow().view.scroll_display(-amount)?;
        self.shared.after_change();
        Ok(())
    }

    #[wasm_bindgen(js_name = scrollToBottom)]
    pub fn scroll_to_bottom(&self) -> Result<(), JsValue> {
        self.shared.core.borrow().view.scroll_to_bottom()?;
        self.shared.after_change();
        Ok(())
    }

    /// Drops pending writes without running their callbacks, like xterm.js. Safe to call from any listener.
    pub fn dispose(&self) {
        let mut core = self.shared.core.borrow_mut();
        if core.disposed {
            return;
        }
        core.disposed_size = core.size();
        core.disposed = true;
        core.queue.clear();
        // Hook: input, mouse, selection and accessibility remove their listeners here.
        crate::input::dispose(&self.shared);
        crate::selection::dispose(&self.shared);
        crate::links::dispose(&self.shared);
        core.a11y = None;
        core.scrollbar = None;
        core.renderer = None;
        core.view.set_remote(None);
        if let Some(element) = core.element.take() {
            element.remove();
        }
        core.screen = None;
        core.view.engine().dispose();
        drop(core);
        crate::markers::drop_all(&self.shared);
        crate::parser::dispose(&self.shared);
        crate::image::dispose(&self.shared);
        self.shared.frame.borrow_mut().take();
        self.shared.options_proxy.borrow_mut().take();
        let e = &self.shared.events;
        for emitter in [
            &e.data,
            &e.binary,
            &e.resize,
            &e.title_change,
            &e.bell,
            &e.scroll,
            &e.render,
            &e.write_parsed,
            &e.cursor_move,
            &e.buffer_change,
            &e.user_input,
            &e.dimensions_change,
            &e.selection_change,
        ] {
            emitter.clear();
        }
    }
}

impl Shared {
    pub(crate) fn schedule_drain(self: &Rc<Self>) {
        if self.drain_queued.replace(true) {
            return;
        }
        timer::schedule(Rc::downgrade(self));
    }

    /// A turn of the write scheduler ([`timer::schedule`]): parses until `until`; returns whether writes are left.
    pub(crate) fn drain_turn(self: Rc<Self>, until: f64) -> bool {
        self.drain(until);
        // A paused parser queues the terminal again when it resumes; a disposed one keeps nothing queued.
        let more = !crate::parser::paused(&self) && self.writes_queued();
        self.drain_queued.set(more);
        more
    }

    /// Whether writes wait in the queue.
    pub(crate) fn writes_queued(&self) -> bool {
        !self.core.borrow().queue.is_empty()
    }

    /// Feeds queued writes until `until` (a [`timer::now`] time), at least one slice. A crashed engine drops the
    /// chunk it was fed. An async parser handler holds the queue: the chunk it stopped in stays first, its
    /// callback waits.
    fn drain(self: &Rc<Self>, until: f64) {
        if crate::parser::paused(self) {
            return;
        }
        let mut paused = false;
        loop {
            let (slice, rest) = {
                let core = self.core.borrow();
                if core.disposed {
                    return;
                }
                let Some(chunk) = core.queue.front() else {
                    break;
                };
                split_chunk(&chunk.data)
            };
            // Hook: parser handlers run in here, with no borrow held.
            let fed = crate::parser::feed(self, &slice);
            let finished = {
                let mut core = self.core.borrow_mut();
                if core.disposed {
                    return;
                }
                let Some(chunk) = core.queue.front_mut() else {
                    break;
                };
                match (fed, rest) {
                    (Ok(Parsed::Paused), rest) => {
                        chunk.data = rest.unwrap_or_else(|| JsValue::from_str(""));
                        paused = true;
                        None
                    }
                    (Ok(Parsed::Done), Some(rest)) => {
                        chunk.data = rest;
                        None
                    }
                    _ => core.queue.pop_front().map(|chunk| chunk.callback),
                }
            };
            if paused {
                break;
            }
            if let Some(callback) = finished {
                self.handle_events();
                if let Some(callback) = callback {
                    if let Err(error) = callback.call0(&JsValue::UNDEFINED) {
                        web_sys::console::error_1(&error);
                    }
                }
            }
            if timer::now() >= until {
                break;
            }
        }
        {
            let mut core = self.core.borrow_mut();
            if core.disposed {
                return;
            }
            // xterm.js refreshes the cursor row after every write (`InputHandler.parse` marks `buffer.y` dirty), so
            // `onRender` reports it even when nothing changed; the engine damages only rows that changed.
            let line = core.view.cursor().line;
            if let (Dirty::Rows(rows), Ok(line)) = (&mut core.dirty, u32::try_from(line)) {
                // Kept once: without frames (no `open`) nothing takes the rows.
                if !rows.contains(&line) {
                    rows.push(line);
                }
            }
        }
        self.handle_events();
        if !paused {
            self.events.write_parsed.fire(&[]);
        }
        self.after_change();
    }

    /// Hands what the program asked of the host to the events; runs after every parsed write, frames or not.
    pub(crate) fn handle_events(self: &Rc<Self>) {
        let (events, clipboard) = {
            let mut core = self.core.borrow_mut();
            if core.disposed {
                return;
            }
            let events = core.view.take_events();
            // Hook: viewport-only mode counts the lines that went to the host.
            core.view.sync(events.resets > 0);
            (events, get(&core.options, "clipboard"))
        };
        // Hook: input encodes with the modes this parse left, as xterm.js has them at once.
        crate::input::follow_modes(self, events.modes, events.kitty_flags);
        // One `onData` per reply, as xterm.js `triggerDataEvent`s them. Replies are not user input: `disableStdin`
        // drops them, they do not scroll the view.
        for reply in &events.replies {
            let text = String::from_utf8_lossy(reply).into_owned();
            crate::input::send_data(self, &text.into(), false);
        }
        if let Some(title) = events.title {
            self.events.title_change.fire(&[&title.into()]);
        }
        for _ in 0..events.bells {
            self.events.bell.fire(&[]);
        }
        crate::selection::handle_resets(self, events.resets);
        if events.resets > 0 {
            crate::image::full_reset(self);
        }
        let write: Option<Function> = get(&clipboard, "write").dyn_into().ok();
        for w in events.clipboard_writes {
            if let Some(write) = &write {
                if let Err(error) =
                    write.call2(&clipboard, &w.text.into(), &w.selection.to_string().into())
                {
                    web_sys::console::error_1(&error);
                }
            }
        }
        for selection in events.clipboard_reads {
            self.queue_clipboard_read(&clipboard, selection);
        }
        crate::markers::sync(self);
    }

    /// Reads are answered in order, each with the oldest pending read of its selection; a failed read answers "".
    fn queue_clipboard_read(self: &Rc<Self>, clipboard: &JsValue, selection: char) {
        let read: Option<Function> = get(clipboard, "read").dyn_into().ok();
        let clipboard = clipboard.clone();
        let weak = Rc::downgrade(self);
        let previous = self.clipboard_reads.borrow().clone();
        let next = wasm_bindgen_futures::future_to_promise(async move {
            let _ = wasm_bindgen_futures::JsFuture::from(previous).await;
            let mut text = String::new();
            if let Some(read) = read {
                if let Ok(value) = read.call1(&clipboard, &selection.to_string().into()) {
                    let value =
                        wasm_bindgen_futures::JsFuture::from(Promise::resolve(&value)).await;
                    text = value.ok().and_then(|v| v.as_string()).unwrap_or_default();
                }
            }
            if let Some(shared) = weak.upgrade() {
                let replied = {
                    let core = shared.core.borrow();
                    !core.disposed
                        && core
                            .view
                            .engine()
                            .reply_clipboard(&selection.to_string(), &text)
                            .is_ok()
                };
                if replied {
                    shared.handle_events();
                }
            }
            Ok(JsValue::UNDEFINED)
        });
        *self.clipboard_reads.borrow_mut() = next;
    }

    fn set_option(self: &Rc<Self>, key: &str, value: JsValue) -> Result<(), JsValue> {
        {
            let core = self.core.borrow();
            if INIT_ONLY.contains(&key) {
                if !js_sys::Object::is(&value, &get(&core.options, key)) {
                    return Err(JsValue::from_str(&format!(
                        "wongtty/xterm: option \"{key}\" can only be set in the constructor"
                    )));
                }
                return Ok(());
            }
            Reflect::set(&core.options, &key.into(), &value)?;
        }
        if key == "clipboard" {
            self.apply_clipboard();
        }
        if key == "windowsPty" {
            self.apply_windows_pty();
        }
        if key == "cursorStyle" || key == "cursorBlink" {
            self.apply_cursor_default();
        }
        // Hook: theme (engine colours, renderer), renderer options, input, mouse and accessibility options.
        crate::input::set_option(self, key);
        self.apply_renderer_option(key);
        match key {
            "screenReaderMode" => self.apply_screen_reader_mode(),
            "scrollbar" => self.apply_scrollbar(),
            "theme" => self.apply_scrollbar_theme(),
            _ => {}
        }
        Ok(())
    }

    fn apply_windows_pty(&self) {
        let core = self.core.borrow();
        let mut pty = get(&core.options, "windowsPty");
        if !pty.is_object() {
            pty = JsValue::NULL;
        }
        let _ = core.view.engine().set_windows_pty(&pty);
    }

    /// The engine answers DECRQSS for DECSCUSR with these options, as xterm.js does.
    fn apply_cursor_default(&self) {
        let core = self.core.borrow();
        let blink = get(&core.options, "cursorBlink").is_truthy();
        let _ = core
            .view
            .engine()
            .set_cursor_default(&get(&core.options, "cursorStyle"), blink);
    }

    fn apply_clipboard(&self) {
        let core = self.core.borrow();
        let clipboard = get(&core.options, "clipboard");
        let access = Object::new();
        let _ = Reflect::set(
            &access,
            &"read".into(),
            &get(&clipboard, "read").is_function().into(),
        );
        let _ = Reflect::set(
            &access,
            &"write".into(),
            &get(&clipboard, "write").is_function().into(),
        );
        let _ = core.view.engine().set_clipboard_access(&access);
    }

    /// Fires the state events and schedules a frame after the engine changed.
    pub(crate) fn after_change(self: &Rc<Self>) {
        match self.core.try_borrow_mut() {
            Ok(mut core) if !core.disposed => core.view.sync(false),
            Ok(_) => return,
            // Under a caller's borrow the next change syncs; the events still fire.
            Err(_) if self.core.borrow().disposed => return,
            Err(_) => {}
        }
        let (old, new) = (self.state.get(), self.read_state());
        self.state.set(new);
        crate::markers::sync(self);
        if new.alternate != old.alternate && self.events.buffer_change.has_listeners() {
            let buffer = crate::buffer::active(self);
            self.events.buffer_change.fire(&[&buffer.into()]);
        }
        if new.viewport_y != old.viewport_y {
            self.events.scroll.fire(&[&new.viewport_y.into()]);
        }
        if new.cursor != old.cursor {
            self.events.cursor_move.fire(&[]);
        }
        self.schedule_frame();
    }

    fn read_state(&self) -> State {
        let core = self.core.borrow();
        let cursor = core.view.cursor();
        let view = &core.view;
        State {
            cursor: (cursor.line, cursor.col),
            viewport_y: view.history_length() - view.display_offset(),
            alternate: view.engine().modes().unwrap_or(0) & mode::ALT_SCREEN != 0,
        }
    }

    /// After an engine crash the terminal starts over empty: redraw everything.
    fn handle_crash(self: &Rc<Self>) {
        {
            let mut core = self.core.borrow_mut();
            core.view.restarted();
            core.view.invalidate();
            core.dirty = Dirty::All;
        }
        crate::selection::handle_crash(self);
        crate::links::after_resize(self);
        crate::markers::drop_all(self);
        crate::image::engine_restarted(self);
        self.after_change();
    }

    /// Asks for a frame; touches only the frame slot, so it is safe from anywhere (renderers wake it this way).
    pub(crate) fn schedule_frame(self: &Rc<Self>) {
        if self.frame.borrow().is_some() {
            return;
        }
        let weak: Weak<Self> = Rc::downgrade(self);
        *self.frame.borrow_mut() = timer::AnimationFrame::new(move || {
            if let Some(shared) = weak.upgrade() {
                shared.frame.borrow_mut().take();
                shared.render_frame();
            }
        });
    }

    fn render_frame(self: &Rc<Self>) {
        // Writes that came since the last frame show in this one, within the scheduler's frame budget.
        let deadline = timer::frame_started(self.writes_queued());
        if let Some(until) = deadline {
            self.drain(until);
        }
        // The drain asked for the frame being drawn now.
        if deadline.is_some() {
            self.frame.borrow_mut().take();
        }
        self.handle_renderer_requests();
        self.apply_scroll_requests();
        let (rows, content, switched) = {
            let mut core = self.core.borrow_mut();
            if core.disposed || core.element.is_none() {
                return;
            }
            let Some(frame) = core.view.take_frame() else {
                // A synchronized update holds the picture; when it ends every row is drawn, as in xterm.js.
                core.dirty = Dirty::All;
                return;
            };
            let changed = core.viewport.apply(frame);
            let switched = crate::selection::follow(self, &core.viewport);
            // Without a renderer (none yet, or no WebGL2) the viewport still follows the engine: input reads it.
            if core.renderer.is_none() {
                drop(core);
                if switched {
                    crate::selection::screen_switched(self);
                }
                crate::input::after_frame(self);
                crate::remote::fetch_wanted(self);
                return;
            }
            let dirty = std::mem::replace(&mut core.dirty, Dirty::Rows(Vec::new()));
            let Core {
                renderer,
                viewport,
                a11y,
                scrollbar,
                options,
                ..
            } = &mut *core;
            let renderer = renderer.as_mut().unwrap();
            // The selection is handed over before the draw, so the rows it moved to are drawn now.
            crate::selection::paint(self, renderer.as_mut(), viewport);
            renderer.render(viewport, &changed, &dirty);
            // The rows whose content changed (ascending), not those only redrawn
            let content = changed.first().zip(changed.last()).map(|(&a, &b)| (a, b));
            let range = match dirty {
                Dirty::All => Some((0, viewport.rows.saturating_sub(1))),
                Dirty::Rows(rows) => changed.iter().chain(&rows).fold(None, |range, &y| {
                    Some(range.map_or((y, y), |(a, b): (u32, u32)| (a.min(y), b.max(y))))
                }),
            };
            let dimensions = renderer.dimensions();
            if let Some(scrollbar) = scrollbar {
                scrollbar.sync(viewport, &dimensions);
            }
            if let Some(a11y) = a11y {
                let font_size = crate::options::option(options, "fontSize").as_f64();
                a11y.frame(viewport, range, &dimensions, font_size.unwrap_or(15.0));
            }
            (range, content, switched)
        };
        if switched {
            crate::selection::screen_switched(self);
        }
        crate::input::after_frame(self);
        // Hook: viewport-only mode fetches the host rows the frame drew as placeholders.
        crate::remote::fetch_wanted(self);
        // A hovered link on rows the output changed is looked up again (xterm.js `onRenderedViewportChange`).
        if let Some((start, end)) = content {
            crate::links::after_render(self, start, end);
        }
        if let Some((start, end)) = rows {
            let range = Object::new();
            let _ = Reflect::set(&range, &"start".into(), &start.into());
            let _ = Reflect::set(&range, &"end".into(), &end.into());
            self.events.render.fire(&[&range]);
        }
    }
}

/// The first [`SLICE`] of a chunk and what is left, or `None` when it fits. Strings are cut between surrogates.
fn split_chunk(data: &JsValue) -> (JsValue, Option<JsValue>) {
    if let Some(bytes) = data.dyn_ref::<Uint8Array>() {
        if bytes.length() <= SLICE {
            return (data.clone(), None);
        }
        return (
            bytes.subarray(0, SLICE).into(),
            Some(bytes.subarray(SLICE, bytes.length()).into()),
        );
    }
    let text: &JsString = data.unchecked_ref();
    if text.length() <= SLICE {
        return (data.clone(), None);
    }
    let mut n = SLICE;
    if is_high_surrogate(text.char_code_at(n - 1) as u16) {
        n -= 1;
    }
    (
        text.slice(0, n).into(),
        Some(text.slice(n, text.length()).into()),
    )
}

fn is_high_surrogate(unit: u16) -> bool {
    (0xd800..=0xdbff).contains(&unit)
}

/// xterm.js 6 defaults of the options the terminal keeps; modules apply their own defaults to the rest.
fn default_options(options: Object) -> Object {
    let defaults: [(&str, JsValue); 3] = [
        ("cols", 80.into()),
        ("rows", 24.into()),
        ("scrollback", 1000.into()),
    ];
    let merged = Object::new();
    for (key, value) in defaults {
        let _ = Reflect::set(&merged, &key.into(), &value);
    }
    Object::assign(&merged, &options)
}
