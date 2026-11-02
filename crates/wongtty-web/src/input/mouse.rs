//! Mouse reports, wheels and view scrolling, and the part of the selection the mouse depends on (whether a
//! modifier forces selection, Alt+click), around `wongtty::input::MouseEncoder`.
//!
//! Ported from xterm.js @ 08829c7eb6279dda2a9e86df901353a1a89858ca (npm `@xterm/xterm` 6.1.0-beta.336), MIT license
//! (`crates/wongtty/LICENSE-XTERM`):
//! - `src/browser/services/MouseService.ts` (Copyright (c) 2017 The xterm.js authors) lines 49-102 (`bindMouse`
//!   without touch), 104-294 (`_sendEvent` and the DOM handlers), 380-443 (`_syncMouseModeState`,
//!   `_handleProtocolChange`) and 582-637 (`AltMouseCursorController`);
//! - `src/browser/services/MouseCoordsService.ts` lines 19-51 and `src/browser/input/Mouse.ts` lines 6-50
//!   (Copyright (c) 2017 and 2026 The xterm.js authors): cell coordinates;
//! - `src/browser/services/SelectionService.ts` (Copyright (c) 2017 The xterm.js authors) lines 437-447
//!   (`shouldForceSelection`), the start of `handleMouseDown` (453-481) and the Alt+click branch of `_handleMouseUp`
//!   (722-744); the rest of the selection comes with its own module (see the "Hook:" comments).
//!
//! View scrolling follows xterm.js's VS Code scrollable element as wongtty's TypeScript front end did (0510a7d
//! `packages/wongtty/xterm/src/mouse/MouseController.ts`, `_scrollView` and `wheelUnits`, after VS Code's
//! `scrollableElement.ts` `_handleMouseWheel` and `mouseEvent.ts` `StandardWheelEvent`, MIT).
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Element, Event, KeyboardEvent, MouseEvent, WheelEvent, Window};
use wongtty::input::{
    mouse_event, mouse_events, Modifiers, MouseAction, MouseButton, MouseEncoder, MouseGrid,
    MouseInput, MouseOptions, MouseReport, WheelDeltaMode, WheelInput, WheelOutcome,
};

use super::move_to_cell::{move_to_cell_sequence, Buffer};
use super::{
    browser, custom_handler_allows, dimensions, dynamic, is_disposed, send_binary, send_data,
    terminal, Shared,
};
use crate::engine::{get, mode};
use crate::options::option;
use crate::render::Dimensions;
use crate::terminal::Core;

/// Alt+click moves the cursor only for clicks shorter than this (xterm.js `ALT_CLICK_MOVE_CURSOR_TIME`).
const ALT_CLICK_MOVE_CURSOR_TIME: f64 = 500.0;
/// CSS pixels the view scrolls per wheel unit (VS Code `scrollableElement.ts`).
const SCROLL_WHEEL_SENSITIVITY: f64 = 50.0;

#[derive(Default)]
pub(super) struct Mouse {
    encoder: MouseEncoder,
    /// [`mouse_event`] bits the protocol of the last parsed write reports; `None` until `bind`.
    events: Option<u8>,
    /// MouseService's document listeners for releases and drags after a reported press.
    document_up: bool,
    document_drag: bool,
    /// Selection is on (no reporting, or reporting with `mouseEventsRequireAlt`): xterm.js `SelectionService._enabled`.
    selection_enabled: bool,
    /// The selection's document mousemove and mouseup listeners are on (the button is held after its mousedown).
    selection_listening: bool,
    alternate: bool,
    /// View scroll position in CSS pixels, kept between wheels like xterm.js `Viewport`.
    scroll_top: f64,
    /// The line the last wheel scrolled the view to.
    latest_ydisp: Option<u32>,
}

impl Mouse {
    /// `Terminal.reset`: xterm.js `MouseService.reset`.
    pub(super) fn reset(&mut self) {
        self.encoder.reset();
    }

    fn active(&self) -> bool {
        self.events.unwrap_or(0) != 0
    }

    fn requested(&self, event: u8) -> bool {
        self.events.unwrap_or(0) & event != 0
    }
}

fn require_alt(core: &Core) -> bool {
    option(&core.options, "mouseEventsRequireAlt").is_truthy()
}

/// The [`dynamic`] listeners the state needs.
fn wanted(m: &Mouse, require_alt: bool) -> u8 {
    let alt_cursor = m.active() && require_alt;
    let mut wanted = 0;
    if m.requested(mouse_event::MOVE) || alt_cursor {
        wanted |= dynamic::ELEMENT_MOUSEMOVE;
    }
    if m.document_up || m.selection_listening {
        wanted |= dynamic::DOCUMENT_MOUSEUP;
    }
    if m.document_drag || m.selection_listening {
        wanted |= dynamic::DOCUMENT_MOUSEMOVE;
    }
    if alt_cursor {
        wanted |= dynamic::DOCUMENT_KEYDOWN | dynamic::DOCUMENT_KEYUP | dynamic::WINDOW_BLUR;
    }
    wanted
}

fn reconcile(shared: &Shared, require_alt: bool) {
    let wanted = wanted(&shared.input.mouse.borrow(), require_alt);
    shared.input.dom_mut(|dom| dom.set_dynamic(wanted));
}

fn reconcile_now(shared: &Shared) {
    let Ok(core) = shared.core.try_borrow() else {
        return;
    };
    let require_alt = require_alt(&core);
    drop(core);
    reconcile(shared, require_alt);
}

/// xterm.js `bindMouse` and the initial `onProtocolChange` it forces. Under the core borrow of `open`.
/// Under the core borrow: the selection clears itself when `open` finds it disabled (`crate::selection::open`).
pub(super) fn bind(shared: &Shared, core: &Core) {
    sync_modes_with(shared, core, true);
}

/// Follows the mouse modes of the last parsed write (xterm.js `onProtocolChange`).
pub(super) fn sync_modes(shared: &Rc<Shared>, force: bool) {
    let disable_selection = match shared.core.try_borrow() {
        Ok(core) if !core.disposed => sync_modes_with(shared, &core, force),
        _ => false,
    };
    if disable_selection {
        crate::selection::disable(shared);
    }
}

/// Returns whether the selection was disabled, which clears it once the core borrow is dropped.
fn sync_modes_with(shared: &Shared, core: &Core, force: bool) -> bool {
    let modes = shared.input.modes.get();
    let events = mouse_events(modes);
    let changed = {
        let mut m = shared.input.mouse.borrow_mut();
        let alternate = modes & mode::ALT_SCREEN != 0;
        if alternate != m.alternate {
            // A buffer switch forgets the view position, like xterm.js `Viewport` on buffer activation.
            m.alternate = alternate;
            m.latest_ydisp = None;
        }
        if !force && m.events == Some(events) {
            false
        } else {
            m.events = Some(events);
            if events & mouse_event::UP == 0 {
                m.document_up = false;
            }
            if events & mouse_event::DRAG == 0 {
                m.document_drag = false;
            }
            true
        }
    };
    if !changed {
        return false;
    }
    let require_alt = require_alt(core);
    let enabled = sync_mouse_mode_state(shared, require_alt);
    reconcile(shared, require_alt);
    !enabled
}

/// xterm.js `_syncMouseModeState`: while the program takes clicks they are not selections, unless
/// `mouseEventsRequireAlt` leaves clicks without Alt to the selection. Returns whether selection is on; the caller
/// clears the selection when it is not (xterm.js `disable`), without holding a borrow.
fn sync_mouse_mode_state(shared: &Shared, require_alt: bool) -> bool {
    let active = shared.input.mouse.borrow().active();
    let enable_selection = !active || require_alt;
    shared.input.mouse.borrow_mut().selection_enabled = enable_selection;
    if let Some(element) = shared.input.element() {
        let classes = element.class_list();
        let _ = if active && !require_alt {
            classes.add_1("enable-mouse-events")
        } else {
            classes.remove_1("enable-mouse-events")
        };
    }
    enable_selection
}

/// `mouseEventsRequireAlt` changed.
pub(super) fn require_alt_changed(shared: &Rc<Shared>) {
    let Ok(core) = shared.core.try_borrow() else {
        return;
    };
    let require_alt = require_alt(&core);
    drop(core);
    if shared.input.mouse.borrow().events.is_some() {
        let enabled = sync_mouse_mode_state(shared, require_alt);
        reconcile(shared, require_alt);
        if !enabled {
            crate::selection::disable(shared);
        }
    }
}

/// Whether this click is a selection although the program takes clicks (xterm.js
/// `SelectionService.shouldForceSelection`): any click without Alt under `mouseEventsRequireAlt`, else Option with
/// `macOptionClickForcesSelection` on macOS, Shift elsewhere. The selection module reads it too.
pub(crate) fn should_force_selection(shared: &Shared, event: &MouseEvent) -> bool {
    let active = shared.input.mouse.borrow().active();
    let Ok(core) = shared.core.try_borrow() else {
        return false;
    };
    if require_alt(&core) && active {
        return !event.alt_key();
    }
    if browser().mac {
        return event.alt_key()
            && option(&core.options, "macOptionClickForcesSelection").is_truthy();
    }
    event.shift_key()
}

/// Whether selection is on (xterm.js `SelectionService.enable`/`disable`), for the selection module.
pub(crate) fn selection_enabled(shared: &Shared) -> bool {
    shared.input.mouse.borrow().selection_enabled
}

/// Turns the selection's document mousemove and mouseup listeners on or off (xterm.js
/// `_addMouseDownListeners`/`_removeMouseDownListeners`).
pub(crate) fn set_selection_listeners(shared: &Shared, on: bool) {
    if let Ok(mut m) = shared.input.mouse.try_borrow_mut() {
        if m.selection_listening == on {
            return;
        }
        m.selection_listening = on;
    }
    reconcile_now(shared);
}

/// `mouseEventsRequireAlt` is on while the program takes clicks.
pub(crate) fn require_alt_active(shared: &Shared) -> bool {
    alt_cursor_active(shared)
}

pub(crate) fn platform_mac() -> bool {
    browser().mac
}

pub(crate) fn platform_windows() -> bool {
    browser().windows
}

pub(crate) fn platform_linux() -> bool {
    browser().linux
}

pub(crate) fn textarea(shared: &Shared) -> Option<web_sys::HtmlTextAreaElement> {
    shared.input.textarea()
}

pub(crate) fn element(shared: &Shared) -> Option<web_sys::HtmlElement> {
    shared.input.element()
}

pub(crate) fn screen(shared: &Shared) -> Option<web_sys::HtmlElement> {
    shared.input.dom(|dom| dom.screen.clone())
}

/// xterm.js `MouseCoordsService.getCoords` over the screen with the engine's size: the cell under the mouse,
/// 1-based; `selection` puts the right half of a cell on the next one.
pub(crate) fn screen_coords(
    shared: &Shared,
    event: &MouseEvent,
    selection: bool,
) -> Option<(i32, i32)> {
    let (screen, window) = shared.input.dom(|dom| {
        (
            dom.screen.clone().unchecked_into::<Element>(),
            dom.window.clone(),
        )
    })?;
    let (cols, rows, d) = {
        let core = shared.core.try_borrow().ok()?;
        if core.disposed {
            return None;
        }
        let (cols, rows) = core.size();
        (cols, rows, dimensions(&core))
    };
    coords(event, &screen, window.as_ref(), cols, rows, &d, selection)
}

/// The mouse's y in CSS pixels from the top of the screen, and the canvas height (drag scrolling).
pub(crate) fn screen_offset_y(shared: &Shared, event: &MouseEvent) -> Option<(f64, f64)> {
    let (screen, window) = shared.input.dom(|dom| {
        (
            dom.screen.clone().unchecked_into::<Element>(),
            dom.window.clone(),
        )
    })?;
    let height = {
        let core = shared.core.try_borrow().ok()?;
        dimensions(&core).css_canvas.height
    };
    Some((relative_coords(event, &screen, window.as_ref()).1, height))
}

fn number(target: &JsValue, key: &str) -> f64 {
    get(target, key).as_f64().unwrap_or(f64::NAN)
}

fn mods(event: &MouseEvent) -> Modifiers {
    Modifiers {
        shift: event.shift_key(),
        alt: event.alt_key(),
        ctrl: event.ctrl_key(),
        meta: event.meta_key(),
    }
}

fn cancel(event: &Event) {
    event.prevent_default();
    event.stop_propagation();
}

/// xterm.js `getCoordsRelativeToElement`: client pixels minus the element's position and padding.
fn relative_coords(event: &MouseEvent, element: &Element, window: Option<&Window>) -> (f64, f64) {
    let rect = element.get_bounding_client_rect();
    let padding = |side: &str| {
        window
            .and_then(|w| w.get_computed_style(element).ok().flatten())
            .map_or(f64::NAN, |style| {
                js_sys::parse_int(&style.get_property_value(side).unwrap_or_default(), 10)
            })
    };
    (
        number(event, "clientX") - rect.left() - padding("padding-left"),
        number(event, "clientY") - rect.top() - padding("padding-top"),
    )
}

/// The cell, 1-based, like xterm.js `getCoords`; `selection` puts the right half of a cell on the next one.
fn coords(
    event: &MouseEvent,
    element: &Element,
    window: Option<&Window>,
    cols: u32,
    rows: u32,
    d: &Dimensions,
    selection: bool,
) -> Option<(i32, i32)> {
    let cell = d.css_cell;
    if !(cell.width > 0.0 && cell.height > 0.0) {
        return None;
    }
    let (x, y) = relative_coords(event, element, window);
    let half = if selection { cell.width / 2.0 } else { 0.0 };
    let x = ((x + half) / cell.width).ceil();
    let y = (y / cell.height).ceil();
    let x = x
        .max(1.0)
        .min(f64::from(cols) + if selection { 1.0 } else { 0.0 });
    let y = y.max(1.0).min(f64::from(rows));
    (x.is_finite() && y.is_finite()).then_some((x as i32, y as i32))
}

/// xterm.js `getMouseReportCoords`: the 0-based cell and the pixel within the screen.
fn report_coords(
    event: &MouseEvent,
    screen: &Element,
    window: Option<&Window>,
    d: &Dimensions,
) -> Option<(u32, u32, u32, u32)> {
    let (x, y) = relative_coords(event, screen, window);
    let (cell, canvas) = (d.css_cell, d.css_canvas);
    // NaN when the element left the DOM or the event has no position
    if !(x.is_finite() && y.is_finite()) || !(cell.width > 0.0 && cell.height > 0.0) {
        return None;
    }
    let x = x.max(0.0).min(canvas.width - 1.0);
    let y = y.max(0.0).min(canvas.height - 1.0);
    let (col, row) = ((x / cell.width).floor(), (y / cell.height).floor());
    // A canvas narrower than a pixel puts the event outside the grid, where xterm.js drops it.
    if col < 0.0 || row < 0.0 || x < 0.0 || y < 0.0 {
        return None;
    }
    Some((col as u32, row as u32, x.floor() as u32, y.floor() as u32))
}

/// What a mouse report needs, read in one borrow.
struct Context {
    dimensions: Dimensions,
    modes: u32,
    grid: MouseGrid,
    options: MouseOptions,
    screen: Element,
    window: Option<Window>,
}

fn context(shared: &Shared) -> Option<Context> {
    let (screen, window) = shared.input.dom(|dom| {
        (
            dom.screen.clone().unchecked_into::<Element>(),
            dom.window.clone(),
        )
    })?;
    let core = shared.core.try_borrow().ok()?;
    if core.disposed {
        return None;
    }
    let dimensions = dimensions(&core);
    let v = &core.viewport;
    let number = |key: &str, default: f64| option(&core.options, key).as_f64().unwrap_or(default);
    Some(Context {
        dimensions,
        modes: shared.input.modes.get(),
        grid: MouseGrid {
            cols: v.cols,
            rows: v.rows,
            device_cell_height: dimensions.device_cell.height,
            device_pixel_ratio: window.as_ref().map_or(1.0, Window::device_pixel_ratio),
        },
        options: MouseOptions {
            require_alt: require_alt(&core),
            scroll_sensitivity: number("scrollSensitivity", 1.0),
            fast_scroll_sensitivity: number("fastScrollSensitivity", 5.0),
            scrollback: number("scrollback", 1000.0).max(0.0) as u32,
        },
        screen,
        window,
    })
}

/// Hands a report to the program: default-encoding reports to `onBinary`, the rest to `onData` as user input.
fn deliver(shared: &Rc<Shared>, report: MouseReport) {
    match report {
        MouseReport::Data(data) => send_data(shared, &data.into(), true),
        // One char per byte, like xterm.js's binary strings
        MouseReport::Binary(bytes) => send_binary(
            shared,
            &bytes.iter().map(|&b| char::from(b)).collect::<String>(),
        ),
    }
}

/// xterm.js `_sendEvent` for buttons and motion.
fn send(shared: &Rc<Shared>, event: &MouseEvent, action: MouseAction) {
    let Some(c) = context(shared) else {
        return;
    };
    let Some((col, row, x, y)) = report_coords(event, &c.screen, c.window.as_ref(), &c.dimensions)
    else {
        return;
    };
    let by_button = |b: i16| match b {
        0 => MouseButton::Left,
        1 => MouseButton::Middle,
        2 => MouseButton::Right,
        _ => MouseButton::None,
    };
    let button = match action {
        // `buttons` only reports up to button 5
        MouseAction::Move => {
            let buttons = event.buttons();
            if buttons & 1 != 0 {
                MouseButton::Left
            } else if buttons & 4 != 0 {
                MouseButton::Middle
            } else if buttons & 2 != 0 {
                MouseButton::Right
            } else {
                MouseButton::None
            }
        }
        _ => by_button(event.button()),
    };
    let input = MouseInput {
        col,
        row,
        x,
        y,
        button,
        action,
        mods: mods(event),
    };
    let report = shared
        .input
        .mouse
        .borrow_mut()
        .encoder
        .encode(&input, c.modes, &c.grid, &c.options);
    if let Some(report) = report {
        deliver(shared, report);
    }
}

/// The element's mousedown: the selection's handler, then MouseService's.
pub(super) fn mouse_down(shared: &Rc<Shared>, event: &MouseEvent) {
    selection_mouse_down(shared, event);
    // Firefox doesn't appear to fire the contextmenu event on right click (CoreBrowserTerminal `_initGlobal`)
    if browser().firefox && event.button() == 2 {
        crate::selection::right_click(shared, event);
    }
    if is_disposed(shared) {
        return;
    }
    // MouseService `_handleMouseDown`
    event.prevent_default();
    terminal(shared).focus();
    if is_disposed(shared) {
        return;
    }
    // Not for the program while it takes no clicks, or while a modifier forces selection
    if !shared.input.mouse.borrow().active() || should_force_selection(shared, event) {
        return;
    }
    send(shared, event, MouseAction::Down);
    // Releases and drags are reported from the whole document after a reported press
    {
        let Ok(mut m) = shared.input.mouse.try_borrow_mut() else {
            return;
        };
        if m.requested(mouse_event::UP) {
            m.document_up = true;
        }
        if m.requested(mouse_event::DRAG) {
            m.document_drag = true;
        }
    }
    reconcile_now(shared);
}

/// The start of xterm.js `SelectionService.handleMouseDown`.
fn selection_mouse_down(shared: &Rc<Shared>, event: &MouseEvent) {
    crate::selection::set_mouse_down_time(shared, event.time_stamp());
    // If we have selection, we want the context menu on right click even if the terminal is in mouse mode.
    if event.button() == 2 && crate::selection::has_selection(shared) {
        return;
    }
    // Only action the primary button
    if event.button() != 0 {
        return;
    }
    // Alt clicks go to the program under `mouseEventsRequireAlt`
    if event.alt_key() && shared.input.mouse.borrow().active() && alt_cursor_active(shared) {
        return;
    }
    // Allow selection when using a specific modifier key, even when disabled
    if !shared.input.mouse.borrow().selection_enabled {
        if !should_force_selection(shared, event) {
            return;
        }
        // Don't send the mouse down event to the current process, we want to select
        event.stop_propagation();
    }
    // Tell the browser not to start a regular selection
    event.prevent_default();
    crate::selection::mouse_down(shared, event);
}

/// A mouseup anywhere in the document: the selection's listener (added by the earlier mousedown listener), then
/// MouseService's.
pub(super) fn document_mouse_up(shared: &Rc<Shared>, event: &MouseEvent) {
    if shared.input.mouse.borrow().selection_listening {
        crate::selection::mouse_up(shared);
        selection_mouse_up(shared, event);
        if is_disposed(shared) {
            return;
        }
    }
    if !shared.input.mouse.borrow().document_up {
        return;
    }
    send(shared, event, MouseAction::Up);
    // No other button held: remove the document listeners
    if event.buttons() == 0 {
        if let Ok(mut m) = shared.input.mouse.try_borrow_mut() {
            m.document_up = false;
            m.document_drag = false;
        }
        reconcile_now(shared);
    }
}

/// The Alt+click branch of xterm.js `SelectionService._handleMouseUp`.
fn selection_mouse_up(shared: &Rc<Shared>, event: &MouseEvent) {
    let elapsed = event.time_stamp() - crate::selection::mouse_down_time(shared);
    let alt_click_moves = shared.core.try_borrow().is_ok_and(|core| {
        !core.disposed && option(&core.options, "altClickMovesCursor").is_truthy()
    });
    if !(elapsed < ALT_CLICK_MOVE_CURSOR_TIME
        && event.alt_key()
        && alt_click_moves
        && crate::selection::text_is_at_most_one_unit(shared))
    {
        crate::selection::mouse_up_changed(shared);
        return;
    }
    let view = crate::buffer::active(shared);
    if view.base_y() != view.viewport_y() {
        return;
    }
    let Some((element, window)) = shared.input.dom(|dom| {
        (
            dom.element.clone().unchecked_into::<Element>(),
            dom.window.clone(),
        )
    }) else {
        return;
    };
    let Some((cols, rows, d, app_cursor, scrollback)) = shared.core.try_borrow().ok().map(|core| {
        let (cols, rows) = core.size();
        let scrollback = option(&core.options, "scrollback").as_f64().unwrap_or(0.0);
        (
            cols,
            rows,
            dimensions(&core),
            shared.input.modes.get() & mode::APP_CURSOR != 0,
            scrollback,
        )
    }) else {
        return;
    };
    let Some((x, y)) = coords(event, &element, window.as_ref(), cols, rows, &d, false) else {
        return;
    };
    let buffer = Buffer {
        x: view.cursor_x() as i32,
        y: view.cursor_y() as i32,
        cols: cols as i32,
        rows: rows as i32,
        has_scrollback: view.kind() == "normal" && scrollback > 0.0,
        view,
    };
    let sequence = move_to_cell_sequence(x - 1, y - 1, &buffer, app_cursor);
    send_data(shared, &sequence.into(), true);
}

/// A mousemove anywhere in the document after a reported press: drags.
pub(super) fn document_mouse_move(shared: &Rc<Shared>, event: &MouseEvent) {
    // The selection's listener came first and stops the event from reaching MouseService's
    if shared.input.mouse.borrow().selection_listening {
        crate::selection::mouse_move(shared, event);
        return;
    }
    if shared.input.mouse.borrow().document_drag && event.buttons() != 0 {
        send(shared, event, MouseAction::Move);
    }
}

/// A mousemove over the element: the Alt cursor, and motion without a button while the program asks for all
/// motion.
pub(super) fn element_mouse_move(shared: &Rc<Shared>, event: &MouseEvent) {
    alt_cursor_from_modifier(shared, event);
    if shared.input.mouse.borrow().requested(mouse_event::MOVE) && event.buttons() == 0 {
        send(shared, event, MouseAction::Move);
    }
}

fn alt_cursor_active(shared: &Shared) -> bool {
    let active = shared.input.mouse.borrow().active();
    active
        && shared
            .core
            .try_borrow()
            .is_ok_and(|core| require_alt(&core))
}

fn set_enable_mouse_events(shared: &Shared, on: bool) {
    if let Some(element) = shared.input.element() {
        let classes = element.class_list();
        let _ = if on {
            classes.add_1("enable-mouse-events")
        } else {
            classes.remove_1("enable-mouse-events")
        };
    }
}

/// xterm.js `AltMouseCursorController.syncFromModifier`: with `mouseEventsRequireAlt`, the mouse cursor shows
/// reporting while Alt is held.
pub(super) fn alt_cursor_from_modifier(shared: &Shared, event: &Event) {
    if !alt_cursor_active(shared) {
        return;
    }
    let alt = if let Some(e) = event.dyn_ref::<KeyboardEvent>() {
        e.get_modifier_state("Alt")
    } else if let Some(e) = event.dyn_ref::<MouseEvent>() {
        e.get_modifier_state("Alt")
    } else {
        return;
    };
    set_enable_mouse_events(shared, alt);
}

/// The window lost focus while the Alt cursor is on.
pub(super) fn alt_cursor_reset(shared: &Shared) {
    if alt_cursor_active(shared) {
        set_enable_mouse_events(shared, false);
    }
}

/// The element's wheel: MouseService's report wheel while the program takes wheels, else its passive wheel.
pub(super) fn wheel(shared: &Rc<Shared>, event: &WheelEvent) {
    if shared.input.mouse.borrow().requested(mouse_event::WHEEL) {
        report_wheel(shared, event);
    } else {
        passive_wheel(shared, event);
    }
}

fn wheel_input(event: &WheelEvent, at: (u32, u32, u32, u32)) -> WheelInput {
    let (col, row, x, y) = at;
    WheelInput {
        col,
        row,
        x,
        y,
        delta_y: event.delta_y(),
        delta_mode: match event.delta_mode() {
            1 => WheelDeltaMode::Line,
            2 => WheelDeltaMode::Page,
            _ => WheelDeltaMode::Pixel,
        },
        mods: mods(event),
    }
}

/// xterm.js `_handleWheel`: report the wheel and always take it.
fn report_wheel(shared: &Rc<Shared>, event: &WheelEvent) {
    let at = context(shared)
        .and_then(|c| report_coords(event, &c.screen, c.window.as_ref(), &c.dimensions));
    if let Some(at) = at {
        if custom_handler_allows(&shared.input.custom_wheel_handler, event) {
            if let Some(c) = context(shared) {
                let outcome = shared.input.mouse.borrow_mut().encoder.wheel(
                    &wheel_input(event, at),
                    c.modes,
                    &c.grid,
                    &c.options,
                );
                if let WheelOutcome::Send(report) = outcome {
                    deliver(shared, report);
                }
            }
        }
    }
    cancel(event);
}

/// xterm.js `_handlePassiveWheel`: without scrollback (the alternate screen) wheels become arrow keys.
fn passive_wheel(shared: &Rc<Shared>, event: &WheelEvent) {
    if !custom_handler_allows(&shared.input.custom_wheel_handler, event) {
        return;
    }
    let Some(c) = context(shared) else {
        return;
    };
    // Arrow keys need no cell
    let at = report_coords(event, &c.screen, c.window.as_ref(), &c.dimensions).unwrap_or_default();
    let outcome = shared.input.mouse.borrow_mut().encoder.wheel(
        &wheel_input(event, at),
        c.modes,
        &c.grid,
        &c.options,
    );
    match outcome {
        WheelOutcome::Send(report) => {
            deliver(shared, report);
            cancel(event);
        }
        WheelOutcome::Consumed => cancel(event),
        WheelOutcome::Unhandled => {}
    }
}

/// Scrolls the view like xterm.js 6.1, whose VS Code scrollable element takes the wheel in CSS pixels: 50 px per
/// wheel unit, Alt multiplies by `fastScrollSensitivity`, and the view lands on the nearest row with the rest kept
/// for the next wheel (no scrollbar, `smoothScrollDuration` 0). Off while the program takes wheel reports; with
/// nothing to scroll the wheel goes on to the element, which sends arrow keys without scrollback.
pub(super) fn scroll_view(shared: &Rc<Shared>, event: &WheelEvent) {
    if event.default_prevented() || shared.input.mouse.borrow().requested(mouse_event::WHEEL) {
        return;
    }
    let Some((d, sensitivity, fast)) = shared.core.try_borrow().ok().and_then(|core| {
        let number = |key: &str| option(&core.options, key).as_f64().unwrap_or(f64::NAN);
        let renderer = core.renderer.as_ref()?;
        (!core.disposed).then(|| {
            (
                renderer.dimensions(),
                number("scrollSensitivity"),
                number("fastScrollSensitivity"),
            )
        })
    }) else {
        return;
    };
    let cell_height = d.css_cell.height;
    let view = crate::buffer::active(shared);
    let (ydisp, length) = (view.viewport_y(), view.length());
    let mut delta_y = wheel_units(event, event.delta_y(), "wheelDeltaY") * sensitivity;
    let delta_x = wheel_units(event, event.delta_x(), "wheelDeltaX") * sensitivity;
    // Only the predominant axis scrolls, and Shift turns vertical into horizontal off macOS: there is no
    // horizontal scrolling.
    if delta_y.abs() < delta_x.abs() || (!browser().mac && event.shift_key()) {
        delta_y = 0.0;
    }
    if event.alt_key() {
        delta_y *= fast;
    }
    if delta_y == 0.0 || delta_y.is_nan() {
        return;
    }
    let row = {
        let mut m = shared.input.mouse.borrow_mut();
        // Output or the API moved the view since the last wheel (xterm.js `Viewport._sync`).
        if Some(ydisp) != m.latest_ydisp {
            m.scroll_top = f64::from(ydisp) * cell_height;
        }
        let delta = SCROLL_WHEEL_SENSITIVITY * delta_y;
        let max_scroll_top = (cell_height * f64::from(length) - d.css_canvas.height).max(0.0);
        let step = if delta < 0.0 {
            delta.floor()
        } else {
            delta.ceil()
        };
        let scroll_top = (m.scroll_top - step).min(max_scroll_top).max(0.0);
        if scroll_top == m.scroll_top {
            return;
        }
        m.scroll_top = scroll_top;
        // JavaScript's Math.round
        let row = (scroll_top / cell_height + 0.5).floor() as u32;
        if row != ydisp {
            m.latest_ydisp = Some(row);
        }
        row
    };
    cancel(event);
    if row != ydisp {
        let _ = terminal(shared).scroll_lines(row as i32 - ydisp as i32);
    }
}

/// Wheel units, positive up or left, like VS Code's `StandardWheelEvent` for `wheel` events: Chromium's
/// `wheelDelta*` in 120ths (times devicePixelRatio up to Chrome 122), else lines, else 40 px.
fn wheel_units(event: &WheelEvent, delta: f64, legacy: &str) -> f64 {
    if let Some(wheel_delta) = get(event, legacy).as_f64() {
        let old_chrome = browser().chrome.is_some_and(|v| v <= 122);
        let dpr = event.view().map_or(1.0, |w| w.device_pixel_ratio());
        return wheel_delta / if old_chrome { 120.0 * dpr } else { 120.0 };
    }
    if event.delta_mode() == 1 {
        let b = browser();
        return if b.firefox && !b.mac {
            -delta / 3.0
        } else {
            -delta
        };
    }
    -delta / 40.0
}
