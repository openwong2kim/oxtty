//! The built-in `FitAddon` and `Terminal.loadAddon`.
//!
//! `proposeDimensions` is ported from xterm.js `addons/addon-fit/src/FitAddon.ts` lines 22-95, as published in
//! `@xterm/addon-fit` 0.12.0-beta.332 (MIT): the terminal fills its element's parent, less the element's padding and
//! the width xterm.js keeps for its scrollbar.
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use js_sys::{Array, Function, Object, Proxy, Reflect, WeakRef};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{CssStyleDeclaration, Element};

use crate::emitter::Disposable;
use crate::engine::get;
use crate::render::Dirty;
use crate::terminal::{Shared, Terminal};

const MINIMUM_COLS: f64 = 2.0;
const MINIMUM_ROWS: f64 = 1.0;
/// xterm.js keeps this much room for its scrollbar when there is scrollback.
const DEFAULT_SCROLL_BAR_WIDTH: f64 = 14.0;

#[wasm_bindgen(typescript_custom_section)]
const TS_ADDON: &str = r#"
export interface ITerminalAddon {
  activate(terminal: Terminal): void;
  dispose(): void;
}
"#;

struct FitState {
    id: u32,
    terminal: RefCell<Weak<Shared>>,
    /// The JS object `loadAddon` got, weakly (it owns this state): a refit calls its `fit`, which a subclass may
    /// override.
    addon: RefCell<Option<WeakRef>>,
}

thread_local! {
    static ADDONS: RefCell<Vec<Weak<FitState>>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: Cell<u32> = const { Cell::new(1) };
    /// `activate`/`dispose` and `onContextLoss` of an absorbed WebGL addon. They live as long as the page; the GC owns
    /// the closures (`into_js_value`).
    static WEBGL_NOOP: JsValue = Closure::<dyn Fn()>::new(|| {}).into_js_value();
    static WEBGL_ON_CONTEXT_LOSS: JsValue =
        Closure::<dyn Fn(JsValue) -> JsValue>::new(|_listener: JsValue| Disposable::new(|| {}).into())
            .into_js_value();
}

/// Sizes the terminal to its element's parent, like `@xterm/addon-fit`.
#[wasm_bindgen]
pub struct FitAddon {
    state: Rc<FitState>,
}

impl Default for FitAddon {
    fn default() -> Self {
        Self::new()
    }
}

#[wasm_bindgen]
impl FitAddon {
    #[wasm_bindgen(constructor)]
    pub fn new() -> FitAddon {
        let state = Rc::new(FitState {
            id: NEXT_ID.with(|n| n.replace(n.get() + 1)),
            terminal: RefCell::new(Weak::new()),
            addon: RefCell::new(None),
        });
        ADDONS.with(|a| a.borrow_mut().push(Rc::downgrade(&state)));
        FitAddon { state }
    }

    /// `terminal.loadAddon(fitAddon)` calls this.
    pub fn activate(&self, terminal: &Terminal) {
        *self.state.terminal.borrow_mut() = Rc::downgrade(&terminal.shared);
    }

    pub fn dispose(&self) {
        *self.state.terminal.borrow_mut() = Weak::new();
    }

    /// Resizes the terminal to `proposeDimensions()`.
    pub fn fit(&self) -> Result<(), JsValue> {
        self.state.fit()
    }

    /// The size that fills the parent, `undefined` before `open` or while the cell size is unknown.
    #[wasm_bindgen(
        js_name = proposeDimensions,
        unchecked_return_type = "{ cols: number; rows: number } | undefined"
    )]
    pub fn propose_dimensions(&self) -> JsValue {
        let Some((cols, rows)) = self.state.propose() else {
            return JsValue::UNDEFINED;
        };
        let o = Object::new();
        let _ = Reflect::set(&o, &"cols".into(), &cols.into());
        let _ = Reflect::set(&o, &"rows".into(), &rows.into());
        o.into()
    }

    /// Internal: marks the built-in addon for `loadAddon`.
    #[wasm_bindgen(getter, js_name = __wongttyFitAddon, skip_typescript)]
    pub fn marker(&self) -> u32 {
        self.state.id
    }
}

impl Drop for FitAddon {
    fn drop(&mut self) {
        let state = Rc::downgrade(&self.state);
        ADDONS.with(|a| {
            if let Ok(mut a) = a.try_borrow_mut() {
                a.retain(|w| w.strong_count() > 0 && !Weak::ptr_eq(w, &state));
            }
        });
    }
}

impl FitState {
    fn fit(&self) -> Result<(), JsValue> {
        let Some((cols, rows)) = self.propose() else {
            return Ok(());
        };
        let Some(shared) = self.terminal.borrow().upgrade() else {
            return Ok(());
        };
        Terminal { shared }.resize(cols, rows)
    }

    fn propose(&self) -> Option<(u32, u32)> {
        let shared = self.terminal.borrow().upgrade()?;
        let (element, cell, scrollback, scrollbar) = {
            let core = shared.core.borrow();
            let element = core.element.clone()?;
            let cell = core.renderer.as_ref()?.dimensions().css_cell;
            (
                element,
                cell,
                get(&core.options, "scrollback"),
                get(&core.options, "scrollbar"),
            )
        };
        if cell.width == 0.0 || cell.height == 0.0 {
            return None;
        }
        let parent = element.parent_element()?;
        let show_scrollbar = get(&scrollbar, "showScrollbar").as_bool().unwrap_or(true);
        let scrollbar_width = if scrollback.as_f64() == Some(0.0) || !show_scrollbar {
            0.0
        } else {
            get(&scrollbar, "width")
                .as_f64()
                .unwrap_or(DEFAULT_SCROLL_BAR_WIDTH)
        };
        let window = parent.owner_document()?.default_view()?;
        let style = |e: &Element| window.get_computed_style(e).ok().flatten();
        let parent_style = style(&parent)?;
        let element_style = style(&element)?;
        let px = |s: &CssStyleDeclaration, name: &str| {
            parse_int(&s.get_property_value(name).unwrap_or_default())
        };
        let height = px(&parent_style, "height").max(0.0)
            - px(&element_style, "padding-top")
            - px(&element_style, "padding-bottom");
        let width = px(&parent_style, "width").max(0.0)
            - px(&element_style, "padding-right")
            - px(&element_style, "padding-left")
            - scrollbar_width;
        let cols = (width / cell.width).floor().max(MINIMUM_COLS);
        let rows = (height / cell.height).floor().max(MINIMUM_ROWS);
        (cols.is_finite() && rows.is_finite()).then_some((cols as u32, rows as u32))
    }
}

/// `parseInt(s, 10) || 0`.
fn parse_int(s: &str) -> f64 {
    let s = s.trim_start();
    let (sign, digits) = match s.strip_prefix('-') {
        Some(rest) => (-1.0, rest),
        None => (1.0, s.strip_prefix('+').unwrap_or(s)),
    };
    let digits: String = digits.chars().take_while(char::is_ascii_digit).collect();
    digits.parse::<f64>().map_or(0.0, |v| sign * v)
}

/// Refits every fit addon loaded into `shared` (the cell size changed by itself).
pub(crate) fn refit(shared: &Rc<Shared>) {
    let loaded: Vec<Rc<FitState>> = ADDONS.with(|a| {
        a.borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .filter(|s| {
                s.terminal
                    .borrow()
                    .upgrade()
                    .is_some_and(|t| Rc::ptr_eq(&t, shared))
            })
            .collect()
    });
    for state in loaded {
        let addon = state.addon.borrow().as_ref().and_then(|a| a.deref());
        let result = match addon.map(|a| (get(&a, "fit").dyn_into::<Function>(), a)) {
            Some((Ok(fit), addon)) => fit.call0(&addon).map(drop),
            _ => state.fit(),
        };
        if let Err(error) = result {
            web_sys::console::error_1(&error);
        }
    }
}

/// `@xterm/addon-webgl`, built in: the terminal already draws with WebGL2 (one context per page) and restores a lost
/// context by itself. Loading this addon, or `@xterm/addon-webgl` itself, only keeps code written for xterm.js working.
#[wasm_bindgen]
pub struct WebglAddon;

#[wasm_bindgen]
impl WebglAddon {
    /// `preserveDrawingBuffer` is ignored: the renderer owns its context.
    #[wasm_bindgen(constructor)]
    pub fn new(_preserve_drawing_buffer: Option<bool>) -> WebglAddon {
        WebglAddon
    }

    pub fn activate(&self, _terminal: &Terminal) {}

    pub fn dispose(&self) {}

    /// Never fires: the renderer restores or replaces a lost context by itself.
    #[wasm_bindgen(js_name = onContextLoss)]
    pub fn on_context_loss(
        &self,
        #[wasm_bindgen(unchecked_param_type = "() => void")] _listener: JsValue,
    ) -> Disposable {
        Disposable::new(|| {})
    }

    /// Redraws the terminal the addon was loaded into in full (`loadAddon` binds it).
    #[wasm_bindgen(js_name = clearTextureAtlas)]
    pub fn clear_texture_atlas(&self) {}
}

/// `terminal.unicode`: the engine measures widths with the Unicode 11 tables only.
#[wasm_bindgen]
pub struct UnicodeHandling;

#[wasm_bindgen]
impl UnicodeHandling {
    #[wasm_bindgen(getter, unchecked_return_type = "ReadonlyArray<string>")]
    pub fn versions(&self) -> Array {
        Array::of1(&"11".into())
    }

    /// Always `"11"`; setting another version throws.
    #[wasm_bindgen(getter, js_name = activeVersion)]
    pub fn active_version(&self) -> String {
        "11".into()
    }

    #[wasm_bindgen(setter, js_name = activeVersion)]
    pub fn set_active_version(
        &self,
        #[wasm_bindgen(unchecked_param_type = "string")] version: JsValue,
    ) -> Result<(), JsValue> {
        if version.as_string().as_deref() == Some("11") {
            return Ok(());
        }
        Err(js_sys::Error::new(&format!(
            "wongtty/xterm: the engine uses Unicode 11 widths only, not \"{}\"",
            version.as_string().unwrap_or_default()
        ))
        .into())
    }
}

/// An addon with `onContextLoss` and `clearTextureAtlas` (the built-in `WebglAddon` or `@xterm/addon-webgl`) is
/// never activated: its methods are replaced so that only `clearTextureAtlas` does something, a full redraw of
/// `shared`. The replacements are GC-owned closures set on the addon object.
fn absorb_webgl_addon(shared: &Rc<Shared>, addon: &JsValue) -> bool {
    if !(get(addon, "onContextLoss").is_function() && get(addon, "clearTextureAtlas").is_function())
    {
        return false;
    }
    let weak = Rc::downgrade(shared);
    let clear = Closure::<dyn Fn()>::new(move || {
        let Some(shared) = weak.upgrade() else {
            return;
        };
        {
            let mut core = shared.core.borrow_mut();
            if core.disposed {
                return;
            }
            core.dirty = Dirty::All;
        }
        shared.schedule_frame();
    });
    WEBGL_NOOP.with(|noop| {
        let _ = Reflect::set(addon, &"activate".into(), noop);
        let _ = Reflect::set(addon, &"dispose".into(), noop);
    });
    WEBGL_ON_CONTEXT_LOSS.with(|f| {
        let _ = Reflect::set(addon, &"onContextLoss".into(), f);
    });
    let _ = Reflect::set(addon, &"clearTextureAtlas".into(), &clear.into_js_value());
    true
}

/// True for an addon whose `activate` only registers a Unicode 11 width provider (`@xterm/addon-unicode11`): the
/// engine already measures with those tables. `activate` runs against a stand-in that has only `unicode`.
fn is_unicode11_addon(addon: &JsValue) -> bool {
    let Some(activate) = get(addon, "activate").dyn_into::<Function>().ok() else {
        return false;
    };
    let registered = Rc::new(Cell::new(false));
    let flag = registered.clone();
    let register = Closure::<dyn FnMut(JsValue)>::new(move |provider: JsValue| {
        flag.set(get(&provider, "version").as_string().as_deref() == Some("11"));
    });
    let unicode = Object::new();
    let _ = Reflect::set(&unicode, &"register".into(), register.as_ref());
    let trap =
        Closure::<dyn FnMut(JsValue, JsValue) -> JsValue>::new(move |_target, key: JsValue| {
            if key.as_string().as_deref() == Some("unicode") {
                unicode.clone().into()
            } else {
                JsValue::UNDEFINED
            }
        });
    let handler = Object::new();
    let _ = Reflect::set(&handler, &"get".into(), trap.as_ref());
    let probe = Proxy::new(&Object::new(), &handler);
    let ok = activate.call1(addon, &probe).is_ok();
    ok && registered.get()
}

#[wasm_bindgen]
impl Terminal {
    /// The engine measures widths with the Unicode 11 tables only.
    #[wasm_bindgen(getter)]
    pub fn unicode(&self) -> UnicodeHandling {
        UnicodeHandling
    }

    /// Accepts the built-in `FitAddon` (refitted by itself when the cell size changes) and `SearchAddon`, and
    /// absorbs `@xterm/addon-webgl` (and the built-in `WebglAddon`) and `@xterm/addon-unicode11`, whose work is
    /// built in. Any other addon throws.
    #[wasm_bindgen(js_name = loadAddon)]
    pub fn load_addon(
        &self,
        #[wasm_bindgen(unchecked_param_type = "ITerminalAddon")] addon: JsValue,
    ) -> Result<(), JsValue> {
        if let Some(id) = get(&addon, "__wongttyFitAddon").as_f64() {
            let state = ADDONS.with(|a| {
                a.borrow()
                    .iter()
                    .filter_map(Weak::upgrade)
                    .find(|s| s.id as f64 == id)
            });
            if let Some(state) = state {
                *state.terminal.borrow_mut() = Rc::downgrade(&self.shared);
                if let Ok(object) = addon.clone().dyn_into::<Object>() {
                    *state.addon.borrow_mut() = Some(WeakRef::new(&object));
                }
                // A subclass may override `activate`; it gets a handle on this terminal (another JS object).
                if let Ok(activate) = get(&addon, "activate").dyn_into::<Function>() {
                    let handle = Terminal {
                        shared: self.shared.clone(),
                    };
                    activate.call1(&addon, &handle.into())?;
                }
                return Ok(());
            }
        }
        if crate::search::load(self, &addon)
            || crate::links::load(self, &addon)
            || absorb_webgl_addon(&self.shared, &addon)
            || crate::image::load(self, &addon)
        {
            return Ok(());
        }
        if get(&addon, "findNext").is_function() {
            return Err(js_sys::Error::new(
                "wongtty/xterm: @xterm/addon-search is built in; load this package's SearchAddon instead",
            )
            .into());
        }
        if get(&addon, "serialize").is_function() {
            return Err(js_sys::Error::new(
                "wongtty/xterm: @xterm/addon-serialize is not supported; use terminal.ansiSnapshot()",
            )
            .into());
        }
        if is_unicode11_addon(&addon) {
            return Ok(());
        }
        let name = get(&get(&addon, "constructor"), "name")
            .as_string()
            .unwrap_or_default();
        Err(js_sys::Error::new(&format!(
            "wongtty/xterm: unsupported addon {name}; only the built-in FitAddon, SearchAddon, WebLinksAddon, WebglAddon and ImageAddon, @xterm/addon-webgl, @xterm/addon-image and @xterm/addon-unicode11 can be loaded"
        ))
        .into())
    }
}
