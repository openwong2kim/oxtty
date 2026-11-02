//! Inline images as `@xterm/addon-image` draws them: sixel (`DCS Ps;Ps;Ps q … ST`) and iTerm2's inline image
//! protocol (`OSC 1337 ; File=… : base64 ST`).
//!
//! `loadAddon` takes this package's `ImageAddon` or absorbs an `@xterm/addon-image` instance (its options are read,
//! it is never activated, like `WebglAddon`). Sixel is decoded here (`wongtty::graphics::sixel`), PNG, JPEG, GIF, WebP
//! and AVIF by the browser (`createImageBitmap`), so no image codec is in the wasm. The engine keeps which tile of
//! which image covers each cell ([`crate::engine::CellExtra::image`]), so tiles scroll, reflow and leave with their
//! lines and text over a cell removes its tile; [`store`] keeps the pixels within the storage limit, and the
//! renderer draws the visible tiles from GPU textures it creates only while they are on screen. The sequences
//! reach it as Rust handlers on [`crate::parser`], so they keep their order among the host's handlers. A terminal
//! that never loaded the addon has no state here and no handlers, so its parser keeps its path without hooks.
//!
//! The handlers, the storage options, the DA1 and XTSMGRAPHICS answers and the cursor rules follow
//! `@xterm/addon-image` 0.10.0-beta.331 (xterm.js commit 08829c7eb6279dda2a9e86df901353a1a89858ca, MIT):
//! `addons/addon-image/src/ImageAddon.ts` lines 61-378, `SixelHandler.ts` lines 1-151, `IIPHandler.ts` lines 1-205,
//! `SixelImageStorage.ts` and `IIPImageStorage.ts`. Kitty graphics, which the addon also has, are not supported.
mod store;

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use js_sys::{Array, Function, Map, Object, Promise, Reflect, Uint32Array, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wongtty::graphics::iip::{self, Base64, HeaderParser, Kind, Parsed};
use wongtty::graphics::sixel::{self, Decoder, Sink};

use crate::emitter::{Disposable, Emitter};
use crate::engine::{get, mode, Engine};
use crate::parser;
use crate::render::color::ColorSet;
use crate::terminal::{Shared, Terminal};

pub(crate) use store::{with_image, Pixels};

/// The cell size the addon assumes before the renderer has measured one (`CELL_SIZE_DEFAULT`).
const CELL_SIZE_DEFAULT: (f64, f64) = (7.0, 14.0);
/// Bytes copied out of a payload at a time.
const PIECE: u32 = 64 * 1024;

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
/** `@xterm/addon-image` options; `kittySupport` and `kittySizeLimit` are read but kitty graphics are not supported. */
export interface IImageAddonOptions {
  enableSizeReports?: boolean;
  pixelLimit?: number;
  storageLimit?: number;
  showPlaceholder?: boolean;
  sixelSupport?: boolean;
  sixelScrolling?: boolean;
  sixelPaletteLimit?: number;
  sixelSizeLimit?: number;
  iipSupport?: boolean;
  iipSizeLimit?: number;
  kittySupport?: boolean;
  kittySizeLimit?: number;
}
"#;

/// `IImageAddonOptions`; `Default` is the addon's `DEFAULT_OPTIONS`.
#[derive(Clone, Debug, PartialEq)]
struct Options {
    enable_size_reports: bool,
    pixel_limit: f64,
    sixel_support: bool,
    sixel_scrolling: bool,
    sixel_palette_limit: f64,
    sixel_size_limit: f64,
    storage_limit: f64,
    show_placeholder: bool,
    iip_support: bool,
    iip_size_limit: f64,
    kitty_support: bool,
    kitty_size_limit: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            enable_size_reports: true,
            pixel_limit: 16_777_216.0,
            sixel_support: true,
            sixel_scrolling: true,
            sixel_palette_limit: 4096.0,
            sixel_size_limit: 33_554_432.0,
            storage_limit: 128.0,
            show_placeholder: true,
            iip_support: true,
            iip_size_limit: 33_554_432.0,
            kitty_support: true,
            kitty_size_limit: 33_554_432.0,
        }
    }
}

impl Options {
    /// `Object.assign({}, base, value)` for the keys the addon has: a key the object sets wins, whatever its type.
    fn read(value: &JsValue, base: &Options) -> Options {
        let mut o = base.clone();
        if !value.is_object() {
            return o;
        }
        let has = |key: &str| Reflect::has(value, &key.into()).unwrap_or(false);
        let flag = |key: &str, slot: &mut bool| {
            if has(key) {
                *slot = get(value, key).is_truthy();
            }
        };
        let number = |key: &str, slot: &mut f64| {
            if has(key) {
                *slot = get(value, key).as_f64().unwrap_or(f64::NAN);
            }
        };
        flag("enableSizeReports", &mut o.enable_size_reports);
        number("pixelLimit", &mut o.pixel_limit);
        flag("sixelSupport", &mut o.sixel_support);
        flag("sixelScrolling", &mut o.sixel_scrolling);
        number("sixelPaletteLimit", &mut o.sixel_palette_limit);
        number("sixelSizeLimit", &mut o.sixel_size_limit);
        number("storageLimit", &mut o.storage_limit);
        flag("showPlaceholder", &mut o.show_placeholder);
        flag("iipSupport", &mut o.iip_support);
        number("iipSizeLimit", &mut o.iip_size_limit);
        flag("kittySupport", &mut o.kitty_support);
        number("kittySizeLimit", &mut o.kitty_size_limit);
        o
    }

    fn pixel_limit(&self) -> usize {
        if self.pixel_limit.is_finite() && self.pixel_limit > 0.0 {
            self.pixel_limit as usize
        } else {
            0
        }
    }
}

/// `storageLimit` in MB to the pixels it holds (`setLimit`), or `None` when out of range.
fn storage_pixels(mb: f64) -> Option<usize> {
    (0.5..=1000.0)
        .contains(&mb)
        .then(|| (mb / 4.0 * 1_000_000.0) as u32 as usize)
}

/// One addon object: the built-in `ImageAddon` or an absorbed `@xterm/addon-image`.
struct AddonCore {
    id: u32,
    opts: RefCell<Options>,
    defaults: Options,
    /// `_handlers`: `'sixel'` and `'iip'` stand-ins wmux reaches into (see [`Handlers`]).
    handlers: Map,
    terminal: RefCell<Weak<Shared>>,
    on_image_added: Rc<Emitter>,
}

thread_local! {
    static ADDONS: RefCell<Vec<Weak<AddonCore>>> = const { RefCell::new(Vec::new()) };
    static NEXT_ADDON: Cell<u32> = const { Cell::new(1) };
    /// `release()` of the decoder stand-in. Lives as long as the page; the GC owns the closure.
    static NOOP: JsValue = Closure::<dyn Fn()>::new(|| {}).into_js_value();
}

impl AddonCore {
    fn new(opts: Options, defaults: Options, handlers: Map) -> Rc<Self> {
        let core = Rc::new(Self {
            id: NEXT_ADDON.with(|n| n.replace(n.get() + 1)),
            opts: RefCell::new(opts),
            defaults,
            handlers,
            terminal: RefCell::new(Weak::new()),
            on_image_added: Rc::default(),
        });
        ADDONS.with(|a| {
            let mut a = a.borrow_mut();
            a.retain(|w| w.strong_count() > 0);
            a.push(Rc::downgrade(&core));
        });
        core
    }

    /// The terminal this addon is loaded into, and its image state.
    fn state(&self) -> Option<(Rc<Shared>, Rc<State>)> {
        let shared = self.terminal.borrow().upgrade()?;
        let state = shared.images.borrow().clone()?;
        std::ptr::eq(Rc::as_ptr(&state.addon), self).then_some((shared, state))
    }

    fn storage_limit(&self) -> f64 {
        match self.state() {
            Some((_, state)) => state.limit.get() as f64 * 4.0 / 1_000_000.0,
            None => -1.0,
        }
    }

    fn set_storage_limit(&self, mb: f64) -> Result<(), JsValue> {
        if let Some((_, state)) = self.state() {
            let pixels = storage_pixels(mb).ok_or_else(|| {
                JsValue::from(js_sys::RangeError::new(
                    "invalid storageLimit, should be at least 0.5 MB and not exceed 1G",
                ))
            })?;
            state.limit.set(pixels);
            store::set_limit(state.key, pixels);
        }
        self.opts.borrow_mut().storage_limit = mb;
        Ok(())
    }

    fn storage_usage(&self) -> f64 {
        match self.state() {
            Some((_, state)) => store::usage(state.key) as f64 * 4.0 / 1_000_000.0,
            None => -1.0,
        }
    }

    fn set_show_placeholder(&self, show: bool) {
        self.opts.borrow_mut().show_placeholder = show;
        if let Some((shared, _)) = self.state() {
            apply_placeholder(&shared);
        }
    }

    fn dispose(&self) {
        if let Some((shared, _)) = self.state() {
            unload(&shared);
        }
        *self.terminal.borrow_mut() = Weak::new();
        self.handlers.clear();
        self.on_image_added.clear();
    }
}

/// The `'sixel'` and `'iip'` entries of `_handlers`. wmux's sixel cap wraps `handlers.get('sixel').unhook` and reads
/// `_dec.width`/`_dec.height` and `_aborted` in it, so a decoded sixel goes through `unhook` on that object: the
/// cap may drop it (sets `_aborted`, returns without calling the original), otherwise the original places it.
struct Handlers {
    sixel: Object,
    iip: Object,
    /// The original `unhook`; dropped with the terminal's image state.
    _unhook: Closure<dyn FnMut(JsValue) -> JsValue>,
}

impl Handlers {
    fn new(shared: &Rc<Shared>) -> Self {
        let weak = Rc::downgrade(shared);
        let unhook = Closure::<dyn FnMut(JsValue) -> JsValue>::new(move |success: JsValue| {
            if let Some(shared) = weak.upgrade() {
                place_sixel(&shared, success.is_truthy());
            }
            JsValue::TRUE
        });
        let sixel = Object::new();
        let _ = Reflect::set(&sixel, &"unhook".into(), unhook.as_ref());
        let _ = Reflect::set(&sixel, &"_aborted".into(), &JsValue::FALSE);
        NOOP.with(|noop| {
            let _ = Reflect::set(&sixel, &"reset".into(), noop);
        });
        let iip = Object::new();
        NOOP.with(|noop| {
            let _ = Reflect::set(&iip, &"reset".into(), noop);
        });
        Self {
            sixel,
            iip,
            _unhook: unhook,
        }
    }

    fn register(&self, map: &Map, opts: &Options) {
        map.clear();
        if opts.sixel_support {
            map.set(&"sixel".into(), &self.sixel);
        }
        if opts.iip_support {
            map.set(&"iip".into(), &self.iip);
        }
    }
}

/// A decoded sixel waiting for `unhook`.
struct Decoded {
    width: usize,
    height: usize,
    pixels: Option<Uint32Array>,
}

/// The iTerm2 handler's state between sequences (multipart files span several).
#[derive(Default)]
struct Iip {
    header: iip::Header,
    multipart: bool,
    abort_multi: bool,
    decoder: Option<Payload>,
}

/// A base64 payload decoded into JS memory, `PIECE` bytes per array.
#[derive(Default)]
struct Payload {
    base64: Base64,
    pieces: Vec<Uint8Array>,
    len: usize,
    buffer: Vec<u8>,
}

impl Payload {
    fn put(&mut self, data: &[u8], limit: usize) -> bool {
        if self.base64.total() + data.len() > limit {
            return false;
        }
        let ok = self.base64.put(data, &mut self.buffer);
        self.flush(false);
        ok
    }

    fn end(&mut self) -> bool {
        let ok = self.base64.end(&mut self.buffer);
        self.flush(true);
        ok
    }

    fn flush(&mut self, all: bool) {
        while self.buffer.len() >= PIECE as usize || (all && !self.buffer.is_empty()) {
            let n = self.buffer.len().min(PIECE as usize);
            self.pieces.push(Uint8Array::from(&self.buffer[..n]));
            self.buffer.drain(..n);
            self.len += n;
        }
    }

    fn at(&self, i: usize) -> u8 {
        let piece = PIECE as usize;
        self.pieces
            .get(i / piece)
            .map_or(0, |p| p.get_index((i % piece) as u32))
    }
}

/// The image state of one terminal (`Shared::images`).
pub(crate) struct State {
    key: u32,
    addon: Rc<AddonCore>,
    /// Storage limit in pixels.
    limit: Cell<usize>,
    handlers: Handlers,
    sixel: RefCell<Option<Box<Decoder>>>,
    decoded: RefCell<Option<Decoded>>,
    iip: RefCell<Iip>,
    /// The addon's parser handlers, removed with the state.
    parser: Vec<Disposable>,
}

impl Drop for State {
    fn drop(&mut self) {
        for handler in &self.parser {
            handler.dispose();
        }
        store::leave(self.key);
    }
}

/// Built-in image addon: `new ImageAddon(options)` takes `@xterm/addon-image`'s options.
#[wasm_bindgen]
pub struct ImageAddon {
    core: Rc<AddonCore>,
    ops: Closure<dyn Fn(u32, JsValue) -> JsValue>,
}

#[wasm_bindgen]
impl ImageAddon {
    #[wasm_bindgen(constructor)]
    pub fn new(
        #[wasm_bindgen(unchecked_param_type = "IImageAddonOptions")] options: Option<Object>,
    ) -> ImageAddon {
        let opts = Options::read(
            &options.map(JsValue::from).unwrap_or_default(),
            &Options::default(),
        );
        let core = AddonCore::new(opts.clone(), opts, Map::new());
        let ops = dispatcher(&core);
        ImageAddon { core, ops }
    }

    /// `terminal.loadAddon(addon)` calls this.
    pub fn activate(&self, terminal: &Terminal) {
        load_core(&terminal.shared, self.core.clone());
    }

    /// Removes the handlers and every image.
    pub fn dispose(&self) {
        self.core.dispose();
    }

    /// Resets the options sequences change and drops every image (RIS, DECSTR).
    pub fn reset(&self) -> bool {
        if let Some((shared, _)) = self.core.state() {
            reset(&shared);
        }
        false
    }

    /// Storage limit in MB; -1 before `loadAddon`.
    #[wasm_bindgen(getter = storageLimit)]
    pub fn storage_limit(&self) -> f64 {
        self.core.storage_limit()
    }

    #[wasm_bindgen(setter = storageLimit)]
    pub fn set_storage_limit(&self, limit: f64) -> Result<(), JsValue> {
        self.core.set_storage_limit(limit)
    }

    /// MB the images hold; -1 before `loadAddon`.
    #[wasm_bindgen(getter = storageUsage)]
    pub fn storage_usage(&self) -> f64 {
        self.core.storage_usage()
    }

    #[wasm_bindgen(getter = showPlaceholder)]
    pub fn show_placeholder(&self) -> bool {
        self.core.opts.borrow().show_placeholder
    }

    #[wasm_bindgen(setter = showPlaceholder)]
    pub fn set_show_placeholder(&self, value: bool) {
        self.core.set_show_placeholder(value);
    }

    /// Fires after an image is placed.
    #[wasm_bindgen(getter = onImageAdded, unchecked_return_type = "IEvent<void>")]
    pub fn on_image_added(&self) -> Function {
        let ops: &Function = self.ops.as_ref().unchecked_ref();
        JsValue::from(ops.bind1(&JsValue::UNDEFINED, &SUBSCRIBE.into())).unchecked_into()
    }

    /// The protocol handlers by name (`'sixel'`, `'iip'`), as `@xterm/addon-image` keeps them.
    #[wasm_bindgen(getter = _handlers, unchecked_return_type = "Map<string, unknown>")]
    pub fn handlers(&self) -> Map {
        self.core.handlers.clone()
    }

    /// Internal: marks the built-in addon for `loadAddon`.
    #[wasm_bindgen(getter, js_name = __wongttyImageAddon, skip_typescript)]
    pub fn marker(&self) -> u32 {
        self.core.id
    }
}

/// `loadAddon` with the built-in `ImageAddon` (by its marker) or an `@xterm/addon-image` instance; false for any
/// other addon.
pub(crate) fn load(terminal: &Terminal, addon: &JsValue) -> bool {
    if let Some(id) = get(addon, "__wongttyImageAddon").as_f64() {
        let core = ADDONS.with(|a| {
            a.borrow()
                .iter()
                .filter_map(Weak::upgrade)
                .find(|c| c.id as f64 == id)
        });
        if let Some(core) = core {
            // A subclass may override `activate`
            if let Ok(activate) = get(addon, "activate").dyn_into::<Function>() {
                let handle = Terminal {
                    shared: terminal.shared.clone(),
                };
                if let Err(error) = activate.call1(addon, &handle.into()) {
                    web_sys::console::error_1(&error);
                }
            } else {
                load_core(&terminal.shared, core);
            }
            return true;
        }
    }
    let handlers = get(addon, "_handlers");
    let is_image_addon = get(addon, "getImageAtBufferCell").is_function()
        && get(addon, "_opts").is_object()
        && handlers.is_instance_of::<Map>();
    if !is_image_addon {
        return false;
    }
    let defaults = Options::read(&get(addon, "_defaultOpts"), &Options::default());
    let opts = Options::read(&get(addon, "_opts"), &defaults);
    let core = AddonCore::new(opts, defaults, handlers.unchecked_into());
    absorb(addon, &core);
    load_core(&terminal.shared, core);
    true
}

/// Puts the absorbed addon's API onto this terminal's state: its methods and accessors become one GC-owned
/// function bound to each operation, which keeps the core alive.
fn absorb(addon: &JsValue, core: &Rc<AddonCore>) {
    let op: Function = dispatcher(core).into_js_value().unchecked_into();
    let bound = |n: u32| op.bind1(&JsValue::UNDEFINED, &n.into());
    for (name, n) in [
        ("dispose", DISPOSE),
        ("reset", RESET),
        ("onImageAdded", SUBSCRIBE),
    ] {
        let _ = Reflect::set(addon, &name.into(), &bound(n));
    }
    NOOP.with(|noop| {
        let _ = Reflect::set(addon, &"activate".into(), noop);
    });
    let Ok(target) = addon.clone().dyn_into::<Object>() else {
        return;
    };
    for (name, get, set) in [
        ("storageLimit", STORAGE_LIMIT, SET_STORAGE_LIMIT),
        ("storageUsage", STORAGE_USAGE, NONE),
        ("showPlaceholder", SHOW_PLACEHOLDER, SET_SHOW_PLACEHOLDER),
    ] {
        let descriptor = Object::new();
        let _ = Reflect::set(&descriptor, &"get".into(), &bound(get));
        let _ = Reflect::set(&descriptor, &"set".into(), &bound(set));
        let _ = Reflect::set(&descriptor, &"configurable".into(), &JsValue::TRUE);
        let _ = Reflect::define_property(&target, &name.into(), &descriptor);
    }
}

const NONE: u32 = 0;
const DISPOSE: u32 = 1;
const RESET: u32 = 2;
const SUBSCRIBE: u32 = 3;
const STORAGE_LIMIT: u32 = 4;
const SET_STORAGE_LIMIT: u32 = 5;
const STORAGE_USAGE: u32 = 6;
const SHOW_PLACEHOLDER: u32 = 7;
const SET_SHOW_PLACEHOLDER: u32 = 8;

/// One function for every operation of an addon object (bound to its number): methods, accessors, `onImageAdded`.
fn dispatcher(core: &Rc<AddonCore>) -> Closure<dyn Fn(u32, JsValue) -> JsValue> {
    let core = core.clone();
    Closure::new(move |op: u32, arg: JsValue| {
        let error = match op {
            DISPOSE => {
                core.dispose();
                None
            }
            RESET => {
                if let Some((shared, _)) = core.state() {
                    reset(&shared);
                }
                return JsValue::FALSE;
            }
            SUBSCRIBE => {
                return match arg.dyn_into::<Function>() {
                    Ok(listener) => core.on_image_added.event(listener).into(),
                    Err(_) => crate::emitter::Disposable::new(|| {}).into(),
                };
            }
            STORAGE_LIMIT => return core.storage_limit().into(),
            SET_STORAGE_LIMIT => core
                .set_storage_limit(arg.as_f64().unwrap_or(f64::NAN))
                .err(),
            STORAGE_USAGE => return core.storage_usage().into(),
            SHOW_PLACEHOLDER => return core.opts.borrow().show_placeholder.into(),
            SET_SHOW_PLACEHOLDER => {
                core.set_show_placeholder(arg.is_truthy());
                None
            }
            _ => None,
        };
        if let Some(error) = error {
            wasm_bindgen::throw_val(error);
        }
        JsValue::UNDEFINED
    })
}

/// Gives `shared` the addon's state: a storage owner, the handler stand-ins, the size report window options.
fn load_core(shared: &Rc<Shared>, core: Rc<AddonCore>) {
    unload(shared);
    let opts = core.opts.borrow().clone();
    let limit = storage_pixels(opts.storage_limit).unwrap_or_else(|| {
        web_sys::console::warn_1(&"storageLimit is set to 10 MB".into());
        2_500_000
    });
    let handlers = Handlers::new(shared);
    handlers.register(&core.handlers, &opts);
    *core.terminal.borrow_mut() = Rc::downgrade(shared);
    let parser = register_handlers(shared, &opts);
    let state = Rc::new(State {
        key: store::join(Rc::downgrade(shared), limit),
        addon: core,
        limit: Cell::new(limit),
        handlers,
        sixel: RefCell::new(None),
        decoded: RefCell::new(None),
        iip: RefCell::default(),
        parser,
    });
    *shared.images.borrow_mut() = Some(state);
    if opts.enable_size_reports {
        let window_options = {
            let core = shared.core.borrow();
            let current = get(&core.options, "windowOptions");
            if current.is_object() {
                current
            } else {
                Object::new().into()
            }
        };
        for key in ["getWinSizePixels", "getCellSizePixels", "getWinSizeChars"] {
            let _ = Reflect::set(&window_options, &key.into(), &JsValue::TRUE);
        }
        let _ = Terminal {
            shared: shared.clone(),
        }
        .options()
        .map(|proxy| Reflect::set(&proxy, &"windowOptions".into(), &window_options));
    }
    apply_placeholder(shared);
}

/// The engine's id of a CSI or DCS sequence: private marker, intermediates and final byte (see [`parser`]).
fn sequence_id(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0, |id, &b| id << 8 | b as u32)
}

/// The addon's sequence handlers (`activate`). Only terminals with the addon register them, so the others keep
/// the parser's path without hooks.
fn register_handlers(shared: &Rc<Shared>, opts: &Options) -> Vec<Disposable> {
    use parser::Kind::{Csi, Dcs, Osc};
    type Handler = fn(&Rc<Shared>, &Uint8Array, &[u32]) -> JsValue;
    let weak = Rc::downgrade(shared);
    let on = |kind, id, f: Handler| {
        let weak = weak.clone();
        parser::register_rust(shared, kind, id, move |data, params| match weak.upgrade() {
            Some(shared) => f(&shared, data, params),
            None => JsValue::FALSE,
        })
    };
    let mut handlers = vec![
        on(Csi, sequence_id(b"?h"), |s, _, p| decset(s, p, true).into()),
        on(Csi, sequence_id(b"?l"), |s, _, p| {
            decset(s, p, false).into()
        }),
        on(Csi, sequence_id(b"c"), |s, _, p| {
            primary_device_attributes(s, p).into()
        }),
        on(Csi, sequence_id(b"?S"), |s, _, p| {
            graphics_attributes(s, p).into()
        }),
        // DECSTR; RIS comes as the engine's `resets` ([`full_reset`])
        on(Csi, sequence_id(b"!p"), |s, _, _| {
            reset(s);
            JsValue::FALSE
        }),
        on(Csi, sequence_id(b"t"), |s, _, p| {
            window_options(s, p).into()
        }),
    ];
    if opts.sixel_support {
        handlers.push(on(Dcs, sequence_id(b"q"), |s, d, p| sixel(s, p, d).into()));
    }
    if opts.iip_support {
        handlers.push(on(Osc, 1337, |s, d, _| iip(s, d)));
    }
    handlers
}

/// Removes the addon from `shared`: its images go, placeholders are no longer drawn.
fn unload(shared: &Rc<Shared>) {
    let state = shared.images.borrow_mut().take();
    if let Some(state) = state {
        state.addon.handlers.clear();
        drop(state);
        apply_placeholder(shared);
        shared.schedule_frame();
    }
}

/// Tells the renderer whether tiles of images no longer stored show the placeholder.
fn apply_placeholder(shared: &Rc<Shared>) {
    let show = shared
        .images
        .borrow()
        .as_ref()
        .is_some_and(|s| s.addon.opts.borrow().show_placeholder);
    if let Ok(mut core) = shared.core.try_borrow_mut() {
        if let Some(renderer) = core.renderer.as_mut() {
            renderer.set_image_placeholder(show);
        }
    }
    shared.schedule_frame();
}

/// Hook: the renderer was created (`open`).
pub(crate) fn after_open(shared: &Rc<Shared>) {
    if shared.images.borrow().is_some() {
        apply_placeholder(shared);
    }
}

/// Hook: the terminal is disposed.
pub(crate) fn dispose(shared: &Rc<Shared>) {
    let state = shared.images.borrow_mut().take();
    if let Some(state) = state {
        *state.addon.terminal.borrow_mut() = Weak::new();
    }
}

/// Hook: the engine started over empty (a crash); its images are gone with its cells.
pub(crate) fn engine_restarted(shared: &Rc<Shared>) {
    if let Some(state) = shared.images.borrow().clone() {
        store::delete(state.key, |_| true);
    }
}

/// Hook: the engine dropped markers; images whose end marker went are deleted.
pub(crate) fn markers_gone(shared: &Rc<Shared>, ids: &[u32]) {
    if let Some(state) = shared.images.borrow().clone() {
        store::markers_gone(state.key, ids);
    }
}

/// Whether the terminal may have image markers (the marker sync then takes the engine's dropped markers).
pub(crate) fn has_markers(shared: &Shared) -> bool {
    shared.images.borrow().is_some()
}

/// Hook: the program did a full reset (RIS, `Terminal.reset`); the addon resets with it.
pub(crate) fn full_reset(shared: &Rc<Shared>) {
    if shared.images.borrow().is_some() {
        reset(shared);
    }
}

/// The active buffer switches (`wipeAlternate`): the alternate screen's images go.
fn buffer_changed(shared: &Rc<Shared>) {
    if let Some(state) = shared.images.borrow().clone() {
        store::delete(state.key, |i| i.alternate);
        shared.schedule_frame();
    }
}

/// `ImageAddon.reset`: the options sequences change go back to their defaults, the images and the decoders'
/// state are dropped (the sixel palette back to the default 256 colours).
fn reset(shared: &Rc<Shared>) {
    let Some(state) = shared.images.borrow().clone() else {
        return;
    };
    {
        let mut opts = state.addon.opts.borrow_mut();
        opts.sixel_scrolling = state.addon.defaults.sixel_scrolling;
        opts.sixel_palette_limit = state.addon.defaults.sixel_palette_limit;
    }
    store::delete(state.key, |_| true);
    reset_handlers(&state);
    shared.schedule_frame();
}

fn reset_handlers(state: &State) {
    if let Some(decoder) = state.sixel.borrow_mut().as_mut() {
        decoder.release();
        *decoder.palette_mut() = *sixel::default_palette();
    }
    *state.iip.borrow_mut() = Iip::default();
}

/// `DECSET`/`DECRST` (`CSI ? Pm h`/`l`): 80 turns sixel scrolling off/on, and a switch to or from the alternate
/// screen (47, 1047, 1049) drops its images, as `onBufferChange` does. The default action runs too (false).
fn decset(shared: &Rc<Shared>, params: &[u32], set: bool) -> bool {
    let Some(state) = shared.images.borrow().clone() else {
        return false;
    };
    if params.contains(&80) {
        state.addon.opts.borrow_mut().sixel_scrolling = !set;
    }
    if params.iter().any(|p| matches!(p, 47 | 1047 | 1049)) {
        let modes = shared.core.borrow().view.engine().modes().unwrap_or(0);
        if (modes & mode::ALT_SCREEN != 0) != set {
            buffer_changed(shared);
        }
    }
    false
}

/// DA1 (`CSI c`) with sixel on reports sixel support; true when it answered (the engine's answer is skipped).
fn primary_device_attributes(shared: &Rc<Shared>, params: &[u32]) -> bool {
    if params.first().is_some_and(|&p| p != 0) {
        return true;
    }
    let sixel = shared
        .images
        .borrow()
        .as_ref()
        .is_some_and(|s| s.addon.opts.borrow().sixel_support);
    if sixel {
        reply(shared, "\x1b[?62;4;9;22c");
    }
    sixel
}

/// XTSMGRAPHICS (`CSI ? Pi ; Pa ; Pv S`): colour registers and the sixel geometry; true when handled.
fn graphics_attributes(shared: &Rc<Shared>, params: &[u32]) -> bool {
    let Some(state) = shared.images.borrow().clone() else {
        return false;
    };
    if params.len() < 2 {
        return true;
    }
    let (item, action) = (params[0], params[1]);
    let square = |limit: f64| {
        let x = limit.sqrt() as u64;
        (0, vec![x, x])
    };
    let (status, values): (u64, Vec<u64>) = match (item, action) {
        (1, 1) => (
            0,
            vec![state.addon.opts.borrow().sixel_palette_limit as u64],
        ),
        (1, 2) => {
            let limit = state.addon.defaults.sixel_palette_limit;
            state.addon.opts.borrow_mut().sixel_palette_limit = limit;
            reset_handlers(&state);
            (0, vec![limit as u64])
        }
        (1, 3) => match params.get(2) {
            Some(&n) if n as usize <= sixel::PALETTE_SIZE => {
                state.addon.opts.borrow_mut().sixel_palette_limit = n as f64;
                (0, vec![n as u64])
            }
            _ => (2, vec![]),
        },
        (1, 4) => (0, vec![sixel::PALETTE_SIZE as u64]),
        (2, 1) => {
            let (_, (w, h)) = metrics(shared);
            let limit = state.addon.opts.borrow().pixel_limit;
            if w * h < limit {
                (0, vec![round(w), round(h)])
            } else {
                square(limit)
            }
        }
        (2, 4) => square(state.addon.opts.borrow().pixel_limit),
        (1 | 2, _) => (2, vec![]),
        _ => (1, vec![]),
    };
    let mut answer = vec![item as u64, status];
    answer.extend(values);
    reply(shared, &sequence("\x1b[?", &answer, "S"));
    true
}

/// Window operations (`CSI Ps ; Ps ; Ps t`) 14 (text area in pixels), 16 (cell in pixels) and 18 (text area in
/// cells) as xterm.js answers them for `windowOptions`: not at all when not allowed. True for those three (the
/// engine's own answer is skipped); the others go to the engine.
fn window_options(shared: &Rc<Shared>, params: &[u32]) -> bool {
    let allowed = |key: &str| {
        let core = shared.core.borrow();
        get(&get(&core.options, "windowOptions"), key).is_truthy()
    };
    let second = params.get(1).copied().unwrap_or(0);
    let values = match params.first() {
        Some(14) if allowed("getWinSizePixels") && second != 2 => {
            let (_, (w, h)) = metrics(shared);
            [4, round(h), round(w)]
        }
        Some(16) if allowed("getCellSizePixels") => {
            let ((w, h), _) = metrics(shared);
            [6, round(h), round(w)]
        }
        Some(18) if allowed("getWinSizeChars") => {
            let (cols, rows) = shared.core.borrow().size();
            [8, rows as u64, cols as u64]
        }
        Some(14 | 16 | 18) => return true,
        _ => return false,
    };
    reply(shared, &sequence("\x1b[", &values, "t"));
    true
}

/// `toFixed(0)` of a non-negative size.
fn round(x: f64) -> u64 {
    (x.max(0.0) + 0.5).floor() as u64
}

/// `prefix`, the values joined by `;`, then `end` (integer formatting only: no float formatting code).
fn sequence(prefix: &str, values: &[u64], end: &str) -> String {
    let mut s = String::from(prefix);
    for (i, v) in values.iter().enumerate() {
        if i > 0 {
            s.push(';');
        }
        s.push_str(&v.to_string());
    }
    s.push_str(end);
    s
}

/// Bytes for the program (not user input).
fn reply(shared: &Rc<Shared>, text: &str) {
    crate::input::send_data(shared, &text.into(), false);
}

/// The renderer's CSS cell and canvas size, zeros before it measured.
fn metrics(shared: &Shared) -> ((f64, f64), (f64, f64)) {
    let core = shared.core.borrow();
    core.renderer
        .as_ref()
        .map_or(((0.0, 0.0), (0.0, 0.0)), |r| {
            let d = r.dimensions();
            (
                (d.css_cell.width, d.css_cell.height),
                (d.css_canvas.width, d.css_canvas.height),
            )
        })
}

/// The cell size images are laid out on (`ImageRenderer.cellSize`, else `CELL_SIZE_DEFAULT`).
fn cell_size(shared: &Shared) -> (f64, f64) {
    let ((w, h), _) = metrics(shared);
    if w > 0.0 && h > 0.0 {
        (w, h)
    } else {
        CELL_SIZE_DEFAULT
    }
}

/// `0xAABBGGRR` of the background text written now gets, the foreground when inverse (`extractActiveBg`); 0
/// before `open`, as the addon gets no theme then.
fn active_background(shared: &Shared) -> u32 {
    use wongtty::color;
    let core = shared.core.borrow();
    if core.element.is_none() {
        return 0;
    }
    let Ok(pen) = core.view.engine().pen() else {
        return 0;
    };
    let colors = ColorSet::new(&crate::render::options::parse_theme(&get(
        &core.options,
        "theme",
    )));
    let inverse = pen.get_index(2) & crate::engine::attr::INVERSE != 0;
    let (packed, default) = if inverse {
        (pen.get_index(0), &colors.foreground)
    } else {
        (pen.get_index(1), &colors.background)
    };
    let value = packed & color::VALUE_MASK;
    let rgba = match packed & color::KIND_MASK {
        color::RGB => return 0xff00_0000 | (value & 0xff) << 16 | (value & 0xff00) | value >> 16,
        color::INDEXED => colors.ansi.get(value as usize).map(|c| c.rgba),
        color::NAMED if value < 16 => colors.ansi.get(value as usize).map(|c| c.rgba),
        _ => None,
    }
    .unwrap_or(default.rgba);
    rgba.swap_bytes()
}

/// Rows of a sixel straight into a JS array, so the image never sits in this module's memory.
struct JsRows {
    data: Option<Uint32Array>,
    width: usize,
    at: u32,
}

impl Sink for JsRows {
    fn start(&mut self, width: usize, height: usize) -> bool {
        self.width = width;
        self.data = new_u32_array(width * height);
        self.data.is_some()
    }

    fn row(&mut self, pixels: &[u32]) {
        if let Some(data) = &self.data {
            // SAFETY: the view is used at once, before anything can allocate wasm memory
            let view = unsafe { Uint32Array::view(pixels) };
            data.set(&view, self.at);
            self.at += self.width as u32;
        }
    }
}

/// A zeroed `Uint32Array`, `None` if the browser refuses the size.
fn new_u32_array(len: usize) -> Option<Uint32Array> {
    let constructor: Function = get(&js_sys::global(), "Uint32Array").dyn_into().ok()?;
    Reflect::construct(&constructor, &Array::of1(&(len as f64).into()))
        .ok()?
        .dyn_into()
        .ok()
}

/// Calls `f` on `data` in pieces copied into this module's memory.
fn pieces(data: &Uint8Array, mut f: impl FnMut(&[u8]) -> bool) {
    let mut buffer = vec![0; PIECE.min(data.length()) as usize];
    let mut at = 0;
    while at < data.length() {
        let end = (at + PIECE).min(data.length());
        let n = (end - at) as usize;
        data.subarray(at, end).copy_to(&mut buffer[..n]);
        if !f(&buffer[..n]) {
            return;
        }
        at = end;
    }
}

/// A sixel sequence (`DCS params q data ST`); false when the addon does not take it (the default action runs).
fn sixel(shared: &Rc<Shared>, params: &[u32], data: &Uint8Array) -> bool {
    let Some(state) = shared.images.borrow().clone() else {
        return false;
    };
    let opts = state.addon.opts.borrow().clone();
    if !opts.sixel_support {
        return false;
    }
    if data.length() as f64 > opts.sixel_size_limit {
        web_sys::console::warn_1(&"SIXEL: too much data, aborting".into());
        return true;
    }
    let fill = if params.get(1) == Some(&1) {
        0
    } else {
        active_background(shared)
    };
    let palette_limit = opts
        .sixel_palette_limit
        .clamp(1.0, sixel::PALETTE_SIZE as f64) as usize;
    let mut rows = JsRows {
        data: None,
        width: 0,
        at: 0,
    };
    let (width, height) = {
        let mut decoder = state.sixel.borrow_mut();
        let decoder =
            decoder.get_or_insert_with(|| Box::new(Decoder::new(sixel::default_palette())));
        decoder.init(fill, palette_limit, opts.pixel_limit());
        pieces(data, |piece| {
            decoder.decode(piece, &mut rows);
            !decoder.aborted()
        });
        if decoder.aborted() {
            web_sys::console::warn_1(
                &"SIXEL: error while decoding image - image exceeds memory limit".into(),
            );
            return true;
        }
        decoder.finish(&mut rows)
    };
    *state.decoded.borrow_mut() = Some(Decoded {
        width,
        height,
        pixels: rows.data,
    });
    // Through the `_handlers` stand-in, which a host may have wrapped (see `Handlers`)
    let handler = state.handlers.sixel.clone();
    let dec = Object::new();
    let _ = Reflect::set(&dec, &"width".into(), &(width as f64).into());
    let _ = Reflect::set(&dec, &"height".into(), &(height as f64).into());
    NOOP.with(|noop| {
        let _ = Reflect::set(&dec, &"release".into(), noop);
    });
    let _ = Reflect::set(&handler, &"_dec".into(), &dec);
    let _ = Reflect::set(&handler, &"_aborted".into(), &JsValue::FALSE);
    let unhook = get(&handler, "unhook").dyn_into::<Function>();
    drop(state);
    if let Ok(unhook) = unhook {
        if let Err(error) = unhook.call1(&handler, &JsValue::TRUE) {
            web_sys::console::error_1(&error);
        }
    }
    if let Some(state) = shared.images.borrow().as_ref() {
        state.decoded.borrow_mut().take();
    }
    true
}

/// The original `unhook`: places the decoded sixel unless a wrapper aborted it.
fn place_sixel(shared: &Rc<Shared>, success: bool) {
    let Some(state) = shared.images.borrow().clone() else {
        return;
    };
    let decoded = state.decoded.borrow_mut().take();
    let aborted = get(&state.handlers.sixel, "_aborted").is_truthy();
    let Some(decoded) = decoded.filter(|_| success && !aborted) else {
        return;
    };
    let scrolling = state.addon.opts.borrow().sixel_scrolling;
    drop(state);
    let (width, height) = (decoded.width, decoded.height);
    if width == 0 || height == 0 {
        // An empty sixel with a height only moves the cursor (`advanceCursor`)
        if height > 0 && scrolling {
            let rows = (height as f64 / cell_size(shared).1).ceil();
            let core = shared.core.borrow();
            place(core.view.engine(), 0, 0, rows as u32, true, false);
        }
        return;
    }
    let Some(pixels) = decoded.pixels else {
        return;
    };
    let pixels = Pixels::Rgba(Uint8Array::new(&pixels.buffer()));
    add_image(
        shared,
        width as u32,
        height as u32,
        pixels,
        scrolling,
        false,
    );
}

/// An iTerm2 sequence (`OSC 1337 ; data ST`): false when the addon does not take it, a Promise while an image
/// decodes.
fn iip(shared: &Rc<Shared>, data: &Uint8Array) -> JsValue {
    let Some(state) = shared.images.borrow().clone() else {
        return JsValue::FALSE;
    };
    let opts = state.addon.opts.borrow().clone();
    if !opts.iip_support {
        return JsValue::FALSE;
    }
    let limit = (opts.iip_size_limit * 4.0 / 3.0).ceil();
    let limit = if limit.is_finite() && limit > 0.0 {
        limit as usize
    } else {
        0
    };
    let mut parser = HeaderParser::default();
    let mut aborted = false;
    {
        let mut iip = state.iip.borrow_mut();
        pieces(data, |piece| {
            put_iip(&mut iip, &mut parser, piece, limit, &mut aborted);
            !aborted
        });
    }
    if aborted {
        return JsValue::TRUE;
    }
    end_iip(shared, &state, &mut parser, opts.pixel_limit())
}

/// `IIPHandler.put`.
fn put_iip(
    iip: &mut Iip,
    parser: &mut HeaderParser,
    data: &[u8],
    limit: usize,
    aborted: &mut bool,
) {
    if parser.ended() {
        if !iip.decoder.as_mut().is_some_and(|d| d.put(data, limit)) {
            iip.decoder = None;
            *aborted = true;
        }
        return;
    }
    let at = match parser.parse(data) {
        Parsed::Abort => {
            *aborted = true;
            return;
        }
        Parsed::More => return,
        Parsed::Data(at) => at,
    };
    if parser.header.kind == Kind::File {
        if iip.multipart {
            iip.multipart = false;
            iip.abort_multi = false;
            iip.decoder = None;
        }
        iip.header = parser.header.clone();
        if iip.header.inline == 0 {
            *aborted = true;
            return;
        }
        iip.decoder = Some(Payload::default());
    } else if iip.abort_multi {
        *aborted = true;
        return;
    }
    if !iip
        .decoder
        .as_mut()
        .is_some_and(|d| d.put(&data[at..], limit))
    {
        iip.decoder = None;
        *aborted = true;
        if iip.multipart {
            iip.abort_multi = true;
        }
    }
}

/// `IIPHandler.end`: a finished file is decoded by the browser and placed when that is done.
fn end_iip(
    shared: &Rc<Shared>,
    state: &State,
    parser: &mut HeaderParser,
    pixel_limit: usize,
) -> JsValue {
    if !parser.ended() && !parser.end() {
        return JsValue::TRUE;
    }
    let payload = {
        let mut iip = state.iip.borrow_mut();
        match parser.header.kind {
            Kind::FilePart | Kind::Invalid => return JsValue::TRUE,
            Kind::ReportCellSize => {
                drop(iip);
                report_cell_size(shared);
                return JsValue::TRUE;
            }
            Kind::MultipartFile => {
                iip.header = parser.header.clone();
                iip.multipart = true;
                iip.abort_multi = false;
                iip.decoder = Some(Payload::default());
                return JsValue::TRUE;
            }
            Kind::FileEnd => {
                if !iip.multipart {
                    return JsValue::TRUE;
                }
                iip.multipart = false;
                if iip.abort_multi || iip.header.kind != Kind::MultipartFile {
                    return JsValue::TRUE;
                }
            }
            Kind::File => {}
        }
        iip.decoder.take()
    };
    let Some(mut payload) = payload else {
        return JsValue::TRUE;
    };
    if !payload.end() {
        web_sys::console::warn_1(&"IIP: error during BASE64 decoding".into());
        return JsValue::TRUE;
    }
    let Some(metrics) = iip::image_type(payload.len, |i| payload.at(i)) else {
        web_sys::console::warn_1(&"IIP: unsupported image type".into());
        return JsValue::TRUE;
    };
    let fits = |w: f64, h: f64| w > 0.0 && h > 0.0 && w * h < pixel_limit as f64;
    let (w, h) = (metrics.width as f64, metrics.height as f64);
    if !fits(w, h) {
        web_sys::console::warn_1(
            &format!(
                "IIP: image dimension issue {}x{}",
                metrics.width, metrics.height
            )
            .into(),
        );
        return JsValue::TRUE;
    }
    let header = state.iip.borrow().header.clone();
    let ((cw, ch), (canvas_w, canvas_h)) = metrics_or_default(shared);
    let area = iip::Area {
        cell_width: cw,
        cell_height: ch,
        width: canvas_w,
        height: canvas_h,
    };
    let (w, h) = iip::fit(w, h, &header, area);
    if !fits(w, h) {
        return JsValue::TRUE;
    }
    let (w, h) = (w as u32, h as u32);
    let parts: Array = payload.pieces.iter().collect();
    let bag = web_sys::BlobPropertyBag::new();
    bag.set_type(metrics.mime);
    let Ok(blob) = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &bag) else {
        return JsValue::TRUE;
    };
    decode(shared, w, h, &blob)
}

/// The cell and canvas size the iTerm2 handler sizes images by (`_resize`), with the addon's fallbacks.
fn metrics_or_default(shared: &Shared) -> ((f64, f64), (f64, f64)) {
    let ((cw, ch), (w, h)) = metrics(shared);
    let (cols, rows) = shared.core.borrow().size();
    let cw = if cw > 0.0 { cw } else { CELL_SIZE_DEFAULT.0 };
    let ch = if ch > 0.0 { ch } else { CELL_SIZE_DEFAULT.1 };
    let w = if w > 0.0 { w } else { cw * cols as f64 };
    let h = if h > 0.0 { h } else { ch * rows as f64 };
    ((cw, ch), (w, h))
}

/// Decodes `blob` (`createImageBitmap`, resized to `width` × `height`) and places the image when that is done;
/// the parser waits on the returned Promise, as on the addon's. A decoding error rejects it (the parser reports
/// it on a later task and the sequence falls through).
fn decode(shared: &Rc<Shared>, width: u32, height: u32, blob: &web_sys::Blob) -> JsValue {
    let Some(window) = web_sys::window() else {
        return JsValue::TRUE;
    };
    let options = web_sys::ImageBitmapOptions::new();
    options.set_resize_width(width);
    options.set_resize_height(height);
    let promise =
        match window.create_image_bitmap_with_blob_and_image_bitmap_options(blob, &options) {
            Ok(promise) => promise,
            Err(error) => return Promise::reject(&error).into(),
        };
    let weak = Rc::downgrade(shared);
    wasm_bindgen_futures::future_to_promise(async move {
        let bitmap: web_sys::ImageBitmap = wasm_bindgen_futures::JsFuture::from(promise)
            .await?
            .dyn_into()?;
        match weak.upgrade() {
            Some(shared) => {
                add_image(&shared, width, height, Pixels::Bitmap(bitmap), true, true);
            }
            None => bitmap.close(),
        }
        Ok(JsValue::TRUE)
    })
    .into()
}

/// `OSC 1337 ; ReportCellSize ST`: the cell height, width and devicePixelRatio.
fn report_cell_size(shared: &Rc<Shared>) {
    let (_, canvas) = metrics(shared);
    let (cols, rows) = shared.core.borrow().size();
    let (w, h) = if shared.core.borrow().renderer.is_some() && cols > 0 && rows > 0 {
        (canvas.0 / cols as f64, canvas.1 / rows as f64)
    } else {
        CELL_SIZE_DEFAULT
    };
    let scale = web_sys::window().map_or(1.0, |w| w.device_pixel_ratio());
    // `toFixed(3)` of each
    let fixed = |v: f64| {
        let n = round(v * 1000.0);
        let f = n % 1000;
        format!("{}.{}{}{}", n / 1000, f / 100, f / 10 % 10, f % 10)
    };
    reply(
        shared,
        &format!(
            "\x1b]1337;ReportCellSize={};{};{}\x1b\\",
            fixed(h),
            fixed(w),
            fixed(scale)
        ),
    );
}

/// `ImageStorage.addImage`: makes room, covers cells from the cursor with the image's tiles and stores it.
fn add_image(
    shared: &Rc<Shared>,
    width: u32,
    height: u32,
    pixels: Pixels,
    scrolling: bool,
    iip_cursor: bool,
) {
    let Some(state) = shared.images.borrow().clone() else {
        return discard(pixels);
    };
    let key = state.key;
    store::evict(key, width as usize * height as usize);
    let cell = cell_size(shared);
    let cols = (width as f64 / cell.0).ceil() as u32;
    let rows = (height as f64 / cell.1).ceil() as u32;
    let id = store::reserve_id();
    let placed = {
        let core = shared.core.borrow();
        let engine = core.view.engine();
        let placed = (!core.disposed)
            .then(|| place(engine, id, cols, rows, scrolling, iip_cursor))
            .flatten();
        placed.map(|placed| {
            let alternate = engine.modes().unwrap_or(0) & mode::ALT_SCREEN != 0;
            let line = engine.history_length() as i32 + core.view.cursor().line;
            let marker = if alternate {
                0
            } else {
                engine.add_marker(line).unwrap_or(0)
            };
            let counts = if alternate {
                screen_tiles(engine, engine.rows())
            } else {
                Vec::new()
            };
            (placed, marker, alternate, counts)
        })
    };
    let Some((placed, marker, alternate, counts)) = placed else {
        return discard(pixels);
    };
    let (tiles, covered) = placed;
    store::covered(&covered);
    store::delete(key, |i| i.tiles < 1);
    if alternate {
        store::recount_alternate(key, &counts);
    }
    store::add(store::Image {
        id,
        owner: key,
        width,
        height,
        cell,
        cols,
        pixels,
        marker,
        alternate,
        tiles,
    });
    let emitter = state.addon.on_image_added.clone();
    drop(state);
    emitter.fire(&[]);
    shared.schedule_frame();
}

/// Frees pixels that were not stored.
fn discard(pixels: Pixels) {
    if let Pixels::Bitmap(bitmap) = pixels {
        bitmap.close();
    }
}

/// `Engine.placeImage`: the tiles written and the image ids of the tiles covered.
fn place(
    engine: &Engine,
    id: u32,
    cols: u32,
    rows: u32,
    scrolling: bool,
    iip_cursor: bool,
) -> Option<(i64, Vec<u32>)> {
    let options = Object::new();
    let _ = Reflect::set(&options, &"scrolling".into(), &scrolling.into());
    let cursor = if iip_cursor { "iip" } else { "vt340" };
    let _ = Reflect::set(&options, &"cursor".into(), &cursor.into());
    let placed = engine.place_image(id, cols, rows, &options).ok()?;
    let covered = Array::from(&get(&placed, "covered"))
        .iter()
        .filter_map(|v| v.as_f64())
        .map(|v| v as u32)
        .collect();
    Some((
        get(&placed, "tiles").as_f64().unwrap_or(0.0) as i64,
        covered,
    ))
}

/// Tiles per image on the screen rows (`_evictOnAlternate`).
fn screen_tiles(engine: &Engine, rows: u32) -> Vec<(u32, i64)> {
    let mut counts: Vec<(u32, i64)> = Vec::new();
    for line in 0..rows as i32 {
        let Ok(Some(cells)) = engine.row_images(line) else {
            continue;
        };
        for k in (0..cells.length()).step_by(3) {
            let id = cells.get_index(k + 1);
            match counts.iter_mut().find(|(i, _)| *i == id) {
                Some((_, n)) => *n += 1,
                None => counts.push((id, 1)),
            }
        }
    }
    counts
}

#[wasm_bindgen]
impl Terminal {
    /// State for the tests: the image tiles of the last frame (`[row, col, id, tile]`), this terminal's images and
    /// the page's (`images`, `pixels`), the GPU textures of images.
    #[wasm_bindgen(js_name = _imageDebug, skip_typescript)]
    pub fn image_debug(&self) -> JsValue {
        let info = Object::new();
        let set = |k: &str, v: JsValue| {
            let _ = Reflect::set(&info, &k.into(), &v);
        };
        let mut tiles: Vec<u32> = Vec::new();
        {
            let core = self.shared.core.borrow();
            for y in 0..core.viewport.rows {
                for e in core.viewport.extras.get(&y).into_iter().flatten() {
                    if let Some((id, tile)) = e.image {
                        tiles.extend([y, e.col, id, tile]);
                    }
                }
            }
        }
        let tiles = Uint32Array::from(&tiles[..]);
        set("tiles", tiles.into());
        let key = self.shared.images.borrow().as_ref().map(|s| s.key);
        set("loaded", key.is_some().into());
        set("pixels", key.map_or(0, store::usage).into());
        let (images, pixels) = store::totals();
        set("pageImages", images.into());
        set("pagePixels", pixels.into());
        set("textures", crate::render::image_textures().into());
        info.into()
    }
}
