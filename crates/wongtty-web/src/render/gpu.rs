//! The page's one WebGL2 context, shared by every terminal.
//!
//! A context per pane would hit the browser's limit (16 in Chrome) at the 17th terminal and multiply GPU memory,
//! so the page keeps one, on an `OffscreenCanvas` when the browser has one with WebGL2, else on a canvas that is
//! never attached. A pane draws its whole picture into it at its own size and hands the result to its own canvas
//! on the page, which keeps the last frame without redrawing. Programs, buffers and atlas textures exist once; a
//! pane keeps only its vertex data.
//!
//! On a GPU, the frame is handed over without a copy: `transferToImageBitmap` on the `OffscreenCanvas` and
//! `transferFromImageBitmap` into the pane's `bitmaprenderer` canvas ([`Present`]). Against `drawImage` into a 2D
//! canvas this saves the GPU process a copy of every frame and the buffers Chromium keeps behind a 2D canvas that
//! keeps drawing (15 panes of 120×40 printing logs: GPU process 446 → 320 MB); once every pane idles, the surface
//! shrinks to a pixel ([`trim`]). A region (the cursor layer) is drawn as a frame of its own, the pane's picture
//! shifted by the viewport. Software GL (SwiftShader) and a surface that is not an `OffscreenCanvas` keep the copy
//! into a 2D canvas.
//!
//! Chrome crashes the page's renderer when a WebGL context is lost on purpose (`WEBGL_lose_context`, or Chrome
//! dropping the oldest context once a page has more than 16) between a transfer and the commit of that frame; a
//! lost GPU process is fine. [`release`] loses the context only after every pane's canvas is gone, and the test
//! suite's context-loss tests run on SwiftShader, which copies.
//!
//! Context loss is handled here once for every pane: a lost context waits [`CONTEXT_RESTORE_TIMEOUT`] for
//! `webglcontextrestored`, then a new one is made under a back-off throttle; either way every pane redraws.
//!
//! State is behind a page-wide `RefCell`, borrowed only for a draw or an event; listeners retry on a later task
//! when it is busy and never call into a terminal (they ask panes for redraws, which queue requests).
use std::cell::RefCell;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{
    CanvasRenderingContext2d, Document, Event, EventTarget, HtmlCanvasElement, ImageBitmap,
    OffscreenCanvas, WebGl2RenderingContext as Gl, WebGlContextAttributes, WebGlTexture,
    WebglLoseContext,
};

use super::atlas::{self, TextureAtlas};
use super::cursor_layer::Region;
use super::glyphs::{GlyphProgram, GlyphVertices};
use super::images::{self, ImageLayer, ImageProgram};
use super::model::RenderModel;
use super::rects::{RectProgram, RectVertices};
use crate::timer::{self, Timeout};

/// How long a lost context may take to come back by itself before a new one is made (as in xterm.js).
const CONTEXT_RESTORE_TIMEOUT: i32 = 3000;

/// A DOM listener, removed when dropped.
pub(super) struct Listener {
    target: EventTarget,
    kind: &'static str,
    closure: Closure<dyn FnMut(Event)>,
}

impl Listener {
    pub fn new(
        target: &EventTarget,
        kind: &'static str,
        closure: Closure<dyn FnMut(Event)>,
    ) -> Self {
        let _ = target.add_event_listener_with_callback(kind, closure.as_ref().unchecked_ref());
        Self {
            target: target.clone(),
            kind,
            closure,
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        let _ = self
            .target
            .remove_event_listener_with_callback(self.kind, self.closure.as_ref().unchecked_ref());
    }
}

#[derive(Clone)]
enum Surface {
    Offscreen(OffscreenCanvas),
    Element(HtmlCanvasElement),
}

impl Surface {
    fn target(&self) -> &EventTarget {
        match self {
            Self::Offscreen(c) => c,
            Self::Element(c) => c,
        }
    }

    fn size(&self) -> (u32, u32) {
        match self {
            Self::Offscreen(c) => (c.width(), c.height()),
            Self::Element(c) => (c.width(), c.height()),
        }
    }

    fn set_size(&self, width: u32, height: u32) {
        // Setting even the same size reallocates the drawing buffer
        if self.size() == (width, height) {
            return;
        }
        match self {
            Self::Offscreen(c) => {
                c.set_width(width);
                c.set_height(height);
            }
            Self::Element(c) => {
                c.set_width(width);
                c.set_height(height);
            }
        }
    }
}

#[wasm_bindgen]
extern "C" {
    /// A `bitmaprenderer` canvas context; its `transferFromImageBitmap` also takes `null` (web-sys's does not).
    #[wasm_bindgen(extends = js_sys::Object)]
    pub type BitmapRenderer;
    #[wasm_bindgen(method, js_name = transferFromImageBitmap)]
    fn show(this: &BitmapRenderer, bitmap: Option<&ImageBitmap>);
}

/// Where a pane's picture goes: its canvas on the page, which takes the shared surface's frames as they are where
/// the surface can hand them over (see the module docs), else copies of them.
pub enum Present {
    Copy(CanvasRenderingContext2d),
    Transfer(BitmapRenderer),
}

impl Present {
    pub fn new(canvas: &HtmlCanvasElement) -> Option<Self> {
        let transfer = GPU.with(|g| {
            g.try_borrow()
                .is_ok_and(|g| g.as_ref().is_some_and(|g| g.transfer))
        });
        if transfer {
            if let Some(ctx) = canvas.get_context("bitmaprenderer").ok().flatten() {
                return Some(Self::Transfer(ctx.unchecked_into()));
            }
        }
        Some(Self::Copy(canvas.get_context("2d").ok()??.dyn_into().ok()?))
    }

    /// Whether the canvas keeps its frames in buffers of its own, which a resize frees (a 2D canvas).
    pub fn has_buffers(&self) -> bool {
        matches!(self, Self::Copy(_))
    }

    /// Lets go of the frame a transferred canvas shows; a 2D canvas lets go of its pixels when it is resized.
    pub fn clear(&self) {
        if let Self::Transfer(ctx) = self {
            ctx.show(None);
        }
    }
}

struct AtlasTextures {
    atlas: u32,
    /// Per page: texture and the page it holds ([`TextureAtlas::page_stamp`]), `None` before its first upload.
    pages: Vec<(WebGlTexture, Option<(u64, u32)>)>,
}

struct Programs {
    glyph: GlyphProgram,
    rect: RectProgram,
    /// Text still draws without it.
    image: Option<ImageProgram>,
}

/// The texture of an image on screen: the pane that drew it last, and whether its current frame used it.
struct ImageTexture {
    id: u32,
    pane: u32,
    texture: WebGlTexture,
    used: bool,
}

struct Gpu {
    document: Document,
    surface: Surface,
    gl: Gl,
    /// New panes take frames by transfer (an `OffscreenCanvas` on a GPU).
    transfer: bool,
    live: bool,
    programs: Option<Programs>,
    textures: Vec<AtlasTextures>,
    image_textures: Vec<ImageTexture>,
    scratch: Vec<f32>,
    listeners: Vec<Listener>,
    restore_timeout: Option<Timeout>,
    recreate_timeout: Option<Timeout>,
    last_recreate: f64,
    recreate_gap: f64,
    draws: u32,
    /// When a pane last drew (`timer::now`).
    last_draw: f64,
}

thread_local! {
    static GPU: RefCell<Option<Gpu>> = const { RefCell::new(None) };
}

fn attributes() -> WebGlContextAttributes {
    let attributes = WebGlContextAttributes::new();
    attributes.set_antialias(false);
    attributes.set_depth(false);
    attributes.set_preserve_drawing_buffer(false);
    attributes
}

/// A new surface and context: an `OffscreenCanvas`, else a canvas that is never attached.
fn new_surface(document: &Document) -> Result<(Surface, Gl), JsValue> {
    if let Ok(canvas) = OffscreenCanvas::new(1, 1) {
        let gl = canvas
            .get_context_with_context_options("webgl2", &attributes())
            .ok()
            .flatten()
            .and_then(|c| c.dyn_into::<Gl>().ok());
        if let Some(gl) = gl {
            return Ok((Surface::Offscreen(canvas), gl));
        }
    }
    let canvas: HtmlCanvasElement = document.create_element("canvas")?.unchecked_into();
    let gl: Gl = canvas
        .get_context_with_context_options("webgl2", &attributes())?
        .and_then(|c| c.dyn_into().ok())
        .ok_or("wongtty/xterm: WebGL2 is not supported")?;
    Ok((Surface::Element(canvas), gl))
}

/// Whether `gl` is software GL (SwiftShader), from the renderer name Chrome only gives through the extension.
fn is_software(gl: &Gl) -> bool {
    let name = |p| gl.get_parameter(p).ok().and_then(|v| v.as_string());
    let mut renderer = name(Gl::RENDERER).unwrap_or_default();
    if renderer == "WebKit WebGL"
        && gl
            .get_extension("WEBGL_debug_renderer_info")
            .is_ok_and(|e| e.is_some())
    {
        // UNMASKED_RENDERER_WEBGL
        renderer = name(0x9246).unwrap_or_default();
    }
    renderer.contains("SwiftShader")
}

fn can_transfer(surface: &Surface, gl: &Gl) -> bool {
    matches!(surface, Surface::Offscreen(_)) && !is_software(gl)
}

/// Makes the page's context if there is none yet.
pub fn ensure(document: &Document) -> Result<(), JsValue> {
    if GPU.with(|g| g.borrow().is_some()) {
        return Ok(());
    }
    let (surface, gl) = new_surface(document)?;
    let mut gpu = Gpu {
        document: document.clone(),
        transfer: can_transfer(&surface, &gl),
        surface,
        gl,
        live: true,
        programs: None,
        textures: Vec::new(),
        image_textures: Vec::new(),
        scratch: Vec::new(),
        listeners: Vec::new(),
        restore_timeout: None,
        recreate_timeout: None,
        last_recreate: f64::NEG_INFINITY,
        recreate_gap: 1000.0,
        draws: 0,
        last_draw: 0.0,
    };
    gpu.listen();
    gpu.build();
    GPU.with(|g| *g.borrow_mut() = Some(gpu));
    Ok(())
}

/// Frees the context once no terminal is left.
pub fn release() {
    let gpu = GPU.with(|g| g.try_borrow_mut().ok().and_then(|mut g| g.take()));
    if let Some(mut gpu) = gpu {
        gpu.dispose_programs();
        gpu.listeners.clear();
        lose_context(&gpu.gl);
    }
}

/// Whether panes can draw now.
pub fn is_live() -> bool {
    GPU.with(|g| {
        g.try_borrow()
            .is_ok_and(|g| g.as_ref().is_some_and(|g| g.live && g.programs.is_some()))
    })
}

/// `"live"`, `"lost"` or `"none"`, and the context and draw count, for the tests.
pub fn debug() -> (&'static str, JsValue, u32) {
    GPU.with(|g| match g.try_borrow().ok().as_deref() {
        Some(Some(g)) => (
            if g.live { "live" } else { "lost" },
            g.gl.clone().into(),
            g.draws,
        ),
        _ => ("none", JsValue::UNDEFINED, 0),
    })
}

fn lose_context(gl: &Gl) {
    if let Ok(Some(ext)) = gl.get_extension("WEBGL_lose_context") {
        ext.unchecked_into::<WebglLoseContext>().lose_context();
    }
}

/// Runs `f` on the context, on a later task if it is borrowed now.
fn with_gpu(f: impl FnOnce(&mut Gpu) + 'static) {
    GPU.with(|g| match g.try_borrow_mut() {
        Ok(mut g) => {
            if let Some(gpu) = g.as_mut() {
                f(gpu);
            }
        }
        Err(_) => timer::queue(move || with_gpu(f)),
    });
}

/// Drops the texture of image `id` (the image was deleted).
pub fn release_image(id: u32) {
    with_gpu(move |gpu| {
        gpu.image_textures.retain(|t| {
            if t.id == id {
                gpu.gl.delete_texture(Some(&t.texture));
            }
            t.id != id
        });
    });
}

/// Textures of images the context holds.
pub fn image_textures() -> u32 {
    GPU.with(|g| {
        g.try_borrow()
            .ok()
            .and_then(|g| g.as_ref().map(|g| g.image_textures.len() as u32))
            .unwrap_or(0)
    })
}

/// Shrinks the shared surface to a pixel once no pane drew for `idle_ms`: after transfers it keeps drawing
/// buffers of the last frame's size that no canvas shows.
pub fn trim(idle_ms: f64) {
    with_gpu(move |gpu| {
        if timer::now() - gpu.last_draw >= idle_ms {
            gpu.surface.set_size(1, 1);
        }
    });
}

/// What a pane draws in one frame.
pub struct Frame<'a> {
    pub pane: u32,
    pub width: f64,
    pub height: f64,
    pub rects: &'a RectVertices,
    pub glyphs: &'a GlyphVertices,
    pub model: &'a RenderModel,
    pub cursor: bool,
    pub images: &'a ImageLayer,
    pub present: &'a Present,
    /// Draws only this part of the frame and copies it to `present`'s origin (the cursor layer); `None` is all of it.
    pub region: Option<Region>,
}

/// Draws `frame` and hands it to the pane's canvas; false if the context cannot draw now.
pub fn draw(frame: Frame<'_>) -> bool {
    GPU.with(|g| {
        let Ok(mut g) = g.try_borrow_mut() else {
            return false;
        };
        let Some(gpu) = g.as_mut() else { return false };
        gpu.draw(frame)
    })
}

impl Gpu {
    fn listen(&mut self) {
        self.listeners.clear();
        let lost = Closure::<dyn FnMut(Event)>::new(move |e: Event| {
            // Allows the context to be restored
            e.prevent_default();
            with_gpu(Gpu::handle_context_lost);
        });
        let restored =
            Closure::<dyn FnMut(Event)>::new(move |_| with_gpu(Gpu::handle_context_restored));
        let target = self.surface.target().clone();
        self.listeners
            .push(Listener::new(&target, "webglcontextlost", lost));
        self.listeners
            .push(Listener::new(&target, "webglcontextrestored", restored));
    }

    /// Programs for the current context; atlas textures are uploaded again as they are used.
    fn build(&mut self) {
        let gl = &self.gl;
        let units = gl
            .get_parameter(Gl::MAX_TEXTURE_IMAGE_UNITS)
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let max_size = gl
            .get_parameter(Gl::MAX_TEXTURE_SIZE)
            .ok()
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        self.textures.clear();
        self.image_textures.clear();
        self.programs = None;
        if units < 1.0 || max_size < 1.0 {
            return;
        }
        atlas::init_limits(units as u32, max_size as u32);
        let Some(pages) = atlas::max_atlas_pages() else {
            return;
        };
        let (Some(glyph), Some(rect)) = (GlyphProgram::new(gl, pages), RectProgram::new(gl)) else {
            return;
        };
        // Glyphs blend over the backgrounds
        gl.enable(Gl::BLEND);
        gl.blend_func(Gl::SRC_ALPHA, Gl::ONE_MINUS_SRC_ALPHA);
        self.programs = Some(Programs {
            glyph,
            rect,
            image: ImageProgram::new(gl),
        });
    }

    fn dispose_programs(&mut self) {
        if let Some(p) = self.programs.take() {
            p.glyph.dispose(&self.gl);
            p.rect.dispose(&self.gl);
            if let Some(image) = p.image {
                image.dispose(&self.gl);
            }
        }
        for t in self.image_textures.drain(..) {
            self.gl.delete_texture(Some(&t.texture));
        }
        for t in self.textures.drain(..) {
            for (texture, _) in t.pages {
                self.gl.delete_texture(Some(&texture));
            }
        }
    }

    /// Uploads the pages of `atlas` that changed and binds them to the texture units: only the glyphs added since
    /// the last upload when that is all that changed, else the whole page.
    fn bind_atlas(&mut self, atlas: &mut TextureAtlas) {
        let gl = &self.gl;
        // Textures of atlases that are gone
        let live = atlas::live_ids();
        self.textures.retain(|t| {
            let keep = live.contains(&t.atlas) || t.atlas == atlas.id;
            if !keep {
                for (texture, _) in &t.pages {
                    gl.delete_texture(Some(texture));
                }
            }
            keep
        });
        let index = match self.textures.iter().position(|t| t.atlas == atlas.id) {
            Some(i) => i,
            None => {
                self.textures.push(AtlasTextures {
                    atlas: atlas.id,
                    pages: Vec::new(),
                });
                self.textures.len() - 1
            }
        };
        let units = atlas::max_atlas_pages().unwrap_or(0) as usize;
        let entry = &mut self.textures[index];
        for i in 0..atlas.page_count().min(units) {
            gl.active_texture(Gl::TEXTURE0 + i as u32);
            if entry.pages.len() <= i {
                let Some(texture) = gl.create_texture() else {
                    return;
                };
                entry.pages.push((texture, None));
            }
            let (texture, have) = &mut entry.pages[i];
            gl.bind_texture(Gl::TEXTURE_2D, Some(texture));
            let stamp = atlas.page_stamp(i);
            if *have == Some(stamp) {
                continue;
            }
            if let Some(updates) = atlas.take_page_updates(i, *have) {
                for g in updates {
                    let _ = gl.tex_sub_image_2d_with_i32_and_i32_and_u32_and_type_and_opt_u8_array(
                        Gl::TEXTURE_2D,
                        0,
                        g.x,
                        g.y,
                        g.w,
                        g.h,
                        Gl::RGBA,
                        Gl::UNSIGNED_BYTE,
                        Some(&g.data),
                    );
                }
                *have = Some(stamp);
                continue;
            }
            let Some(canvas) = atlas.page_canvas(i) else {
                continue;
            };
            gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_WRAP_S, Gl::CLAMP_TO_EDGE as i32);
            gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_WRAP_T, Gl::CLAMP_TO_EDGE as i32);
            // Glyphs are drawn at the atlas's own scale: no mipmaps to rebuild at every upload (as in
            // @xterm/addon-webgl 0.20.0-beta.329, `src/GlyphRenderer.ts` lines 410-414)
            gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_MIN_FILTER, Gl::LINEAR as i32);
            gl.tex_parameteri(Gl::TEXTURE_2D, Gl::TEXTURE_MAG_FILTER, Gl::LINEAR as i32);
            let _ = gl.tex_image_2d_with_u32_and_u32_and_html_canvas_element(
                Gl::TEXTURE_2D,
                0,
                Gl::RGBA as i32,
                Gl::RGBA,
                Gl::UNSIGNED_BYTE,
                canvas,
            );
            *have = Some(stamp);
        }
    }

    fn draw(&mut self, frame: Frame<'_>) -> bool {
        if !self.live || self.programs.is_none() || frame.width < 1.0 || frame.height < 1.0 {
            return false;
        }
        let (width, height) = (frame.width as i32, frame.height as i32);
        let transfer = matches!(frame.present, Present::Transfer(_));
        // GL's origin is the bottom left
        let region = frame
            .region
            .map(|[x, y, w, h]| [x as i32, (frame.height - y - h) as i32, w as i32, h as i32]);
        match region {
            // The region is a frame of its own: the pane's picture, shifted so the region is all that is drawn
            Some([x, y, w, h]) if transfer => {
                self.surface.set_size(w as u32, h as u32);
                self.gl.viewport(-x, -y, width, height);
            }
            _ => {
                self.surface.set_size(width as u32, height as u32);
                self.gl.viewport(0, 0, width, height);
            }
        }
        let Some(programs) = self.programs.take() else {
            return false;
        };
        let scissor = region.filter(|_| !transfer);
        if let Some([x, y, w, h]) = scissor {
            self.gl.enable(Gl::SCISSOR_TEST);
            self.gl.scissor(x, y, w, h);
        }
        programs.rect.draw_backgrounds(&self.gl, frame.rects);
        if let Some(atlas) = frame.glyphs.atlas().cloned() {
            if let Ok(mut atlas) = atlas.try_borrow_mut() {
                self.bind_atlas(&mut atlas);
                frame.glyphs.pack(frame.model, &mut self.scratch);
                programs
                    .glyph
                    .draw(&self.gl, &self.scratch, frame.width, frame.height);
            }
        }
        programs.rect.draw_overlay(&self.gl, frame.rects);
        if frame.cursor {
            programs.rect.draw_cursor(&self.gl, frame.rects);
        }
        if let Some(program) = &programs.image {
            self.draw_images(program, frame.pane, frame.images);
        }
        self.programs = Some(programs);
        if scissor.is_some() {
            self.gl.disable(Gl::SCISSOR_TEST);
        }
        let ctx = match frame.present {
            Present::Copy(ctx) => ctx,
            Present::Transfer(ctx) => {
                // A surface that cannot hand frames over (the context was made again on another backend) leaves
                // the pane as it is
                let Surface::Offscreen(canvas) = &self.surface else {
                    return false;
                };
                let Ok(bitmap) = canvas.transfer_to_image_bitmap() else {
                    return false;
                };
                ctx.show(Some(&bitmap));
                self.draws = self.draws.wrapping_add(1);
                self.last_draw = timer::now();
                return true;
            }
        };
        // The frame replaces the canvas's last one
        let _ = ctx.set_global_composite_operation("copy");
        let _ = match (&self.surface, frame.region) {
            (Surface::Offscreen(canvas), None) => {
                ctx.draw_image_with_offscreen_canvas(canvas, 0.0, 0.0)
            }
            (Surface::Element(canvas), None) => {
                ctx.draw_image_with_html_canvas_element(canvas, 0.0, 0.0)
            }
            (Surface::Offscreen(canvas), Some([x, y, w, h])) => ctx
                .draw_image_with_offscreen_canvas_and_sw_and_sh_and_dx_and_dy_and_dw_and_dh(
                    canvas, x, y, w, h, 0.0, 0.0, w, h,
                ),
            (Surface::Element(canvas), Some([x, y, w, h])) => ctx
                .draw_image_with_html_canvas_element_and_sw_and_sh_and_dx_and_dy_and_dw_and_dh(
                    canvas, x, y, w, h, 0.0, 0.0, w, h,
                ),
        };
        self.draws = self.draws.wrapping_add(1);
        true
    }

    /// Draws the pane's image layer, making textures for images it shows for the first time and dropping those of
    /// its images that went off screen.
    fn draw_images(&mut self, program: &ImageProgram, pane: u32, layer: &ImageLayer) {
        for t in self.image_textures.iter_mut().filter(|t| t.pane == pane) {
            t.used = false;
        }
        if !layer.is_empty() {
            let max_size = atlas::max_texture_size();
            let (gl, textures) = (&self.gl, &mut self.image_textures);
            program.draw(gl, layer, |id| {
                if let Some(t) = textures.iter_mut().find(|t| t.id == id) {
                    t.pane = pane;
                    t.used = true;
                    return Some(t.texture.clone());
                }
                let texture = images::upload(gl, id, max_size)?;
                textures.push(ImageTexture {
                    id,
                    pane,
                    texture: texture.clone(),
                    used: true,
                });
                Some(texture)
            });
        }
        let gl = &self.gl;
        self.image_textures.retain(|t| {
            let keep = t.pane != pane || t.used;
            if !keep {
                gl.delete_texture(Some(&t.texture));
            }
            keep
        });
    }

    fn handle_context_lost(&mut self) {
        self.live = false;
        // The context's objects are gone with it
        self.programs = None;
        self.textures.clear();
        self.image_textures.clear();
        self.restore_timeout = Some(Timeout::new(CONTEXT_RESTORE_TIMEOUT, || {
            with_gpu(|gpu| {
                gpu.restore_timeout = None;
                gpu.recreate_throttled();
            })
        }));
    }

    fn handle_context_restored(&mut self) {
        self.restore_timeout = None;
        if self.live {
            return;
        }
        self.live = true;
        self.build();
        timer::queue(super::webgl::redraw_all);
    }

    /// Backs off when contexts keep getting lost (a driver that keeps resetting).
    fn recreate_throttled(&mut self) {
        let wait = self.last_recreate + self.recreate_gap - timer::now();
        if wait <= 0.0 {
            self.recreate();
            return;
        }
        self.recreate_timeout = Some(Timeout::new(wait.ceil() as i32, || {
            with_gpu(|gpu| {
                gpu.recreate_timeout = None;
                if !gpu.live {
                    gpu.recreate();
                }
            })
        }));
    }

    fn recreate(&mut self) {
        let now = timer::now();
        self.recreate_gap = if now - self.last_recreate < 60_000.0 {
            (self.recreate_gap * 2.0).min(60_000.0)
        } else {
            1000.0
        };
        self.last_recreate = now;
        self.restore_timeout = None;
        self.recreate_timeout = None;
        match new_surface(&self.document) {
            Ok((surface, gl)) => {
                self.transfer = can_transfer(&surface, &gl);
                self.surface = surface;
                self.gl = gl;
            }
            Err(error) => {
                web_sys::console::warn_1(&error);
                return;
            }
        }
        self.listen();
        self.live = true;
        self.build();
        timer::queue(super::webgl::redraw_all);
    }
}
