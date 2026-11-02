//! Timers through the JS globals, so the terminal core also runs in Node (tests). Only macrotasks
//! (`setTimeout`) and animation frames: the IME and input ordering borrowed from xterm.js depends on it.
//!
//! A timer owns its callback and frees it when dropped, also when cancelled before it fires; dropping it from
//! inside its own callback is fine (wasm-bindgen frees a running closure once the call returns).
use js_sys::Function;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = setTimeout)]
    fn set_timeout(callback: &Function, ms: i32) -> JsValue;
    #[wasm_bindgen(js_name = clearTimeout)]
    fn clear_timeout(id: &JsValue);
    #[wasm_bindgen(js_name = requestAnimationFrame, catch)]
    fn request_animation_frame(callback: &Function) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = cancelAnimationFrame)]
    fn cancel_animation_frame(id: &JsValue);
    #[wasm_bindgen(js_namespace = performance, js_name = now)]
    fn performance_now() -> f64;
}

/// Milliseconds, monotonic.
pub fn now() -> f64 {
    performance_now()
}

/// Runs `f` on a later task.
pub fn queue(f: impl FnOnce() + 'static) {
    set_timeout(Closure::once_into_js(f).unchecked_ref(), 0);
}

/// A `setTimeout` cleared when dropped.
pub struct Timeout {
    id: JsValue,
    _callback: Closure<dyn FnMut()>,
}

impl Timeout {
    pub fn new(ms: i32, f: impl FnOnce() + 'static) -> Self {
        let callback = once(f);
        Self {
            id: set_timeout(callback.as_ref().unchecked_ref(), ms),
            _callback: callback,
        }
    }
}

impl Drop for Timeout {
    fn drop(&mut self) {
        clear_timeout(&self.id);
    }
}

/// A `requestAnimationFrame` cancelled when dropped; `None` where there are no frames (Node).
pub struct AnimationFrame {
    id: JsValue,
    _callback: Closure<dyn FnMut()>,
}

impl AnimationFrame {
    pub fn new(f: impl FnOnce() + 'static) -> Option<Self> {
        let callback = once(f);
        let id = request_animation_frame(callback.as_ref().unchecked_ref()).ok()?;
        Some(Self {
            id,
            _callback: callback,
        })
    }
}

impl Drop for AnimationFrame {
    fn drop(&mut self) {
        cancel_animation_frame(&self.id);
    }
}

fn once(f: impl FnOnce() + 'static) -> Closure<dyn FnMut()> {
    let mut f = Some(f);
    Closure::new(move || {
        if let Some(f) = f.take() {
            f();
        }
    })
}
