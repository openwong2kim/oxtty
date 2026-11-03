//! Terminal members that read, scroll or reset the engine state: `modes`, `reset`, `scrollPages`, `scrollToTop`,
//! `scrollToLine` and `ansiSnapshot`.
use js_sys::{Object, Reflect};
use wasm_bindgen::prelude::*;

use crate::engine::{mode, Engine};
use crate::render::Dirty;
use crate::terminal::Terminal;

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
/** xterm.js `IModes`. */
export interface IModes {
    readonly applicationCursorKeysMode: boolean;
    readonly applicationKeypadMode: boolean;
    readonly bracketedPasteMode: boolean;
    readonly insertMode: boolean;
    readonly mouseTrackingMode: 'none' | 'x10' | 'vt200' | 'drag' | 'any';
    readonly originMode: boolean;
    readonly reverseWraparoundMode: boolean;
    readonly sendFocusMode: boolean;
    readonly showCursor: boolean;
    readonly synchronizedOutputMode: boolean;
    readonly win32InputMode: boolean;
    readonly wraparoundMode: boolean;
}

/** What `ansiSnapshot` leaves out, like `@xterm/addon-serialize`'s `serialize` options. */
export interface AnsiSnapshotOptions {
    /** Keep only the newest this many history lines (0: the screen alone). All of them by default. */
    scrollback?: number;
    /** Leave out the modes and what continues the output, so the bytes only paint the screen. */
    excludeModes?: boolean;
}

/** Bytes that rebuild the screen, scrollback included, in a fresh terminal of the same size, or why they cannot. */
export type AnsiSnapshot =
    | { ok: true; payload: Uint8Array }
    /** `"unsupported-wide-edge"`, `"margins"` or `"partial-tail-overflow"`. */
    | { ok: false; reason: string };
"#;

#[wasm_bindgen(raw_module = "./index.js")]
extern "C" {
    /// Ends a synchronized update.
    #[wasm_bindgen(method, catch, js_class = "Terminal", js_name = flushSync)]
    fn flush_sync(this: &Engine) -> Result<bool, JsValue>;
}

#[wasm_bindgen]
impl Terminal {
    /// A new object per read, like xterm.js. `reverseWraparoundMode` and `win32InputMode` are always false (the
    /// engine has neither).
    #[wasm_bindgen(getter, unchecked_return_type = "IModes")]
    pub fn modes(&self) -> Object {
        let (m, sync) = match self.shared.core.try_borrow() {
            Ok(core) if !core.disposed => {
                let e = core.view.engine();
                (e.modes().unwrap_or(0), e.sync_pending().unwrap_or(false))
            }
            _ => (mode::SHOW_CURSOR | mode::LINE_WRAP, false),
        };
        let on = |bit: u32| JsValue::from_bool(m & bit != 0);
        let mouse = if m & mode::MOUSE_X10 != 0 {
            "x10"
        } else if m & mode::MOUSE_MOTION != 0 {
            "any"
        } else if m & mode::MOUSE_DRAG != 0 {
            "drag"
        } else if m & mode::MOUSE_CLICK != 0 {
            "vt200"
        } else {
            "none"
        };
        let modes = Object::new();
        for (key, value) in [
            ("applicationCursorKeysMode", on(mode::APP_CURSOR)),
            ("applicationKeypadMode", on(mode::APP_KEYPAD)),
            ("bracketedPasteMode", on(mode::BRACKETED_PASTE)),
            ("insertMode", on(mode::INSERT)),
            ("mouseTrackingMode", mouse.into()),
            ("originMode", on(mode::ORIGIN)),
            ("reverseWraparoundMode", false.into()),
            ("sendFocusMode", on(mode::FOCUS_EVENTS)),
            ("showCursor", on(mode::SHOW_CURSOR)),
            ("synchronizedOutputMode", sync.into()),
            ("win32InputMode", false.into()),
            ("wraparoundMode", on(mode::LINE_WRAP)),
        ] {
            let _ = Reflect::set(&modes, &key.into(), &value);
        }
        modes
    }

    /// Full reset (RIS), applied at once: a synchronized update ends, writes still queued are parsed afterwards (as
    /// in xterm.js, whose write buffer `reset` leaves alone).
    pub fn reset(&self) {
        {
            let Ok(mut core) = self.shared.core.try_borrow_mut() else {
                return;
            };
            if core.disposed {
                return;
            }
            core.high_surrogate = None;
            let engine = core.view.engine();
            let _ = engine.flush_sync();
            let _ = engine.write(&JsValue::from_str("\x1bc"));
            core.view.invalidate();
            core.dirty = Dirty::All;
        }
        crate::input::reset(&self.shared);
        self.shared.handle_events();
        self.shared.after_change();
    }

    /// Scrolls by `pageCount` screens less one row, like xterm.js.
    #[wasm_bindgen(js_name = scrollPages)]
    pub fn scroll_pages(
        &self,
        #[wasm_bindgen(js_name = pageCount)] page_count: i32,
    ) -> Result<(), JsValue> {
        let rows = self.rows() as i32;
        self.scroll_by(page_count.saturating_mul(rows - 1))
    }

    #[wasm_bindgen(js_name = scrollToTop)]
    pub fn scroll_to_top(&self) -> Result<(), JsValue> {
        let top = self.viewport_y().map_or(0, |y| y as i32);
        self.scroll_by(-top)
    }

    /// Scrolls so that buffer line `line` is the top viewport row.
    #[wasm_bindgen(js_name = scrollToLine)]
    pub fn scroll_to_line(&self, line: i32) -> Result<(), JsValue> {
        let top = self.viewport_y().map_or(0, |y| y as i32);
        self.scroll_by(line.saturating_sub(top))
    }

    /// Bytes that rebuild this screen, scrollback included, in a fresh terminal of the same size (the engine's
    /// `ansiSnapshot`; replaces `@xterm/addon-serialize`). `{ scrollback: 0, excludeModes: true }` paints just the
    /// screen it shows (a cold frame), as `serialize` with the same options does.
    #[wasm_bindgen(js_name = ansiSnapshot, unchecked_return_type = "AnsiSnapshot")]
    pub fn ansi_snapshot(
        &self,
        #[wasm_bindgen(unchecked_optional_param_type = "AnsiSnapshotOptions")] options: JsValue,
    ) -> Result<JsValue, JsValue> {
        // At the size `cols`/`rows` report, also with a resize waiting for the frame.
        self.shared.apply_pending_resize_logged();
        match self.shared.core.try_borrow() {
            Ok(core) if !core.disposed => core.view.engine().ansi_snapshot(&options),
            _ => Err("wongtty/xterm: the terminal is disposed".into()),
        }
    }
}

impl Terminal {
    /// `buffer.active.viewportY`, or `None` after dispose.
    fn viewport_y(&self) -> Option<u32> {
        let core = self.shared.core.try_borrow().ok()?;
        if core.disposed {
            return None;
        }
        Some(
            core.view
                .history_length()
                .saturating_sub(core.view.display_offset()),
        )
    }

    /// `scrollLines(amount)` unless the amount is 0 or the terminal is disposed (xterm.js does nothing then).
    fn scroll_by(&self, amount: i32) -> Result<(), JsValue> {
        if amount == 0 || self.viewport_y().is_none() {
            return Ok(());
        }
        self.scroll_lines(amount)
    }
}
