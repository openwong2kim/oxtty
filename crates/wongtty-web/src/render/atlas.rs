//! The glyph atlas: glyphs rasterised with Canvas2D `fillText` (the browser's own font fallback, as in xterm.js)
//! into texture pages, and the cache that shares one atlas between renderers with equal options.
//!
//! Ported from xterm.js 6.0.0 (commit f447274f430fd22513f6adbf9862d19524471c04, MIT):
//! `addons/addon-webgl/src/TextureAtlas.ts` lines 20-1138 (all of it except the idle-time warm-up and the
//! page-canvas events), `addons/addon-webgl/src/CharAtlasCache.ts` lines 20-97, `addons/addon-webgl/src/
//! CharAtlasUtils.ts` lines 13-75 (`generateConfig`, `configEquals`), `addons/addon-webgl/src/Constants.ts`
//! lines 8-12 and the glyph classes of `src/browser/renderer/shared/RendererUtils.ts` lines 15-37.
//!
//! Wide glyphs are drawn into a slot of `max(2, chars.length)` cells and clipped there, never scaled, exactly as
//! xterm.js does; whether a code point is wide comes from the engine (the `WIDE` cell bit), not from xterm.js's
//! Unicode tables.
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use wasm_bindgen::{Clamped, JsCast, JsValue};
use web_sys::{
    CanvasRenderingContext2d, Document, HtmlCanvasElement, HtmlElement, ImageData, Path2d,
};

use super::cell::{to_color_rgb, Attrs, UnderlineStyle, CM_P16, CM_P256, CM_RGB};
use super::color::{
    channels_to_color, ensure_contrast_ratio, multiply_opacity, null_color, opaque, Color,
};
use super::custom_glyphs::{stroke_like_fill, try_draw_custom_char};

const TMP_CANVAS_GLYPH_PADDING: f64 = 2.0;
/// Pixel padding allowed in each atlas row.
const ROW_PIXEL_THRESHOLD: f64 = 2.0;
/// Largest texture regardless of the hardware maximum, so uploads stay fast (4096² is 16 MB of GPU memory).
const FORCED_MAX_TEXTURE_SIZE: f64 = 4096.0;
const TEXTURE_SIZE: f64 = 512.0;
const DIM_OPACITY: f64 = 0.5;

thread_local! {
    /// Texture units and texture size of the first WebGL context (`TextureAtlas.maxAtlasPages/maxTextureSize`).
    static MAX_ATLAS_PAGES: Cell<Option<u32>> = const { Cell::new(None) };
    static MAX_TEXTURE_SIZE: Cell<Option<u32>> = const { Cell::new(None) };
    /// Code points the engine measured as wide; the atlas sizes their underlines by it.
    static WIDE: RefCell<HashSet<u32>> = RefCell::new(HashSet::new());
    static CACHE: RefCell<Vec<CacheEntry>> = const { RefCell::new(Vec::new()) };
    static LIVE_ATLASES: Cell<u32> = const { Cell::new(0) };
    static NEXT_ATLAS_ID: Cell<u32> = const { Cell::new(1) };
}

/// The texture size limit of the first context (0 before there is one).
pub fn max_texture_size() -> f64 {
    MAX_TEXTURE_SIZE.with(Cell::get).map_or(0.0, |s| s as f64)
}

pub fn max_atlas_pages() -> Option<u32> {
    MAX_ATLAS_PAGES.with(Cell::get)
}

/// Records the limits of the first context, like the `GlyphRenderer` constructor does.
pub fn init_limits(max_pages: u32, max_texture_size: u32) {
    if MAX_ATLAS_PAGES.with(Cell::get).is_none() {
        MAX_ATLAS_PAGES.with(|c| c.set(Some(max_pages.min(32))));
        MAX_TEXTURE_SIZE.with(|c| c.set(Some(max_texture_size)));
    }
}

pub fn record_wide(codepoint: u32) {
    WIDE.with(|w| {
        if !w.borrow().contains(&codepoint) {
            w.borrow_mut().insert(codepoint);
        }
    });
}

fn wcwidth(codepoint: u32) -> u32 {
    if WIDE.with(|w| w.borrow().contains(&codepoint)) {
        2
    } else {
        1
    }
}

/// Ids of the atlases in the cache.
pub fn live_ids() -> Vec<u32> {
    CACHE.with(|cache| {
        cache
            .borrow()
            .iter()
            .filter_map(|e| e.atlas.try_borrow().ok().map(|a| a.id))
            .collect()
    })
}

/// Atlases alive, for the tests.
pub fn live_atlases() -> u32 {
    LIVE_ATLASES.with(Cell::get)
}

pub fn is_powerline_glyph(codepoint: u32) -> bool {
    (0xe0a4..=0xe0d6).contains(&codepoint)
}

fn is_restricted_powerline_glyph(codepoint: u32) -> bool {
    (0xe0b0..=0xe0b7).contains(&codepoint)
}

fn is_box_or_block_glyph(codepoint: u32) -> bool {
    (0x2500..=0x259f).contains(&codepoint)
}

pub fn treat_glyph_as_background_color(codepoint: u32) -> bool {
    is_powerline_glyph(codepoint) || is_box_or_block_glyph(codepoint)
}

/// What an atlas depends on; atlases with equal configs are shared (`ICharAtlasConfig`).
#[derive(Clone, Debug)]
pub struct AtlasConfig {
    pub custom_glyphs: bool,
    pub device_pixel_ratio: f64,
    pub device_max_texture_size: f64,
    pub letter_spacing: f64,
    pub line_height: f64,
    pub font_size: f64,
    pub font_family: String,
    pub font_weight: String,
    pub font_weight_bold: String,
    pub device_cell_width: f64,
    pub device_cell_height: f64,
    pub device_char_width: f64,
    pub device_char_height: f64,
    pub allow_transparency: bool,
    pub draw_bold_text_in_bright_colors: bool,
    pub minimum_contrast_ratio: f64,
    pub foreground: Color,
    pub background: Color,
    pub ansi: Vec<Color>,
}

impl AtlasConfig {
    /// `configEquals`: the cell size and texture limit are left out, as in xterm.js.
    fn same(&self, b: &Self) -> bool {
        self.ansi.len() == b.ansi.len()
            && self.ansi.iter().zip(&b.ansi).all(|(x, y)| x.rgba == y.rgba)
            && self.device_pixel_ratio == b.device_pixel_ratio
            && self.custom_glyphs == b.custom_glyphs
            && self.line_height == b.line_height
            && self.letter_spacing == b.letter_spacing
            && self.font_family == b.font_family
            && self.font_size == b.font_size
            && self.font_weight == b.font_weight
            && self.font_weight_bold == b.font_weight_bold
            && self.allow_transparency == b.allow_transparency
            && self.device_char_width == b.device_char_width
            && self.device_char_height == b.device_char_height
            && self.draw_bold_text_in_bright_colors == b.draw_bold_text_in_bright_colors
            && self.minimum_contrast_ratio == b.minimum_contrast_ratio
            && self.foreground.rgba == b.foreground.rgba
            && self.background.rgba == b.background.rgba
    }
}

struct CacheEntry {
    atlas: Rc<RefCell<TextureAtlas>>,
    config: AtlasConfig,
    owned_by: Vec<u32>,
}

/// `acquireTextureAtlas`: the atlas for `config`, shared with renderers whose config is equal. Releases
/// `owner`'s previous atlas when the config changed, disposing it once nobody holds it.
pub fn acquire(
    owner: u32,
    config: AtlasConfig,
    document: &Document,
) -> Option<Rc<RefCell<TextureAtlas>>> {
    let found = CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(i) = cache.iter().position(|e| e.owned_by.contains(&owner)) {
            if cache[i].config.same(&config) {
                return Some(cache[i].atlas.clone());
            }
            release_at(&mut cache, i, owner);
        }
        let entry = cache.iter_mut().find(|e| e.config.same(&config))?;
        entry.owned_by.push(owner);
        Some(entry.atlas.clone())
    });
    if found.is_some() {
        return found;
    }
    let atlas = Rc::new(RefCell::new(TextureAtlas::new(document, config.clone())?));
    CACHE.with(|cache| {
        cache.borrow_mut().push(CacheEntry {
            atlas: atlas.clone(),
            config,
            owned_by: vec![owner],
        })
    });
    Some(atlas)
}

/// `removeTerminalFromCache`.
pub fn release(owner: u32) {
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(i) = cache.iter().position(|e| e.owned_by.contains(&owner)) {
            release_at(&mut cache, i, owner);
        }
    });
}

fn release_at(cache: &mut Vec<CacheEntry>, i: usize, owner: u32) {
    if cache[i].owned_by.len() == 1 {
        let entry = cache.remove(i);
        if let Ok(mut atlas) = entry.atlas.try_borrow_mut() {
            atlas.dispose();
        };
    } else {
        cache[i].owned_by.retain(|&o| o != owner);
    }
}

/// Where a glyph sits in the atlas; all zero for one that draws nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Glyph {
    /// From the glyph's top left to the cell's, in pixels.
    pub offset: (f64, f64),
    pub texture_page: usize,
    pub texture_position: (f64, f64),
    pub texture_position_clip_space: (f64, f64),
    pub size: (f64, f64),
    pub size_clip_space: (f64, f64),
}

#[derive(Clone, Copy, Debug, Default)]
struct Row {
    x: f64,
    y: f64,
    height: f64,
}

#[derive(Clone, Copy, PartialEq)]
enum RowRef {
    Current,
    Fixed(usize),
}

struct AtlasPage {
    id: u64,
    canvas: HtmlCanvasElement,
    ctx: CanvasRenderingContext2d,
    used_pixels: f64,
    /// Indices into `TextureAtlas::glyphs`.
    glyphs: Vec<usize>,
    version: u32,
    // Filled left to right, top to bottom. A glyph less than half the current row's height fixes that row, gets
    // a fixed row of its own height below, and a new current row starts under that.
    current_row: Row,
    fixed_rows: Vec<Row>,
}

impl AtlasPage {
    fn new(document: &Document, id: u64, size: f64) -> Option<Self> {
        let canvas = create_canvas(document, size, size)?;
        // Alpha: clear_color turns the background transparent
        let options = js_sys::Object::new();
        js_sys::Reflect::set(&options, &"alpha".into(), &true.into()).ok()?;
        let ctx = context_2d(&canvas, &options)?;
        Some(Self {
            id,
            canvas,
            ctx,
            used_pixels: 0.0,
            glyphs: Vec::new(),
            version: 0,
            current_row: Row::default(),
            fixed_rows: Vec::new(),
        })
    }

    fn width(&self) -> f64 {
        self.canvas.width() as f64
    }

    fn percentage_used(&self) -> f64 {
        self.used_pixels / (self.width() * self.canvas.height() as f64)
    }

    fn row_at(&self, r: RowRef) -> Row {
        match r {
            RowRef::Fixed(i) if i < self.fixed_rows.len() => self.fixed_rows[i],
            _ => self.current_row,
        }
    }

    fn row(&mut self, r: RowRef) -> &mut Row {
        match r {
            RowRef::Fixed(i) if i < self.fixed_rows.len() => &mut self.fixed_rows[i],
            _ => &mut self.current_row,
        }
    }

    fn clear(&mut self) {
        self.ctx
            .clear_rect(0.0, 0.0, self.width(), self.canvas.height() as f64);
        self.current_row = Row::default();
        self.fixed_rows.clear();
        self.version += 1;
    }
}

fn create_canvas(document: &Document, width: f64, height: f64) -> Option<HtmlCanvasElement> {
    let canvas: HtmlCanvasElement = document.create_element("canvas").ok()?.dyn_into().ok()?;
    canvas.set_width(width as u32);
    canvas.set_height(height as u32);
    Some(canvas)
}

fn context_2d(canvas: &HtmlCanvasElement, options: &JsValue) -> Option<CanvasRenderingContext2d> {
    canvas
        .get_context_with_context_options("2d", options)
        .ok()??
        .dyn_into()
        .ok()
}

enum Key<'a> {
    Code(u32),
    Chars(&'a str),
}

pub struct TextureAtlas {
    pub id: u32,
    document: Document,
    config: AtlasConfig,
    glyphs: Vec<Glyph>,
    /// `None` is a glyph that draws nothing.
    cache: HashMap<(u32, u32, u32, u32), Option<usize>>,
    cache_combined: HashMap<(String, u32, u32, u32), Option<usize>>,
    contrast_cache: HashMap<(u32, u32), Option<Color>>,
    half_contrast_cache: HashMap<(u32, u32), Option<Color>>,
    pages: Vec<AtlasPage>,
    /// Ids of the pages glyphs may still go to.
    active_pages: Vec<u64>,
    overflow_size_page: Option<u64>,
    next_page_id: u64,
    tmp_canvas: HtmlCanvasElement,
    tmp_ctx: CanvasRenderingContext2d,
    request_clear_model: bool,
    disposed: bool,
}

impl TextureAtlas {
    fn new(document: &Document, config: AtlasConfig) -> Option<Self> {
        let tmp_canvas = create_canvas(
            document,
            config.device_cell_width * 4.0 + TMP_CANVAS_GLYPH_PADDING * 2.0,
            config.device_cell_height + TMP_CANVAS_GLYPH_PADDING * 2.0,
        )?;
        let options = js_sys::Object::new();
        js_sys::Reflect::set(&options, &"alpha".into(), &config.allow_transparency.into()).ok()?;
        js_sys::Reflect::set(&options, &"willReadFrequently".into(), &true.into()).ok()?;
        let tmp_ctx = context_2d(&tmp_canvas, &options)?;
        let id = NEXT_ATLAS_ID.with(|n| n.replace(n.get() + 1));
        let mut atlas = Self {
            id,
            document: document.clone(),
            config,
            glyphs: Vec::new(),
            cache: HashMap::new(),
            cache_combined: HashMap::new(),
            contrast_cache: HashMap::new(),
            half_contrast_cache: HashMap::new(),
            pages: Vec::new(),
            active_pages: Vec::new(),
            overflow_size_page: None,
            next_page_id: 0,
            tmp_canvas,
            tmp_ctx,
            request_clear_model: false,
            disposed: false,
        };
        atlas.create_new_page()?;
        LIVE_ATLASES.with(|n| n.set(n.get() + 1));
        Some(atlas)
    }

    fn dispose(&mut self) {
        if self.disposed {
            return;
        }
        self.disposed = true;
        self.tmp_canvas.remove();
        LIVE_ATLASES.with(|n| n.set(n.get().saturating_sub(1)));
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    pub fn page_canvas(&self, i: usize) -> Option<&HtmlCanvasElement> {
        self.pages.get(i).map(|p| &p.canvas)
    }

    pub fn page_version(&self, i: usize) -> u32 {
        self.pages.get(i).map_or(0, |p| p.version)
    }

    pub fn page_width(&self, i: usize) -> f64 {
        self.pages.get(i).map_or(1.0, AtlasPage::width)
    }

    /// True once pages were merged or an oversized page was added: the model must be rebuilt.
    pub fn begin_frame(&self) -> bool {
        self.request_clear_model
    }

    /// Drops every glyph (fonts finished loading: what was drawn used fallback fonts).
    pub fn clear_texture(&mut self) {
        if let Some(first) = self.pages.first() {
            if first.current_row.x == 0.0 && first.current_row.y == 0.0 {
                return;
            }
        }
        for page in &mut self.pages {
            page.clear();
        }
        self.cache.clear();
        self.cache_combined.clear();
    }

    fn page_index(&self, id: u64) -> Option<usize> {
        self.pages.iter().position(|p| p.id == id)
    }

    fn create_new_page(&mut self) -> Option<u64> {
        // Merge the 4 most used pages of the largest size once the page count reaches the texture unit limit
        let max_pages = max_atlas_pages().unwrap_or(0) as usize;
        if max_pages > 0 && self.pages.len() >= max_pages.max(4) {
            let max_size = MAX_TEXTURE_SIZE
                .with(Cell::get)
                .map_or(FORCED_MAX_TEXTURE_SIZE, |s| s as f64);
            let mut by_size: Vec<usize> = (0..self.pages.len())
                .filter(|&i| self.pages[i].width() * 2.0 <= max_size)
                .collect();
            by_size.sort_by(|&a, &b| {
                let (a, b) = (&self.pages[a], &self.pages[b]);
                b.width()
                    .total_cmp(&a.width())
                    .then(b.percentage_used().total_cmp(&a.percentage_used()))
            });
            let mut same_size_i = 0;
            let mut size = -1.0;
            for (i, &p) in by_size.iter().enumerate() {
                if self.pages[p].width() != size {
                    same_size_i = i;
                    size = self.pages[p].width();
                } else if i - same_size_i == 3 {
                    break;
                }
            }
            let merging: Vec<usize> = by_size.iter().skip(same_size_i).take(4).copied().collect();
            if !merging.is_empty() {
                let merged_index = self.pages.len() - merging.len();
                let mut merged = self.merge_pages(&merging, merged_index)?;
                merged.version += 1;
                let mut indices = merging.clone();
                indices.sort_unstable();
                for &i in indices.iter().rev() {
                    self.delete_page(i);
                }
                self.pages.push(merged);
                self.request_clear_model = true;
            }
        }
        // New pages start small as they are highly dynamic
        let id = self.next_page_id;
        self.next_page_id += 1;
        let page = AtlasPage::new(&self.document, id, TEXTURE_SIZE)?;
        self.pages.push(page);
        self.active_pages.push(id);
        Some(id)
    }

    fn merge_pages(&mut self, merging: &[usize], merged_index: usize) -> Option<AtlasPage> {
        let merged_size = self.pages[merging[0]].width() * 2.0;
        let id = self.next_page_id;
        self.next_page_id += 1;
        let mut merged = AtlasPage::new(&self.document, id, merged_size)?;
        for (i, &p) in merging.iter().enumerate() {
            let page = &self.pages[p];
            merged.glyphs.extend_from_slice(&page.glyphs);
            merged.used_pixels += page.used_pixels;
            let x_offset = (i as f64 * page.width()) % merged_size;
            let y_offset = (i / 2) as f64 * page.canvas.height() as f64;
            let _ =
                merged
                    .ctx
                    .draw_image_with_html_canvas_element(&page.canvas, x_offset, y_offset);
            for &g in &page.glyphs {
                if let Some(g) = self.glyphs.get_mut(g) {
                    g.texture_page = merged_index;
                    g.size_clip_space = (g.size.0 / merged_size, g.size.1 / merged_size);
                    g.texture_position.0 += x_offset;
                    g.texture_position.1 += y_offset;
                    g.texture_position_clip_space = (
                        g.texture_position.0 / merged_size,
                        g.texture_position.1 / merged_size,
                    );
                }
            }
            let page_id = page.id;
            self.active_pages.retain(|&a| a != page_id);
        }
        Some(merged)
    }

    fn delete_page(&mut self, index: usize) {
        if index >= self.pages.len() {
            return;
        }
        self.pages.remove(index);
        for page in &mut self.pages[index..] {
            for &g in &page.glyphs {
                if let Some(g) = self.glyphs.get_mut(g) {
                    g.texture_page = g.texture_page.saturating_sub(1);
                }
            }
            page.version += 1;
        }
    }

    fn glyph(&self, slot: Option<usize>) -> Glyph {
        slot.and_then(|i| self.glyphs.get(i).copied())
            .unwrap_or_default()
    }

    pub fn get_rasterized_glyph(
        &mut self,
        code: u32,
        bg: u32,
        fg: u32,
        ext: u32,
        container: Option<&HtmlElement>,
    ) -> Glyph {
        let slot = match self.cache.get(&(code, bg, fg, ext)) {
            Some(&slot) => slot,
            None => {
                let slot = self.draw_to_cache(Key::Code(code), bg, fg, ext, container);
                self.cache.insert((code, bg, fg, ext), slot);
                slot
            }
        };
        self.glyph(slot)
    }

    pub fn get_rasterized_glyph_combined_char(
        &mut self,
        chars: &str,
        bg: u32,
        fg: u32,
        ext: u32,
        container: Option<&HtmlElement>,
    ) -> Glyph {
        let key = (chars.to_string(), bg, fg, ext);
        let slot = match self.cache_combined.get(&key) {
            Some(&slot) => slot,
            None => {
                let slot = self.draw_to_cache(Key::Chars(chars), bg, fg, ext, container);
                self.cache_combined.insert(key, slot);
                slot
            }
        };
        self.glyph(slot)
    }

    fn ansi(&self, i: i64) -> Color {
        usize::try_from(i)
            .ok()
            .and_then(|i| self.config.ansi.get(i))
            .cloned()
            .unwrap_or_else(null_color)
    }

    fn background_color(&self, mode: u32, color: i64, inverse: bool) -> Color {
        if self.config.allow_transparency {
            // A translucent background is drawn transparent in the atlas, or anti-aliased edges would show it
            // twice
            return null_color();
        }
        let result = match mode {
            CM_P16 | CM_P256 => self.ansi(color),
            CM_RGB => {
                let [r, g, b] = to_color_rgb(color as u32);
                channels_to_color(r, g, b, None)
            }
            _ if inverse => opaque(&self.config.foreground),
            _ => self.config.background.clone(),
        };
        opaque(&result)
    }

    #[allow(clippy::too_many_arguments)]
    fn foreground_color(
        &mut self,
        bg: u32,
        bg_mode: u32,
        bg_color: i64,
        fg: u32,
        fg_mode: u32,
        mut fg_color: i64,
        inverse: bool,
        dim: bool,
        bold: bool,
        exclude_from_contrast: bool,
    ) -> Color {
        if let Some(color) = self.minimum_contrast_color(
            bg,
            bg_mode,
            bg_color,
            fg,
            fg_mode,
            fg_color,
            inverse,
            bold,
            dim,
            exclude_from_contrast,
        ) {
            return color;
        }
        let mut result = match fg_mode {
            CM_P16 | CM_P256 => {
                if self.config.draw_bold_text_in_bright_colors && bold && fg_color < 8 {
                    fg_color += 8;
                }
                self.ansi(fg_color)
            }
            CM_RGB => {
                let [r, g, b] = to_color_rgb(fg_color as u32);
                channels_to_color(r, g, b, None)
            }
            _ if inverse => self.config.background.clone(),
            _ => self.config.foreground.clone(),
        };
        if self.config.allow_transparency {
            result = opaque(&result);
        }
        if dim {
            result = multiply_opacity(&result, DIM_OPACITY);
        }
        result
    }

    fn resolve_background_rgba(&self, mode: u32, color: i64, inverse: bool) -> u32 {
        match mode {
            CM_P16 | CM_P256 => self.ansi(color).rgba,
            CM_RGB => (color as u32) << 8,
            _ if inverse => self.config.foreground.rgba,
            _ => self.config.background.rgba,
        }
    }

    fn resolve_foreground_rgba(&self, mode: u32, mut color: i64, inverse: bool, bold: bool) -> u32 {
        match mode {
            CM_P16 | CM_P256 => {
                if self.config.draw_bold_text_in_bright_colors && bold && color < 8 {
                    color += 8;
                }
                self.ansi(color).rgba
            }
            CM_RGB => (color as u32) << 8,
            _ if inverse => self.config.background.rgba,
            _ => self.config.foreground.rgba,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn minimum_contrast_color(
        &mut self,
        bg: u32,
        bg_mode: u32,
        bg_color: i64,
        fg: u32,
        fg_mode: u32,
        fg_color: i64,
        inverse: bool,
        bold: bool,
        dim: bool,
        exclude: bool,
    ) -> Option<Color> {
        if self.config.minimum_contrast_ratio == 1.0 || exclude {
            return None;
        }
        let cached = if dim {
            &self.half_contrast_cache
        } else {
            &self.contrast_cache
        }
        .get(&(bg, fg))
        .cloned();
        if let Some(cached) = cached {
            return cached;
        }
        let bg_rgba = self.resolve_background_rgba(bg_mode, bg_color, inverse);
        let fg_rgba = self.resolve_foreground_rgba(fg_mode, fg_color, inverse, bold);
        // Dim cells only need half the contrast, or they could not be told from normal ones
        let ratio = self.config.minimum_contrast_ratio / if dim { 2.0 } else { 1.0 };
        let color = ensure_contrast_ratio(bg_rgba, fg_rgba, ratio)
            .map(|c| channels_to_color(c >> 24, (c >> 16) & 0xff, (c >> 8) & 0xff, None));
        if dim {
            &mut self.half_contrast_cache
        } else {
            &mut self.contrast_cache
        }
        .insert((bg, fg), color.clone());
        color
    }

    fn draw_to_cache(
        &mut self,
        key: Key<'_>,
        bg: u32,
        fg: u32,
        ext: u32,
        container: Option<&HtmlElement>,
    ) -> Option<usize> {
        let chars: String = match key {
            Key::Code(code) => char::from_u32(code).map(String::from).unwrap_or_default(),
            Key::Chars(chars) => chars.to_string(),
        };
        let first_unit = chars.encode_utf16().next().unwrap_or(0) as u32;
        let len16 = chars.encode_utf16().count();
        let cfg = &self.config;
        let (cell_w, cell_h) = (cfg.device_cell_width, cfg.device_cell_height);

        // In the DOM, the canvas inherits font-feature-settings (ligatures, variants) from its parents
        if let Some(container) = container {
            if self.tmp_canvas.parent_element().as_ref() != Some(container) {
                let _ = self.tmp_canvas.style().set_property("display", "none");
                let _ = container.append_child(&self.tmp_canvas);
            }
        }
        // One cell per character, at least two (CJK), plus padding; also bounds the glyph search so giant
        // ligatures stay cheap
        let allowed_width = (cell_w * (len16.max(2) as f64) + TMP_CANVAS_GLYPH_PADDING * 2.0)
            .min(cfg.device_max_texture_size);
        if (self.tmp_canvas.width() as f64) < allowed_width {
            self.tmp_canvas.set_width(allowed_width as u32);
        }
        let allowed_height = (cell_h + TMP_CANVAS_GLYPH_PADDING * 4.0).min(TEXTURE_SIZE);
        if (self.tmp_canvas.height() as f64) < allowed_height {
            self.tmp_canvas.set_height(allowed_height as u32);
        }

        let a = Attrs { fg, bg, ext };
        if a.is_invisible() {
            return None;
        }
        let ctx = self.tmp_ctx.clone();
        let (tmp_w, tmp_h) = (
            self.tmp_canvas.width() as f64,
            self.tmp_canvas.height() as f64,
        );
        ctx.save();
        let (bold, inverse, dim, italic) = (a.is_bold(), a.is_inverse(), a.is_dim(), a.is_italic());
        let (underline, strikethrough, overline) =
            (a.is_underline(), a.is_strikethrough(), a.is_overline());
        let (mut fg_color, mut fg_mode) = (a.fg_color(), a.fg_mode());
        let (mut bg_color, mut bg_mode) = (a.bg_color(), a.bg_mode());
        if inverse {
            std::mem::swap(&mut fg_color, &mut bg_color);
            std::mem::swap(&mut fg_mode, &mut bg_mode);
        }

        // The background, with 'copy' so whatever glyph was there before goes
        let background = self.background_color(bg_mode, bg_color, inverse);
        let _ = ctx.set_global_composite_operation("copy");
        ctx.set_fill_style_str(&background.css);
        ctx.fill_rect(0.0, 0.0, tmp_w, tmp_h);
        let _ = ctx.set_global_composite_operation("source-over");

        let cfg = &self.config;
        let font_weight = if bold {
            &cfg.font_weight_bold
        } else {
            &cfg.font_weight
        };
        let font_style = if italic { "italic" } else { "" };
        ctx.set_font(&format!(
            "{font_style} {font_weight} {}px {}",
            cfg.font_size * cfg.device_pixel_ratio,
            cfg.font_family
        ));
        ctx.set_text_baseline(text_baseline());

        let powerline = len16 == 1 && is_powerline_glyph(first_unit);
        let restricted_powerline = len16 == 1 && is_restricted_powerline_glyph(first_unit);
        let foreground = self.foreground_color(
            bg,
            bg_mode,
            bg_color,
            fg,
            fg_mode,
            fg_color,
            inverse,
            dim,
            bold,
            treat_glyph_as_background_color(first_unit),
        );
        ctx.set_fill_style_str(&foreground.css);
        let cfg = &self.config;

        // Powerline glyphs get no left/top padding (microsoft/vscode#120129)
        let padding = if restricted_powerline {
            0.0
        } else {
            TMP_CANVAS_GLYPH_PADDING * 2.0
        };
        let custom_glyph = cfg.custom_glyphs
            && try_draw_custom_char(
                &ctx,
                &chars,
                padding,
                padding,
                cell_w,
                cell_h,
                cfg.font_size,
                cfg.device_pixel_ratio,
            );

        // Clear pixels close to the background colour, unless the glyph has several colours (an underline colour)
        let mut enable_clear_threshold_check = !powerline;
        let ch_width = match key {
            Key::Code(code) => wcwidth(code),
            Key::Chars(chars) => wcwidth(chars.chars().next().map_or(0, u32::from)),
        };

        if underline {
            ctx.save();
            let line_width = (cfg.font_size * cfg.device_pixel_ratio / 15.0)
                .floor()
                .max(1.0);
            // An odd line width is drawn at a .5 position
            let y_offset = if line_width % 2.0 == 1.0 { 0.5 } else { 0.0 };
            ctx.set_line_width(line_width);
            if a.is_underline_color_default() {
                stroke_like_fill(&ctx);
            } else if a.is_underline_color_rgb() {
                enable_clear_threshold_check = false;
                let [r, g, b] = to_color_rgb(a.underline_color() as u32);
                ctx.set_stroke_style_str(&format!("rgb({r},{g},{b})"));
            } else {
                enable_clear_threshold_check = false;
                let mut color = a.underline_color();
                if cfg.draw_bold_text_in_bright_colors && a.is_bold() && color < 8 {
                    color += 8;
                }
                ctx.set_stroke_style_str(&self.ansi(color).css);
            }
            let cfg = &self.config;
            ctx.begin_path();
            let x_left = padding;
            let y_top = (padding + cfg.device_char_height).ceil() - y_offset;
            let y_mid = y_top + line_width;
            let y_bot = y_top + line_width * 2.0;
            let mut next_offset = a.underline_variant_offset() as f64;
            for i in 0..ch_width {
                ctx.save();
                let x_left_ch = x_left + i as f64 * cell_w;
                let x_right_ch = x_left + (i + 1) as f64 * cell_w;
                let x_mid_ch = x_left_ch + cell_w / 2.0;
                match a.underline_style() {
                    UnderlineStyle::Double => {
                        ctx.move_to(x_left_ch, y_top);
                        ctx.line_to(x_right_ch, y_top);
                        ctx.move_to(x_left_ch, y_bot);
                        ctx.line_to(x_right_ch, y_bot);
                    }
                    UnderlineStyle::Curly => {
                        // Taller curves when the line is thick, or the wave is hard to see
                        let y_curly_bot = if line_width <= 1.0 {
                            y_bot
                        } else {
                            (padding + cfg.device_char_height - line_width / 2.0).ceil() - y_offset
                        };
                        let y_curly_top = if line_width <= 1.0 {
                            y_top
                        } else {
                            (padding + cfg.device_char_height + line_width / 2.0).ceil() - y_offset
                        };
                        // Clipped to the cell and drawn half a cell past each edge, so neighbouring cells join
                        if let Ok(clip) = Path2d::new() {
                            clip.rect(x_left_ch, y_top, cell_w, y_bot - y_top);
                            ctx.clip_with_path_2d(&clip);
                        }
                        ctx.move_to(x_left_ch - cell_w / 2.0, y_mid);
                        ctx.bezier_curve_to(
                            x_left_ch - cell_w / 2.0,
                            y_curly_top,
                            x_left_ch,
                            y_curly_top,
                            x_left_ch,
                            y_mid,
                        );
                        ctx.bezier_curve_to(
                            x_left_ch,
                            y_curly_bot,
                            x_mid_ch,
                            y_curly_bot,
                            x_mid_ch,
                            y_mid,
                        );
                        ctx.bezier_curve_to(
                            x_mid_ch,
                            y_curly_top,
                            x_right_ch,
                            y_curly_top,
                            x_right_ch,
                            y_mid,
                        );
                        ctx.bezier_curve_to(
                            x_right_ch,
                            y_curly_bot,
                            x_right_ch + cell_w / 2.0,
                            y_curly_bot,
                            x_right_ch + cell_w / 2.0,
                            y_mid,
                        );
                    }
                    UnderlineStyle::Dotted => {
                        let offset_width = if next_offset == 0.0 {
                            0.0
                        } else if next_offset >= line_width {
                            line_width * 2.0 - next_offset
                        } else {
                            line_width - next_offset
                        };
                        let is_line_start = next_offset < line_width;
                        let dash = super::color::js_round(line_width);
                        let _ = ctx.set_line_dash(&js_sys::Array::of2(&dash.into(), &dash.into()));
                        if !is_line_start || offset_width == 0.0 {
                            ctx.move_to(x_left_ch + offset_width, y_top);
                            ctx.line_to(x_right_ch, y_top);
                        } else {
                            ctx.move_to(x_left_ch, y_top);
                            ctx.line_to(x_left_ch + offset_width, y_top);
                            ctx.move_to(x_left_ch + offset_width + line_width, y_top);
                            ctx.line_to(x_right_ch, y_top);
                        }
                        next_offset = compute_next_variant_offset(
                            x_right_ch - x_left_ch,
                            line_width,
                            next_offset,
                        );
                    }
                    UnderlineStyle::Dashed => {
                        let width = x_right_ch - x_left_ch;
                        let line = (0.6 * width).floor();
                        let gap = (0.3 * width).floor();
                        let end = width - line - gap;
                        let _ = ctx.set_line_dash(&js_sys::Array::of3(
                            &line.into(),
                            &gap.into(),
                            &end.into(),
                        ));
                        ctx.move_to(x_left_ch, y_top);
                        ctx.line_to(x_right_ch, y_top);
                    }
                    _ => {
                        ctx.move_to(x_left_ch, y_top);
                        ctx.line_to(x_right_ch, y_top);
                    }
                }
                ctx.stroke();
                ctx.restore();
            }
            ctx.restore();

            // Outline the text in the background colour where it crosses the underline (font sizes from 12 up,
            // opaque backgrounds only)
            if !custom_glyph && cfg.font_size >= 12.0 && !cfg.allow_transparency && chars != " " {
                ctx.save();
                ctx.set_text_baseline("alphabetic");
                let descent = ctx
                    .measure_text(&chars)
                    .map_or(0.0, |m| m.actual_bounding_box_descent());
                ctx.restore();
                if descent > 0.0 {
                    ctx.save();
                    if let Ok(clip) = Path2d::new() {
                        clip.rect(
                            x_left,
                            y_top - (line_width / 2.0).ceil(),
                            cell_w * ch_width as f64,
                            y_bot - y_top + (line_width / 2.0).ceil(),
                        );
                        ctx.clip_with_path_2d(&clip);
                    }
                    ctx.set_line_width(cfg.device_pixel_ratio * 3.0);
                    ctx.set_stroke_style_str(&background.css);
                    let _ = ctx.stroke_text(&chars, padding, padding + cfg.device_char_height);
                    ctx.restore();
                }
            }
        }

        let cfg = &self.config;
        if overline {
            let line_width = (cfg.font_size * cfg.device_pixel_ratio / 15.0)
                .floor()
                .max(1.0);
            let y_offset = if line_width % 2.0 == 1.0 { 0.5 } else { 0.0 };
            ctx.set_line_width(line_width);
            stroke_like_fill(&ctx);
            ctx.begin_path();
            ctx.move_to(padding, padding + y_offset);
            ctx.line_to(
                padding + cfg.device_char_width * ch_width as f64,
                padding + y_offset,
            );
            ctx.stroke();
        }

        if !custom_glyph {
            let _ = ctx.fill_text(&chars, padding, padding + cfg.device_char_height);
        }

        // An underscore below the cell is moved up until it shows, even on the last row (up to 5 pixels)
        if chars == "_" && !cfg.allow_transparency {
            let beyond = |ctx: &CanvasRenderingContext2d| {
                ctx.get_image_data(padding, padding, cell_w, cell_h)
                    .map(|data| {
                        let mut data = data.data().0;
                        clear_color(
                            &mut data,
                            &background,
                            &foreground,
                            enable_clear_threshold_check,
                        )
                    })
                    .unwrap_or(false)
            };
            if beyond(&ctx) {
                for offset in 1..=5 {
                    ctx.save();
                    ctx.set_fill_style_str(&background.css);
                    ctx.fill_rect(0.0, 0.0, tmp_w, tmp_h);
                    ctx.restore();
                    let _ = ctx.fill_text(
                        &chars,
                        padding,
                        padding + cfg.device_char_height - offset as f64,
                    );
                    if !beyond(&ctx) {
                        break;
                    }
                }
            }
        }

        if strikethrough {
            let line_width = (cfg.font_size * cfg.device_pixel_ratio / 10.0)
                .floor()
                .max(1.0);
            // xterm.js reads the line width before setting it here
            let y_offset = if ctx.line_width() % 2.0 == 1.0 {
                0.5
            } else {
                0.0
            };
            ctx.set_line_width(line_width);
            stroke_like_fill(&ctx);
            ctx.begin_path();
            let y = padding + (cfg.device_char_height / 2.0).floor() - y_offset;
            ctx.move_to(padding, y);
            ctx.line_to(padding + cfg.device_char_width * ch_width as f64, y);
            ctx.stroke();
        }

        ctx.restore();

        // Clear the background so the glyph does not paint over neighbours it overlaps
        let mut data = ctx.get_image_data(0.0, 0.0, tmp_w, tmp_h).ok()?.data().0;
        let empty = if !self.config.allow_transparency {
            clear_color(
                &mut data,
                &background,
                &foreground,
                enable_clear_threshold_check,
            )
        } else {
            data.chunks_exact(4).all(|px| px[3] == 0)
        };
        if empty {
            return None;
        }

        let (mut glyph, bounds) = self.find_glyph_bounding_box(
            &data,
            tmp_w as usize,
            tmp_h as usize,
            allowed_width,
            restricted_powerline,
            custom_glyph,
            padding,
        );
        let (page_id, row) = self.place(glyph.size)?;
        let page_index = self.page_index(page_id)?;
        let page = &mut self.pages[page_index];
        let (row_x, row_y) = {
            let r = page.row(row);
            (r.x, r.y)
        };
        glyph.texture_page = page_index;
        glyph.texture_position = (row_x, row_y);
        glyph.texture_position_clip_space =
            (row_x / page.width(), row_y / page.canvas.height() as f64);
        // Pages differ in size: clip space is relative to this one
        glyph.size_clip_space.0 /= page.width();
        glyph.size_clip_space.1 /= page.canvas.height() as f64;
        let r = page.row(row);
        r.height = r.height.max(glyph.size.1);
        r.x += glyph.size.0;

        // putImageData doesn't blend, so it overwrites whatever was cached there
        let image =
            ImageData::new_with_u8_clamped_array_and_sh(Clamped(&data), tmp_w as u32, tmp_h as u32)
                .ok()?;
        let _ = page
            .ctx
            .put_image_data_with_dirty_x_and_dirty_y_and_dirty_width_and_dirty_height(
                &image,
                glyph.texture_position.0 - bounds.0,
                glyph.texture_position.1 - bounds.1,
                bounds.0,
                bounds.1,
                glyph.size.0,
                glyph.size.1,
            );
        let index = self.glyphs.len();
        self.glyphs.push(glyph);
        page.glyphs.push(index);
        page.used_pixels += glyph.size.0 * glyph.size.1;
        page.version += 1;
        Some(index)
    }

    /// The page and row a glyph of `size` goes to (the row search of `_drawToCache`).
    fn place(&mut self, size: (f64, f64)) -> Option<(u64, RowRef)> {
        loop {
            // No active pages left (the smallest were merged): start a new one
            if self.active_pages.is_empty() {
                let id = self.create_new_page()?;
                let i = self.page_index(id)?;
                self.pages[i].current_row.height = size.1;
                return Some((id, RowRef::Current));
            }
            let mut page = *self.active_pages.last()?;
            let mut row = RowRef::Current;
            for &p in &self.active_pages {
                let i = self.page_index(p)?;
                if size.1 <= self.pages[i].current_row.height {
                    page = p;
                    row = RowRef::Current;
                }
            }
            // A fixed row at least as good as the current one wins; older pages first
            for &p in self.active_pages.iter().rev() {
                let i = self.page_index(p)?;
                for (j, fixed) in self.pages[i].fixed_rows.iter().enumerate() {
                    let current_height = {
                        let k = self.page_index(page)?;
                        self.pages[k].row_at(row).height
                    };
                    if fixed.height <= current_height && size.1 <= fixed.height {
                        page = p;
                        row = RowRef::Fixed(j);
                    }
                }
            }

            // Oversized glyphs get a page of their own size
            if size.0 > TEXTURE_SIZE {
                let id = match self.overflow_size_page {
                    Some(id) => id,
                    None => {
                        let id = self.next_page_id;
                        self.next_page_id += 1;
                        let overflow = AtlasPage::new(
                            &self.document,
                            id,
                            self.config.device_max_texture_size,
                        )?;
                        self.pages.push(overflow);
                        self.overflow_size_page = Some(id);
                        self.request_clear_model = true;
                        id
                    }
                };
                let i = self.page_index(id)?;
                let width = self.pages[i].width();
                let r = &mut self.pages[i].current_row;
                if r.x + size.0 >= width {
                    r.x = 0.0;
                    r.y += r.height;
                    r.height = 0.0;
                }
                return Some((id, RowRef::Current));
            }

            let mut i = self.page_index(page)?;
            let (row_y, row_height) = {
                let r = self.pages[i].row(row);
                (r.y, r.height)
            };
            let page_height = self.pages[i].canvas.height() as f64;
            // A new row (or page) when too much height would be wasted or the page is full; the current row
            // becomes fixed
            if row_y + size.1 >= page_height || row_height > size.1 + ROW_PIXEL_THRESHOLD {
                let mut found = false;
                let cur = self.pages[i].current_row;
                if cur.y + cur.height + size.1 >= page_height {
                    let candidate = self.active_pages.iter().copied().find(|&p| {
                        self.page_index(p).is_some_and(|k| {
                            let c = self.pages[k].current_row;
                            c.y + c.height + size.1 < self.pages[k].canvas.height() as f64
                        })
                    });
                    if let Some(candidate) = candidate {
                        page = candidate;
                        i = self.page_index(page)?;
                    } else {
                        // Before a page that would trigger a merge, use the current row ignoring the threshold
                        let r = *self.pages[i].row(row);
                        let width = self.pages[i].width();
                        if max_atlas_pages().is_some_and(|m| self.pages.len() >= m as usize)
                            && r.y + size.1 <= page_height
                            && r.height >= size.1
                            && r.x + size.0 <= width
                        {
                            found = true;
                        } else {
                            let id = self.create_new_page()?;
                            page = id;
                            i = self.page_index(id)?;
                            row = RowRef::Current;
                            self.pages[i].current_row.height = size.1;
                            found = true;
                        }
                    }
                }
                if !found {
                    let p = &mut self.pages[i];
                    if p.current_row.height > 0.0 {
                        p.fixed_rows.push(p.current_row);
                    }
                    let fixed = Row {
                        x: 0.0,
                        y: p.current_row.y + p.current_row.height,
                        height: size.1,
                    };
                    p.fixed_rows.push(fixed);
                    row = RowRef::Fixed(p.fixed_rows.len() - 1);
                    p.current_row = Row {
                        x: 0.0,
                        y: fixed.y + fixed.height,
                        height: 0.0,
                    };
                }
            }

            let p = &mut self.pages[i];
            let width = p.width();
            if p.row(row).x + size.0 <= width {
                return Some((page, row));
            }
            // Not enough room in the row: finish it and try again
            match row {
                RowRef::Current => {
                    let r = &mut p.current_row;
                    r.x = 0.0;
                    r.y += r.height;
                    r.height = 0.0;
                }
                RowRef::Fixed(j) => {
                    if j < p.fixed_rows.len() {
                        p.fixed_rows.remove(j);
                    }
                }
            }
        }
    }

    /// The bounding box of the glyph's opaque pixels, as a glyph without a page yet, and the box's `(left, top)`.
    #[allow(clippy::too_many_arguments)]
    fn find_glyph_bounding_box(
        &self,
        data: &[u8],
        tmp_w: usize,
        tmp_h: usize,
        allowed_width: f64,
        restricted: bool,
        custom: bool,
        padding: f64,
    ) -> (Glyph, (f64, f64)) {
        let cfg = &self.config;
        let height = if restricted {
            cfg.device_cell_height as usize
        } else {
            tmp_h
        };
        let width = if restricted {
            cfg.device_cell_width as usize
        } else {
            allowed_width as usize
        };
        let pad = padding as usize;
        // Past the end counts as ink, as `undefined !== 0` does in xterm.js
        let ink = |x: usize, y: usize| data.get(y * tmp_w * 4 + x * 4 + 3).is_none_or(|&a| a != 0);
        let top = (0..height)
            .find(|&y| (0..width).any(|x| ink(x, y)))
            .unwrap_or(0);
        let left = (0..pad + width)
            .find(|&x| (0..height).any(|y| ink(x, y)))
            .unwrap_or(0);
        let right = (pad..pad + width)
            .rev()
            .find(|&x| (0..height).any(|y| ink(x, y)))
            .unwrap_or(width);
        let bottom = (0..height)
            .rev()
            .find(|&y| (0..width).any(|x| ink(x, y)))
            .unwrap_or(height);
        let (w, h) = (
            right as f64 - left as f64 + 1.0,
            bottom as f64 - top as f64 + 1.0,
        );
        let centred = restricted || custom;
        let glyph = Glyph {
            size: (w, h),
            size_clip_space: (w, h),
            offset: (
                -(left as f64)
                    + padding
                    + if centred {
                        ((cfg.device_cell_width - cfg.device_char_width) / 2.0).floor()
                    } else {
                        0.0
                    },
                -(top as f64)
                    + padding
                    + if centred && cfg.line_height != 1.0 {
                        super::color::js_round(
                            (cfg.device_cell_height - cfg.device_char_height) / 2.0,
                        )
                    } else {
                        0.0
                    },
            ),
            ..Glyph::default()
        };
        (glyph, (left as f64, top as f64))
    }
}

fn compute_next_variant_offset(cell_width: f64, line_width: f64, current: f64) -> f64 {
    let period = super::color::js_round(line_width) * 2.0;
    (cell_width - (period - current)) % period
}

/// Firefox (and legacy Edge) truncate text with 'ideographic'; Chrome misaligns Powerline fonts with 'bottom'.
fn text_baseline() -> &'static str {
    thread_local! {
        static BOTTOM: bool = web_sys::window()
            .and_then(|w| w.navigator().user_agent().ok())
            .is_some_and(|ua| ua.contains("Firefox") || ua.contains("Edge"));
    }
    if BOTTOM.with(|b| *b) {
        "bottom"
    } else {
        "ideographic"
    }
}

/// Makes the background colour, and colours within a threshold of it, transparent. True when nothing is left.
fn clear_color(data: &mut [u8], bg: &Color, fg: &Color, threshold_check: bool) -> bool {
    let (r, g, b) = (bg.rgba >> 24, (bg.rgba >> 16) & 0xff, (bg.rgba >> 8) & 0xff);
    let (fr, fg_, fb) = (fg.rgba >> 24, (fg.rgba >> 16) & 0xff, (fg.rgba >> 8) & 0xff);
    // Relative to the fg/bg difference so details survive low contrast; 12 covers fg=#8ae234 on bg=#c4a000
    let threshold = (r.abs_diff(fr) + g.abs_diff(fg_) + b.abs_diff(fb)) / 12;
    let mut empty = true;
    for px in data.chunks_exact_mut(4) {
        let (pr, pg, pb) = (px[0] as u32, px[1] as u32, px[2] as u32);
        let exact = pr == r && pg == g && pb == b;
        if exact
            || (threshold_check && pr.abs_diff(r) + pg.abs_diff(g) + pb.abs_diff(b) < threshold)
        {
            px[3] = 0;
        } else {
            empty = false;
        }
    }
    empty
}
