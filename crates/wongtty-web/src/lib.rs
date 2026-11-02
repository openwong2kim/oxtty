//! wongtty's browser front end: an xterm.js-compatible `Terminal` in Rust, compiled to wasm with wasm-bindgen.
//!
//! The engine is not linked in: it is the `wongtty` package's own WebAssembly instance, reached through its JS
//! binding (see [`engine`]), so an engine crash rebuilds one terminal and the page keeps running, and the
//! engine wasm stays small for headless users. `wongtty/xterm`'s `init()` loads both modules.
//!
//! # Rules every module follows
//!
//! - **Boundary (PRD D7).** Rendering, input, selection and accessibility read [`engine::Events`],
//!   [`engine::Frame`] and [`engine::Viewport`] only. The buffer API, search, serialize and the public getters
//!   that report engine state (`modes`, `kittyKeyboardFlags`) may call [`engine::Engine`]; nothing else does.
//! - **Re-entrancy.** Never call JS (event listeners, write callbacks, host handlers) while a `RefCell` borrow is
//!   held: collect, drop the borrow, then call. A listener may `write`, `resize` or `dispose` the terminal.
//!   Exported methods take `&self`. Renderers and DOM listeners never call back into the terminal; they queue
//!   requests and wake it with `schedule_frame`, which touches nothing else.
//! - **Panics.** The front end is built with `panic = "abort"`: a panic stops every terminal on the page, so it
//!   must not panic on any input (indexing is checked, JS values are parsed defensively). Engine calls are
//!   `catch`: a crash there is a JS error the caller handles.
//! - **Closures.** No `Closure::forget`. The terminal owns every DOM listener closure and removes it in
//!   `dispose`; one-shot timers use [`timer`]. A JS exception must not unwind through Rust frames that own
//!   values (throw only after dropping them).
//! - **Timers.** Only `setTimeout` and `requestAnimationFrame` ([`timer`]), never microtasks for input
//!   ordering; listener registration order and capture/passive flags follow the xterm.js original.
//! - **Types.** The generated `.d.ts` is the public API: give JS-facing parameters and returns their xterm.js
//!   types (`unchecked_param_type`, `unchecked_return_type`, `typescript_custom_section`), per
//!   `packages/wongtty/test/web/api-checklist.json`.
//! - **Borrowed xterm.js logic** (MIT) names its source file, commit and line range in a header comment, and is
//!   credited in `NOTICE.md` and `packages/wongtty/THIRD_PARTY_NOTICES.md`.
pub mod a11y;
pub mod buffer;
pub mod emitter;
pub mod engine;
pub mod events;
pub mod fit;
pub mod image;
mod input;
pub mod links;
pub mod markers;
pub mod options;
pub mod parser;
pub mod remote;
pub mod render;
pub mod scrollbar;
pub mod search;
mod selection;
pub mod state;
pub mod terminal;
pub mod timer;

pub use terminal::Terminal;

/// Logs panics to the console before the abort, once per page.
pub(crate) fn set_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        std::panic::set_hook(Box::new(|info| {
            web_sys::console::error_1(&info.to_string().into())
        }))
    });
}
