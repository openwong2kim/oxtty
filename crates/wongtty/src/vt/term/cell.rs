use std::mem::ManuallyDrop;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

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
    zerowidth: Vec<char>,
    underline_color: Option<Color>,
    hyperlink: Option<Hyperlink>,
    // wongtty: an image tile drawn over the cell (sixel and iTerm2 images, as @xterm/addon-image stores them).
    image: Option<ImageTile>,
}

/// wongtty: tile `tile` (`row * cols + col`) of image `id`, which is `cols` tiles wide, covers the cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ImageTile {
    pub id: u32,
    pub tile: u32,
    pub cols: u32,
}

impl CellExtra {
    /// Holds nothing but zerowidth characters.
    fn marks_only(&self) -> bool {
        self.underline_color.is_none() && self.hyperlink.is_none() && self.image.is_none()
    }
}

/// wongtty: a cell's extra storage, one pointer wide. Either a reference-counted [`CellExtra`], or (the low bit
/// set) a cluster of zerowidth characters interned for the whole process (`intern`): a cell with combining marks,
/// a variation selector or a joiner then takes no allocation and no reference count of its own.
pub struct Extra(NonNull<u8>);

// SAFETY: an `Extra` is an `Arc<CellExtra>` (`Send` and `Sync`) or a shared reference to an interned cluster that
// is never freed nor changed.
unsafe impl Send for Extra {}
unsafe impl Sync for Extra {}

impl Extra {
    fn from_arc(extra: Arc<CellExtra>) -> Self {
        // An `Arc`'s data is aligned to at least 4 bytes, so its low bit is clear.
        Self(NonNull::new(Arc::into_raw(extra) as *mut u8).expect("Arc is never null"))
    }

    #[inline]
    fn is_marks(&self) -> bool {
        self.0.as_ptr() as usize & 1 != 0
    }

    /// The reference-counted extra, `None` for an interned cluster.
    #[inline]
    pub fn get(&self) -> Option<&CellExtra> {
        // SAFETY: without the low bit the pointer came from `Arc::into_raw`, and this `Extra` holds a count.
        (!self.is_marks()).then(|| unsafe { &*(self.0.as_ptr() as *const CellExtra) })
    }

    /// The zerowidth characters.
    #[inline]
    pub fn zerowidth(&self) -> &[char] {
        match self.get() {
            Some(extra) => &extra.zerowidth,
            // SAFETY: an interned cluster is its length, then its characters, leaked (`intern`).
            None => unsafe {
                let at = self.0.as_ptr().wrapping_byte_sub(1) as *const char;
                std::slice::from_raw_parts(at.add(1), *at as usize)
            },
        }
    }

    fn into_arc(self) -> Arc<CellExtra> {
        if self.is_marks() {
            return Arc::new(CellExtra { zerowidth: self.zerowidth().to_vec(), ..Default::default() });
        }
        let this = ManuallyDrop::new(self);
        // SAFETY: the count this `Extra` held moves to the `Arc`.
        unsafe { Arc::from_raw(this.0.as_ptr() as *const CellExtra) }
    }
}

impl From<Arc<CellExtra>> for Extra {
    fn from(extra: Arc<CellExtra>) -> Self {
        Self::from_arc(extra)
    }
}

impl Clone for Extra {
    #[inline]
    fn clone(&self) -> Self {
        if !self.is_marks() {
            // SAFETY: as `get`.
            unsafe { Arc::increment_strong_count(self.0.as_ptr() as *const CellExtra) };
        }
        Self(self.0)
    }
}

impl Drop for Extra {
    #[inline]
    fn drop(&mut self) {
        if !self.is_marks() {
            // SAFETY: as `get`; the count this `Extra` held.
            unsafe { drop(Arc::from_raw(self.0.as_ptr() as *const CellExtra)) };
        }
    }
}

/// Equal when they hold the same: an interned cluster equals an extra with those zerowidth characters only.
impl PartialEq for Extra {
    fn eq(&self, other: &Self) -> bool {
        match (self.get(), other.get()) {
            (Some(a), Some(b)) => a == b,
            (a, b) => {
                self.zerowidth() == other.zerowidth()
                    && a.is_none_or(CellExtra::marks_only)
                    && b.is_none_or(CellExtra::marks_only)
            },
        }
    }
}

impl Eq for Extra {}

impl std::fmt::Debug for Extra {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.get() {
            Some(extra) => extra.fmt(f),
            None => f.debug_tuple("Marks").field(&self.zerowidth()).finish(),
        }
    }
}

#[cfg(feature = "serde")]
impl Serialize for Extra {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.get() {
            Some(extra) => extra.serialize(serializer),
            None => CellExtra { zerowidth: self.zerowidth().to_vec(), ..Default::default() }
                .serialize(serializer),
        }
    }
}

#[cfg(feature = "serde")]
impl<'de> Deserialize<'de> for Extra {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        CellExtra::deserialize(deserializer).map(|extra| Self::from_arc(Arc::new(extra)))
    }
}

/// The most zerowidth characters an interned cluster holds, and the most clusters interned: past them a cell
/// keeps its characters in a `CellExtra` of its own, as before, so the table stays small whatever a program prints
/// (at most about 300 KiB, for 4,096 clusters of 8).
const MAX_MARKS: usize = 8;
const MAX_INTERNED: usize = 4096;

/// The interned clusters, each its length then its characters, by an open-addressed hash of the characters.
struct Interned {
    slots: Vec<Option<&'static [char]>>,
    len: usize,
}

static INTERNED: Mutex<Interned> = Mutex::new(Interned { slots: Vec::new(), len: 0 });

/// The interned cluster of `marks` (1 to `MAX_MARKS` characters), `None` when there is no room for it.
pub fn intern(marks: &[char]) -> Option<Extra> {
    if marks.is_empty() || marks.len() > MAX_MARKS {
        return None;
    }
    let mut table = INTERNED.lock().unwrap_or_else(PoisonError::into_inner);
    if table.slots.len() < 2 * (table.len + 1) {
        if table.len == MAX_INTERNED {
            return find(&table.slots, marks).map(marked);
        }
        // Doubles the table, which is kept at most half full.
        let size = (table.slots.len() * 2).max(64);
        let old = std::mem::replace(&mut table.slots, vec![None; size]);
        for entry in old.into_iter().flatten() {
            let at = slot(&table.slots, &entry[1..]);
            table.slots[at] = Some(entry);
        }
    }
    let at = slot(&table.slots, marks);
    let entry = match table.slots[at] {
        Some(entry) => entry,
        None => {
            let mut entry = Vec::with_capacity(marks.len() + 1);
            entry.push(char::from(marks.len() as u8));
            entry.extend_from_slice(marks);
            let entry: &'static [char] = Vec::leak(entry);
            table.slots[at] = Some(entry);
            table.len += 1;
            entry
        },
    };
    Some(marked(entry))
}

/// The slot that holds `marks`, or the empty one where it goes.
fn slot(slots: &[Option<&'static [char]>], marks: &[char]) -> usize {
    let hash = marks.iter().fold(marks.len() as u32, |h, &c| (h ^ c as u32).wrapping_mul(0x9e37_79b1));
    let mask = slots.len() - 1;
    let mut at = (hash >> 16) as usize & mask;
    while slots[at].is_some_and(|entry| entry[1..] != *marks) {
        at = (at + 1) & mask;
    }
    at
}

fn find(slots: &[Option<&'static [char]>], marks: &[char]) -> Option<&'static [char]> {
    slots[slot(slots, marks)]
}

fn marked(entry: &'static [char]) -> Extra {
    // A `char` is aligned to 4 bytes, so the low bit is free for the tag.
    let at = (entry.as_ptr() as *mut u8).wrapping_byte_add(1);
    Extra(NonNull::new(at).expect("a leaked slice is never null"))
}

/// Content and attributes of a single cell in the terminal grid.
#[derive(Clone, Debug, Eq, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Cell {
    pub c: char,
    pub fg: Color,
    pub bg: Color,
    pub flags: Flags,
    pub extra: Option<Extra>,
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
        self.extra.as_ref().map(Extra::zerowidth)
    }

    /// Write a new zerowidth character to this cell.
    #[inline]
    pub fn push_zerowidth(&mut self, character: char) {
        // wongtty: a cluster of zerowidth characters alone is interned, the one with the new character too.
        let mut marks = [character; MAX_MARKS + 1];
        let old = self.zerowidth().unwrap_or_default();
        let len = old.len() + 1;
        if self.extra.as_ref().is_none_or(Extra::is_marks) && len <= MAX_MARKS {
            marks[..len - 1].copy_from_slice(old);
            if let Some(extra) = intern(&marks[..len]) {
                self.extra = Some(extra);
                return;
            }
        }
        self.extra_mut().zerowidth.push(character);
    }

    /// wongtty: sets the zerowidth characters of a cell that has no extra (`marks` not empty).
    #[inline]
    pub fn set_zerowidth(&mut self, marks: &[char]) {
        debug_assert!(self.extra.is_none() && !marks.is_empty());
        self.extra = intern(marks);
        if self.extra.is_none() {
            self.extra_mut().zerowidth = marks.to_vec();
        }
    }

    /// wongtty: the extra, for a change: a cluster or one shared with other cells is copied first.
    fn extra_mut(&mut self) -> &mut CellExtra {
        let mut extra = self.extra.take().map_or_else(Default::default, Extra::into_arc);
        Arc::make_mut(&mut extra);
        let at = Arc::into_raw(extra) as *mut CellExtra;
        self.extra = Some(Extra(NonNull::new(at as *mut u8).expect("Arc is never null")));
        // SAFETY: `make_mut` left this the only reference, which `self` holds for the borrow's lifetime.
        unsafe { &mut *at }
    }

    /// Whether the extra is an interned cluster, or holds nothing else than `keep` says.
    fn extra_empty_but(&self, keep: impl Fn(&CellExtra) -> bool) -> bool {
        self.extra.as_ref().is_none_or(|extra| match extra.get() {
            Some(extra) => extra.zerowidth.is_empty() && keep(extra),
            None => false,
        })
    }

    /// Remove all wide char data from a cell.
    #[inline(never)]
    pub fn clear_wide(&mut self) {
        self.flags.remove(Flags::WIDE_CHAR);
        match &self.extra {
            Some(extra) if extra.is_marks() => self.extra = None,
            Some(_) => self.extra_mut().zerowidth = Vec::new(),
            None => (),
        }
        self.c = ' ';
    }

    /// Set underline color on the cell.
    pub fn set_underline_color(&mut self, color: Option<Color>) {
        // If we reset color and we don't have zerowidth we should drop extra storage.
        if color.is_none()
            && self.extra_empty_but(|extra| extra.hyperlink.is_none() && extra.image.is_none())
        {
            self.extra = None;
        } else {
            self.extra_mut().underline_color = color;
        }
    }

    /// Underline color stored in this cell.
    #[inline]
    pub fn underline_color(&self) -> Option<Color> {
        self.extra.as_ref()?.get()?.underline_color
    }

    /// Set hyperlink.
    pub fn set_hyperlink(&mut self, hyperlink: Option<Hyperlink>) {
        let should_drop = hyperlink.is_none()
            && self.extra_empty_but(|extra| extra.underline_color.is_none() && extra.image.is_none());

        if should_drop {
            self.extra = None;
        } else {
            self.extra_mut().hyperlink = hyperlink;
        }
    }

    /// Hyperlink stored in this cell.
    #[inline]
    pub fn hyperlink(&self) -> Option<Hyperlink> {
        self.extra.as_ref()?.get()?.hyperlink.clone()
    }

    /// wongtty: sets the image tile over this cell; returns the one it replaced.
    pub fn set_image(&mut self, image: Option<ImageTile>) -> Option<ImageTile> {
        let old = self.image();
        let should_drop = image.is_none()
            && self.extra_empty_but(|extra| extra.underline_color.is_none() && extra.hyperlink.is_none());
        if should_drop {
            self.extra = None;
        } else {
            self.extra_mut().image = image;
        }
        old
    }

    /// wongtty: the image tile over this cell.
    #[inline]
    pub fn image(&self) -> Option<ImageTile> {
        self.extra.as_ref()?.get()?.image
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
            && self.zerowidth().is_none_or(<[char]>::is_empty)
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
            if cell.c != ' ' || cell.zerowidth().is_some_and(|marks| !marks.is_empty()) {
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
    fn interned_marks_are_shared_and_counted_by_no_cell() {
        let (mut a, mut b) = (Cell::default(), Cell::default());
        a.push_zerowidth('\u{fe0f}');
        b.push_zerowidth('\u{fe0f}');
        let (ea, eb) = (a.extra.as_ref().unwrap(), b.extra.as_ref().unwrap());
        assert!(ea.get().is_none() && eb.get().is_none());
        assert_eq!(ea.0, eb.0);
        assert_eq!(a.zerowidth(), Some(&['\u{fe0f}'][..]));
        a.push_zerowidth('\u{20e3}');
        assert_eq!(a.zerowidth(), Some(&['\u{fe0f}', '\u{20e3}'][..]));
        assert_ne!(a, b);
        let mut c = Cell::default();
        c.set_zerowidth(&['\u{fe0f}', '\u{20e3}']);
        assert_eq!(a, c);
        assert_eq!(a.extra.as_ref().unwrap().0, c.extra.as_ref().unwrap().0);
    }

    #[test]
    fn marks_keep_through_other_extras_and_long_clusters_get_their_own() {
        let mut cell = Cell::default();
        cell.push_zerowidth('\u{301}');
        let plain = cell.clone();
        cell.set_hyperlink(Some(Hyperlink::new(Some("a"), "https://a".into())));
        assert!(cell.extra.as_ref().unwrap().get().is_some());
        assert_eq!(cell.zerowidth(), Some(&['\u{301}'][..]));
        cell.set_hyperlink(None);
        assert_eq!(cell.hyperlink(), None);
        // Equal to the interned cluster, though held in an extra of its own.
        assert_eq!(cell, plain);
        cell.clear_wide();
        assert_eq!(cell.zerowidth(), Some(&[][..]));
        let mut long = Cell::default();
        for i in 0..MAX_MARKS + 2 {
            long.push_zerowidth(char::from_u32(0x300 + i as u32).unwrap());
        }
        assert!(long.extra.as_ref().unwrap().get().is_some());
        assert_eq!(long.zerowidth().unwrap().len(), MAX_MARKS + 2);
        let mut wide = plain.clone();
        wide.clear_wide();
        assert!(wide.extra.is_none());
    }

    #[test]
    fn an_extra_holds_one_count_of_its_arc() {
        let arc = Arc::new(CellExtra::default());
        let extra = Extra::from(arc.clone());
        let copies = vec![extra.clone(), extra.clone()];
        assert_eq!(Arc::strong_count(&arc), 4);
        drop(copies);
        let mut cell = Cell { extra: Some(extra), ..Cell::default() };
        assert_eq!(Arc::strong_count(&arc), 2);
        // A change copies an extra shared with others.
        cell.set_underline_color(Some(Color::Indexed(1)));
        assert_eq!(Arc::strong_count(&arc), 1);
        assert_eq!(cell.underline_color(), Some(Color::Indexed(1)));
        drop(cell);
        assert_eq!(Arc::strong_count(&arc), 1);
    }

    #[test]
    fn line_length_works_with_wrapline() {
        let mut row = Row::<Cell>::new(10);
        row[Column(9)].flags.insert(super::Flags::WRAPLINE);

        assert_eq!(row.line_length(), Column(10));
    }
}
