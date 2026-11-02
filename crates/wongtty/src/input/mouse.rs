//! Mouse reports and the wheel.
//!
//! Ported from xterm.js @ 08829c7eb6279dda2a9e86df901353a1a89858ca (npm `@xterm/xterm` 6.1.0-beta.336), MIT
//! license (`crates/wongtty/LICENSE-XTERM`):
//! - `src/common/services/MouseStateService.ts` lines 13-151 (Copyright (c) 2019 The xterm.js authors): the
//!   protocols and their restrictions, `eventCode` and the encodings;
//! - `src/browser/services/MouseService.ts` (Copyright (c) 2017 The xterm.js authors) lines 104-192
//!   (`_sendEvent`, without the DOM coordinates), 252-294 (`_handlePassiveWheel`), 445-485 (`_applyScrollModifier`,
//!   `_consumeWheelEvent`), 497-548 (`_triggerMouseEvent`) and 560-574 (`_equalEvents`).
//!
//! The rules and their order are unchanged. The DOM event listeners, coordinates, touch and the custom wheel
//! handler stay with the host.

use super::{Modifiers, MouseAction, MouseButton, MouseInput};
use crate::mode;

/// Which events the program asked for (xterm.js `CoreMouseEventType`), as [`mouse_events`] returns them.
/// Re-exported as `wongtty::input::mouse_event`.
pub mod events {
    /// Any button press.
    pub const DOWN: u8 = 1;
    /// Any button release.
    pub const UP: u8 = 2;
    /// Motion while a button is held.
    pub const DRAG: u8 = 4;
    /// Motion without a button.
    pub const MOVE: u8 = 8;
    /// Any wheel.
    pub const WHEEL: u8 = 16;
}

/// The mouse tracking protocol of the active modes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseProtocol {
    #[default]
    None,
    /// `?9`: presses only, no modifiers.
    X10,
    /// `?1000`: presses, releases and wheels.
    Vt200,
    /// `?1002`: and motion while a button is held.
    Drag,
    /// `?1003`: and all motion.
    Any,
}

impl MouseProtocol {
    /// The protocol of [`mode`] bits. Only one is on at a time (setting one replaces the others, as in xterm.js);
    /// should several bits be set, X10 wins, then any, drag and VT200.
    pub fn from_modes(modes: u32) -> Self {
        if modes & mode::MOUSE_X10 != 0 {
            Self::X10
        } else if modes & mode::MOUSE_MOTION != 0 {
            Self::Any
        } else if modes & mode::MOUSE_DRAG != 0 {
            Self::Drag
        } else if modes & mode::MOUSE_CLICK != 0 {
            Self::Vt200
        } else {
            Self::None
        }
    }

    /// The [`events`] the protocol reports: the host listens for these, and while any are on, mouse presses go
    /// to the program instead of selecting.
    pub fn events(self) -> u8 {
        use events::*;
        match self {
            Self::None => 0,
            Self::X10 => DOWN,
            Self::Vt200 => DOWN | UP | WHEEL,
            Self::Drag => DOWN | UP | WHEEL | DRAG,
            Self::Any => DOWN | UP | WHEEL | DRAG | MOVE,
        }
    }

    /// xterm.js `restrict`: false drops the event; X10 also clears the modifiers.
    fn restrict(self, e: &mut CoreEvent) -> bool {
        match self {
            Self::None => false,
            Self::X10 => {
                if e.button == WHEEL || e.action != DOWN {
                    return false;
                }
                e.ctrl = false;
                e.alt = false;
                e.shift = false;
                true
            }
            Self::Vt200 => e.action != MOVE,
            Self::Drag => !(e.action == MOVE && e.button == NONE),
            Self::Any => true,
        }
    }
}

/// [`MouseProtocol::events`] of the protocol of `modes`.
pub fn mouse_events(modes: u32) -> u8 {
    MouseProtocol::from_modes(modes).events()
}

/// The report encoding of the active modes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MouseEncoding {
    /// `CSI M Cb Cx Cy`, one byte each, up to 223.
    #[default]
    Default,
    /// `?1006`: `CSI < b ; x ; y M|m`.
    Sgr,
    /// `?1016`: SGR with pixel coordinates.
    SgrPixels,
}

impl MouseEncoding {
    pub fn from_modes(modes: u32) -> Self {
        if modes & mode::SGR_PIXELS_MOUSE != 0 {
            Self::SgrPixels
        } else if modes & mode::SGR_MOUSE != 0 {
            Self::Sgr
        } else {
            Self::Default
        }
    }
}

/// A report for the PTY. xterm.js hands default-encoding reports to `onBinary` (bytes above 127 are not UTF-8)
/// and everything else to `onData` as user input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MouseReport {
    Data(String),
    Binary(Vec<u8>),
}

impl MouseReport {
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Data(s) => s.as_bytes(),
            Self::Binary(b) => b,
        }
    }
}

/// Terminal options the mouse reads, with the xterm.js defaults.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MouseOptions {
    /// xterm.js `mouseEventsRequireAlt`: while reporting is on, only Alt + button events are reported, without the
    /// Alt bit (wheels are not affected).
    pub require_alt: bool,
    /// xterm.js `scrollSensitivity`.
    pub scroll_sensitivity: f64,
    /// xterm.js `fastScrollSensitivity`, applied with Alt, Ctrl or Shift.
    pub fast_scroll_sensitivity: f64,
    /// xterm.js `scrollback`: with none (or on the alternate screen) wheels turn into arrow keys.
    pub scrollback: u32,
}

impl Default for MouseOptions {
    fn default() -> Self {
        Self {
            require_alt: false,
            scroll_sensitivity: 1.0,
            fast_scroll_sensitivity: 5.0,
            scrollback: 1000,
        }
    }
}

/// The grid a mouse event lands on.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MouseGrid {
    pub cols: u32,
    pub rows: u32,
    /// Cell height in device pixels, for pixel wheels; 0 when not measured yet (wheels then do nothing).
    pub device_cell_height: f64,
    /// `window.devicePixelRatio`.
    pub device_pixel_ratio: f64,
}

/// DOM `WheelEvent.deltaMode`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WheelDeltaMode {
    #[default]
    Pixel,
    Line,
    Page,
}

/// One wheel event at a cell (DOM `WheelEvent`; only `deltaY` counts).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct WheelInput {
    pub col: u32,
    pub row: u32,
    pub x: u32,
    pub y: u32,
    pub delta_y: f64,
    pub delta_mode: WheelDeltaMode,
    pub mods: Modifiers,
}

/// What a wheel did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WheelOutcome {
    /// Send this (a mouse report, or arrow keys on a screen without scrollback) and cancel the event.
    Send(MouseReport),
    /// The terminal took the wheel but sends nothing (too small, or filtered): cancel the event.
    Consumed,
    /// Not the terminal's: the host scrolls its view (or does nothing).
    Unhandled,
}

// xterm.js `CoreMouseButton` and `CoreMouseAction` values.
const LEFT: u8 = 0;
const NONE: u8 = 3;
const WHEEL: u8 = 4;
const UP: u8 = 0;
const DOWN: u8 = 1;
const WHEEL_LEFT: u8 = 2;
const WHEEL_RIGHT: u8 = 3;
const MOVE: u8 = 32;

/// xterm.js `ICoreMouseEvent`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CoreEvent {
    col: u32,
    row: u32,
    x: u32,
    y: u32,
    button: u8,
    action: u8,
    ctrl: bool,
    alt: bool,
    shift: bool,
}

/// xterm.js `eventCode`: the button byte before the default encoding adds 32.
fn event_code(e: &CoreEvent, sgr: bool) -> u32 {
    let mut code =
        if e.ctrl { 16 } else { 0 } | if e.shift { 4 } else { 0 } | if e.alt { 8 } else { 0 };
    if e.button == WHEEL {
        code |= 64 | u32::from(e.action);
    } else {
        code |= u32::from(e.button & 3);
        if e.button & 4 != 0 {
            code |= 64;
        }
        if e.button & 8 != 0 {
            code |= 128;
        }
        if e.action == MOVE {
            code |= u32::from(MOVE);
        } else if e.action == UP && !sgr {
            // Only SGR can report which button was released; the others send NONE.
            code |= u32::from(NONE);
        }
    }
    code
}

fn encode(e: &CoreEvent, encoding: MouseEncoding) -> Option<MouseReport> {
    let final_byte = if e.action == UP && e.button != WHEEL {
        'm'
    } else {
        'M'
    };
    match encoding {
        MouseEncoding::Default => {
            let params = [
                event_code(e, false) + 32,
                e.col.saturating_add(32),
                e.row.saturating_add(32),
            ];
            // No report past the addressable range (xterm sends 0;0 instead, vte and konsole nothing).
            if params.iter().any(|&p| p > 255) {
                return None;
            }
            let mut bytes = b"\x1b[M".to_vec();
            bytes.extend(params.iter().map(|&p| p as u8));
            Some(MouseReport::Binary(bytes))
        }
        MouseEncoding::Sgr => Some(MouseReport::Data(format!(
            "\x1b[<{};{};{}{final_byte}",
            event_code(e, true),
            e.col,
            e.row
        ))),
        MouseEncoding::SgrPixels => Some(MouseReport::Data(format!(
            "\x1b[<{};{};{}{final_byte}",
            event_code(e, true),
            e.x,
            e.y
        ))),
    }
}

/// Turns mouse and wheel events into reports. It keeps what xterm.js `MouseService` keeps between events: the
/// last report (motion to the same cell, or pixel in SGR-pixels mode, is not reported again) and the part of a
/// line pixel wheels have scrolled. One per terminal.
#[derive(Clone, Debug, Default)]
pub struct MouseEncoder {
    last: Option<CoreEvent>,
    wheel_partial: f64,
}

impl MouseEncoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets the last report and the partial wheel scroll (xterm.js `reset`, on a terminal reset).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The report for a button or motion event, or `None`. `Wheel` actions with a wheel button report one wheel
    /// step as is; DOM hosts use [`MouseEncoder::wheel`] instead.
    ///
    /// The host maps DOM events as xterm.js does: presses and releases of buttons 0-2 are left, middle and right
    /// and other buttons [`MouseButton::None`] (not reported); motion takes the first held button of left, middle,
    /// right (`buttons` bits 1, 4, 2), else `None`. It hands presses only while reporting is on and no modifier
    /// forces selection, releases and drags (from the whole document) only after a press it reported, and motion
    /// without a button only over the terminal, each only if the protocol asks for it ([`MouseProtocol::events`]).
    pub fn encode(
        &mut self,
        input: &MouseInput,
        modes: u32,
        grid: &MouseGrid,
        options: &MouseOptions,
    ) -> Option<MouseReport> {
        let button = |b: MouseButton| match b {
            MouseButton::Left => LEFT,
            MouseButton::Middle => 1,
            MouseButton::Right => 2,
            _ => NONE,
        };
        let (button, action) = match input.action {
            MouseAction::Down => (button(input.button), DOWN),
            MouseAction::Up => (button(input.button), UP),
            MouseAction::Move => (button(input.button), MOVE),
            MouseAction::Wheel => match input.button {
                MouseButton::WheelUp => (WHEEL, UP),
                MouseButton::WheelDown => (WHEEL, DOWN),
                MouseButton::WheelLeft => (WHEEL, WHEEL_LEFT),
                MouseButton::WheelRight => (WHEEL, WHEEL_RIGHT),
                _ => return None,
            },
        };
        let active = mouse_events(modes) != 0;
        if button != WHEEL && options.require_alt && active && !input.mods.alt {
            return None;
        }
        // Alt only gates the report locally; programs like tmux ignore Alt-modified reports.
        let strip_alt = button != WHEEL && options.require_alt && active;
        self.trigger(
            CoreEvent {
                col: input.col,
                row: input.row,
                x: input.x,
                y: input.y,
                button,
                action,
                ctrl: input.mods.ctrl,
                alt: input.mods.alt && !strip_alt,
                shift: input.mods.shift,
            },
            modes,
            grid,
        )
    }

    /// A DOM wheel event: a wheel report while the protocol takes wheels, arrow keys (`CSI A`/`CSI B`, SS3 in
    /// application cursor mode) on a screen without scrollback, else [`WheelOutcome::Unhandled`]. Pixel wheels
    /// add up to whole lines first; one report or arrow is sent per event however many lines it scrolls. The host
    /// asks its custom wheel handler before calling this.
    pub fn wheel(
        &mut self,
        input: &WheelInput,
        modes: u32,
        grid: &MouseGrid,
        options: &MouseOptions,
    ) -> WheelOutcome {
        if mouse_events(modes) & events::WHEEL != 0 {
            if input.delta_y == 0.0 || self.consume_wheel(input, grid, options) == 0.0 {
                return WheelOutcome::Consumed;
            }
            let event = CoreEvent {
                col: input.col,
                row: input.row,
                x: input.x,
                y: input.y,
                button: WHEEL,
                action: if input.delta_y < 0.0 { UP } else { DOWN },
                ctrl: input.mods.ctrl,
                alt: input.mods.alt,
                shift: input.mods.shift,
            };
            return match self.trigger(event, modes, grid) {
                Some(report) => WheelOutcome::Send(report),
                None => WheelOutcome::Consumed,
            };
        }
        // Without scrollback (the alternate screen) wheels scroll programs like vim or tmux with arrow keys.
        let has_scrollback = modes & mode::ALT_SCREEN == 0 && options.scrollback > 0;
        if has_scrollback || input.delta_y == 0.0 {
            return WheelOutcome::Unhandled;
        }
        if self.consume_wheel(input, grid, options) == 0.0 {
            return WheelOutcome::Consumed;
        }
        let csi = if modes & mode::APP_CURSOR != 0 {
            'O'
        } else {
            '['
        };
        let arrow = if input.delta_y < 0.0 { 'A' } else { 'B' };
        WheelOutcome::Send(MouseReport::Data(format!("\x1b{csi}{arrow}")))
    }

    /// xterm.js `_consumeWheelEvent`: the lines a wheel scrolls, keeping partial pixel scrolls (trackpads) for the
    /// next one. Shift wheels and non-vertical ones scroll nothing.
    fn consume_wheel(
        &mut self,
        input: &WheelInput,
        grid: &MouseGrid,
        options: &MouseOptions,
    ) -> f64 {
        if input.delta_y == 0.0 || input.mods.shift {
            return 0.0;
        }
        // xterm.js returns 0 while the size is unknown; non-positive or non-finite sizes count as unknown here.
        let (cell, dpr) = (grid.device_cell_height, grid.device_pixel_ratio);
        if !(cell.is_finite()
            && cell > 0.0
            && dpr.is_finite()
            && dpr > 0.0
            && input.delta_y.is_finite())
        {
            return 0.0;
        }
        let target_wheel_event_pixels = cell / dpr;
        let m = input.mods;
        let mut amount = if m.alt || m.ctrl || m.shift {
            input.delta_y * options.fast_scroll_sensitivity * options.scroll_sensitivity
        } else {
            input.delta_y * options.scroll_sensitivity
        };
        match input.delta_mode {
            WheelDeltaMode::Pixel => {
                amount /= target_wheel_event_pixels + 0.0;
                let likely_trackpad = input.delta_y.abs() < 50.0;
                if likely_trackpad {
                    amount *= 0.3;
                }
                self.wheel_partial += amount;
                amount = self.wheel_partial.abs().floor()
                    * if self.wheel_partial > 0.0 { 1.0 } else { -1.0 };
                self.wheel_partial %= 1.0;
                if !self.wheel_partial.is_finite() {
                    self.wheel_partial = 0.0;
                }
            }
            WheelDeltaMode::Page => amount *= f64::from(grid.rows),
            WheelDeltaMode::Line => {}
        }
        amount
    }

    /// xterm.js `_triggerMouseEvent`: range and sanity checks, 1-based cells, motion dedup, the protocol's
    /// restrictions and the encoding. A report suppressed by the encoding still counts as the last one.
    fn trigger(&mut self, mut e: CoreEvent, modes: u32, grid: &MouseGrid) -> Option<MouseReport> {
        if e.col >= grid.cols || e.row >= grid.rows {
            return None;
        }
        // Nonsense combinations of button and action
        if (e.button == WHEEL && e.action == MOVE)
            || (e.button == NONE && e.action != MOVE)
            || (e.button != WHEEL && (e.action == WHEEL_LEFT || e.action == WHEEL_RIGHT))
        {
            return None;
        }
        e.col += 1;
        e.row += 1;
        let encoding = MouseEncoding::from_modes(modes);
        if e.action == MOVE {
            if let Some(last) = &self.last {
                let pixels = encoding == MouseEncoding::SgrPixels;
                let same_place = if pixels {
                    last.x == e.x && last.y == e.y
                } else {
                    last.col == e.col && last.row == e.row
                };
                if same_place
                    && last.button == e.button
                    && last.action == e.action
                    && last.ctrl == e.ctrl
                    && last.alt == e.alt
                    && last.shift == e.shift
                {
                    return None;
                }
            }
        }
        if !MouseProtocol::from_modes(modes).restrict(&mut e) {
            return None;
        }
        let report = encode(&e, encoding);
        self.last = Some(e);
        report
    }
}
