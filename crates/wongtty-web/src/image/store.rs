//! The page's image pixels, shared by every terminal that loaded an image addon.
//!
//! Each image belongs to one terminal and counts against that terminal's `storageLimit` (oldest first, as
//! `ImageStorage._evictOldest`), and the page as a whole keeps no more than the largest limit of its terminals, so
//! 17 panes with the addon's 128 MB default hold 128 MB of images, not 17 × 128 MB. Pixels live outside the wasm
//! memory (a typed array or an `ImageBitmap`), so a deleted image gives its memory back; GPU textures exist only
//! for images on screen ([`crate::render`]). Terminals without images have no entry here.
//!
//! Lifetime and accounting follow `@xterm/addon-image` 0.10.0-beta.331 (xterm.js commit
//! 08829c7eb6279dda2a9e86df901353a1a89858ca, MIT), `addons/addon-image/src/ImageStorage.ts` lines 74-167 and
//! 503-583.
use std::cell::RefCell;
use std::rc::Weak;

use js_sys::Uint8Array;
use web_sys::ImageBitmap;

use crate::terminal::Shared;

/// Where an image's pixels are.
pub(crate) enum Pixels {
    /// RGBA bytes.
    Rgba(Uint8Array),
    Bitmap(ImageBitmap),
}

pub(crate) struct Image {
    pub id: u32,
    pub owner: u32,
    pub width: u32,
    pub height: u32,
    /// The CSS cell size it was placed with; a later cell size scales it.
    pub cell: (f64, f64),
    /// Tiles per row.
    pub cols: u32,
    pub pixels: Pixels,
    /// The engine marker on its last line (0 on the alternate screen): the image goes when the marker does.
    pub marker: u32,
    pub alternate: bool,
    /// Tiles not covered by later images (`tileCount`).
    pub tiles: i64,
}

impl Image {
    fn size(&self) -> usize {
        self.width as usize * self.height as usize
    }
}

struct Owner {
    key: u32,
    /// `storageLimit` in pixels.
    limit: usize,
    shared: Weak<Shared>,
}

#[derive(Default)]
struct Store {
    /// Oldest first (ids only grow).
    images: Vec<Image>,
    owners: Vec<Owner>,
    next_id: u32,
    next_key: u32,
}

thread_local! {
    static STORE: RefCell<Store> = RefCell::default();
}

fn with<R>(f: impl FnOnce(&mut Store) -> R) -> R {
    STORE.with(|s| f(&mut s.borrow_mut()))
}

/// A new owner key for a terminal that loaded the addon.
pub(crate) fn join(shared: Weak<Shared>, limit: usize) -> u32 {
    with(|s| {
        s.next_key += 1;
        let key = s.next_key;
        s.owners.push(Owner { key, limit, shared });
        key
    })
}

/// Drops the owner and all its images.
pub(crate) fn leave(key: u32) {
    let gone = with(|s| {
        s.owners.retain(|o| o.key != key);
        take(s, |i| i.owner == key)
    });
    release(gone);
}

/// Removes the images `pick` selects and returns them.
fn take(s: &mut Store, mut pick: impl FnMut(&Image) -> bool) -> Vec<Image> {
    let mut gone = Vec::new();
    let mut i = 0;
    while i < s.images.len() {
        if pick(&s.images[i]) {
            gone.push(s.images.remove(i));
        } else {
            i += 1;
        }
    }
    gone
}

/// Frees deleted images (`_delImg`): bitmaps closed, textures dropped. Called with no borrow held.
fn release(gone: Vec<Image>) {
    for image in gone {
        if let Pixels::Bitmap(bitmap) = &image.pixels {
            bitmap.close();
        }
        crate::render::release_image(image.id);
    }
}

/// Pixels the owner's images hold (`_getStoredPixels`).
pub(crate) fn usage(key: u32) -> usize {
    with(|s| {
        s.images
            .iter()
            .filter(|i| i.owner == key)
            .map(Image::size)
            .sum()
    })
}

pub(crate) fn set_limit(key: u32, limit: usize) {
    with(|s| {
        if let Some(o) = s.owners.iter_mut().find(|o| o.key == key) {
            o.limit = limit;
        }
    });
    evict(key, 0);
}

/// Makes room for `room` more pixels of `key`: its own oldest images first, then the page's oldest while the page
/// holds more than its largest limit (`_evictOldest`). An image larger than the limit still goes in, alone.
pub(crate) fn evict(key: u32, room: usize) {
    let gone = with(|s| {
        let mut gone = Vec::new();
        let limit = s
            .owners
            .iter()
            .find(|o| o.key == key)
            .map_or(0, |o| o.limit);
        let mut used: usize = s
            .images
            .iter()
            .filter(|i| i.owner == key)
            .map(Image::size)
            .sum();
        while used + room > limit {
            let Some(at) = s.images.iter().position(|i| i.owner == key) else {
                break;
            };
            let image = s.images.remove(at);
            used -= image.size();
            gone.push(image);
        }
        let page = s.owners.iter().map(|o| o.limit).max().unwrap_or(0);
        let mut total: usize = s.images.iter().map(Image::size).sum();
        while total + room > page && !s.images.is_empty() {
            let image = s.images.remove(0);
            total -= image.size();
            gone.push(image);
        }
        gone
    });
    redraw_owners(&gone);
    release(gone);
}

/// The id for the next image, unique on the page.
pub(crate) fn reserve_id() -> u32 {
    with(|s| {
        s.next_id = s.next_id.wrapping_add(1).max(1);
        s.next_id
    })
}

/// Adds an image (after [`evict`], with an id from [`reserve_id`]).
pub(crate) fn add(image: Image) {
    with(|s| s.images.push(image));
}

/// Tiles of image `id` were covered (`tileCount--`).
pub(crate) fn covered(ids: &[u32]) {
    with(|s| {
        for id in ids {
            if let Some(image) = s.images.iter_mut().find(|i| i.id == *id) {
                image.tiles -= 1;
            }
        }
    });
}

/// Deletes the owner's images that `pick` selects.
pub(crate) fn delete(key: u32, mut pick: impl FnMut(&Image) -> bool) {
    let gone = with(|s| take(s, |i| i.owner == key && pick(i)));
    release(gone);
}

/// The images whose engine marker went (their last line left the scrollback or was cleared).
pub(crate) fn markers_gone(key: u32, markers: &[u32]) {
    delete(key, |i| i.marker != 0 && markers.contains(&i.marker));
}

/// Recounts the tiles of the owner's alternate-screen images from `counts` (`_evictOnAlternate`) and deletes those
/// with none left.
pub(crate) fn recount_alternate(key: u32, counts: &[(u32, i64)]) {
    with(|s| {
        for image in s.images.iter_mut().filter(|i| i.owner == key) {
            if image.alternate {
                image.tiles = 0;
            }
        }
        for &(id, n) in counts {
            if let Some(image) = s.images.iter_mut().find(|i| i.id == id && i.owner == key) {
                image.tiles += n;
            }
        }
    });
    delete(key, |i| i.alternate && i.tiles == 0);
}

/// Runs `f` on image `id`.
pub(crate) fn with_image<R>(id: u32, f: impl FnOnce(&Image) -> R) -> Option<R> {
    STORE.with(|s| {
        let s = s.try_borrow().ok()?;
        s.images.iter().find(|i| i.id == id).map(f)
    })
}

fn redraw_owners(gone: &[Image]) {
    let mut owners: Vec<u32> = gone.iter().map(|i| i.owner).collect();
    owners.dedup();
    for owner in owners {
        redraw(owner);
    }
}

/// Asks the owner's terminal for a frame (it only schedules one).
fn redraw(key: u32) {
    let shared = with(|s| {
        s.owners
            .iter()
            .find(|o| o.key == key)
            .and_then(|o| o.shared.upgrade())
    });
    if let Some(shared) = shared {
        shared.schedule_frame();
    }
}

/// `[images, pixels]` on the page, for the tests.
pub(crate) fn totals() -> (usize, usize) {
    with(|s| (s.images.len(), s.images.iter().map(Image::size).sum()))
}
