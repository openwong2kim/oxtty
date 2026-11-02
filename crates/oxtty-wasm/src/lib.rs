//! Plain C-ABI exports for `wasm32-unknown-unknown` (no wasm-bindgen).
//!
//! Terminals are addressed by integer handles (0 = invalid). Variable-length
//! results are left in shared output buffers; the call returns their length
//! and the host reads them through `oxtty_out_ptr` / `oxtty_out32_ptr` before
//! the next call. The JavaScript wrapper in `packages/oxtty` does all of this.

use std::cell::RefCell;

use oxtty::{
    Clipboard, CursorShape, Damage, Engine, HookKind, ImageCursor, SnapshotOptions, WindowsPty,
    WindowsPtyBackend, COLOR_SLOTS,
};

/// The same dlmalloc std uses on wasm, but growing linear memory in larger
/// steps: at 64 KiB per `memory.grow`, filling a scrollback calls it thousands
/// of times and that dominated `malloc`. Untouched pages cost no RSS.
#[cfg(target_arch = "wasm32")]
mod alloc {
    use std::alloc::{GlobalAlloc, Layout};
    use std::arch::wasm32::memory_grow;
    use std::cell::UnsafeCell;

    use dlmalloc::{Allocator, Dlmalloc};

    const PAGE: usize = 64 << 10;
    const GROW_STEP: usize = 4 << 20;

    struct Stepped;

    // Same as dlmalloc's wasm System, except the grow size is raised to at least GROW_STEP
    unsafe impl Allocator for Stepped {
        fn alloc(&self, size: usize) -> (*mut u8, usize, u32) {
            let pages = size.max(GROW_STEP).div_ceil(PAGE);
            match memory_grow(0, pages) {
                usize::MAX => (std::ptr::null_mut(), 0, 0),
                prev => ((prev * PAGE) as *mut u8, pages * PAGE, 0),
            }
        }
        fn remap(&self, _: *mut u8, _: usize, _: usize, _: bool) -> *mut u8 {
            std::ptr::null_mut()
        }
        fn free_part(&self, _: *mut u8, _: usize, _: usize) -> bool {
            false
        }
        fn free(&self, _: *mut u8, _: usize) -> bool {
            false
        }
        fn can_release_part(&self, _: u32) -> bool {
            false
        }
        fn allocates_zeros(&self) -> bool {
            true
        }
        fn page_size(&self) -> usize {
            PAGE
        }
    }

    struct Global(UnsafeCell<Dlmalloc<Stepped>>);

    // wasm32-unknown-unknown (no atomics) has only one thread
    unsafe impl Sync for Global {}

    unsafe impl GlobalAlloc for Global {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            (*self.0.get()).malloc(l.size(), l.align())
        }
        unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
            (*self.0.get()).calloc(l.size(), l.align())
        }
        unsafe fn dealloc(&self, ptr: *mut u8, l: Layout) {
            (*self.0.get()).free(ptr, l.size(), l.align())
        }
        unsafe fn realloc(&self, ptr: *mut u8, l: Layout, new: usize) -> *mut u8 {
            (*self.0.get()).realloc(ptr, l.size(), l.align(), new)
        }
    }

    #[global_allocator]
    static GLOBAL: Global = Global(UnsafeCell::new(Dlmalloc::new_with_allocator(Stepped)));
}

thread_local! {
    static TERMS: RefCell<Vec<Option<Box<Engine>>>> = const { RefCell::new(Vec::new()) };
    static OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
    static OUT32: RefCell<Vec<u32>> = const { RefCell::new(Vec::new()) };
}

fn with<R>(h: u32, default: R, f: impl FnOnce(&mut Engine) -> R) -> R {
    TERMS.with_borrow_mut(|terms| {
        match h
            .checked_sub(1)
            .and_then(|i| terms.get_mut(i as usize))
            .and_then(Option::as_mut)
        {
            Some(engine) => f(engine),
            None => default,
        }
    })
}

fn set_out(bytes: Vec<u8>) -> u32 {
    OUT.with_borrow_mut(|out| {
        *out = bytes;
        out.len() as u32
    })
}

/// ABI version of these exports; bump on any incompatible change.
#[no_mangle]
pub extern "C" fn oxtty_abi() -> u32 {
    13
}

#[no_mangle]
pub extern "C" fn oxtty_alloc(len: u32) -> *mut u8 {
    let mut buf = Vec::<u8>::with_capacity(len as usize);
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    ptr
}

/// # Safety
/// `ptr`/`len` must come from `oxtty_alloc` and not be freed twice.
#[no_mangle]
pub unsafe extern "C" fn oxtty_free(ptr: *mut u8, len: u32) {
    if !ptr.is_null() {
        drop(Vec::from_raw_parts(ptr, 0, len as usize));
    }
}

#[no_mangle]
pub extern "C" fn oxtty_out_ptr() -> *const u8 {
    OUT.with_borrow(|out| out.as_ptr())
}

/// Byte length of OUT, for results that return a count instead.
#[no_mangle]
pub extern "C" fn oxtty_out_len() -> u32 {
    OUT.with_borrow(|out| out.len() as u32)
}

#[no_mangle]
pub extern "C" fn oxtty_out32_ptr() -> *const u32 {
    OUT32.with_borrow(|out| out.as_ptr())
}

#[no_mangle]
pub extern "C" fn oxtty_new(cols: u32, rows: u32, scrollback: u32) -> u32 {
    let engine = Box::new(Engine::new(
        cols.min(u16::MAX as u32) as u16,
        rows.min(u16::MAX as u32) as u16,
        scrollback as usize,
    ));
    TERMS.with_borrow_mut(|terms| {
        if let Some(i) = terms.iter().position(Option::is_none) {
            terms[i] = Some(engine);
            i as u32 + 1
        } else {
            terms.push(Some(engine));
            terms.len() as u32
        }
    })
}

#[no_mangle]
pub extern "C" fn oxtty_drop(h: u32) {
    TERMS.with_borrow_mut(|terms| {
        if let Some(slot) = h.checked_sub(1).and_then(|i| terms.get_mut(i as usize)) {
            *slot = None;
        }
    });
}

/// # Safety
/// `ptr` must point to `len` readable bytes in linear memory.
#[no_mangle]
pub unsafe extern "C" fn oxtty_feed(h: u32, ptr: *const u8, len: u32) {
    let bytes = std::slice::from_raw_parts(ptr, len as usize);
    with(h, (), |t| t.feed(bytes));
}

#[no_mangle]
pub extern "C" fn oxtty_resize(h: u32, cols: u32, rows: u32) {
    with(h, (), |t| {
        t.resize(
            cols.min(u16::MAX as u32) as u16,
            rows.min(u16::MAX as u32) as u16,
        )
    });
}

#[no_mangle]
pub extern "C" fn oxtty_cols(h: u32) -> u32 {
    with(h, 0, |t| t.cols() as u32)
}

#[no_mangle]
pub extern "C" fn oxtty_rows(h: u32) -> u32 {
    with(h, 0, |t| t.rows() as u32)
}

#[no_mangle]
pub extern "C" fn oxtty_history_len(h: u32) -> u32 {
    with(h, 0, |t| t.history_len() as u32)
}

/// Lines that have left the top of the scrollback (a double, so the count does not wrap).
#[no_mangle]
pub extern "C" fn oxtty_trimmed_lines(h: u32) -> f64 {
    with(h, 0.0, |t| t.trimmed_lines() as f64)
}

/// UTF-8 row text into OUT; returns its length, or -1 if out of range.
#[no_mangle]
pub extern "C" fn oxtty_row_text(h: u32, line: i32) -> i32 {
    with(h, -1, |t| {
        t.row_text(line)
            .map_or(-1, |s| set_out(s.into_bytes()) as i32)
    })
}

/// `[ch, fg, bg, attrs]` × cols into OUT32; returns the cell count, or -1.
#[no_mangle]
pub extern "C" fn oxtty_row_cells(h: u32, line: i32) -> i32 {
    with(h, -1, |t| {
        OUT32.with_borrow_mut(|out| {
            if t.row_packed(line, out) {
                (out.len() / 4) as i32
            } else {
                -1
            }
        })
    })
}

#[no_mangle]
pub extern "C" fn oxtty_row_wrapped(h: u32, line: i32) -> u32 {
    with(h, 0, |t| t.row_wrapped(line) as u32)
}

/// UTF-8 text of one cell (with combining marks) into OUT; -1 if out of range.
#[no_mangle]
pub extern "C" fn oxtty_cell_text(h: u32, line: i32, col: u32) -> i32 {
    with(h, -1, |t| {
        t.cell_text(line, col.min(u16::MAX as u32) as u16)
            .map_or(-1, |s| set_out(s.into_bytes()) as i32)
    })
}

/// `line << 16 | col`, with bit 31 set when the cursor is visible.
#[no_mangle]
pub extern "C" fn oxtty_cursor(h: u32) -> u32 {
    with(h, 0, |t| {
        let c = t.cursor();
        (c.visible as u32) << 31 | (c.line as u32) << 16 | c.col as u32
    })
}

/// The host's cursor style (shape 0 block, 1 underline, 2 bar; `blinking` 1), reported until the program sets one
/// and by DECRQSS.
#[no_mangle]
pub extern "C" fn oxtty_set_cursor_default(h: u32, shape: u32, blinking: u32) {
    let shape = match shape {
        1 => CursorShape::Underline,
        2 => CursorShape::Bar,
        _ => CursorShape::Block,
    };
    with(h, (), |t| t.set_cursor_default(shape, blinking != 0));
}

/// Cursor shape (0 block, 1 underline, 2 bar), with bit 8 set when blinking and bit 9 when the program set the style.
#[no_mangle]
pub extern "C" fn oxtty_cursor_style(h: u32) -> u32 {
    with(h, 0, |t| {
        let c = t.cursor();
        let shape = match c.shape {
            CursorShape::Block => 0,
            CursorShape::Underline => 1,
            CursorShape::Bar => 2,
        };
        (c.styled as u32) << 9 | (c.blinking as u32) << 8 | shape
    })
}

#[no_mangle]
pub extern "C" fn oxtty_display_offset(h: u32) -> u32 {
    with(h, 0, |t| t.display_offset() as u32)
}

#[no_mangle]
pub extern "C" fn oxtty_scroll_display(h: u32, delta: i32) {
    with(h, (), |t| t.scroll_display(delta));
}

#[no_mangle]
pub extern "C" fn oxtty_scroll_to_bottom(h: u32) {
    with(h, (), |t| t.scroll_to_bottom());
}

/// Cells with combining marks, an underline colour or a hyperlink; returns
/// their count, or -1 if out of range. OUT32 holds `[col, underline colour
/// (0 = none), text byte length (0 = none), link (0 = none, else 1-based
/// index)]` per cell. OUT holds the texts in order, then each link of the row
/// as `id \0 uri \0`.
#[no_mangle]
pub extern "C" fn oxtty_row_extras(h: u32, line: i32) -> i32 {
    with(h, -1, |t| {
        let mut extras = Vec::new();
        if !t.row_extras(line, &mut extras) {
            return -1;
        }
        let mut bytes = Vec::new();
        let mut links: Vec<&oxtty::Hyperlink> = Vec::new();
        let mut words = Vec::with_capacity(extras.len() * 4);
        for e in &extras {
            let text = e.text.as_deref().unwrap_or("");
            bytes.extend_from_slice(text.as_bytes());
            let link =
                e.hyperlink
                    .as_ref()
                    .map_or(0, |l| match links.iter().position(|x| *x == l) {
                        Some(i) => i + 1,
                        None => {
                            links.push(l);
                            links.len()
                        }
                    });
            words.extend([
                e.col as u32,
                e.underline_color.unwrap_or(0),
                text.len() as u32,
                link as u32,
            ]);
        }
        for l in links {
            bytes.extend_from_slice(l.id.as_bytes());
            bytes.push(0);
            bytes.extend_from_slice(l.uri.as_bytes());
            bytes.push(0);
        }
        set_out(bytes);
        OUT32.with_borrow_mut(|out| *out = words);
        extras.len() as i32
    })
}

#[no_mangle]
pub extern "C" fn oxtty_modes(h: u32) -> u32 {
    with(h, 0, |t| t.modes())
}

#[no_mangle]
pub extern "C" fn oxtty_set_kitty_keyboard(h: u32, enabled: u32) {
    with(h, (), |t| t.set_kitty_keyboard(enabled != 0));
}

#[no_mangle]
pub extern "C" fn oxtty_reset_kitty_keyboard(h: u32) {
    with(h, (), |t| t.reset_kitty_keyboard());
}

#[no_mangle]
pub extern "C" fn oxtty_kitty_flags(h: u32) -> u32 {
    with(h, 0, |t| t.kitty_flags() as u32)
}

/// The host's focus for the report on `CSI ? 1004 h`: 1 focused, 0 not, anything else unknown (no report).
#[no_mangle]
pub extern "C" fn oxtty_set_focus(h: u32, focused: i32) {
    let focused = match focused {
        1 => Some(true),
        0 => Some(false),
        _ => None,
    };
    with(h, (), |t| t.set_focus(focused));
}

/// -1 = full redraw; otherwise the number of changed lines. OUT32 holds the
/// rows to scroll up first, then the changed lines.
#[no_mangle]
pub extern "C" fn oxtty_take_damage(h: u32) -> i32 {
    with(h, -1, |t| match t.take_damage() {
        Damage::Full => -1,
        Damage::Lines { scroll, lines } => OUT32.with_borrow_mut(|out| {
            out.clear();
            out.push(scroll.into());
            out.extend(lines.into_iter().map(u32::from));
            out.len() as i32 - 1
        }),
    })
}

/// Reply bytes for the PTY into OUT; returns their length.
#[no_mangle]
pub extern "C" fn oxtty_take_replies(h: u32) -> u32 {
    with(h, 0, |t| set_out(t.take_replies()))
}

/// The same replies one by one: returns their count, OUT holds them in order and OUT32 each one's byte length.
#[no_mangle]
pub extern "C" fn oxtty_take_reply_list(h: u32) -> u32 {
    with(h, 0, |t| {
        let replies = t.take_reply_list();
        OUT32.with_borrow_mut(|out| *out = replies.iter().map(|r| r.len() as u32).collect());
        set_out(replies.concat());
        replies.len() as u32
    })
}

/// New title into OUT and its length, or -1 if unchanged.
#[no_mangle]
pub extern "C" fn oxtty_take_title(h: u32) -> i32 {
    with(h, -1, |t| {
        t.take_title()
            .map_or(-1, |s| set_out(s.into_bytes()) as i32)
    })
}

#[no_mangle]
pub extern "C" fn oxtty_take_bells(h: u32) -> u32 {
    with(h, 0, |t| t.take_bells())
}

#[no_mangle]
pub extern "C" fn oxtty_take_resets(h: u32) -> u32 {
    with(h, 0, |t| t.take_resets())
}

#[no_mangle]
pub extern "C" fn oxtty_set_clipboard_access(h: u32, read: u32, write: u32) {
    with(h, (), |t| t.set_clipboard_access(read != 0, write != 0));
}

/// Oldest clipboard write into OUT: the selection byte (`c` or `p`), then
/// the UTF-8 text. Returns its length, or -1 if none is waiting.
#[no_mangle]
pub extern "C" fn oxtty_take_clipboard_write(h: u32) -> i32 {
    with(h, -1, |t| {
        t.take_clipboard_write().map_or(-1, |(clipboard, text)| {
            let mut out = Vec::with_capacity(1 + text.len());
            out.push(selection_byte(clipboard));
            out.extend_from_slice(text.as_bytes());
            set_out(out) as i32
        })
    })
}

/// Oldest clipboard read: the selection byte (`c` or `p`), or -1 if none.
#[no_mangle]
pub extern "C" fn oxtty_take_clipboard_read(h: u32) -> i32 {
    with(h, -1, |t| {
        t.take_clipboard_read()
            .map_or(-1, |clipboard| selection_byte(clipboard) as i32)
    })
}

/// Answers the oldest read of `selection` (`c` or `p`); 1 if one was waiting.
///
/// # Safety
/// `ptr` must point to `len` readable bytes of UTF-8 in linear memory.
#[no_mangle]
pub unsafe extern "C" fn oxtty_reply_clipboard(
    h: u32,
    selection: u32,
    ptr: *const u8,
    len: u32,
) -> u32 {
    let clipboard = match selection as u8 {
        b'c' => Clipboard::Clipboard,
        b'p' => Clipboard::Selection,
        _ => return 0,
    };
    let Ok(text) = std::str::from_utf8(std::slice::from_raw_parts(ptr, len as usize)) else {
        return 0;
    };
    with(h, 0, |t| t.reply_clipboard(clipboard, text) as u32)
}

/// Colours for OSC 4/10/11/12 queries: `COLOR_SLOTS` little-endian `u32`
/// words (`0xRRGGBB`) at `ptr`, or `len` 0 to stop answering.
///
/// # Safety
/// `ptr` must point to `len` readable bytes in linear memory.
#[no_mangle]
pub unsafe extern "C" fn oxtty_set_colors(h: u32, ptr: *const u8, len: u32) {
    if len as usize != COLOR_SLOTS * 4 {
        with(h, (), |t| t.set_colors(None));
        return;
    }
    let bytes = std::slice::from_raw_parts(ptr, len as usize);
    let colors: [u32; COLOR_SLOTS] =
        std::array::from_fn(|i| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()));
    with(h, (), |t| t.set_colors(Some(colors)));
}

fn selection_byte(clipboard: Clipboard) -> u8 {
    match clipboard {
        Clipboard::Clipboard => b'c',
        Clipboard::Selection => b'p',
    }
}

#[no_mangle]
pub extern "C" fn oxtty_sync_pending(h: u32) -> u32 {
    with(h, 0, |t| t.sync_pending() as u32)
}

#[no_mangle]
pub extern "C" fn oxtty_flush_sync(h: u32) {
    with(h, (), |t| t.flush_sync());
}

/// ANSI snapshot bytes into OUT and their length; on refusal the reason
/// (`SnapshotRefusal::as_str`) into OUT and minus its length. A payload is
/// never empty, so 0 means a bad handle. `scrollback` below 0 keeps all the
/// history; `flags` bit 0 leaves out the modes (`SnapshotOptions`).
#[no_mangle]
pub extern "C" fn oxtty_ansi_snapshot(h: u32, scrollback: i32, flags: u32) -> i32 {
    let options = SnapshotOptions {
        scrollback: usize::try_from(scrollback).ok(),
        exclude_modes: flags & 1 != 0,
    };
    with(h, 0, |t| match t.ansi_snapshot_with(options) {
        Ok(bytes) => set_out(bytes) as i32,
        Err(why) => -(set_out(why.as_str().as_bytes().to_vec()) as i32),
    })
}

// Parser hooks. `kind` is 0 for CSI, 1 for OSC, 2 for DCS.

/// Stops parsing at sequences of `kind` with `id` (`on` 1), or no longer does (`on` 0).
#[no_mangle]
pub extern "C" fn oxtty_set_hook(h: u32, kind: u32, id: u32, on: u32) {
    let kind = match kind {
        0 => HookKind::Csi,
        1 => HookKind::Osc,
        2 => HookKind::Dcs,
        _ => return,
    };
    with(h, (), |t| t.set_hook(kind, id, on != 0));
}

/// Like `oxtty_feed`, but stops right after a hooked sequence; returns the bytes taken.
///
/// # Safety
/// `ptr` must point to `len` readable bytes in linear memory (or `len` is 0).
#[no_mangle]
pub unsafe extern "C" fn oxtty_feed_until_hook(h: u32, ptr: *const u8, len: u32) -> u32 {
    // Called with nothing to go on where parsing stopped; `ptr` may then be null.
    let bytes = if len == 0 {
        &[]
    } else {
        std::slice::from_raw_parts(ptr, len as usize)
    };
    with(h, 0, |t| t.feed_until_hook(bytes) as u32)
}

/// The pending hook's kind, or -1 if none. OUT32 holds its id, the parameter count, then each parameter as its
/// length and values (sub-parameters after the first, an empty one as -1 in two's complement); OUT holds its data.
#[no_mangle]
pub extern "C" fn oxtty_pending_hook(h: u32) -> i32 {
    with(h, -1, |t| {
        let Some(hook) = t.pending_hook() else {
            return -1;
        };
        OUT32.with_borrow_mut(|out| {
            out.clear();
            out.push(hook.id);
            out.push(hook.params.len() as u32);
            for param in &hook.params {
                out.push(param.len() as u32);
                out.extend(param.iter().map(|&v| v as u32));
            }
        });
        set_out(hook.data);
        match hook.kind {
            HookKind::Csi => 0,
            HookKind::Osc => 1,
            HookKind::Dcs => 2,
        }
    })
}

/// Runs the pending hook's default action (`run_default` 1) or drops it. Parsing goes on with the next
/// `oxtty_feed_until_hook` (`len` may be 0).
#[no_mangle]
pub extern "C" fn oxtty_finish_hook(h: u32, run_default: u32) {
    with(h, (), |t| t.finish_hook(run_default != 0));
}

/// xterm.js `windowsPty`: `backend` 0 none, 1 conpty, 2 winpty; `build` NaN for none.
#[no_mangle]
pub extern "C" fn oxtty_set_windows_pty(h: u32, backend: u32, build: f64) {
    let backend = match backend {
        1 => Some(WindowsPtyBackend::Conpty),
        2 => Some(WindowsPtyBackend::Winpty),
        _ => None,
    };
    let build_number = (!build.is_nan()).then(|| build.clamp(0.0, u32::MAX as f64) as u32);
    with(h, (), |t| {
        t.set_windows_pty(WindowsPty {
            backend,
            build_number,
        })
    });
}

// Search: the host matches over the engine's logical lines (`Engine::search_line`); the engine's own matching
// stays out of the wasm to keep it small.

/// `Engine::search_line`: the text's character count (-1 outside the buffer), the UTF-8 text in OUT and
/// `[first row, rows, cell of each character…]` in OUT32.
#[no_mangle]
pub extern "C" fn oxtty_search_line(h: u32, line: i32) -> i32 {
    with(h, -1, |t| {
        let Some(found) = t.search_line(line) else {
            return -1;
        };
        set_out(found.text.into_bytes());
        OUT32.with_borrow_mut(|out| {
            out.clear();
            out.extend([found.first as u32, found.rows as u32]);
            out.extend(&found.cells);
        });
        found.cells.len() as i32
    })
}

// Markers (lines as xterm.js buffer lines: 0 is the oldest history line).

/// `Engine::add_marker`: the marker id, or 0 when `line` is outside the buffer.
#[no_mangle]
pub extern "C" fn oxtty_add_marker(h: u32, line: i32) -> u32 {
    with(h, 0, |t| t.add_marker(line as i64).unwrap_or(0))
}

/// `Engine::marker_line`, or -1 once the marker is gone.
#[no_mangle]
pub extern "C" fn oxtty_marker_line(h: u32, id: u32) -> i32 {
    with(h, -1, |t| t.marker_line(id).map_or(-1, |line| line as i32))
}

#[no_mangle]
pub extern "C" fn oxtty_remove_marker(h: u32, id: u32) {
    with(h, (), |t| t.remove_marker(id));
}

/// `Engine::take_disposed_markers`: their count, the ids in OUT32.
#[no_mangle]
pub extern "C" fn oxtty_take_disposed_markers(h: u32) -> i32 {
    with(h, 0, |t| {
        let ids = t.take_disposed_markers();
        let n = ids.len() as i32;
        OUT32.with_borrow_mut(|out| *out = ids);
        n
    })
}

/// `Engine::place_image` of `size` (columns in the low 16 bits, rows in the high; `flags` bit 0: scrolling, bit 1:
/// the iTerm2 cursor): how many tiles it covered; OUT32 holds the count of tiles written, then the image id of each
/// covered tile. Four parameters, as the host's export wrapper passes.
#[no_mangle]
pub extern "C" fn oxtty_place_image(h: u32, id: u32, size: u32, flags: u32) -> i32 {
    with(h, 0, |t| {
        let cursor = if flags & 2 != 0 {
            ImageCursor::Iip
        } else {
            ImageCursor::Vt340
        };
        let (cols, rows) = (size as u16, (size >> 16) as u16);
        let placed = t.place_image(id, cols, rows, flags & 1 != 0, cursor);
        let n = placed.covered.len() as i32;
        OUT32.with_borrow_mut(|out| {
            out.clear();
            out.push(placed.tiles);
            out.extend(placed.covered);
        });
        n
    })
}

/// `Engine::row_images`: their count, or -1 if out of range; OUT32 holds `[col, id, tile]` per cell.
#[no_mangle]
pub extern "C" fn oxtty_row_images(h: u32, line: i32) -> i32 {
    with(h, -1, |t| {
        let mut cells = Vec::new();
        if !t.row_images(line, &mut cells) {
            return -1;
        }
        let words = cells
            .iter()
            .flat_map(|c| [c.col as u32, c.id, c.tile])
            .collect();
        OUT32.with_borrow_mut(|out| *out = words);
        cells.len() as i32
    })
}

/// `Engine::pen`: `[fg, bg, attrs]` in OUT32.
#[no_mangle]
pub extern "C" fn oxtty_pen(h: u32) {
    with(h, (), |t| {
        let pen = t.pen();
        OUT32.with_borrow_mut(|out| *out = vec![pen.fg, pen.bg, pen.attrs]);
    })
}
