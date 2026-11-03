use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use bitflags::bitflags;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::vt::grid::{self, GridCell};
use crate::vt::index::Column;
use crate::vt::vte::ansi::{Color, Hyperlink as VteHyperlink, NamedColor};

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    #[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
    pub struct Flags: u32 {
        const INVERSE                   = 0b0000_0000_0000_0001;
        const BOLD                      = 0b0000_0000_0000_0010;
        const ITALIC                    = 0b0000_0000_0000_0100;
        const BOLD_ITALIC               = 0b0000_0000_0000_0110;
        const UNDERLINE                 = 0b0000_0000_0000_1000;
        const WRAPLINE                  = 0b0000_0000_0001_0000;
        const WIDE_CHAR                 = 0b0000_0000_0010_0000;
        const WIDE_CHAR_SPACER          = 0b0000_0000_0100_0000;
        const DIM                       = 0b0000_0000_1000_0000;
        const DIM_BOLD                  = 0b0000_0000_1000_0010;
        const HIDDEN                    = 0b0000_0001_0000_0000;
        const STRIKEOUT                 = 0b0000_0010_0000_0000;
        const LEADING_WIDE_CHAR_SPACER  = 0b0000_0100_0000_0000;
        const DOUBLE_UNDERLINE          = 0b0000_1000_0000_0000;
        const UNDERCURL                 = 0b0001_0000_0000_0000;
        const DOTTED_UNDERLINE          = 0b0010_0000_0000_0000;
        const DASHED_UNDERLINE          = 0b0100_0000_0000_0000;
        // wmux replay: retain SGR blink without increasing the cell size.
        const BLINK                    = 0b1000_0000_0000_0000;
        // wongtty: a space the program printed, which is text, unlike a blank that was erased or never written
        // (xterm.js keeps the two apart). The flags are 32 bits for it; the cell stays 24 bytes.
        const TYPED_SPACE              = 1 << 16;
        const ALL_UNDERLINES            = Self::UNDERLINE.bits() | Self::DOUBLE_UNDERLINE.bits()
                                        | Self::UNDERCURL.bits() | Self::DOTTED_UNDERLINE.bits()
                                        | Self::DASHED_UNDERLINE.bits();
    }
}

/// Counter for hyperlinks without explicit ID.
static HYPERLINK_ID_SUFFIX: AtomicU32 = AtomicU32::new(0);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Hyperlink {
    inner: Arc<HyperlinkInner>,
}

impl Hyperlink {
    pub fn new<T: ToString>(id: Option<T>, uri: String) -> Self {
        let inner = Arc::new(HyperlinkInner::new(id, uri));
        Self { inner }
    }

    pub fn id(&self) -> &str {
        &self.inner.id
    }

    pub fn uri(&self) -> &str {
        &self.inner.uri
    }
}

impl From<VteHyperlink> for Hyperlink {
    fn from(value: VteHyperlink) -> Self {
        Self::new(value.id, value.uri)
    }
}

impl From<Hyperlink> for VteHyperlink {
    fn from(val: Hyperlink) -> Self {
        VteHyperlink { id: Some(val.id().to_owned()), uri: val.uri().to_owned() }
    }
}

#[derive(Debug, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
struct HyperlinkInner {
    /// Identifier for the given hyperlink.
    id: String,

    /// Resource identifier of the hyperlink.
    uri: String,
}

impl HyperlinkInner {
    pub fn new<T: ToString>(id: Option<T>, uri: String) -> Self {
        let id = match id {
            Some(id) => id.to_string(),
            None => {
                let mut id = HYPERLINK_ID_SUFFIX.fetch_add(1, Ordering::Relaxed).to_string();
                id.push_str("_alacritty");
                id
            },
        };

        Self { id, uri }
    }
}

/// Trait for determining if a reset should be performed.
pub trait ResetDiscriminant<T> {
    /// Value based on which equality for the reset will be determined.
    fn discriminant(&self) -> T;
}

impl<T: Copy> ResetDiscriminant<T> for T {
    fn discriminant(&self) -> T {
        *self
    }
}

impl ResetDiscriminant<Color> for Cell {
    fn discriminant(&self) -> Color {
        self.bg
    }
}

/// Dynamically allocated cell content.
///
/// This storage is reserved for cell attributes which are rarely set. This allows reducing the
/// allocation required ahead of time for every cell, with some additional overhead when the extra
/// storage is actually required.
#[derive(Default, Debug, Clone, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct CellExtra {
    pub(crate) zerowidth: Vec<char>,
    underline_color: Option<Color>,
    hyperlink: Option<Hyperlink>,
    // wongtty: an image tile drawn over the cell (sixel and iTerm2 images, as @xterm/addon-image stores them).
    image: Option<ImageTile>,
}

impl CellExtra {
    /// wongtty: holds nothing but zerowidth characters.
    #[inline]
    pub fn marks_only(&self) -> bool {
        self.underline_color.is_none() && self.hyperlink.is_none() && self.image.is_none()
    }
}

/// wongtty: the most zerowidth characters an interned cluster holds, and the most clusters interned per thread. Past
/// them a cell keeps its characters in a `CellExtra` of its own, as before, so the table stays small whatever a
/// program prints: at most 512 entries of 1 to 8 characters, under 80 KiB.
const MAX_MARKS: usize = 8;
const MAX_INTERNED: usize = 512;

thread_local! {
    /// wongtty: the interned clusters, `CellExtra`s with zerowidth characters only, shared by every cell (of any
    /// terminal) that has those characters: a cell with combining marks, a variation selector or a joiner then
    /// takes no allocation of its own. A few distinct clusters make up most text, so a list searched from the
    /// front, the first seen first. Entries are never dropped.
    static INTERNED: RefCell<Vec<Arc<CellExtra>>> = const { RefCell::new(Vec::new()) };
}

/// wongtty: the interned cluster of `marks` (1 to `MAX_MARKS` characters), `None` when the table has no room left.
pub fn intern(marks: &[char]) -> Option<Arc<CellExtra>> {
    if marks.is_empty() || marks.len() > MAX_MARKS {
        return None;
    }
    INTERNED.with_borrow_mut(|table| {
        if let Some(extra) = table.iter().find(|extra| extra.zerowidth == marks) {
            return Some(extra.clone());
        }
        if table.len() == MAX_INTERNED {
            return None;
        }
        let extra = Arc::new(CellExtra { zerowidth: marks.to_vec(), ..Default::default() });
        table.push(extra.clone());
        Some(extra)
    })
}

/// wongtty: tile `tile` (`row * cols + col`) of image `id`, which is `cols` tiles wide, covers the cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ImageTile {
    pub id: u32,
    pub tile: u32,
    pub cols: u32,
}

/// Content and attributes of a single cell in the terminal grid.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Cell {
    pub c: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: Flags,
    pub extra: Option<Arc<CellExtra>>,
}

impl Default for Cell {
    #[inline]
    fn default() -> Cell {
        Cell {
            c: ' ',
            bg: Color::Named(NamedColor::Background),
            fg: Color::Named(NamedColor::Foreground),
            flags: Flags::empty(),
            extra: None,
        }
    }
}

impl Cell {
    /// Zerowidth characters stored in this cell.
    #[inline]
    pub fn zerowidth(&self) -> Option<&[char]> {
        self.extra.as_ref().map(|extra| extra.zerowidth.as_slice())
    }

    /// Write a new zerowidth character to this cell.
    #[inline(never)]
    pub fn push_zerowidth(&mut self, character: char) {
        // wongtty: a cluster of zerowidth characters alone is interned, the one with the new character too.
        if let Some(marks) = self.extra.as_ref().map_or(Some(&[][..]), |e| e.marks_only().then_some(&e.zerowidth[..])) {
            if marks.len() < MAX_MARKS {
                let mut cluster = [character; MAX_MARKS];
                cluster[..marks.len()].copy_from_slice(marks);
                if let Some(extra) = intern(&cluster[..marks.len() + 1]) {
                    self.extra = Some(extra);
                    return;
                }
            }
        }
        let extra = self.extra.get_or_insert(Default::default());
        Arc::make_mut(extra).zerowidth.push(character);
    }

    /// Remove all wide char data from a cell.
    #[inline(never)]
    pub fn clear_wide(&mut self) {
        self.flags.remove(Flags::WIDE_CHAR);
        if let Some(extra) = self.extra.as_mut() {
            Arc::make_mut(extra).zerowidth = Vec::new();
        }
        self.c = ' ';
    }

    /// Set underline color on the cell.
    pub fn set_underline_color(&mut self, color: Option<Color>) {
        // If we reset color and we don't have zerowidth we should drop extra storage.
        if color.is_none()
            && self
                .extra
                .as_ref()
                .is_none_or(|extra| {
                    extra.zerowidth.is_empty() && extra.hyperlink.is_none() && extra.image.is_none()
                })
        {
            self.extra = None;
        } else {
            let extra = self.extra.get_or_insert(Default::default());
            Arc::make_mut(extra).underline_color = color;
        }
    }

    /// Underline color stored in this cell.
    #[inline]
    pub fn underline_color(&self) -> Option<Color> {
        self.extra.as_ref()?.underline_color
    }

    /// Set hyperlink.
    pub fn set_hyperlink(&mut self, hyperlink: Option<Hyperlink>) {
        let should_drop = hyperlink.is_none()
            && self
                .extra
                .as_ref()
                .is_none_or(|extra| {
                    extra.zerowidth.is_empty()
                        && extra.underline_color.is_none()
                        && extra.image.is_none()
                });

        if should_drop {
            self.extra = None;
        } else {
            let extra = self.extra.get_or_insert(Default::default());
            Arc::make_mut(extra).hyperlink = hyperlink;
        }
    }

    /// Hyperlink stored in this cell.
    #[inline]
    pub fn hyperlink(&self) -> Option<Hyperlink> {
        self.extra.as_ref()?.hyperlink.clone()
    }

    /// wongtty: sets the image tile over this cell; returns the one it replaced.
    pub fn set_image(&mut self, image: Option<ImageTile>) -> Option<ImageTile> {
        let old = self.image();
        let should_drop = image.is_none()
            && self.extra.as_ref().is_none_or(|extra| {
                extra.zerowidth.is_empty()
                    && extra.underline_color.is_none()
                    && extra.hyperlink.is_none()
            });
        if should_drop {
            self.extra = None;
        } else {
            let extra = self.extra.get_or_insert(Default::default());
            Arc::make_mut(extra).image = image;
        }
        old
    }

    /// wongtty: the image tile over this cell.
    #[inline]
    pub fn image(&self) -> Option<ImageTile> {
        self.extra.as_ref()?.image
    }
}

impl GridCell for Cell {
    #[inline]
    fn is_empty(&self) -> bool {
        (self.c == ' ' || self.c == '\t')
            && self.bg == Color::Named(NamedColor::Background)
            && self.fg == Color::Named(NamedColor::Foreground)
            && !self.flags.intersects(
                Flags::INVERSE
                    | Flags::ALL_UNDERLINES
                    | Flags::STRIKEOUT
                    | Flags::WRAPLINE
                    | Flags::WIDE_CHAR_SPACER
                    | Flags::LEADING_WIDE_CHAR_SPACER,
            )
            && self.extra.as_ref().map(|extra| extra.zerowidth.is_empty()) != Some(false)
    }

    #[inline]
    fn flags(&self) -> &Flags {
        &self.flags
    }

    #[inline]
    fn flags_mut(&mut self) -> &mut Flags {
        &mut self.flags
    }

    #[inline]
    fn reset(&mut self, template: &Self) {
        *self = Cell { bg: template.bg, ..Cell::default() };
    }

    #[inline]
    fn reset_plain(&mut self, _: &Self) {
        self.c = ' ';
        self.flags = Flags::empty();
    }
}

impl From<Color> for Cell {
    #[inline]
    fn from(color: Color) -> Self {
        Self { bg: color, ..Cell::default() }
    }
}

/// Get the length of occupied cells in a line.
pub trait LineLength {
    /// Calculate the occupied line length.
    fn line_length(&self) -> Column;
}

impl LineLength for grid::Row<Cell> {
    fn line_length(&self) -> Column {
        let mut length = Column(0);

        if self[Column(self.len() - 1)].flags.contains(Flags::WRAPLINE) {
            return Column(self.len());
        }

        for (index, cell) in self[..].iter().rev().enumerate() {
            if cell.c != ' '
                || cell.extra.as_ref().map(|extra| extra.zerowidth.is_empty()) == Some(false)
            {
                length = Column(self.len() - index);
                break;
            }
        }

        length
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::mem;

    use crate::vt::grid::Row;
    use crate::vt::index::Column;

    #[test]
    fn cell_size_is_below_cap() {
        // Expected cell size on 64-bit architectures.
        const EXPECTED_CELL_SIZE: usize = 24;

        // Ensure that cell size isn't growing by accident.
        assert!(mem::size_of::<Cell>() <= EXPECTED_CELL_SIZE);
    }

    #[test]
    fn line_length_works() {
        let mut row = Row::<Cell>::new(10);
        row[Column(5)].c = 'a';

        assert_eq!(row.line_length(), Column(6));
    }

    #[test]
    fn interned_marks_are_shared() {
        let (mut a, mut b) = (Cell::default(), Cell::default());
        a.push_zerowidth('\u{fe0f}');
        b.push_zerowidth('\u{fe0f}');
        assert!(Arc::ptr_eq(a.extra.as_ref().unwrap(), b.extra.as_ref().unwrap()));
        assert_eq!(a.zerowidth(), Some(&['\u{fe0f}'][..]));
        a.push_zerowidth('\u{20e3}');
        assert_eq!(a.zerowidth(), Some(&['\u{fe0f}', '\u{20e3}'][..]));
        assert_ne!(a, b);
        let mut c = Cell::default();
        c.push_zerowidth('\u{fe0f}');
        c.push_zerowidth('\u{20e3}');
        assert!(Arc::ptr_eq(a.extra.as_ref().unwrap(), c.extra.as_ref().unwrap()));
    }

    #[test]
    fn marks_keep_through_other_extras_and_long_clusters_get_their_own() {
        let mut cell = Cell::default();
        cell.push_zerowidth('\u{301}');
        let shared = cell.extra.clone().unwrap();
        cell.set_hyperlink(Some(Hyperlink::new(Some("a"), "https://a".into())));
        // The interned cluster is copied, not changed.
        assert!(!Arc::ptr_eq(cell.extra.as_ref().unwrap(), &shared));
        assert_eq!(shared.zerowidth, ['\u{301}']);
        assert!(shared.marks_only());
        assert_eq!(cell.zerowidth(), Some(&['\u{301}'][..]));
        cell.set_hyperlink(None);
        assert_eq!(cell.extra.as_deref(), Some(&*shared));
        let mut long = Cell::default();
        for i in 0..MAX_MARKS + 2 {
            long.push_zerowidth(char::from_u32(0x300 + i as u32).unwrap());
        }
        assert_eq!(long.zerowidth().unwrap().len(), MAX_MARKS + 2);
        cell.clear_wide();
        assert_eq!(shared.zerowidth, ['\u{301}']);
    }

    #[test]
    fn line_length_works_with_wrapline() {
        let mut row = Row::<Cell>::new(10);
        row[Column(9)].flags.insert(super::Flags::WRAPLINE);

        assert_eq!(row.line_length(), Column(10));
    }
}
