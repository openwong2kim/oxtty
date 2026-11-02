//! xterm.js `term.parser` on the engine's parser hooks: the engine stops right after a sequence a handler is
//! registered for, the handlers run newest first until one returns true, and the engine runs its default action
//! unless one did. A handler returning a Promise holds the write queue until it settles, as in xterm.js.
//!
//! Memory: a terminal without handlers stores an empty list, sets no engine hook and is fed with plain `write`;
//! only terminals with handlers feed through `writeUntilHook`. `term.parser` is a small object made on the first
//! read and kept (so `term.parser === term.parser`, as in xterm.js); it holds the terminal weakly.
//!
//! The identifier checks are ported from xterm.js `src/common/parser/EscapeSequenceParser.ts` @
//! 08829c7eb6279dda2a9e86df901353a1a89858ca (npm `@xterm/xterm` 6.1.0-beta.336), Copyright (c) 2018 The xterm.js
//! authors, MIT license (`crates/wongtty/LICENSE-XTERM`): lines 338-373 (`_identifier`), with its error messages.
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use js_sys::{Array, Function, JsString, Promise, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;

use crate::emitter::Disposable;
use crate::engine::get;
use crate::terminal::{Shared, Terminal};
use crate::timer;

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
export type IParser = Parser;

/** xterm.js `IFunctionIdentifier`: a CSI or DCS sequence by its private marker, intermediates and final byte. */
export interface IFunctionIdentifier {
    /** One byte of `<`…`?`. */
    prefix?: string;
    /** Up to two bytes of ` `…`/`. */
    intermediates?: string;
    /** One byte of `@`…`~`. */
    final: string;
}

/** Each parameter, followed by an array of its sub-parameters if it has any (an empty one is -1). */
export type IParams = (number | number[])[];
"#;

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    Csi,
    Osc,
    Dcs,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Csi => "csi",
            Kind::Osc => "osc",
            Kind::Dcs => "dcs",
        }
    }
}

/// A handler from JS, or one in Rust (the image addon's) that takes the raw payload (the hook's own copy, so a
/// large one stays out of wasm memory) and each parameter's first value (xterm.js `Params.params`), and returns
/// what a JS handler would.
#[derive(Clone)]
enum Callback {
    Js(Function),
    Rust(Rc<RustHandler>),
}

type RustHandler = dyn Fn(&Uint8Array, &[u32]) -> JsValue;

/// A registered handler of sequence `(kind, id)`; `serial` tells it apart for its disposable.
struct Handler {
    kind: Kind,
    id: u32,
    serial: u32,
    callback: Callback,
}

/// The terminal's parser state (a member of [`Shared`]).
#[derive(Default)]
pub(crate) struct ParserState {
    /// Oldest first.
    handlers: RefCell<Vec<Handler>>,
    next: Cell<u32>,
    /// An async handler holds the write queue.
    paused: Cell<bool>,
    /// The `term.parser` object, once read.
    object: RefCell<Option<JsValue>>,
}

impl ParserState {
    fn hooked(&self) -> bool {
        !self.handlers.borrow().is_empty()
    }

    fn has(&self, kind: Kind, id: u32) -> bool {
        self.handlers
            .borrow()
            .iter()
            .any(|h| h.kind == kind && h.id == id)
    }

    /// The handlers of `(kind, id)`, oldest first.
    fn handlers(&self, kind: Kind, id: u32) -> Vec<Callback> {
        self.handlers
            .borrow()
            .iter()
            .filter(|h| h.kind == kind && h.id == id)
            .map(|h| h.callback.clone())
            .collect()
    }
}

/// What feeding the engine left: all parsed, or an async handler holds the rest.
pub(crate) enum Parsed {
    Done,
    Paused,
}

/// Whether an async handler holds the write queue.
pub(crate) fn paused(shared: &Shared) -> bool {
    shared.parser.paused.get()
}

/// Feeds `slice` to the engine: with plain `write` when no handler is registered, else stopping at each hooked
/// sequence to run its handlers. Called with no borrow held; handlers may write, reset or dispose the terminal.
pub(crate) fn feed(shared: &Rc<Shared>, slice: &JsValue) -> Result<Parsed, JsValue> {
    let done = {
        let core = shared.core.borrow();
        let engine = core.view.engine();
        if !shared.parser.hooked() {
            return engine.write(slice).map(|()| Parsed::Done);
        }
        engine.write_until_hook(slice)?
    };
    if done {
        return Ok(Parsed::Done);
    }
    run_hooks(shared)
}

/// Drops the handlers: they may hold the terminal, and the engine is gone.
pub(crate) fn dispose(shared: &Shared) {
    shared.parser.handlers.borrow_mut().clear();
}

/// Runs the handlers of each hook the engine stopped at, until it has parsed what it was given.
fn run_hooks(shared: &Rc<Shared>) -> Result<Parsed, JsValue> {
    loop {
        let hook = {
            let core = shared.core.borrow();
            if core.disposed {
                return Ok(Parsed::Done);
            }
            core.view.engine().pending_hook()?
        };
        if hook.is_null() || hook.is_undefined() {
            return Ok(Parsed::Done);
        }
        let kind = match get(&hook, "kind").as_string().as_deref() {
            Some("osc") => Kind::Osc,
            Some("dcs") => Kind::Dcs,
            _ => Kind::Csi,
        };
        let id = get(&hook, "id").as_f64().unwrap_or(0.0) as u32;
        let handlers = shared.parser.handlers(kind, id);
        let mut handled = false;
        if !handlers.is_empty() {
            // The output before the sequence reaches the host first, as xterm.js handles it while parsing.
            shared.handle_events();
            match run(&handlers, kind, &hook, handlers.len()) {
                Outcome::Handled(h) => handled = h,
                Outcome::Pending(promise, rest) => {
                    pause(shared, handlers, kind, hook, promise, rest);
                    return Ok(Parsed::Paused);
                }
            }
        }
        if finish(shared, handled)? {
            return Ok(Parsed::Done);
        }
    }
}

/// Runs the pending hook's default action unless a handler took it, then parses on; false at the next hook.
fn finish(shared: &Shared, handled: bool) -> Result<bool, JsValue> {
    let core = shared.core.borrow();
    if core.disposed {
        return Ok(true);
    }
    let engine = core.view.engine();
    engine.finish_hook(!handled)?;
    engine.resume()
}

enum Outcome {
    Handled(bool),
    /// A handler returned this Promise; the first `usize` handlers have not run yet.
    Pending(Promise, usize),
}

/// Calls the first `n` handlers, newest first, each with its own arguments, until one returns true or a Promise.
/// A throwing handler counts as false; its error is thrown again on a later task.
fn run(handlers: &[Callback], kind: Kind, hook: &JsValue, n: usize) -> Outcome {
    for (i, handler) in handlers.iter().enumerate().take(n).rev() {
        let result = match handler {
            Callback::Js(f) => f.apply(&JsValue::UNDEFINED, &arguments(kind, hook)),
            Callback::Rust(f) => Ok(rust_call(f.as_ref(), hook)),
        };
        match result {
            Ok(result) if result.as_bool() == Some(true) => return Outcome::Handled(true),
            Ok(result) => {
                if let Ok(promise) = result.dyn_into::<Promise>() {
                    return Outcome::Pending(promise, i);
                }
            }
            Err(error) => throw_later(error),
        }
    }
    Outcome::Handled(false)
}

/// Holds the write queue until the handlers settle, then finishes the hook and lets the queue go on.
fn pause(
    shared: &Rc<Shared>,
    handlers: Vec<Callback>,
    kind: Kind,
    hook: JsValue,
    promise: Promise,
    n: usize,
) {
    shared.parser.paused.set(true);
    let weak: Weak<Shared> = Rc::downgrade(shared);
    wasm_bindgen_futures::spawn_local(async move {
        let handled = settle(&handlers, kind, &hook, promise, n).await;
        drop((handlers, hook));
        if let Some(shared) = weak.upgrade() {
            resume(&shared, handled);
        }
    });
}

/// Waits for `promise`; on false (or a rejection, thrown again later) the older handlers run, as in xterm.js.
async fn settle(
    handlers: &[Callback],
    kind: Kind,
    hook: &JsValue,
    mut promise: Promise,
    mut n: usize,
) -> bool {
    loop {
        match JsFuture::from(promise).await {
            Ok(result) if result.as_bool() == Some(true) => return true,
            Ok(_) => {}
            Err(error) => throw_later(error),
        }
        match run(handlers, kind, hook, n) {
            Outcome::Handled(handled) => return handled,
            Outcome::Pending(next, rest) => (promise, n) = (next, rest),
        }
    }
}

/// After an async handler settled. A crash on the way drops what the engine held, as a crashed `write` does.
fn resume(shared: &Rc<Shared>, handled: bool) {
    if shared.core.borrow().disposed {
        return;
    }
    let parsed = match finish(shared, handled) {
        Ok(false) => run_hooks(shared),
        _ => Ok(Parsed::Done),
    };
    if matches!(parsed, Ok(Parsed::Paused)) {
        return;
    }
    shared.parser.paused.set(false);
    if shared.core.borrow().disposed {
        return;
    }
    shared.handle_events();
    shared.after_change();
    if shared.writes_queued() {
        shared.schedule_drain();
    }
}

/// xterm.js handler arguments: `(params)` for CSI, `(data, params)` for DCS, `(data)` for OSC.
fn arguments(kind: Kind, hook: &JsValue) -> Array {
    let data = || {
        let bytes = get(hook, "data")
            .dyn_into::<Uint8Array>()
            .map(|b| b.to_vec())
            .unwrap_or_default();
        JsValue::from(String::from_utf8_lossy(&bytes).as_ref())
    };
    // xterm.js `Params.toArray`.
    let params = || {
        let out = Array::new();
        for param in Array::from(&get(hook, "params")).iter() {
            let param = Array::from(&param);
            if param.length() == 0 {
                continue;
            }
            out.push(&param.shift());
            if param.length() > 0 {
                out.push(&param);
            }
        }
        out
    };
    match kind {
        Kind::Csi => Array::of1(&params()),
        Kind::Dcs => Array::of2(&data(), &params()),
        Kind::Osc => Array::of1(&data()),
    }
}

/// Calls a Rust handler with the hook's payload bytes and parameters.
fn rust_call(f: &RustHandler, hook: &JsValue) -> JsValue {
    let data = get(hook, "data")
        .dyn_into::<Uint8Array>()
        .unwrap_or_else(|_| Uint8Array::new_with_length(0));
    let params: Vec<u32> = Array::from(&get(hook, "params"))
        .iter()
        .map(|p| Array::from(&p).get(0).as_f64().unwrap_or(0.0) as u32)
        .collect();
    f(&data, &params)
}

/// Throws `error` from a task of its own, so it is reported without stopping the parsing.
fn throw_later(error: JsValue) {
    timer::queue(move || wasm_bindgen::throw_val(error));
}

/// The engine's id of a CSI or DCS identifier: prefix, intermediates and final byte, one byte each from the most
/// significant (xterm.js `_identifier`, see the module docs).
fn identifier(id: &JsValue) -> Result<u32, JsValue> {
    let mut res = 0;
    for (key, most, range, too_many, out_of_range) in [
        (
            "prefix",
            1,
            0x3c..=0x3f,
            "only one byte as prefix supported",
            "prefix must be in range 0x3c .. 0x3f",
        ),
        (
            "intermediates",
            2,
            0x20..=0x2f,
            "only two bytes as intermediates are supported",
            "intermediate must be in range 0x20 .. 0x2f",
        ),
        (
            "final",
            1,
            0x40..=0x7e,
            "final must be a single byte",
            "final must be in range 64 .. 126",
        ),
    ] {
        let part = get(id, key).dyn_into::<JsString>().ok();
        let n = part.as_ref().map_or(0, |p| p.length());
        if n > most || (key == "final" && n == 0) {
            return Err(js_sys::Error::new(too_many).into());
        }
        for i in 0..n {
            let c = part.as_ref().map_or(0, |p| p.char_code_at(i) as u32);
            if !range.contains(&c) {
                return Err(js_sys::Error::new(out_of_range).into());
            }
            res = res << 8 | c;
        }
    }
    Ok(res)
}

fn set_hook(shared: &Shared, kind: Kind, id: u32, on: bool) {
    if let Ok(core) = shared.core.try_borrow() {
        if !core.disposed {
            let _ = core.view.engine().set_hook(kind.name(), id, on);
        }
    }
}

/// Adds `callback` for the sequence; the engine stops there while it has at least one handler.
fn register(weak: &Weak<Shared>, kind: Kind, id: u32, callback: Callback) -> Disposable {
    let Some(shared) = weak.upgrade() else {
        return Disposable::new(|| {});
    };
    if shared.core.try_borrow().map_or(true, |core| core.disposed) {
        return Disposable::new(|| {});
    }
    let parser = &shared.parser;
    if !parser.has(kind, id) {
        set_hook(&shared, kind, id, true);
    }
    let serial = parser.next.get();
    parser.next.set(serial.wrapping_add(1));
    parser.handlers.borrow_mut().push(Handler {
        kind,
        id,
        serial,
        callback,
    });
    let weak = weak.clone();
    Disposable::new(move || {
        if let Some(shared) = weak.upgrade() {
            shared
                .parser
                .handlers
                .borrow_mut()
                .retain(|h| h.serial != serial);
            if !shared.parser.has(kind, id) {
                set_hook(&shared, kind, id, false);
            }
        }
    })
}

/// Adds a Rust handler (see [`Callback`]); it should hold the terminal weakly.
pub(crate) fn register_rust(
    shared: &Rc<Shared>,
    kind: Kind,
    id: u32,
    f: impl Fn(&Uint8Array, &[u32]) -> JsValue + 'static,
) -> Disposable {
    register(&Rc::downgrade(shared), kind, id, Callback::Rust(Rc::new(f)))
}

/// xterm.js `IParser`: CSI, DCS and OSC handlers. `true` from a handler skips the older handlers and the default
/// action; `false` falls through to them; a `Promise<boolean>` holds the write queue until it settles. `write()`
/// from inside a handler is parsed after the data being parsed. A throwing or rejecting handler counts as false
/// and its error is thrown again on a later task.
#[wasm_bindgen]
pub struct Parser {
    shared: Weak<Shared>,
}

#[wasm_bindgen]
impl Parser {
    #[wasm_bindgen(js_name = registerCsiHandler)]
    pub fn register_csi_handler(
        &self,
        #[wasm_bindgen(unchecked_param_type = "IFunctionIdentifier")] id: JsValue,
        #[wasm_bindgen(unchecked_param_type = "(params: IParams) => boolean | Promise<boolean>")]
        callback: Function,
    ) -> Result<Disposable, JsValue> {
        Ok(register(
            &self.shared,
            Kind::Csi,
            identifier(&id)?,
            Callback::Js(callback),
        ))
    }

    #[wasm_bindgen(js_name = registerDcsHandler)]
    pub fn register_dcs_handler(
        &self,
        #[wasm_bindgen(unchecked_param_type = "IFunctionIdentifier")] id: JsValue,
        #[wasm_bindgen(
            unchecked_param_type = "(data: string, param: IParams) => boolean | Promise<boolean>"
        )]
        callback: Function,
    ) -> Result<Disposable, JsValue> {
        Ok(register(
            &self.shared,
            Kind::Dcs,
            identifier(&id)?,
            Callback::Js(callback),
        ))
    }

    /// `ident` is the number before the first `;`; the handler gets the rest.
    #[wasm_bindgen(js_name = registerOscHandler)]
    pub fn register_osc_handler(
        &self,
        ident: f64,
        #[wasm_bindgen(unchecked_param_type = "(data: string) => boolean | Promise<boolean>")]
        callback: Function,
    ) -> Result<Disposable, JsValue> {
        if !(0.0..=u32::MAX as f64).contains(&ident) || ident.fract() != 0.0 {
            return Err(js_sys::Error::new("invalid OSC identifier").into());
        }
        Ok(register(
            &self.shared,
            Kind::Osc,
            ident as u32,
            Callback::Js(callback),
        ))
    }
}

#[wasm_bindgen]
impl Terminal {
    #[wasm_bindgen(getter, unchecked_return_type = "IParser")]
    pub fn parser(&self) -> JsValue {
        if let Some(object) = self.shared.parser.object.borrow().as_ref() {
            return object.clone();
        }
        // Made with no borrow held: the wrapper is made in JS.
        let object = JsValue::from(Parser {
            shared: Rc::downgrade(&self.shared),
        });
        *self.shared.parser.object.borrow_mut() = Some(object.clone());
        object
    }
}
