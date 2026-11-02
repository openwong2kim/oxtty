//! xterm.js events as `IEvent` properties: `term.onData` is a function that subscribes, and it works unbound
//! (`const { onData } = term; onData(listener)`), like xterm.js.
//!
//! Memory: no closure per terminal or per event. One page-wide JS function subscribes; reading an event property
//! binds it to the terminal's id and the event (a bound function of two numbers that the caller drops), and the
//! terminal caches nothing. Terminals are found by id in a page-wide list of weak references, joined the first
//! time one of their events is read. So `term.onData !== term.onData`, unlike xterm.js.
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use js_sys::Function;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::emitter::{Disposable, Emitter};
use crate::terminal::{Emitters, Shared, Terminal};

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
/** xterm.js `IEvent`: call it with a listener to subscribe; `dispose()` the result to unsubscribe. */
export interface IEvent<T, U = void> {
    (listener: (arg1: T, arg2: U) => any): Disposable;
}
"#;

/// Every event a terminal fires through an `IEvent` property. Modules that add an emitter add a variant here.
#[derive(Clone, Copy)]
pub(crate) enum Event {
    Data,
    Binary,
    Resize,
    TitleChange,
    Bell,
    Scroll,
    Render,
    WriteParsed,
    CursorMove,
    BufferChange,
    UserInput,
    DimensionsChange,
    SelectionChange,
}

impl Event {
    /// In discriminant order: the bound function carries the index.
    const ALL: [Event; 13] = [
        Event::Data,
        Event::Binary,
        Event::Resize,
        Event::TitleChange,
        Event::Bell,
        Event::Scroll,
        Event::Render,
        Event::WriteParsed,
        Event::CursorMove,
        Event::BufferChange,
        Event::UserInput,
        Event::DimensionsChange,
        Event::SelectionChange,
    ];

    fn emitter(self, e: &Emitters) -> &Rc<Emitter> {
        match self {
            Event::Data => &e.data,
            Event::Binary => &e.binary,
            Event::Resize => &e.resize,
            Event::TitleChange => &e.title_change,
            Event::Bell => &e.bell,
            Event::Scroll => &e.scroll,
            Event::Render => &e.render,
            Event::WriteParsed => &e.write_parsed,
            Event::CursorMove => &e.cursor_move,
            Event::BufferChange => &e.buffer_change,
            Event::UserInput => &e.user_input,
            Event::DimensionsChange => &e.dimensions_change,
            Event::SelectionChange => &e.selection_change,
        }
    }
}

thread_local! {
    /// Lives as long as the page; the GC owns the closure (`into_js_value`), nothing is leaked per terminal.
    static SUBSCRIBE: Function = Closure::<dyn Fn(u32, u32, JsValue) -> JsValue>::new(subscribe)
        .into_js_value()
        .unchecked_into();
    /// Terminals whose events were read. A weak entry keeps its allocation, so pointers stay unique while listed;
    /// ids are never reused, so an event read from a dropped terminal never reaches a new one.
    static TERMINALS: RefCell<Vec<(u32, Weak<Shared>)>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: Cell<u32> = const { Cell::new(0) };
    /// Like `SUBSCRIBE`, for emitters outside the terminal's [`Emitters`] (an addon's, a marker's).
    static SUBSCRIBE_EMITTER: Function = Closure::<dyn Fn(u32, JsValue) -> JsValue>::new(subscribe_emitter)
        .into_js_value()
        .unchecked_into();
    /// Those emitters whose event was read, by id, with the same uniqueness rules as `TERMINALS`.
    static EMITTERS: RefCell<Vec<(u32, Weak<Emitter>)>> = const { RefCell::new(Vec::new()) };
}

/// The `IEvent` function of an emitter that is not one of the terminal's: it holds the emitter weakly, so once
/// the emitter is dropped (a disposed marker) subscribing does nothing.
pub(crate) fn emitter_event(emitter: &Rc<Emitter>) -> Function {
    let id = EMITTERS.with(|list| {
        let mut list = list.borrow_mut();
        if let Some((id, _)) = list.iter().find(|(_, w)| w.as_ptr() == Rc::as_ptr(emitter)) {
            return *id;
        }
        list.retain(|(_, w)| w.strong_count() > 0);
        let id = NEXT_ID.with(|next| next.replace(next.get().wrapping_add(1)));
        list.push((id, Rc::downgrade(emitter)));
        id
    });
    SUBSCRIBE_EMITTER.with(|subscribe| {
        subscribe
            .bind1(&JsValue::NULL, &JsValue::from(id))
            .unchecked_into()
    })
}

fn subscribe_emitter(id: u32, listener: JsValue) -> JsValue {
    let emitter = EMITTERS.with(|list| {
        list.borrow()
            .iter()
            .find(|(i, _)| *i == id)
            .and_then(|(_, w)| w.upgrade())
    });
    match (emitter, listener.dyn_into::<Function>()) {
        (Some(emitter), Ok(listener)) => emitter.event(listener).into(),
        _ => Disposable::new(|| {}).into(),
    }
}

/// The `IEvent` function of `event` on this terminal.
pub(crate) fn event(shared: &Rc<Shared>, event: Event) -> Function {
    let id = id_of(shared);
    let (id, event) = (JsValue::from(id), JsValue::from(event as u32));
    SUBSCRIBE.with(|subscribe| {
        subscribe
            .bind2(&JsValue::NULL, &id, &event)
            .unchecked_into()
    })
}

/// The page-wide id of a terminal, for page-wide callbacks bound to it (link provider replies).
pub(crate) fn terminal_id(shared: &Rc<Shared>) -> u32 {
    id_of(shared)
}

/// The terminal of an id from [`terminal_id`], while it lives.
pub(crate) fn terminal_of(id: u32) -> Option<Rc<Shared>> {
    TERMINALS.with(|list| {
        list.borrow()
            .iter()
            .find(|(i, _)| *i == id)
            .and_then(|(_, w)| w.upgrade())
    })
}

fn id_of(shared: &Rc<Shared>) -> u32 {
    TERMINALS.with(|list| {
        let mut list = list.borrow_mut();
        if let Some((id, _)) = list.iter().find(|(_, w)| w.as_ptr() == Rc::as_ptr(shared)) {
            return *id;
        }
        list.retain(|(_, w)| w.strong_count() > 0);
        let id = NEXT_ID.with(|next| next.replace(next.get().wrapping_add(1)));
        list.push((id, Rc::downgrade(shared)));
        id
    })
}

/// A listener that is not a function, or a terminal that is gone or disposed, gets a disposable that does
/// nothing (xterm.js hands out `Disposable.None` after dispose).
fn subscribe(id: u32, event: u32, listener: JsValue) -> JsValue {
    let shared = TERMINALS.with(|list| {
        list.borrow()
            .iter()
            .find(|(i, _)| *i == id)
            .and_then(|(_, w)| w.upgrade())
    });
    let (Some(shared), Some(event), Ok(listener)) = (
        shared,
        Event::ALL.get(event as usize),
        listener.dyn_into::<Function>(),
    ) else {
        return Disposable::new(|| {}).into();
    };
    if shared.core.try_borrow().map_or(true, |core| core.disposed) {
        return Disposable::new(|| {}).into();
    }
    event.emitter(&shared.events).event(listener).into()
}

#[wasm_bindgen]
impl Terminal {
    /// Input for the program (keys, paste) and replies to it (status reports).
    #[wasm_bindgen(getter = onData, unchecked_return_type = "IEvent<string>")]
    pub fn on_data(&self) -> Function {
        event(&self.shared, Event::Data)
    }

    #[wasm_bindgen(getter = onBinary, unchecked_return_type = "IEvent<string>")]
    pub fn on_binary(&self) -> Function {
        event(&self.shared, Event::Binary)
    }

    #[wasm_bindgen(
        getter = onResize,
        unchecked_return_type = "IEvent<{ cols: number, rows: number }>"
    )]
    pub fn on_resize(&self) -> Function {
        event(&self.shared, Event::Resize)
    }

    #[wasm_bindgen(getter = onTitleChange, unchecked_return_type = "IEvent<string>")]
    pub fn on_title_change(&self) -> Function {
        event(&self.shared, Event::TitleChange)
    }

    #[wasm_bindgen(getter = onBell, unchecked_return_type = "IEvent<void>")]
    pub fn on_bell(&self) -> Function {
        event(&self.shared, Event::Bell)
    }

    /// The new viewport top line (`buffer.active.viewportY`).
    #[wasm_bindgen(getter = onScroll, unchecked_return_type = "IEvent<number>")]
    pub fn on_scroll(&self) -> Function {
        event(&self.shared, Event::Scroll)
    }

    #[wasm_bindgen(
        getter = onRender,
        unchecked_return_type = "IEvent<{ start: number, end: number }>"
    )]
    pub fn on_render(&self) -> Function {
        event(&self.shared, Event::Render)
    }

    #[wasm_bindgen(getter = onWriteParsed, unchecked_return_type = "IEvent<void>")]
    pub fn on_write_parsed(&self) -> Function {
        event(&self.shared, Event::WriteParsed)
    }

    #[wasm_bindgen(getter = onCursorMove, unchecked_return_type = "IEvent<void>")]
    pub fn on_cursor_move(&self) -> Function {
        event(&self.shared, Event::CursorMove)
    }

    /// wongtty API: fires just before `onData` for keys, IME, paste and mouse reports, never for replies to the
    /// program.
    #[wasm_bindgen(getter = onUserInput, unchecked_return_type = "IEvent<void>")]
    pub fn on_user_input(&self) -> Function {
        event(&self.shared, Event::UserInput)
    }
}
