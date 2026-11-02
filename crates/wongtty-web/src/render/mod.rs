//! What the terminal needs from a renderer. The WebGL2 renderer implements it; the terminal owns the frame loop
//! (`requestAnimationFrame`), the engine and the DOM element the canvas goes into.
use wasm_bindgen::JsValue;

use crate::engine::Viewport;

mod api;
mod atlas;
mod blink;
mod cell;
pub mod color;
mod cursor_layer;
mod custom_glyphs;
mod gl;
mod glyphs;
mod gpu;
mod images;
mod model;
pub mod options;
mod rects;
pub mod webgl;

/// Colours as CSS strings, named like xterm.js `ITheme`. `None` keeps the default.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Theme {
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub cursor: Option<String>,
    pub cursor_accent: Option<String>,
    pub selection_background: Option<String>,
    pub selection_foreground: Option<String>,
    pub selection_inactive_background: Option<String>,
    /// ANSI 0–15 (black … brightWhite).
    pub ansi: [Option<String>; 16],
    /// Palette entries 16–255.
    pub extended_ansi: Vec<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorStyle {
    #[default]
    Block,
    Underline,
    Bar,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorInactiveStyle {
    #[default]
    Outline,
    Block,
    Bar,
    Underline,
    None,
}

/// The xterm.js options a renderer reads. `Default` gives the xterm.js defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct RendererOptions {
    pub font_family: String,
    pub font_size: f64,
    /// CSS `font-weight` values.
    pub font_weight: String,
    pub font_weight_bold: String,
    pub line_height: f64,
    pub letter_spacing: f64,
    pub theme: Theme,
    pub draw_bold_text_in_bright_colors: bool,
    pub minimum_contrast_ratio: f64,
    pub custom_glyphs: bool,
    pub allow_transparency: bool,
    pub rescale_overlapping_glyphs: bool,
    pub cursor_blink: bool,
    pub cursor_style: CursorStyle,
    pub cursor_width: f64,
    pub cursor_inactive_style: CursorInactiveStyle,
}

impl Default for RendererOptions {
    fn default() -> Self {
        Self {
            font_family: "monospace".into(),
            font_size: 15.0,
            font_weight: "normal".into(),
            font_weight_bold: "bold".into(),
            line_height: 1.0,
            letter_spacing: 0.0,
            theme: Theme::default(),
            draw_bold_text_in_bright_colors: true,
            minimum_contrast_ratio: 1.0,
            custom_glyphs: true,
            allow_transparency: false,
            rescale_overlapping_glyphs: false,
            cursor_blink: false,
            cursor_style: CursorStyle::Block,
            cursor_width: 1.0,
            cursor_inactive_style: CursorInactiveStyle::Outline,
        }
    }
}

/// Selection in viewport cells `(col, row)`, end exclusive; `column_select` is a rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionRange {
    pub start: (u32, u32),
    pub end: (u32, u32),
    pub column_select: bool,
}

/// Cells the host paints over (search matches, hovered links), in viewport cells, end column exclusive.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Highlight {
    pub row: u32,
    pub start: u32,
    pub end: u32,
    /// CSS colours; `None` keeps the cell's own.
    pub background: Option<String>,
    pub foreground: Option<String>,
    /// Outline drawn around the cells (the active search match).
    pub border: Option<String>,
    pub underline: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Size {
    pub width: f64,
    pub height: f64,
}

/// Where a glyph sits in its cell, in device pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CharSize {
    pub width: f64,
    pub height: f64,
    pub left: f64,
    pub top: f64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Dimensions {
    /// CSS pixels of one cell and of the whole canvas.
    pub css_cell: Size,
    pub css_canvas: Size,
    /// Device pixels (CSS × devicePixelRatio).
    pub device_cell: Size,
    pub device_canvas: Size,
    pub device_char: CharSize,
}

/// Rows a render pass must redraw besides those the engine changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Dirty {
    All,
    Rows(Vec<u32>),
}

/// Something the renderer noticed on its own, handed to the terminal on the next frame (never by calling back
/// into it, see the crate docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RendererRequest {
    /// Cell size changed without a call from the host (fonts finished loading, devicePixelRatio changed): refit.
    DimensionsChanged,
    /// A frame is needed without new output (cursor blink, WebGL context restored with [`Dirty::All`]).
    Redraw(Dirty),
}

pub trait Renderer {
    fn dimensions(&self) -> Dimensions;
    /// Re-measures and rebuilds glyphs for the options that change them, then redraws.
    fn set_options(&mut self, options: &RendererOptions);
    /// Resizes the canvas; the next `render` gets a full frame.
    fn resize(&mut self, cols: u32, rows: u32);
    /// Draws rows that changed: `changed` comes from [`Viewport::apply`], `dirty` are rows the host changed.
    /// The renderer reads cells only from `viewport`.
    fn render(&mut self, viewport: &Viewport, changed: &[u32], dirty: &Dirty);
    fn set_selection(&mut self, selection: Option<SelectionRange>);
    /// Replaces one named layer (`"search"`, `"link"`); layers draw in the order they were first set, under the
    /// selection.
    fn set_highlights(&mut self, layer: &str, highlights: Vec<Highlight>);
    /// Focus picks the cursor style (`cursor_inactive_style`) and runs the blink, which the renderer times itself.
    fn set_focused(&mut self, focused: bool);
    /// Requests since the last call; the terminal polls this when the renderer wakes it (see the crate docs).
    fn take_requests(&mut self) -> Vec<RendererRequest>;
    /// Whether image tiles whose image is no longer stored show the placeholder (an image addon is loaded with
    /// `showPlaceholder`); without an addon they show nothing.
    fn set_image_placeholder(&mut self, _show: bool) {}
    /// Internal state for the tests (`term._rendererDebug()`).
    fn debug(&self) -> JsValue {
        JsValue::UNDEFINED
    }
}

/// The image `id` was deleted: its texture goes.
pub(crate) fn release_image(id: u32) {
    gpu::release_image(id);
}

/// GPU textures of images, for the tests.
pub(crate) fn image_textures() -> u32 {
    gpu::image_textures()
}
