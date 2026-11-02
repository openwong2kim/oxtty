//! xterm.js events (`term.onData(listener)` → `IDisposable`).
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use js_sys::Function;
use wasm_bindgen::prelude::*;

/// Listeners of one event. Firing calls a copy of the list, so a listener may add or dispose listeners, write,
/// resize or dispose the terminal (the caller holds no borrow while firing, see the crate docs).
#[derive(Default)]
pub struct Emitter {
    listeners: RefCell<Vec<(u32, Function)>>,
    next_id: Cell<u32>,
}

impl Emitter {
    pub fn event(self: &Rc<Self>, listener: Function) -> Disposable {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        self.listeners.borrow_mut().push((id, listener));
        let emitter: Weak<Self> = Rc::downgrade(self);
        Disposable::new(move || {
            if let Some(emitter) = emitter.upgrade() {
                emitter.listeners.borrow_mut().retain(|(i, _)| *i != id);
            }
        })
    }

    pub fn has_listeners(&self) -> bool {
        !self.listeners.borrow().is_empty()
    }

    /// A throwing listener is reported and the rest still run, like the xterm.js emitter.
    pub fn fire(&self, args: &[&JsValue]) {
        let listeners: Vec<Function> = self
            .listeners
            .borrow()
            .iter()
            .map(|(_, f)| f.clone())
            .collect();
        for listener in listeners {
            let result = match args {
                [] => listener.call0(&JsValue::UNDEFINED),
                [a] => listener.call1(&JsValue::UNDEFINED, a),
                [a, b, ..] => listener.call2(&JsValue::UNDEFINED, a, b),
            };
            if let Err(error) = result {
                web_sys::console::error_1(&error);
            }
        }
    }

    pub fn clear(&self) {
        self.listeners.borrow_mut().clear();
    }
}

/// xterm.js `IDisposable`. Dropping it on the Rust side does nothing; `dispose()` runs the teardown once.
#[wasm_bindgen]
pub struct Disposable {
    teardown: RefCell<Option<Box<dyn FnOnce()>>>,
}

impl Disposable {
    pub fn new(teardown: impl FnOnce() + 'static) -> Self {
        Self {
            teardown: RefCell::new(Some(Box::new(teardown))),
        }
    }
}

#[wasm_bindgen]
impl Disposable {
    pub fn dispose(&self) {
        let teardown = self.teardown.borrow_mut().take();
        if let Some(teardown) = teardown {
            teardown();
        }
    }
}
