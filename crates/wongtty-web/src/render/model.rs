//! The render model (four words per cell), the selection model and the colour resolver that applies selection,
//! highlights and inverse to each cell before it is drawn.
//!
//! Ported from xterm.js 6.0.0 (commit f447274f430fd22513f6adbf9862d19524471c04, MIT):
//! `addons/addon-webgl/src/RenderModel.ts` lines 10-41, `src/browser/renderer/shared/SelectionRenderModel.ts`
//! lines 10-93 and `addons/addon-webgl/src/CellColorResolver.ts` lines 1-236. Highlight layers take the place
//! of xterm.js's bottom-layer decorations (`IDecorationService.forEachDecorationAtCell`).
use super::atlas::treat_glyph_as_background_color;
use super::cell::{
    Attrs, CellData, UnderlineStyle, BG_DIM, BG_HAS_EXTENDED, CM_DEFAULT, CM_MASK, CM_P16, CM_P256,
    CM_RGB, EXT_VARIANT_OFFSET, FG_INVERSE, PCOLOR_MASK, RGB_MASK,
};
use super::color::{blend_rgba, ColorSet};
use super::{CursorInactiveStyle, SelectionRange};

pub const RENDER_MODEL_INDICIES_PER_CELL: usize = 4;
pub const RENDER_MODEL_BG_OFFSET: usize = 1;
pub const RENDER_MODEL_FG_OFFSET: usize = 2;
pub const RENDER_MODEL_EXT_OFFSET: usize = 3;
pub const COMBINED_CHAR_BIT_MASK: u32 = 0x8000_0000;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CursorModel {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub style: CursorInactiveStyle,
    pub cursor_width: f64,
    pub dpr: f64,
}

#[derive(Default)]
pub struct RenderModel {
    pub cells: Vec<u32>,
    pub line_lengths: Vec<u32>,
    pub cursor: Option<CursorModel>,
}

impl RenderModel {
    pub fn resize(&mut self, cols: u32, rows: u32) {
        let count = cols as usize * rows as usize * RENDER_MODEL_INDICIES_PER_CELL;
        if count != self.cells.len() {
            self.cells = vec![0; count];
            self.line_lengths = vec![0; rows as usize];
        }
    }

    pub fn clear(&mut self) {
        self.cells.fill(0);
        self.line_lengths.fill(0);
    }
}

/// The selection in viewport rows (the viewport is the whole picture, so no scroll offset applies).
#[derive(Clone, Copy, Debug, Default)]
pub struct SelectionModel {
    pub has_selection: bool,
    column_select: bool,
    start_row: i64,
    end_row: i64,
    pub capped_start_row: i64,
    pub capped_end_row: i64,
    start_col: i64,
    end_col: i64,
}

impl SelectionModel {
    pub fn update(&mut self, rows: u32, selection: Option<SelectionRange>) {
        *self = Self::default();
        let Some(s) = selection else { return };
        if s.start == s.end {
            return;
        }
        let (start_row, end_row) = (s.start.1 as i64, s.end.1 as i64);
        let capped_start_row = start_row.max(0);
        let capped_end_row = end_row.min(rows as i64 - 1);
        if capped_start_row >= rows as i64 || capped_end_row < 0 {
            return;
        }
        *self = Self {
            has_selection: true,
            column_select: s.column_select,
            start_row,
            end_row,
            capped_start_row,
            capped_end_row,
            start_col: s.start.0 as i64,
            end_col: s.end.0 as i64,
        };
    }

    pub fn is_cell_selected(&self, x: u32, y: u32) -> bool {
        if !self.has_selection {
            return false;
        }
        let (x, y) = (x as i64, y as i64);
        if self.column_select {
            if self.start_col <= self.end_col {
                return x >= self.start_col
                    && y >= self.capped_start_row
                    && x < self.end_col
                    && y <= self.capped_end_row;
            }
            return x < self.start_col
                && y >= self.capped_start_row
                && x >= self.end_col
                && y <= self.capped_end_row;
        }
        (y > self.start_row && y < self.end_row)
            || (self.start_row == self.end_row
                && y == self.start_row
                && x >= self.start_col
                && x < self.end_col)
            || (self.start_row < self.end_row && y == self.end_row && x < self.end_col)
            || (self.start_row < self.end_row && y == self.start_row && x >= self.start_col)
    }
}

/// A highlight's colour overrides (`0xRRGGBB`) on one row, end column exclusive.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ResolvedHighlight {
    pub row: u32,
    pub start: u32,
    pub end: u32,
    pub background: Option<u32>,
    pub foreground: Option<u32>,
}

/// `CellColorResolver.resolve`: the fg/bg/ext a cell is drawn with after selection, highlights and inverse.
#[allow(clippy::too_many_arguments)]
pub fn resolve(
    cell: &CellData,
    x: u32,
    y: u32,
    device_cell_width: f64,
    colors: &ColorSet,
    selection: &SelectionModel,
    highlights: &[ResolvedHighlight],
    focused: bool,
    font_size: f64,
    dpr: f64,
) -> Attrs {
    let a = cell.attrs;
    let mut result = Attrs {
        fg: a.fg,
        bg: a.bg,
        ext: if a.bg & BG_HAS_EXTENDED != 0 {
            a.ext
        } else {
            0
        },
    };
    let (mut bg, mut fg) = (0u32, 0u32);
    let (mut has_bg, mut has_fg) = (false, false);
    let mut variant_offset = 0u32;

    if cell.code != 0 && a.underline_style() == UnderlineStyle::Dotted {
        let line_width = (font_size * dpr / 15.0).floor().max(1.0);
        let period = super::color::js_round(line_width) as i64 * 2;
        if period > 0 {
            variant_offset = ((x as i64 * device_cell_width as i64) % period) as u32;
        }
    }

    // Highlights are the bottom layer, under the selection
    for h in highlights {
        if h.row == y && x >= h.start && x < h.end {
            if let Some(color) = h.background {
                bg = color & RGB_MASK;
                has_bg = true;
            }
            if let Some(color) = h.foreground {
                fg = color & RGB_MASK;
                has_fg = true;
            }
        }
    }

    let selection_opaque = if focused {
        &colors.selection_background_opaque
    } else {
        &colors.selection_inactive_background_opaque
    };
    let palette = |word: u32| {
        colors
            .ansi
            .get((word & PCOLOR_MASK) as usize)
            .map_or(0, |c| c.rgba)
    };
    let selected = selection.is_cell_selected(x, y);
    if selected {
        // A cell with its own background keeps it, blended with the selection colour
        if result.fg & FG_INVERSE != 0 || result.bg & CM_MASK != CM_DEFAULT {
            let base = if result.fg & FG_INVERSE != 0 {
                match result.fg & CM_MASK {
                    CM_P16 | CM_P256 => palette(result.fg),
                    CM_RGB => ((result.fg & RGB_MASK) << 8) | 0xff,
                    _ => colors.foreground.rgba,
                }
            } else {
                match result.bg & CM_MASK {
                    CM_P16 | CM_P256 => palette(result.bg),
                    CM_RGB => ((result.bg & RGB_MASK) << 8) | 0xff,
                    _ => bg,
                }
            };
            bg = (blend_rgba(base, (selection_opaque.rgba & 0xffff_ff00) | 0x80) >> 8) & RGB_MASK;
        } else {
            bg = (selection_opaque.rgba >> 8) & RGB_MASK;
        }
        has_bg = true;

        if let Some(selection_fg) = &colors.selection_foreground {
            fg = (selection_fg.rgba >> 8) & RGB_MASK;
            has_fg = true;
        }

        // Decorative glyphs (Powerline, box drawing) take the selection as their colour
        if treat_glyph_as_background_color(cell.code) {
            if result.fg & FG_INVERSE != 0 && result.bg & CM_MASK == CM_DEFAULT {
                // An inverted default background counts as transparent
                fg = (selection_opaque.rgba >> 8) & RGB_MASK;
            } else {
                let base = if result.fg & FG_INVERSE != 0 {
                    match result.bg & CM_MASK {
                        CM_P16 | CM_P256 => palette(result.bg),
                        CM_RGB => ((result.bg & RGB_MASK) << 8) | 0xff,
                        _ => fg,
                    }
                } else {
                    match result.fg & CM_MASK {
                        CM_P16 | CM_P256 => palette(result.fg),
                        CM_RGB => ((result.fg & RGB_MASK) << 8) | 0xff,
                        _ => colors.foreground.rgba,
                    }
                };
                fg = (blend_rgba(base, (selection_opaque.rgba & 0xffff_ff00) | 0x80) >> 8)
                    & RGB_MASK;
            }
            has_fg = true;
        }
    }

    // Overrides in packed form, resolving inverse ahead of time for the right cache key
    if has_bg {
        let keep = if selected {
            a.bg & !RGB_MASK & !BG_DIM
        } else {
            a.bg & !RGB_MASK
        };
        bg = keep | bg | CM_RGB;
    }
    if has_fg {
        fg = (a.fg & !RGB_MASK & !FG_INVERSE) | fg | CM_RGB;
    }

    // Inverse with only one override: resolve the other colour and drop the inverse flag
    if result.fg & FG_INVERSE != 0 {
        if has_bg && !has_fg {
            fg = if result.bg & CM_MASK == CM_DEFAULT {
                (result.fg & !(RGB_MASK | FG_INVERSE | CM_MASK))
                    | ((colors.background.rgba >> 8) & RGB_MASK)
                    | CM_RGB
            } else {
                (result.fg & !(RGB_MASK | FG_INVERSE | CM_MASK))
                    | (result.bg & (RGB_MASK | CM_MASK))
            };
            has_fg = true;
        }
        if !has_bg && has_fg {
            bg = if result.fg & CM_MASK == CM_DEFAULT {
                (result.bg & !(RGB_MASK | CM_MASK))
                    | ((colors.foreground.rgba >> 8) & RGB_MASK)
                    | CM_RGB
            } else {
                (result.bg & !(RGB_MASK | CM_MASK)) | (result.fg & (RGB_MASK | CM_MASK))
            };
            has_bg = true;
        }
    }

    if has_bg {
        result.bg = bg;
    }
    if has_fg {
        result.fg = fg;
    }
    result.ext &= !EXT_VARIANT_OFFSET;
    result.ext |= (variant_offset << 29) & EXT_VARIANT_OFFSET;
    result
}
