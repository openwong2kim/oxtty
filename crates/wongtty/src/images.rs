//! Image tiles in cells, for a host that draws sixel and iTerm2 inline images the way `@xterm/addon-image` does:
//! the host decodes the image and keeps its pixels, the engine keeps which tile of which image covers each cell.
//! Tiles live in the cells, so they scroll with their lines, go into the (compressed) history with them and leave
//! with them, and text or an erase over a cell removes its tile, as xterm.js extended attributes do.
//!
//! Placement and the right-hand extension on resize follow `ImageStorage.addImage` and `viewportResize` of
//! `@xterm/addon-image` 0.10.0-beta.331 (xterm.js commit 08829c7eb6279dda2a9e86df901353a1a89858ca, MIT),
//! `addons/addon-image/src/ImageStorage.ts` lines 169-226 and 405-462.

use crate::vt::grid::Dimensions;
use crate::vt::index::{Column, Line, Point};
use crate::vt::term::cell::ImageTile;
use crate::vt::vte::ansi::Handler;
use crate::{Cell, Engine, flatten};

/// A cell an image tile covers ([`Engine::row_images`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageCell {
    pub col: u16,
    /// The id the host gave [`Engine::place_image`].
    pub id: u32,
    /// `row * cols + col` within the image, `cols` being its width in cells.
    pub tile: u32,
}

/// What [`Engine::place_image`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImagePlaced {
    /// Tiles it wrote.
    pub tiles: u32,
    /// The image ids of the tiles it covered, one per tile.
    pub covered: Vec<u32>,
}

/// Where [`Engine::place_image`] leaves the cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageCursor {
    /// At the image's left column, on its last row (sixel, as a VT340).
    Vt340,
    /// Right of the image, on its last row (iTerm2 inline images).
    Iip,
}

impl Engine {
    /// Covers `cols` × `rows` cells with the tiles of image `id`, from the cursor (`scrolling`, lines feeding and
    /// the screen scrolling as needed) or from the top left of the screen (sixel scrolling off, DECSET 80; the
    /// cursor stays). Columns past the right edge are cut off. An image without columns only moves the cursor down
    /// `rows - 1` lines (an empty sixel with a height).
    pub fn place_image(
        &mut self,
        id: u32,
        cols: u16,
        rows: u16,
        scrolling: bool,
        cursor: ImageCursor,
    ) -> ImagePlaced {
        let (term_cols, term_rows) = (self.term.columns(), self.term.screen_lines());
        let origin = self.term.grid().cursor.clone();
        // xterm.js keeps a pending wrap as the column past the last one
        let mut offset = if origin.input_needs_wrap {
            term_cols
        } else {
            origin.point.column.0
        };
        if !scrolling {
            offset = 0;
            self.set_cursor(0, 0, term_cols);
        }
        let cols = cols as usize;
        let mut placed = ImagePlaced::default();
        for row in 0..rows as usize {
            let line = self.term.grid().cursor.point.line;
            for col in 0..cols.min(term_cols.saturating_sub(offset)) {
                let tile = ImageTile {
                    id,
                    tile: (row * cols + col) as u32,
                    cols: cols as u32,
                };
                let point = Point::new(line, Column(offset + col));
                if let Some(old) = self.term.set_image_tile(point, Some(tile)) {
                    placed.covered.push(old.id);
                }
                placed.tiles += 1;
            }
            if scrolling {
                if row + 1 < rows as usize {
                    self.term.linefeed();
                    // xterm.js `lineFeed` steps back from the column past the last one
                    if cols == 0 {
                        self.term.grid_mut().cursor.input_needs_wrap = false;
                    }
                }
            } else if line.0 as usize + 1 >= term_rows {
                break;
            } else {
                self.set_cursor(line.0 + 1, offset, term_cols);
            }
            if cols > 0 {
                let line = self.term.grid().cursor.point.line.0;
                self.set_cursor(line, offset, term_cols);
            }
        }
        if !scrolling {
            self.term.grid_mut().cursor = origin;
        } else if cols > 0 && cursor == ImageCursor::Iip {
            let line = self.term.grid().cursor.point.line.0;
            self.set_cursor(line, (offset + cols).min(term_cols), term_cols);
        }
        placed
    }

    /// Puts the cursor at screen `line` and column `col`, where `term_cols` is the pending wrap.
    fn set_cursor(&mut self, line: i32, col: usize, term_cols: usize) {
        let cursor = &mut self.term.grid_mut().cursor;
        cursor.point = Point::new(Line(line), Column(col.min(term_cols - 1)));
        cursor.input_needs_wrap = col >= term_cols;
    }

    /// The image tiles of one row, appended to `out` (cleared first). Returns `false` if `line` is out of range.
    pub fn row_images(&self, line: i32, out: &mut Vec<ImageCell>) -> bool {
        out.clear();
        self.with_row(line, |row| {
            for (col, cell) in row[..].iter().enumerate() {
                if let Some(image) = cell.image() {
                    out.push(ImageCell {
                        col: col as u16,
                        id: image.id,
                        tile: image.tile,
                    });
                }
            }
        })
        .is_some()
    }

    /// The colours and attributes text written now gets (the sixel background reads them).
    pub fn pen(&self) -> Cell {
        flatten(&self.term.grid().cursor.template)
    }

    /// After the screen got wider than `old_cols`: an image cut off at the old right edge grows back to the right,
    /// on the active screen's rows that the grid holds (the screen and the newest history rows; compressed history
    /// is not rewritten).
    pub(crate) fn extend_images(&mut self, old_cols: usize) {
        let grid = self.term.grid_mut();
        let new_cols = grid.columns();
        if old_cols == 0 || new_cols <= old_cols {
            return;
        }
        let old_last = Column(old_cols - 1);
        for line in grid.topmost_line().0..grid.screen_lines() as i32 {
            let row = &mut grid[Line(line)];
            let Some(image) = row[old_last].image() else {
                continue;
            };
            let cols = image.cols.max(1) as usize;
            let col_in_image = image.tile as usize % cols;
            if col_in_image + 1 >= cols {
                continue;
            }
            let end = new_cols.min(cols - col_in_image + old_last.0);
            for (k, col) in (old_cols..end).enumerate() {
                row[Column(col)].set_image(Some(ImageTile {
                    tile: image.tile + 1 + k as u32,
                    ..image
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn images(e: &Engine, line: i32) -> Vec<(u16, u32, u32)> {
        let mut out = Vec::new();
        e.row_images(line, &mut out);
        out.iter().map(|c| (c.col, c.id, c.tile)).collect()
    }

    #[test]
    fn sixel_placement_scrolls_and_leaves_the_cursor_on_the_last_row() {
        let mut e = Engine::new(10, 4, 100);
        e.feed(b"ab\r\n\r\n\r\n  ");
        let placed = e.place_image(7, 3, 3, true, ImageCursor::Vt340);
        assert_eq!(placed.tiles, 9);
        assert!(placed.covered.is_empty());
        // Two line feeds from the last row scrolled the screen twice
        assert_eq!(e.history_len(), 2);
        assert_eq!(images(&e, 1), vec![(2, 7, 0), (3, 7, 1), (4, 7, 2)]);
        assert_eq!(images(&e, 3), vec![(2, 7, 6), (3, 7, 7), (4, 7, 8)]);
        let c = e.cursor();
        assert_eq!((c.line, c.col), (3, 2));
        // Text over a tile removes it; the tiles stay with their lines in the history
        e.feed(b"X\r\n\r\n");
        assert_eq!(images(&e, 1), vec![(3, 7, 7), (4, 7, 8)]);
        assert_eq!(images(&e, -1), vec![(2, 7, 0), (3, 7, 1), (4, 7, 2)]);
    }

    #[test]
    fn iip_cursor_goes_right_of_the_image_and_cut_off_columns_are_dropped() {
        let mut e = Engine::new(10, 5, 100);
        e.feed(b"\x1b[1;8H");
        e.place_image(1, 5, 2, true, ImageCursor::Iip);
        assert_eq!(images(&e, 0), vec![(7, 1, 0), (8, 1, 1), (9, 1, 2)]);
        assert_eq!(images(&e, 1), vec![(7, 1, 5), (8, 1, 6), (9, 1, 7)]);
        // Right of the screen: a pending wrap, so the next character goes to the next row
        e.feed(b"Z");
        assert_eq!(e.row_text(2).unwrap(), "Z");
    }

    #[test]
    fn without_scrolling_the_image_starts_at_the_top_left_and_the_cursor_stays() {
        let mut e = Engine::new(6, 3, 0);
        e.feed(b"\x1b[2;3H");
        e.place_image(4, 2, 5, false, ImageCursor::Vt340);
        assert_eq!(images(&e, 0), vec![(0, 4, 0), (1, 4, 1)]);
        assert_eq!(images(&e, 2), vec![(0, 4, 4), (1, 4, 5)]);
        let c = e.cursor();
        assert_eq!((c.line, c.col), (1, 2));
        // A new image over the old one reports the tiles it covered
        e.feed(b"\x1b[1;2H");
        let placed = e.place_image(5, 1, 1, true, ImageCursor::Vt340);
        assert_eq!((placed.tiles, placed.covered), (1, vec![4]));
        e.feed(b"\x1b[2J");
        assert!(images(&e, 0).is_empty());
    }

    #[test]
    fn an_empty_sixel_only_feeds_lines_and_tiles_survive_compressed_history() {
        let mut e = Engine::new(8, 3, 100);
        e.feed(b"abcdefgh");
        e.place_image(0, 0, 3, true, ImageCursor::Vt340);
        let c = e.cursor();
        assert_eq!((c.line, c.col), (2, 7));
        e.feed(b"\r");
        e.place_image(9, 2, 1, true, ImageCursor::Vt340);
        e.feed(&b"\r\n".repeat(40));
        assert_eq!(images(&e, -40 + 2), vec![(0, 9, 0), (1, 9, 1)]);
    }

    #[test]
    fn widening_extends_an_image_cut_off_at_the_old_edge() {
        let mut e = Engine::new(8, 3, 10);
        e.feed(b"\x1b[1;2H");
        e.place_image(3, 6, 2, true, ImageCursor::Vt340);
        e.resize(4, 3);
        assert_eq!(images(&e, 0).len(), 3);
        e.resize(9, 3);
        let tiles = |line| {
            images(&e, line)
                .iter()
                .map(|&(c, _, t)| (c, t))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            tiles(0),
            vec![(1, 0), (2, 1), (3, 2), (4, 3), (5, 4), (6, 5)]
        );
        assert_eq!(
            tiles(1),
            vec![(1, 6), (2, 7), (3, 8), (4, 9), (5, 10), (6, 11)]
        );
    }
}
