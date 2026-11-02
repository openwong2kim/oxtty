//! The terminal's renderer members: `dimensions`, `refresh`, `onDimensionsChange`, and the glue that creates the
//! renderer in `open`, applies renderer options and the theme, and handles what the renderer asks for.
use std::rc::Rc;

use js_sys::{Array, Function, Object, Reflect, Uint32Array};
use wasm_bindgen::prelude::*;

use super::color::engine_colors;
use super::webgl::WebglRenderer;
use super::{options, Dimensions, Dirty, Highlight, RendererRequest, SelectionRange, Size};
use crate::engine::get;
use crate::terminal::{Shared, Terminal};

#[wasm_bindgen(typescript_custom_section)]
const TS_DIMENSIONS: &str = r#"
export interface IRenderDimensions {
  css: { canvas: { width: number; height: number }; cell: { width: number; height: number } };
  device: {
    canvas: { width: number; height: number };
    cell: { width: number; height: number };
    char: { width: number; height: number; left: number; top: number };
  };
}
"#;

fn size(width: f64, height: f64) -> Object {
    let o = Object::new();
    let _ = Reflect::set(&o, &"width".into(), &width.into());
    let _ = Reflect::set(&o, &"height".into(), &height.into());
    o
}

fn size_object(s: Size) -> Object {
    size(s.width, s.height)
}

/// `IRenderDimensions` as a plain object.
fn dimensions_object(d: &Dimensions) -> Object {
    let css = Object::new();
    let _ = Reflect::set(&css, &"canvas".into(), &size_object(d.css_canvas));
    let _ = Reflect::set(&css, &"cell".into(), &size_object(d.css_cell));
    let device = Object::new();
    let _ = Reflect::set(&device, &"canvas".into(), &size_object(d.device_canvas));
    let _ = Reflect::set(&device, &"cell".into(), &size_object(d.device_cell));
    let char = size(d.device_char.width, d.device_char.height);
    let _ = Reflect::set(&char, &"left".into(), &d.device_char.left.into());
    let _ = Reflect::set(&char, &"top".into(), &d.device_char.top.into());
    let _ = Reflect::set(&device, &"char".into(), &char);
    let o = Object::new();
    let _ = Reflect::set(&o, &"css".into(), &css);
    let _ = Reflect::set(&o, &"device".into(), &device);
    o
}

#[wasm_bindgen]
impl Terminal {
    /// Cell and canvas size, `undefined` before `open`.
    #[wasm_bindgen(getter, unchecked_return_type = "IRenderDimensions | undefined")]
    pub fn dimensions(&self) -> JsValue {
        let core = self.shared.core.borrow();
        core.renderer.as_ref().map_or(JsValue::UNDEFINED, |r| {
            dimensions_object(&r.dimensions()).into()
        })
    }

    /// Redraws viewport rows `start`…`end` on the next frame.
    pub fn refresh(&self, start: i32, end: i32) {
        {
            let mut core = self.shared.core.borrow_mut();
            if core.disposed {
                return;
            }
            let last = core.view.engine().rows() as i32 - 1;
            let rows: Vec<u32> = (start.max(0)..=end.min(last)).map(|y| y as u32).collect();
            if let Dirty::Rows(dirty) = &mut core.dirty {
                dirty.extend(rows);
            }
        }
        self.shared.schedule_frame();
    }

    /// The cell size changed: after a font option, or when fonts finish loading (a loaded fit addon refits then).
    #[wasm_bindgen(
        getter = onDimensionsChange,
        unchecked_return_type = "IEvent<IRenderDimensions>"
    )]
    pub fn on_dimensions_change(&self) -> Function {
        crate::events::event(&self.shared, crate::events::Event::DimensionsChange)
    }

    /// Test-only: the renderer's internal state.
    #[wasm_bindgen(js_name = _rendererDebug, skip_typescript)]
    pub fn renderer_debug(&self) -> JsValue {
        let core = self.shared.core.borrow();
        core.renderer
            .as_ref()
            .map_or(JsValue::UNDEFINED, |r| r.debug())
    }

    /// Internal: the selection the renderer draws, `{ start: [col, row], end: [col, row], columnSelect }` in
    /// viewport cells, or `undefined`.
    #[wasm_bindgen(js_name = _setRendererSelection, skip_typescript)]
    pub fn set_renderer_selection(&self, selection: JsValue) {
        let point = |key: &str| {
            let a = get(&selection, key);
            let n = |i: u32| Reflect::get(&a, &i.into()).ok()?.as_f64();
            Some((n(0)?.max(0.0) as u32, n(1)?.max(0.0) as u32))
        };
        let range = match (point("start"), point("end")) {
            (Some(start), Some(end)) => Some(SelectionRange {
                start,
                end,
                column_select: get(&selection, "columnSelect").is_truthy(),
            }),
            _ => None,
        };
        let mut core = self.shared.core.borrow_mut();
        if let Some(renderer) = core.renderer.as_mut() {
            renderer.set_selection(range);
        }
        drop(core);
        self.shared.schedule_frame();
    }

    /// Internal: replaces highlight layer `layer` with `{ row, start, end, background?, foreground?, border?,
    /// underline? }` entries in viewport cells.
    #[wasm_bindgen(js_name = _setHighlights, skip_typescript)]
    pub fn set_highlights(&self, layer: String, highlights: JsValue) {
        let list: Vec<Highlight> = if Array::is_array(&highlights) {
            Array::from(&highlights)
                .iter()
                .map(|h| {
                    let n = |k: &str| get(&h, k).as_f64().unwrap_or(0.0).max(0.0) as u32;
                    Highlight {
                        row: n("row"),
                        start: n("start"),
                        end: n("end"),
                        background: get(&h, "background").as_string(),
                        foreground: get(&h, "foreground").as_string(),
                        border: get(&h, "border").as_string(),
                        underline: get(&h, "underline").is_truthy(),
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut core = self.shared.core.borrow_mut();
        if let Some(renderer) = core.renderer.as_mut() {
            renderer.set_highlights(&layer, list);
        }
        drop(core);
        self.shared.schedule_frame();
    }
}

impl Shared {
    /// Creates the renderer in `open` (`.xterm-screen` must be set) and sizes the screen to it. Without WebGL2 (a
    /// blocklisted GPU, jsdom) the terminal runs without drawing, as xterm.js runs with its DOM renderer: input,
    /// buffer and events work, `dimensions` stays undefined, and the page is warned once.
    pub(crate) fn create_renderer(self: &Rc<Self>) -> Result<(), JsValue> {
        let mut core = self.core.borrow_mut();
        let (Some(screen), Some(element)) = (core.screen.clone(), core.element.clone()) else {
            return Ok(());
        };
        let weak = Rc::downgrade(self);
        let wake: Rc<dyn Fn()> = Rc::new(move || {
            if let Some(shared) = weak.upgrade() {
                shared.schedule_frame();
            }
        });
        let engine = core.view.engine();
        let (cols, rows) = (engine.cols(), engine.rows());
        let options = options::parse(&core.options);
        let renderer = match WebglRenderer::new(&screen, &element, cols, rows, &options, wake) {
            Ok(renderer) => renderer,
            Err(error) => {
                drop(core);
                thread_local!(static WARNED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
                if !WARNED.with(|w| w.replace(true)) {
                    web_sys::console::warn_2(
                        &"wongtty/xterm: no renderer, the terminal does not draw:".into(),
                        &error,
                    );
                }
                return Ok(());
            }
        };
        core.renderer = Some(Box::new(renderer));
        drop(core);
        self.sync_screen_size();
        Ok(())
    }

    /// Sizes `.xterm-screen` to the canvas.
    pub(crate) fn sync_screen_size(&self) {
        let core = self.core.borrow();
        let (Some(screen), Some(renderer)) = (&core.screen, &core.renderer) else {
            return;
        };
        let canvas = renderer.dimensions().css_canvas;
        let style = screen.style();
        let _ = style.set_property("width", &format!("{}px", canvas.width));
        let _ = style.set_property("height", &format!("{}px", canvas.height));
    }

    /// Tells the engine the theme's colours, so OSC 4/10/11/12 queries answer them.
    pub(crate) fn apply_theme(&self) {
        let core = self.core.borrow();
        let theme = options::parse_theme(&get(&core.options, "theme"));
        let colors = Uint32Array::from(engine_colors(&theme).as_slice());
        let _ = core.view.engine().set_colors(&colors);
    }

    /// Applies a changed option: the theme reaches the engine, renderer options the renderer.
    pub(crate) fn apply_renderer_option(self: &Rc<Self>, key: &str) {
        if key == "theme" {
            self.apply_theme();
        }
        if !options::KEYS.contains(&key) {
            return;
        }
        let changed = {
            let mut core = self.core.borrow_mut();
            let parsed = options::parse(&core.options);
            let Some(renderer) = core.renderer.as_mut() else {
                return;
            };
            let before = renderer.dimensions().css_cell;
            renderer.set_options(&parsed);
            let after = renderer.dimensions();
            core.dirty = Dirty::All;
            (after.css_cell != before).then_some(after)
        };
        self.sync_screen_size();
        if let Some(dimensions) = changed {
            self.events
                .dimensions_change
                .fire(&[&dimensions_object(&dimensions).into()]);
        }
        self.schedule_frame();
    }

    /// Takes what the renderer asked for since the last frame: redraws join the host's dirty rows; a cell size
    /// change resizes the screen, fires `onDimensionsChange` and refits loaded fit addons.
    pub(crate) fn handle_renderer_requests(self: &Rc<Self>) {
        let dimensions = {
            let mut core = self.core.borrow_mut();
            if core.disposed {
                return;
            }
            let Some(renderer) = core.renderer.as_mut() else {
                return;
            };
            let requests = renderer.take_requests();
            let dimensions = renderer.dimensions();
            let mut changed = false;
            for request in requests {
                match request {
                    RendererRequest::DimensionsChanged => changed = true,
                    RendererRequest::Redraw(Dirty::All) => core.dirty = Dirty::All,
                    RendererRequest::Redraw(Dirty::Rows(rows)) => {
                        if let Dirty::Rows(dirty) = &mut core.dirty {
                            dirty.extend(rows);
                        }
                    }
                }
            }
            changed.then_some(dimensions)
        };
        let Some(dimensions) = dimensions else {
            return;
        };
        self.sync_screen_size();
        self.events
            .dimensions_change
            .fire(&[&dimensions_object(&dimensions).into()]);
        crate::fit::refit(self);
    }
}
