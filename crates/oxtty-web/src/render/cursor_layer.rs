//! The blinking cursor's own small canvas, laid over the pane's canvas.
//!
//! While any WebGL canvas redraws periodically, Chromium keeps tens of MB of GPU memory dirty that it otherwise
//! marks purgeable (measured in Electron on macOS: a blinking pane held ~100 MB of the GPU process). So a pane
//! whose cursor blinks draws its canvas once with the cursor hidden, and the area around the cursor once with
//! it shown into this layer; the blink only flips the layer's CSS opacity, with no WebGL draw and no copy.
//!
//! The layer holds the visible-phase pixels of the cursor cells and the cells around them (a glyph drawn with the
//! cursor's colours can spill over its cell), so with it shown the pane looks exactly as the visible phase drawn in
//! full. Its pixels must be opaque to replace those under it: a pane with a translucent background keeps redrawing.
use wasm_bindgen::JsCast;
use web_sys::{Document, HtmlCanvasElement};

use super::gpu::Present;

/// Cells around the cursor cell the layer covers: left, right (a wide glyph and what it spills), rows above and
/// below (glyphs are a few pixels taller than their cell).
const MARGIN_LEFT: u32 = 1;
const MARGIN_RIGHT: u32 = 4;
const MARGIN_ROWS: u32 = 1;

/// Device pixels `[x, y, width, height]` of the pane's canvas.
pub type Region = [f64; 4];

pub struct CursorLayer {
    canvas: HtmlCanvasElement,
    pub present: Present,
    region: Region,
    shown: Option<bool>,
}

impl CursorLayer {
    /// A layer right above `canvas`, the pane's canvas.
    pub fn new(document: &Document, canvas: &HtmlCanvasElement) -> Option<Self> {
        let layer: HtmlCanvasElement = document.create_element("canvas").ok()?.unchecked_into();
        let present = Present::new(&layer)?;
        let style = layer.style();
        let _ = style.set_property("position", "absolute");
        let _ = style.set_property("pointer-events", "none");
        // Its own compositor layer, so an opacity change repaints nothing
        let _ = style.set_property("will-change", "opacity");
        layer.set_width(0);
        layer.set_height(0);
        let parent = canvas.parent_node()?;
        parent
            .insert_before(&layer, canvas.next_sibling().as_ref())
            .ok()?;
        Some(Self {
            canvas: layer,
            present,
            region: [0.0; 4],
            shown: None,
        })
    }

    /// The region for a cursor at cell (`col`, `row`), clamped to the pane; `bar` is the bar cursor's width in
    /// device pixels, which may be wider than a cell.
    pub fn region_for(
        col: u32,
        row: u32,
        (cols, rows): (u32, u32),
        (cell_width, cell_height): (f64, f64),
        bar: f64,
    ) -> Region {
        let left = col.saturating_sub(MARGIN_LEFT) as f64 * cell_width;
        let right = (col.saturating_add(1 + MARGIN_RIGHT).min(cols) as f64 * cell_width).max(
            (col as f64 * cell_width + bar)
                .min(cols as f64 * cell_width)
                .ceil(),
        );
        let top = row.saturating_sub(MARGIN_ROWS) as f64 * cell_height;
        let bottom = row.saturating_add(1 + MARGIN_ROWS).min(rows) as f64 * cell_height;
        [left, top, right - left, bottom - top]
    }

    /// Rows whose change may change the layer's pixels (glyphs of the rows next to it reach into it).
    pub fn rows_around(row: u32) -> std::ops::RangeInclusive<u32> {
        row.saturating_sub(MARGIN_ROWS + 1)..=row.saturating_add(MARGIN_ROWS + 1)
    }

    pub fn region(&self) -> Region {
        self.region
    }

    /// Moves and sizes the layer over `region`; `scale` is CSS pixels per device pixel of the pane's canvas.
    pub fn place(&mut self, region: Region, (scale_x, scale_y): (f64, f64)) {
        if region == self.region {
            return;
        }
        let [x, y, width, height] = region;
        if (width, height) != (self.region[2], self.region[3]) {
            self.canvas.set_width(width as u32);
            self.canvas.set_height(height as u32);
        }
        self.region = region;
        let style = self.canvas.style();
        let _ = style.set_property("left", &format!("{}px", x * scale_x));
        let _ = style.set_property("top", &format!("{}px", y * scale_y));
        let _ = style.set_property("width", &format!("{}px", width * scale_x));
        let _ = style.set_property("height", &format!("{}px", height * scale_y));
    }

    pub fn show(&mut self, shown: bool) {
        if self.shown == Some(shown) {
            return;
        }
        self.shown = Some(shown);
        let _ = self
            .canvas
            .style()
            .set_property("opacity", if shown { "1" } else { "0" });
    }

    pub fn canvas(&self) -> &HtmlCanvasElement {
        &self.canvas
    }
}

impl Drop for CursorLayer {
    fn drop(&mut self) {
        self.canvas.remove();
    }
}
