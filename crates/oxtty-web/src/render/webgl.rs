//! The WebGL2 renderer of one terminal: fills the render model from [`Viewport`] rows, keeps the pane's vertex
//! data, and draws through the page's shared context ([`super::gpu`]) into its own canvas.
//!
//! Ported from xterm.js 6.0.0 (commit f447274f430fd22513f6adbf9862d19524471c04, MIT): the model update, dimensions
//! and font/devicePixelRatio handling of `addons/addon-webgl/src/WebglRenderer.ts` lines 200-560 and
//! `src/browser/services/CharSizeService.ts` lines 13-127 (glyph measurement), by way of the TypeScript
//! orchestrator at 0510a7d (`packages/oxtty/xterm/src/renderer/WebglRenderer.ts`).
//!
//! # Memory
//!
//! A pane owns its canvas (holding the last frame), its render model and glyph vertices; the context, programs,
//! atlas textures, glyph measurement, the visibility observer and the font and window listeners are shared by
//! every pane on the page. A pane draws only when something changed, and not at all while it is out of view
//! (`display: none`, zero size, scrolled away): it redraws everything when it comes back. A blinking cursor
//! draws nothing either: it is a small layer over the canvas whose opacity the blink flips ([`CursorLayer`]).
//!
//! # Re-entrancy
//!
//! State is behind one `RefCell` per pane. DOM listeners, timers and observers borrow it with `try_borrow_mut`
//! and retry on a later task when it is busy; they never call into the terminal, they queue
//! [`RendererRequest`]s and call the terminal's `wake`.
use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::{Rc, Weak};

use js_sys::{Array, Function, Object, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    Document, Event, FocusEvent, HtmlCanvasElement, HtmlElement, IntersectionObserver,
    IntersectionObserverEntry, OffscreenCanvasRenderingContext2d,
};

use super::atlas::{self, AtlasConfig, TextureAtlas};
use super::blink::Blink;
use super::cell::{CellData, CM_RGB, RGB_MASK};
use super::color::{css_to_color, is_opaque, ColorSet};
use super::cursor_layer::{CursorLayer, Region};
use super::glyphs::GlyphVertices;
use super::gpu::{self, Listener, Present};
use super::images::ImageLayer;
use super::model::{
    resolve, CursorModel, RenderModel, ResolvedHighlight, SelectionModel, COMBINED_CHAR_BIT_MASK,
    RENDER_MODEL_BG_OFFSET, RENDER_MODEL_EXT_OFFSET, RENDER_MODEL_FG_OFFSET,
    RENDER_MODEL_INDICIES_PER_CELL,
};
use super::rects::{css_floats, Rect, RectVertices};
use super::{
    CursorInactiveStyle, CursorStyle, Dimensions, Dirty, Highlight, Renderer, RendererOptions,
    RendererRequest, SelectionRange,
};
use crate::engine::{attr, CursorShape, Viewport};
use crate::timer;

/// Requests kept for [`Renderer::debug`].
const LOG_LIMIT: usize = 256;

thread_local! {
    /// Live panes in creation order.
    static PANES: RefCell<Vec<Weak<RefCell<Inner>>>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: Cell<u32> = const { Cell::new(1) };
    /// Canvas text metrics for glyph measurement, `None` where they are missing (then each pane measures in
    /// the DOM).
    static METRICS: Option<OffscreenCanvasRenderingContext2d> = metrics_context();
    /// One observer tells every pane whether it is in view.
    static VISIBILITY: Option<IntersectionObserver> = visibility_observer();
    /// Whether the page-wide font and window listeners are registered.
    static LISTENING: Cell<bool> = const { Cell::new(false) };
}

/// How glyphs are measured (`CharSizeService`): canvas text metrics, or a DOM element where those are missing.
enum Measure {
    Metrics,
    Dom(HtmlElement),
}

impl Measure {
    fn new(document: &Document, parent: &HtmlElement) -> Option<Self> {
        if METRICS.with(Option::is_some) {
            return Some(Self::Metrics);
        }
        const REPEAT: usize = 32;
        let span: HtmlElement = document.create_element("span").ok()?.dyn_into().ok()?;
        let _ = span.class_list().add_1("xterm-char-measure-element");
        span.set_text_content(Some(&"W".repeat(REPEAT)));
        let _ = span.set_attribute("aria-hidden", "true");
        let _ = span.style().set_property("white-space", "pre");
        let _ = span.style().set_property("font-kerning", "none");
        parent.append_child(&span).ok()?;
        Some(Self::Dom(span))
    }

    fn measure(&self, options: &RendererOptions) -> Option<(f64, f64)> {
        let (width, height) = match self {
            Self::Metrics => METRICS.with(|ctx| {
                let ctx = ctx.as_ref()?;
                ctx.set_font(&format!("{}px {}", options.font_size, options.font_family));
                let m = ctx.measure_text("W").ok()?;
                Some((
                    m.width(),
                    m.font_bounding_box_ascent() + m.font_bounding_box_descent(),
                ))
            })?,
            Self::Dom(span) => {
                let style = span.style();
                let _ = style.set_property("font-family", &options.font_family);
                let _ = style.set_property("font-size", &format!("{}px", options.font_size));
                // Note that this triggers a synchronous layout
                (
                    span.offset_width() as f64 / 32.0,
                    span.offset_height() as f64,
                )
            }
        };
        // Zero while the element is display:none: keep the last size then
        (width > 0.0 && height > 0.0).then_some((width, height))
    }
}

impl Drop for Measure {
    fn drop(&mut self) {
        if let Self::Dom(span) = self {
            span.remove();
        }
    }
}

fn visibility_observer() -> Option<IntersectionObserver> {
    let callback = Closure::<dyn FnMut(Array)>::new(|entries: Array| {
        for entry in entries.iter() {
            let Some(entry) = entry.dyn_ref::<IntersectionObserverEntry>() else {
                continue;
            };
            // A pane clipped to nothing (a zero-size or scrolled-away parent) counts as out of view
            let (target, visible) = (entry.target(), entry.intersection_ratio() > 0.0);
            for pane in panes() {
                let weak = Rc::downgrade(&pane);
                let is_target = pane
                    .try_borrow()
                    .is_ok_and(|p| AsRef::<web_sys::Element>::as_ref(&p.container) == &target);
                if is_target {
                    with_inner(&weak, move |i| i.handle_visibility(visible));
                }
            }
        }
    });
    // Lives as long as the page; the GC owns the closure
    let callback: Function = callback.into_js_value().unchecked_into();
    IntersectionObserver::new(&callback).ok()
}

/// The font and window listeners every pane shares, registered with the first pane for the page's lifetime.
fn listen_page(window: &web_sys::Window, document: &Document) {
    if LISTENING.with(|l| l.replace(true)) {
        return;
    }
    let fonts_loaded = Closure::<dyn FnMut(Event)>::new(|_| {
        each_pane(Inner::handle_fonts_loaded);
    });
    let _ = document.fonts().add_event_listener_with_callback(
        "loadingdone",
        &fonts_loaded.into_js_value().unchecked_into(),
    );
    let resize = Closure::<dyn FnMut(Event)>::new(|_| {
        each_pane(|i| {
            i.check_device_pixel_ratio();
        });
    });
    let _ =
        window.add_event_listener_with_callback("resize", &resize.into_js_value().unchecked_into());
}

fn panes() -> Vec<Rc<RefCell<Inner>>> {
    PANES.with(|p| p.borrow().iter().filter_map(Weak::upgrade).collect())
}

fn each_pane(f: fn(&mut Inner)) {
    for pane in panes() {
        with_inner(&Rc::downgrade(&pane), f);
    }
}

/// Every pane redraws everything (the shared context was restored or made anew).
pub(super) fn redraw_all() {
    each_pane(Inner::request_redraw_all);
}

struct Inner {
    id: u32,
    me: Weak<RefCell<Inner>>,
    document: Document,
    /// `.xterm-screen`: the canvas goes in here, and atlas glyphs are drawn inside it.
    container: HtmlElement,
    cols: u32,
    rows: u32,
    options: RendererOptions,
    colors: ColorSet,
    measure: Option<Measure>,
    char_size: (f64, f64),
    dims: Dimensions,
    dpr: f64,
    /// The pane's canvas on the page and how frames reach it.
    canvas: HtmlCanvasElement,
    present: Present,
    rects: RectVertices,
    glyphs: GlyphVertices,
    images: ImageLayer,
    atlas: Option<Rc<RefCell<TextureAtlas>>>,
    model: RenderModel,
    cell: CellData,
    selection: SelectionModel,
    /// Named layers in the order they were first set.
    highlights: Vec<(String, Vec<Highlight>)>,
    resolved_highlights: Vec<ResolvedHighlight>,
    overlay: Vec<Rect>,
    /// The highlights or the cell size changed since they were resolved.
    highlights_stale: bool,
    focused: bool,
    blink: Option<Blink>,
    /// The cursor drawn shown, over the canvas drawn with it hidden, while it blinks.
    cursor_layer: Option<CursorLayer>,
    cursor: crate::engine::Cursor,
    cursor_row: i64,
    /// Resolved background of the last cell drawn (glyphs are clipped where it changes).
    last_bg: u32,
    full_redraw: bool,
    dirty: BTreeSet<u32>,
    visible: bool,
    disposed: bool,
    draws: u32,
    layer_draws: u32,
    listeners: Vec<Listener>,
    requests: Vec<RendererRequest>,
    log: Vec<RendererRequest>,
    wake: Rc<dyn Fn()>,
}

pub struct WebglRenderer {
    inner: Rc<RefCell<Inner>>,
}

impl WebglRenderer {
    /// Puts a canvas into `container` (`.xterm-screen`); `element` (`.xterm`) tells focus. `wake` asks the
    /// terminal for a frame; it must only schedule one.
    pub fn new(
        container: &HtmlElement,
        element: &HtmlElement,
        cols: u32,
        rows: u32,
        options: &RendererOptions,
        wake: Rc<dyn Fn()>,
    ) -> Result<Self, JsValue> {
        let document = container
            .owner_document()
            .ok_or("oxtty/xterm: the element is not in a document")?;
        let window = document
            .default_view()
            .ok_or("oxtty/xterm: the document has no window")?;
        gpu::ensure(&document)?;
        let canvas: HtmlCanvasElement = document.create_element("canvas")?.unchecked_into();
        let present = Present::new(&canvas).ok_or("oxtty/xterm: the canvas has no context")?;
        container.append_child(&canvas)?;
        let measure = Measure::new(&document, container);
        let focused = document
            .active_element()
            .is_some_and(|active| element.contains(Some(&active)));
        let colors = ColorSet::new(&options.theme);
        let dims = Dimensions::default();
        let inner = Rc::new(RefCell::new(Inner {
            id: NEXT_ID.with(|n| n.replace(n.get() + 1)),
            me: Weak::new(),
            document: document.clone(),
            container: container.clone(),
            cols,
            rows,
            options: options.clone(),
            rects: RectVertices::new(cols, rows, dims, &colors),
            colors,
            measure,
            char_size: (0.0, 0.0),
            dims,
            dpr: window.device_pixel_ratio(),
            canvas,
            present,
            glyphs: GlyphVertices::new(cols, rows, dims),
            images: ImageLayer::default(),
            atlas: None,
            model: RenderModel::default(),
            cell: CellData::default(),
            selection: SelectionModel::default(),
            highlights: Vec::new(),
            resolved_highlights: Vec::new(),
            overlay: Vec::new(),
            highlights_stale: false,
            focused,
            blink: None,
            cursor_layer: None,
            cursor: crate::engine::Cursor::default(),
            cursor_row: -1,
            last_bg: 0,
            full_redraw: true,
            dirty: BTreeSet::new(),
            visible: true,
            disposed: false,
            draws: 0,
            layer_draws: 0,
            listeners: Vec::new(),
            requests: Vec::new(),
            log: Vec::new(),
            wake,
        }));
        PANES.with(|p| p.borrow_mut().push(Rc::downgrade(&inner)));
        listen_page(&window, &document);
        VISIBILITY.with(|v| {
            if let Some(v) = v {
                v.observe(container);
            }
        });
        {
            let mut i = inner.borrow_mut();
            i.me = Rc::downgrade(&inner);
            i.glyphs.rescale_overlapping_glyphs = options.rescale_overlapping_glyphs;
            i.measure_chars();
            i.handle_resize();
            i.update_cursor_blink();
            i.listen(element);
        }
        Ok(Self { inner })
    }
}

impl Drop for WebglRenderer {
    fn drop(&mut self) {
        if let Ok(mut inner) = self.inner.try_borrow_mut() {
            inner.dispose();
        }
        let me = Rc::downgrade(&self.inner);
        let empty = PANES.with(|p| {
            let Ok(mut p) = p.try_borrow_mut() else {
                return false;
            };
            p.retain(|w| w.strong_count() > 0 && !Weak::ptr_eq(w, &me));
            p.is_empty()
        });
        // The last terminal frees the context
        if empty {
            gpu::release();
        }
    }
}

/// Runs `f` on the renderer behind `weak`, on a later task if it is borrowed now; nothing if it is gone.
fn with_inner(weak: &Weak<RefCell<Inner>>, f: impl FnOnce(&mut Inner) + 'static) {
    let Some(rc) = weak.upgrade() else { return };
    let Ok(mut inner) = rc.try_borrow_mut() else {
        let weak = weak.clone();
        timer::queue(move || with_inner(&weak, f));
        return;
    };
    if !inner.disposed {
        f(&mut inner);
    }
}

fn metrics_context() -> Option<OffscreenCanvasRenderingContext2d> {
    let canvas = web_sys::OffscreenCanvas::new(1, 1).ok()?;
    let ctx: OffscreenCanvasRenderingContext2d = canvas.get_context("2d").ok()??.dyn_into().ok()?;
    let m = ctx.measure_text("W").ok()?;
    let has = |key: &str| Reflect::has(&m, &key.into()).unwrap_or(false);
    (has("width") && has("fontBoundingBoxAscent") && has("fontBoundingBoxDescent")).then_some(ctx)
}

impl Inner {
    /// Focus picks the cursor style and runs the blink; the textarea that takes focus is inside `element`.
    fn listen(&mut self, element: &HtmlElement) {
        let w = self.me.clone();
        let focus_in =
            Closure::<dyn FnMut(Event)>::new(move |_| with_inner(&w, |i| i.set_focused(true)));
        self.listeners
            .push(Listener::new(element, "focusin", focus_in));
        let w = self.me.clone();
        let el = element.clone();
        let focus_out = Closure::<dyn FnMut(Event)>::new(move |e: Event| {
            let inside = e
                .dyn_ref::<FocusEvent>()
                .and_then(FocusEvent::related_target)
                .and_then(|t| t.dyn_into::<web_sys::Node>().ok())
                .is_some_and(|node| el.contains(Some(&node)));
            with_inner(&w, move |i| i.set_focused(inside));
        });
        self.listeners
            .push(Listener::new(element, "focusout", focus_out));
    }

    fn handle_visibility(&mut self, visible: bool) {
        if visible == self.visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.request_redraw_all();
        } else {
            self.cursor_layer = None;
        }
    }

    fn handle_resize(&mut self) {
        self.update_dimensions();
        self.highlights_stale = true;
        self.model.resize(self.cols, self.rows);
        let d = self.dims;
        self.canvas.set_width(d.device_canvas.width as u32);
        self.canvas.set_height(d.device_canvas.height as u32);
        let style = self.canvas.style();
        let _ = style.set_property("width", &format!("{}px", d.css_canvas.width));
        let _ = style.set_property("height", &format!("{}px", d.css_canvas.height));
        self.rects.resize(self.cols, self.rows, d);
        self.glyphs.resize(self.cols, self.rows, d);
        self.refresh_char_atlas();
        // Resizing the glyph vertices cleared them already
        self.clear_model(false);
    }

    fn dispose(&mut self) {
        if self.disposed {
            return;
        }
        self.disposed = true;
        self.blink = None;
        self.cursor_layer = None;
        self.listeners.clear();
        VISIBILITY.with(|v| {
            if let Some(v) = v {
                v.unobserve(&self.container);
            }
        });
        atlas::release(self.id);
        self.atlas = None;
        self.glyphs.set_atlas(None);
        self.canvas.remove();
        self.measure = None;
    }

    fn measure_chars(&mut self) {
        if let Some(size) = self.measure.as_ref().and_then(|m| m.measure(&self.options)) {
            self.char_size = size;
        }
    }
    fn request(&mut self, request: RendererRequest) {
        if self.log.len() >= LOG_LIMIT {
            self.log.remove(0);
        }
        self.log.push(request.clone());
        self.requests.push(request);
        (self.wake)();
    }
    fn request_redraw_all(&mut self) {
        self.full_redraw = true;
        self.request(RendererRequest::Redraw(Dirty::All));
    }
    fn request_redraw_rows(&mut self, rows: impl IntoIterator<Item = i64>) {
        let rows: Vec<u32> = rows
            .into_iter()
            .filter(|&y| y >= 0)
            .map(|y| y as u32)
            .collect();
        if rows.is_empty() {
            return;
        }
        self.dirty.extend(&rows);
        self.request(RendererRequest::Redraw(Dirty::Rows(rows)));
    }
    fn clear_model(&mut self, clear_glyphs: bool) {
        self.model.clear();
        if clear_glyphs {
            self.glyphs.clear();
        }
        self.full_redraw = true;
    }
    fn update_dimensions(&mut self) {
        let (char_w, char_h) = self.char_size;
        if char_w <= 0.0 || char_h <= 0.0 {
            return;
        }
        let (line_height, letter_spacing) = (self.options.line_height, self.options.letter_spacing);
        let dpr = self.dpr;
        let d = &mut self.dims;
        // Floored so atlas glyphs land on an integer grid and stay sharp
        d.device_char.width = (char_w * dpr).floor();
        // Ceiled so a fractional devicePixelRatio still leaves room for the glyph
        d.device_char.height = (char_h * dpr).ceil();
        d.device_cell.height = (d.device_char.height * line_height).floor();
        d.device_char.top = if line_height == 1.0 {
            0.0
        } else {
            super::color::js_round((d.device_cell.height - d.device_char.height) / 2.0)
        };
        d.device_cell.width = d.device_char.width + super::color::js_round(letter_spacing);
        d.device_char.left = (letter_spacing / 2.0).floor();
        d.device_canvas.height = self.rows as f64 * d.device_cell.height;
        d.device_canvas.width = self.cols as f64 * d.device_cell.width;
        // Rounded, not ceiled: devicePixelRatio is often slightly off (1.100000023841858)
        d.css_canvas.height = super::color::js_round(d.device_canvas.height / dpr);
        d.css_canvas.width = super::color::js_round(d.device_canvas.width / dpr);
        d.css_cell.height = d.device_cell.height / dpr;
        d.css_cell.width = d.device_cell.width / dpr;
    }
    fn refresh_char_atlas(&mut self) {
        let d = self.dims;
        if d.device_char.width <= 0.0 && d.device_char.height <= 0.0 {
            return;
        }
        let o = &self.options;
        let config = AtlasConfig {
            custom_glyphs: o.custom_glyphs,
            device_pixel_ratio: self.dpr,
            device_max_texture_size: atlas::max_texture_size(),
            letter_spacing: o.letter_spacing,
            line_height: o.line_height,
            font_size: o.font_size,
            font_family: o.font_family.clone(),
            font_weight: o.font_weight.clone(),
            font_weight_bold: o.font_weight_bold.clone(),
            device_cell_width: d.device_cell.width,
            device_cell_height: d.device_cell.height,
            device_char_width: d.device_char.width,
            device_char_height: d.device_char.height,
            allow_transparency: o.allow_transparency,
            draw_bold_text_in_bright_colors: o.draw_bold_text_in_bright_colors,
            minimum_contrast_ratio: o.minimum_contrast_ratio,
            foreground: self.colors.foreground.clone(),
            background: self.colors.background.clone(),
            ansi: self.colors.ansi.clone(),
        };
        self.atlas = atlas::acquire(self.id, config, &self.document);
        self.glyphs.set_atlas(self.atlas.clone());
    }
    fn handle_fonts_loaded(&mut self) {
        let before = self.dims.css_cell;
        self.measure_chars();
        self.handle_resize();
        // Glyphs drawn before the font arrived are fallback glyphs
        if let Some(Ok(mut a)) = self.atlas.as_ref().map(|a| a.try_borrow_mut()) {
            a.clear_texture();
        }
        self.clear_model(true);
        if before != self.dims.css_cell {
            self.request(RendererRequest::DimensionsChanged);
        }
        self.request_redraw_all();
    }
    /// Zoom and moving to another screen change the canvas's device pixels; zoom also resizes the window.
    fn check_device_pixel_ratio(&mut self) -> bool {
        let dpr = self
            .document
            .default_view()
            .map_or(self.dpr, |w| w.device_pixel_ratio());
        if dpr == self.dpr {
            return false;
        }
        // New cell size and atlas
        self.dpr = dpr;
        self.handle_resize();
        self.request(RendererRequest::DimensionsChanged);
        self.request_redraw_all();
        true
    }
    fn cursor_style(&self) -> CursorInactiveStyle {
        // Until the program sends DECSCUSR (or after `CSI 0 SP q`) the options decide
        if self.cursor.styled {
            return match self.cursor.shape {
                CursorShape::Block => CursorInactiveStyle::Block,
                CursorShape::Underline => CursorInactiveStyle::Underline,
                CursorShape::Bar => CursorInactiveStyle::Bar,
            };
        }
        match self.options.cursor_style {
            CursorStyle::Block => CursorInactiveStyle::Block,
            CursorStyle::Underline => CursorInactiveStyle::Underline,
            CursorStyle::Bar => CursorInactiveStyle::Bar,
        }
    }
    fn update_cursor_blink(&mut self) {
        let blink = if self.cursor.styled {
            self.cursor.blink
        } else {
            self.options.cursor_blink
        };
        if blink == self.blink.is_some() {
            return;
        }
        self.blink = blink.then(|| {
            let weak = self.me.clone();
            Blink::new(move || with_inner(&weak, Inner::handle_blink), self.focused)
        });
        let row = self.cursor_row;
        self.request_redraw_rows([row]);
    }
    /// A blink tick: the cursor layer shows or hides, or without one the cursor row redraws.
    fn handle_blink(&mut self) {
        let shown = self.blink.as_ref().is_none_or(Blink::is_cursor_visible);
        match &mut self.cursor_layer {
            Some(layer) => layer.show(shown),
            None => {
                let row = self.cursor_row;
                self.request_redraw_rows([row]);
            }
        }
    }
    /// Makes or drops the cursor layer for a cursor in viewport row `row`; true when it is new.
    fn update_cursor_layer(&mut self, row: Option<u32>) -> bool {
        let wanted = row.is_some()
            && self.focused
            && self.blink.is_some()
            && is_opaque(&self.colors.background);
        if !wanted {
            self.cursor_layer = None;
            return false;
        }
        if self.cursor_layer.is_some() {
            return false;
        }
        self.cursor_layer = CursorLayer::new(&self.document, &self.canvas);
        self.cursor_layer.is_some()
    }
    fn set_focused(&mut self, focused: bool) {
        if self.focused == focused {
            return;
        }
        self.focused = focused;
        if let Some(blink) = &self.blink {
            if focused {
                blink.resume();
            } else {
                blink.pause();
            }
        }
        // The cursor style and the selection colour both depend on focus
        self.request_redraw_all();
    }
    fn resolve_highlights(&mut self) {
        let mut resolved = Vec::new();
        let mut overlay = Vec::new();
        let d = self.dims;
        let (cw, ch) = (d.device_cell.width, d.device_cell.height);
        let line = self.dpr.max(1.0);
        for (_, layer) in &self.highlights {
            for h in layer {
                if h.row >= self.rows || h.start >= h.end {
                    continue;
                }
                let end = h.end.min(self.cols);
                let color = |c: &Option<String>| c.as_deref().and_then(css_to_color);
                let background = color(&h.background);
                let foreground = color(&h.foreground);
                resolved.push(ResolvedHighlight {
                    row: h.row,
                    start: h.start,
                    end,
                    background: background.as_ref().map(|c| c.rgba >> 8),
                    foreground: foreground.as_ref().map(|c| c.rgba >> 8),
                });
                let (x, y, w) = (
                    h.start as f64 * cw,
                    h.row as f64 * ch,
                    (end - h.start) as f64 * cw,
                );
                let rect = |x, y, width, height, color| Rect {
                    x,
                    y,
                    width,
                    height,
                    color,
                };
                if let Some(border) = color(&h.border) {
                    let c = css_floats(&super::color::opaque(&border));
                    overlay.push(rect(x, y, w, line, c));
                    overlay.push(rect(x, y + ch - line, w, line, c));
                    overlay.push(rect(x, y, line, ch, c));
                    overlay.push(rect(x + w - line, y, line, ch, c));
                }
                if h.underline {
                    let c = foreground
                        .as_ref()
                        .or(color(&h.border).as_ref())
                        .map_or_else(
                            || css_floats(&self.colors.foreground),
                            |c| css_floats(&super::color::opaque(c)),
                        );
                    overlay.push(rect(x, y + ch - line, w, line, c));
                }
            }
        }
        self.resolved_highlights = resolved;
        self.overlay = overlay;
        self.highlights_stale = false;
    }
    fn render(&mut self, viewport: &Viewport, changed: &[u32], host_dirty: &Dirty) {
        let (cols, rows) = (self.cols, self.rows);
        if viewport.cols != cols || viewport.rows != rows {
            return;
        }
        // Out of view, or no context: nothing is drawn, everything is when it can be again
        if !self.visible || !gpu::is_live() || self.dims.device_canvas.width < 1.0 {
            self.full_redraw = true;
            return;
        }
        let cursor = viewport.cursor;
        let moved = cursor.line != self.cursor.line || cursor.col != self.cursor.col;
        if moved {
            if let Some(blink) = &self.blink {
                blink.restart_blink_animation();
            }
        }
        let blink_changed = cursor.shape != self.cursor.shape
            || cursor.blink != self.cursor.blink
            || cursor.styled != self.cursor.styled;
        // What the cursor layer shows changed
        let mut layer_stale = moved || blink_changed || cursor.visible != self.cursor.visible;
        self.cursor = cursor;
        if blink_changed {
            self.update_cursor_blink();
        }

        let mut all = self.full_redraw || *host_dirty == Dirty::All;
        // The atlas merged pages (or there is none): rebuild the whole model
        if self.glyphs.begin_frame() {
            self.clear_model(true);
            all = true;
        }
        self.full_redraw = false;
        let lines: Vec<u32> = if all {
            (0..rows).collect()
        } else {
            let mut set = std::mem::take(&mut self.dirty);
            set.extend(changed);
            if let Dirty::Rows(r) = host_dirty {
                set.extend(r);
            }
            if cursor.line >= 0 {
                let around = CursorLayer::rows_around(cursor.line as u32);
                layer_stale |= set.range(around).next().is_some();
            }
            // The rows the cursor left and entered (the engine does not report cursor moves as damage)
            if self.cursor_row >= 0 {
                set.insert(self.cursor_row as u32);
            }
            if cursor.line >= 0 {
                set.insert(cursor.line as u32);
            }
            set.into_iter().filter(|&y| y < rows).collect()
        };
        self.dirty.clear();
        self.cursor_row = if cursor.visible {
            cursor.line as i64
        } else {
            -1
        };
        if self.highlights_stale {
            self.resolve_highlights();
        }
        self.images.update(viewport, &self.dims, self.dpr);
        let in_view = cursor.visible && cursor.line >= 0 && (cursor.line as u32) < rows;
        let cursor_row = in_view.then_some(cursor.line as u32);
        layer_stale |= all | self.update_cursor_layer(cursor_row);
        let blink_shown = self.blink.as_ref().is_none_or(Blink::is_cursor_visible);
        let (d, bar) = (self.dims, self.dpr * self.options.cursor_width);
        let layer = self
            .cursor_layer
            .as_mut()
            .zip(cursor_row)
            .map(|(layer, row)| {
                let col = cursor.col.min(cols.saturating_sub(1));
                let cell = (d.device_cell.width, d.device_cell.height);
                let region = CursorLayer::region_for(col, row, (cols, rows), cell, bar);
                let scale = (
                    d.css_canvas.width / d.device_canvas.width,
                    d.css_canvas.height / d.device_canvas.height,
                );
                layer.place(region, scale);
                (region, row)
            });

        match layer {
            Some((region, row)) if layer_stale => {
                // The layer gets the frame with the cursor shown, the canvas the cursor row again without it
                self.update_model(viewport, &lines, true);
                if self.draw(true, Some(region)) {
                    self.layer_draws = self.layer_draws.wrapping_add(1);
                } else {
                    self.full_redraw = true;
                }
                self.update_model(viewport, &[row], false);
            }
            _ => self.update_model(viewport, &lines, layer.is_none() && blink_shown),
        }
        if self.draw(layer.is_none() && blink_shown, None) {
            self.draws = self.draws.wrapping_add(1);
        } else {
            self.full_redraw = true;
        }
        if let Some(layer) = &mut self.cursor_layer {
            layer.show(blink_shown);
        }
    }
    /// Draws the model into the pane's canvas, or `region` of it into the cursor layer.
    fn draw(&self, cursor: bool, region: Option<Region>) -> bool {
        let present = match (region, &self.cursor_layer) {
            (None, _) => &self.present,
            (Some(_), Some(layer)) => &layer.present,
            (Some(_), None) => return false,
        };
        gpu::draw(gpu::Frame {
            pane: self.id,
            width: self.dims.device_canvas.width,
            height: self.dims.device_canvas.height,
            rects: &self.rects,
            glyphs: &self.glyphs,
            model: &self.model,
            cursor,
            images: &self.images,
            present,
            region,
        })
    }
    /// `WebglRenderer._updateModel` minus character joiners, reading cells from the viewport; `cursor_shown` is
    /// the blink phase.
    fn update_model(&mut self, viewport: &Viewport, lines: &[u32], cursor_shown: bool) {
        let cols = self.cols as usize;
        let cursor = viewport.cursor;
        let cursor_x = cursor.col.min(self.cols.saturating_sub(1));
        let cursor_style = if self.focused {
            self.cursor_style()
        } else {
            self.options.cursor_inactive_style
        };
        let cursor_visible = cursor.visible && cursor_shown;
        let mut last_cursor_x: i64 = -1;
        let mut model_updated = false;
        self.model.cursor = None;
        let stride = cols * 4;
        let mut extras: Vec<Option<usize>> = vec![None; cols];
        for &y in lines {
            let start = y as usize * stride;
            let Some(row) = viewport.cells.get(start..start + stride) else {
                continue;
            };
            let row_extras = viewport.extras.get(&y);
            extras.fill(None);
            if let Some(list) = row_extras {
                for (k, e) in list.iter().enumerate() {
                    if let Some(slot) = extras.get_mut(e.col as usize) {
                        *slot = Some(k);
                    }
                }
            }
            if let Some(len) = self.model.line_lengths.get_mut(y as usize) {
                *len = 0;
            }
            for (x, extra) in extras.iter().enumerate() {
                let last_bg = self.last_bg;
                let o = x * 4;
                let cell_words = &row[o..o + 4];
                if cell_words[3] & attr::WIDE != 0 {
                    atlas::record_wide(cell_words[0]);
                }
                let extra = extra.and_then(|k| row_extras.and_then(|l| l.get(k)));
                self.cell.load(
                    cell_words,
                    extra.and_then(|e| e.text.as_deref()),
                    extra.and_then(|e| e.underline_color).unwrap_or(0),
                );
                let mut code = self.cell.code;
                let i = (y as usize * cols + x) * RENDER_MODEL_INDICIES_PER_CELL;
                let mut result = resolve(
                    &self.cell,
                    x as u32,
                    y,
                    self.dims.device_cell.width,
                    &self.colors,
                    &self.selection,
                    &self.resolved_highlights,
                    self.focused,
                    self.options.font_size,
                    self.dpr,
                );

                // The cursor cell's colours
                if cursor_visible && y as i32 == cursor.line {
                    if x as u32 == cursor_x {
                        self.model.cursor = Some(CursorModel {
                            x: cursor_x,
                            y,
                            width: self.cell.width,
                            style: cursor_style,
                            cursor_width: self.options.cursor_width,
                            dpr: self.dpr,
                        });
                        last_cursor_x = cursor_x as i64 + self.cell.width as i64 - 1;
                    }
                    if cursor_style == CursorInactiveStyle::Block
                        && x as i64 >= cursor_x as i64
                        && x as i64 <= last_cursor_x
                    {
                        result.fg = CM_RGB | ((self.colors.cursor_accent.rgba >> 8) & RGB_MASK);
                        result.bg = CM_RGB | ((self.colors.cursor.rgba >> 8) & RGB_MASK);
                    }
                }
                self.last_bg = result.bg;

                if code != 0 {
                    if let Some(len) = self.model.line_lengths.get_mut(y as usize) {
                        *len = x as u32 + 1;
                    }
                }
                let Some(model) = self.model.cells.get_mut(i..i + 4) else {
                    continue;
                };
                // Nothing changed
                if model[0] == code
                    && model[RENDER_MODEL_BG_OFFSET] == result.bg
                    && model[RENDER_MODEL_FG_OFFSET] == result.fg
                    && model[RENDER_MODEL_EXT_OFFSET] == result.ext
                {
                    continue;
                }
                model_updated = true;
                let len16 = self.cell.utf16_len();
                // Combined characters are flagged so they are easily identified
                if len16 > 1 {
                    code |= COMBINED_CHAR_BIT_MASK;
                }
                model[0] = code;
                model[RENDER_MODEL_BG_OFFSET] = result.bg;
                model[RENDER_MODEL_FG_OFFSET] = result.fg;
                model[RENDER_MODEL_EXT_OFFSET] = result.ext;
                {
                    self.glyphs.update_cell(
                        x,
                        y as usize,
                        code,
                        result.bg,
                        result.fg,
                        result.ext,
                        &self.cell.chars,
                        len16,
                        self.cell.width,
                        last_bg,
                        Some(&self.container),
                    );
                }
            }
        }
        if model_updated {
            self.rects.update_backgrounds(&self.model, &self.colors);
        }
        self.rects.update_cursor(self.model.cursor.as_ref());
        self.rects.update_overlay(&self.overlay);
    }
    fn set_options(&mut self, options: &RendererOptions) {
        let old = std::mem::replace(&mut self.options, options.clone());
        if old == *options {
            return;
        }
        if old.theme != options.theme {
            self.colors = ColorSet::new(&options.theme);
            self.highlights_stale = true;
            self.rects.update_colors(&self.colors);
        }
        if old.font_family != options.font_family || old.font_size != options.font_size {
            self.measure_chars();
        }
        self.glyphs.rescale_overlapping_glyphs = options.rescale_overlapping_glyphs;
        if old.font_family != options.font_family
            || old.font_size != options.font_size
            || old.line_height != options.line_height
            || old.letter_spacing != options.letter_spacing
        {
            self.handle_resize();
        } else {
            self.refresh_char_atlas();
            self.clear_model(true);
        }
        self.update_cursor_blink();
        self.request_redraw_all();
    }
    fn resize(&mut self, cols: u32, rows: u32) {
        self.cols = cols;
        self.rows = rows;
        self.handle_resize();
    }
    fn set_selection(&mut self, selection: Option<SelectionRange>) {
        let mut rows = BTreeSet::new();
        let mut add = |m: &SelectionModel| {
            if m.has_selection {
                rows.extend(m.capped_start_row..=m.capped_end_row);
            }
        };
        add(&self.selection);
        self.selection.update(self.rows, selection);
        add(&self.selection);
        self.request_redraw_rows(rows);
    }
    fn set_highlights(&mut self, layer: &str, highlights: Vec<Highlight>) {
        let mut rows: BTreeSet<i64> = BTreeSet::new();
        rows.extend(highlights.iter().map(|h| h.row as i64));
        match self.highlights.iter_mut().find(|(name, _)| name == layer) {
            Some((_, old)) => {
                rows.extend(old.iter().map(|h| h.row as i64));
                *old = highlights;
            }
            None => self.highlights.push((layer.to_string(), highlights)),
        }
        self.highlights_stale = true;
        self.request_redraw_rows(rows);
    }
    /// State for the tests; the requests are those since the last call.
    fn debug(&mut self) -> JsValue {
        let info = Object::new();
        let (state, gl, gpu_draws) = gpu::debug();
        let set = |k: &str, v: JsValue| {
            let _ = Reflect::set(&info, &k.into(), &v);
        };
        set("state", state.into());
        set(
            "atlas",
            self.atlas
                .as_ref()
                .and_then(|a| a.try_borrow().ok().map(|a| a.id))
                .map_or(JsValue::NULL, JsValue::from),
        );
        set("liveAtlases", atlas::live_atlases().into());
        set("canvas", self.canvas.clone().into());
        set("focused", self.focused.into());
        set("visible", self.visible.into());
        set("draws", self.draws.into());
        set("layerDraws", self.layer_draws.into());
        let layer = self.cursor_layer.as_ref();
        set(
            "cursorLayer",
            layer.map_or(JsValue::NULL, |l| l.canvas().clone().into()),
        );
        set(
            "cursorLayerRegion",
            layer.map_or(JsValue::NULL, |l| {
                l.region()
                    .iter()
                    .map(|&v| JsValue::from(v))
                    .collect::<Array>()
                    .into()
            }),
        );
        set("gpuDraws", gpu_draws.into());
        set("gl", gl);
        let requests: Array = std::mem::take(&mut self.log)
            .iter()
            .map(|r| match r {
                RendererRequest::DimensionsChanged => JsValue::from("dimensions"),
                RendererRequest::Redraw(Dirty::All) => JsValue::from("all"),
                RendererRequest::Redraw(Dirty::Rows(rows)) => rows
                    .iter()
                    .map(|&y| JsValue::from(y))
                    .collect::<Array>()
                    .into(),
            })
            .collect();
        set("requests", requests.into());
        info.into()
    }
}

impl Renderer for WebglRenderer {
    fn dimensions(&self) -> Dimensions {
        self.inner.try_borrow().map(|i| i.dims).unwrap_or_default()
    }

    fn set_options(&mut self, options: &RendererOptions) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_options(options);
        }
    }

    fn resize(&mut self, cols: u32, rows: u32) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.resize(cols, rows);
        }
    }

    fn render(&mut self, viewport: &Viewport, changed: &[u32], dirty: &Dirty) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.render(viewport, changed, dirty);
        }
    }

    fn set_selection(&mut self, selection: Option<SelectionRange>) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_selection(selection);
        }
    }

    fn set_highlights(&mut self, layer: &str, highlights: Vec<Highlight>) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_highlights(layer, highlights);
        }
    }

    fn set_focused(&mut self, focused: bool) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.set_focused(focused);
        }
    }

    fn take_requests(&mut self) -> Vec<RendererRequest> {
        self.inner
            .try_borrow_mut()
            .map(|mut i| std::mem::take(&mut i.requests))
            .unwrap_or_default()
    }

    fn set_image_placeholder(&mut self, show: bool) {
        if let Ok(mut i) = self.inner.try_borrow_mut() {
            i.images.placeholder = show;
        }
    }

    fn debug(&self) -> JsValue {
        self.inner
            .try_borrow_mut()
            .map(|mut i| i.debug())
            .unwrap_or(JsValue::UNDEFINED)
    }
}
