//! The xterm.js 6.1 vertical scrollbar: `.xterm-scrollable-element` wraps `.xterm-screen` and holds a
//! `.xterm-scrollbar.xterm-vertical` with an `.xterm-slider`, sized from the scrollback and the view position of the
//! last frame. Dragging the slider or pressing the track scrolls the view; like xterm.js the bar shows while the
//! pointer is over the terminal or the view scrolls, and fades 500 ms later. The wheel is the mouse module's.
//!
//! Memory: three elements and one closure per opened terminal. With `options.scrollbar.showScrollbar: false` they
//! stay, as in xterm.js: the bar is 0 px wide and never shown (`ScrollbarVisibility.HIDDEN`). Not built: the
//! horizontal scrollbar, the arrows (`showArrows`) and the shadows, which xterm.js keeps hidden for a terminal.
//!
//! Ported from xterm.js 6.1.0-beta.336 (commit 08829c7eb6279dda2a9e86df901353a1a89858ca, MIT; the scrollable
//! files derive from VS Code, MIT):
//! - `src/browser/scrollable/scrollbarState.ts` lines 10–245: slider size and position, offsets to positions.
//! - `src/browser/scrollable/scrollable.ts` lines 56–103 and 249–284: clamping the scroll position.
//! - `src/browser/scrollable/scrollbarVisibilityController.ts` lines 11–112: visible, invisible and fade classes.
//! - `src/browser/scrollable/abstractScrollbar.ts` lines 59–266, `verticalScrollbar.ts` lines 17–80 and
//!   `globalPointerMoveMonitor.ts` lines 37–107: the bar's DOM, track presses and slider drags.
//! - `src/browser/scrollable/scrollableElement.ts` lines 184–260 and 452–540: the wrapper, reveal and hide.
//! - `src/browser/Viewport.ts` lines 75–107, 141–154 and 172–219: options, sync from the buffer, scroll requests.
use std::cell::RefCell;
use std::rc::{Rc, Weak};

use js_sys::Function;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Element, Event, EventTarget, HtmlElement, MouseEvent, PointerEvent};

use crate::engine::{get, mode, Viewport};
use crate::options::option;
use crate::render::color::{css_to_color, js_round, opacity, Color};
use crate::render::Dimensions;
use crate::terminal::Shared;
use crate::timer::Timeout;

/// xterm.js `ViewportConstants.DEFAULT_SCROLL_BAR_WIDTH`.
const DEFAULT_WIDTH: f64 = 14.0;
const MINIMUM_SLIDER_SIZE: f64 = 20.0;
/// `scrollableElement.ts` `Constants.HIDE_TIMEOUT`.
const HIDE_TIMEOUT_MS: i32 = 500;
const VISIBLE: &str = "xterm-visible xterm-scrollbar xterm-vertical";
const INVISIBLE: &str = "xterm-invisible xterm-scrollbar xterm-vertical";
/// Events the one closure listens to on the wrapper and the bar; drags add pointer events on the captured target.
const LISTENERS: [(Target, &str); 4] = [
    (Target::Root, "mouseover"),
    (Target::Root, "mouseleave"),
    (Target::Bar, "pointerdown"),
    (Target::Bar, "click"),
];

#[derive(Clone, Copy)]
enum Target {
    Root,
    Bar,
}

/// `ScrollbarState`, without arrows or an opposite scrollbar.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct State {
    scrollbar_size: f64,
    visible_size: f64,
    scroll_size: f64,
    scroll_position: f64,
    available_size: f64,
    is_needed: bool,
    slider_size: f64,
    slider_ratio: f64,
    slider_position: f64,
}

impl State {
    fn set_visible_size(&mut self, size: f64) -> bool {
        self.set(|s| &mut s.visible_size, size)
    }

    fn set_scroll_size(&mut self, size: f64) -> bool {
        self.set(|s| &mut s.scroll_size, size)
    }

    fn set_scroll_position(&mut self, position: f64) -> bool {
        self.set(|s| &mut s.scroll_position, position)
    }

    fn set(&mut self, field: impl Fn(&mut Self) -> &mut f64, value: f64) -> bool {
        let value = js_round(value);
        if *field(self) == value {
            return false;
        }
        *field(self) = value;
        self.refresh();
        true
    }

    fn refresh(&mut self) {
        let available = self.visible_size.max(0.0);
        self.available_size = js_round(available);
        self.is_needed = self.scroll_size > 0.0 && self.scroll_size > self.visible_size;
        if !self.is_needed {
            self.slider_size = js_round(available);
            self.slider_ratio = 0.0;
            self.slider_position = 0.0;
            return;
        }
        let slider = js_round(
            MINIMUM_SLIDER_SIZE.max((self.visible_size * available / self.scroll_size).floor()),
        );
        self.slider_size = slider;
        self.slider_ratio = (available - slider) / (self.scroll_size - self.visible_size);
        self.slider_position = js_round(self.scroll_position * self.slider_ratio);
    }

    fn desired_from_offset(&self, offset: f64) -> f64 {
        if !self.is_needed {
            return 0.0;
        }
        js_round((offset - self.slider_size / 2.0) / self.slider_ratio)
    }

    fn desired_from_delta(&self, delta: f64) -> f64 {
        if !self.is_needed {
            return 0.0;
        }
        js_round((self.slider_position + delta) / self.slider_ratio)
    }
}

/// `ScrollState` of the vertical axis: what the scroll position is clamped to.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Scroll {
    height: f64,
    scroll_height: f64,
    raw_top: f64,
    top: f64,
}

impl Scroll {
    fn new(height: f64, scroll_height: f64, raw_top: f64) -> Self {
        let height = height.max(0.0);
        let mut top = raw_top;
        if top + height > scroll_height {
            top = scroll_height - height;
        }
        Self {
            height,
            scroll_height,
            raw_top,
            top: top.max(0.0),
        }
    }
}

struct Drag {
    target: EventTarget,
    pointer_id: i32,
    buttons: u16,
    start_y: f64,
    state: State,
}

struct Inner {
    root: HtmlElement,
    screen: HtmlElement,
    bar: HtmlElement,
    slider: HtmlElement,
    /// Slider colours from the theme (xterm.js `Viewport` style element); `None` keeps the page-wide defaults.
    style: Option<Element>,
    /// The one listener of the wrapper, the bar and drags, and the same as a function to add and remove.
    _closure: Closure<dyn FnMut(Event)>,
    listener: Function,
    scroll: Scroll,
    state: State,
    should_render: bool,
    /// `showScrollbar: false` (`ScrollbarVisibility.HIDDEN`): the bar is never shown.
    hidden: bool,
    should_be_visible: bool,
    is_needed: bool,
    is_visible: bool,
    reveal_timer: Option<Timeout>,
    hide_timer: Option<Timeout>,
    mouse_over: bool,
    drag: Option<Drag>,
    cell_height: f64,
    ydisp: u32,
    /// `Viewport._latestYDisp`: the row a drag asked for, so the next sync keeps the slider where it is.
    latest_ydisp: Option<u32>,
    alternate: bool,
    /// xterm.js syncs on scroll, resize and buffer switch only, so a terminal that never had scrollback keeps the
    /// zero sizes it was built with: the size of the first frame, until the first sync.
    first_size: Option<(u32, u32)>,
    synced: bool,
    /// The top row a drag or press asked for, applied by the next frame.
    request: Option<u32>,
    wake: Weak<Shared>,
    /// This cell, for timers.
    this: Weak<RefCell<Inner>>,
}

/// The scrollbar of one opened terminal; dropping it removes its DOM and listeners and unwraps the screen.
pub(crate) struct Scrollbar {
    inner: Rc<RefCell<Inner>>,
}

impl Scrollbar {
    fn new(shared: &Rc<Shared>, screen: &HtmlElement, width: f64) -> Result<Self, JsValue> {
        let document = screen
            .owner_document()
            .ok_or("oxtty/xterm: the screen is not in a document")?;
        let create = |class: &str| -> Result<HtmlElement, JsValue> {
            let element: HtmlElement = document.create_element("div")?.unchecked_into();
            element.set_class_name(class);
            Ok(element)
        };
        let root = create(if is_mac() {
            "xterm-scrollable-element  xterm-mac"
        } else {
            "xterm-scrollable-element "
        })?;
        root.set_attribute("role", "presentation")?;
        root.style().set_property("position", "relative")?;
        let bar = create(INVISIBLE)?;
        bar.set_attribute("role", "presentation")?;
        bar.set_attribute("aria-hidden", "true")?;
        bar.style().set_property("position", "absolute")?;
        let slider = create("xterm-slider")?;
        let style = slider.style();
        style.set_property("position", "absolute")?;
        style.set_property("top", "0px")?;
        style.set_property("left", "0px")?;
        style.set_property("width", &px(width))?;
        style.set_property("transform", "translate3d(0px, 0px, 0px)")?;
        style.set_property("contain", "strict")?;
        bar.append_child(&slider)?;
        screen.replace_with_with_node_1(&root)?;
        root.append_child(screen)?;
        root.append_child(&bar)?;

        let wake = Rc::downgrade(shared);
        let inner = Rc::new_cyclic(|this: &Weak<RefCell<Inner>>| {
            let weak = this.clone();
            let closure = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
                let Some(inner) = weak.upgrade() else { return };
                // Busy only if our own DOM call dispatched this synchronously; it needs no handling then.
                let Ok(mut inner) = inner.try_borrow_mut() else {
                    return;
                };
                inner.handle(&event);
            });
            let listener: Function = closure.as_ref().unchecked_ref::<Function>().clone();
            RefCell::new(Inner {
                root,
                screen: screen.clone(),
                bar,
                slider,
                style: None,
                _closure: closure,
                listener,
                scroll: Scroll::default(),
                state: State {
                    scrollbar_size: js_round(width),
                    ..State::default()
                },
                should_render: true,
                hidden: false,
                should_be_visible: false,
                is_needed: false,
                is_visible: false,
                reveal_timer: None,
                hide_timer: None,
                mouse_over: false,
                drag: None,
                cell_height: 0.0,
                ydisp: 0,
                latest_ydisp: None,
                alternate: false,
                first_size: None,
                synced: false,
                request: None,
                wake,
                this: this.clone(),
            })
        });
        {
            let mut i = inner.borrow_mut();
            i.render();
            for (target, kind) in LISTENERS {
                let _ = i
                    .target(target)
                    .add_event_listener_with_callback(kind, &i.listener);
            }
        }
        Ok(Self { inner })
    }

    /// Brings the bar up to date with the frame just drawn (`Viewport._sync`).
    pub(crate) fn sync(&self, viewport: &Viewport, dimensions: &Dimensions) {
        if let Ok(mut inner) = self.inner.try_borrow_mut() {
            inner.sync(viewport, dimensions);
        }
    }

    /// The top row a drag or a track press asked for since the last call.
    fn take_request(&self) -> Option<u32> {
        self.inner.try_borrow_mut().ok()?.request.take()
    }

    fn set_width(&self, width: f64) {
        let Ok(mut inner) = self.inner.try_borrow_mut() else {
            return;
        };
        if inner.state.scrollbar_size == js_round(width) {
            return;
        }
        let _ = inner.slider.style().set_property("width", &px(width));
        inner.state.scrollbar_size = js_round(width);
        inner.should_render = true;
        inner.render();
    }

    /// `ScrollbarVisibilityController.setVisibility` between `AUTO` and `HIDDEN`.
    fn set_hidden(&self, hidden: bool) {
        let Ok(mut inner) = self.inner.try_borrow_mut() else {
            return;
        };
        if inner.hidden != hidden {
            inner.hidden = hidden;
            inner.ensure_visibility();
        }
    }

    /// The slider colours of `ThemeService` (the foreground at 20, 40 and 50%, or the theme's own), in a style
    /// element only when the theme sets one of them; else the page-wide defaults apply.
    fn set_theme(&self, theme: &JsValue) {
        let Ok(mut inner) = self.inner.try_borrow_mut() else {
            return;
        };
        let keys = [
            "scrollbarSliderBackground",
            "scrollbarSliderHoverBackground",
            "scrollbarSliderActiveBackground",
        ];
        let css = |key: &str| get(theme, key).as_string().and_then(|c| css_to_color(&c));
        if css("foreground").is_none() && keys.iter().all(|key| css(key).is_none()) {
            if let Some(style) = inner.style.take() {
                style.remove();
            }
            return;
        }
        let foreground = css("foreground").unwrap_or_else(|| Color {
            css: "#ffffff".into(),
            rgba: 0xffffffff,
        });
        let [normal, hover, active] = [(keys[0], 0.2), (keys[1], 0.4), (keys[2], 0.5)]
            .map(|(key, alpha)| css(key).unwrap_or_else(|| opacity(&foreground, alpha)).css);
        let rule = ".xterm .xterm-scrollable-element > .xterm-scrollbar > .xterm-slider";
        let text = format!(
            "{rule} {{\n  background: {normal};\n}}\n{rule}:hover {{\n  background: {hover};\n}}\n\
             {rule}.xterm-active {{\n  background: {active};\n}}"
        );
        if inner.style.is_none() {
            let Some(document) = inner.root.owner_document() else {
                return;
            };
            let Ok(style) = document.create_element("style") else {
                return;
            };
            let _ = inner.root.append_child(&style);
            inner.style = Some(style);
        }
        if let Some(style) = &inner.style {
            style.set_text_content(Some(&text));
        }
    }
}

impl Drop for Scrollbar {
    fn drop(&mut self) {
        let Ok(mut inner) = self.inner.try_borrow_mut() else {
            return;
        };
        inner.stop_drag(false);
        for (target, kind) in LISTENERS {
            let _ = inner
                .target(target)
                .remove_event_listener_with_callback(kind, &inner.listener);
        }
        inner.reveal_timer = None;
        inner.hide_timer = None;
        // The screen goes back where the wrapper was.
        let _ = inner.root.replace_with_with_node_1(&inner.screen);
    }
}

impl Inner {
    fn target(&self, target: Target) -> &HtmlElement {
        match target {
            Target::Root => &self.root,
            Target::Bar => &self.bar,
        }
    }

    fn handle(&mut self, event: &Event) {
        let target = event.target();
        let is = |element: &HtmlElement| target.as_ref() == Some(element.unchecked_ref());
        match event.type_().as_str() {
            "mouseover" => {
                self.mouse_over = true;
                self.reveal();
            }
            "mouseleave" => {
                self.mouse_over = false;
                self.hide();
            }
            "pointerdown" => {
                let Some(e) = event.dyn_ref::<PointerEvent>() else {
                    return;
                };
                if is(&self.slider) {
                    if e.button() == 0 {
                        e.prevent_default();
                        self.start_drag(e);
                    }
                } else if is(&self.bar) {
                    // A press on the track centres the slider there, then drags it (`_handlePointerDown`).
                    let offset = f64::from(e.offset_y());
                    self.set_scroll_now(self.state.desired_from_offset(offset));
                    if e.button() == 0 {
                        e.prevent_default();
                        self.start_drag(e);
                    }
                }
            }
            "click" => {
                if is(&self.slider)
                    && event.dyn_ref::<MouseEvent>().map(MouseEvent::button) == Some(0)
                {
                    event.stop_propagation();
                }
            }
            "pointermove" => {
                let Some(e) = event.dyn_ref::<PointerEvent>() else {
                    return;
                };
                let Some(drag) = &self.drag else { return };
                if e.buttons() != drag.buttons {
                    self.stop_drag(true);
                    return;
                }
                e.prevent_default();
                let desired = drag
                    .state
                    .desired_from_delta(f64::from(e.page_y()) - drag.start_y);
                self.set_scroll_now(desired);
            }
            "pointerup" => self.stop_drag(true),
            _ => {}
        }
    }

    /// `_sliderPointerDown` with `GlobalPointerMoveMonitor`: moves go to the captured target (else the window).
    fn start_drag(&mut self, e: &PointerEvent) {
        let Some(element) = e.target().and_then(|t| t.dyn_into::<Element>().ok()) else {
            return;
        };
        self.stop_drag(false);
        let target: EventTarget = match element.set_pointer_capture(e.pointer_id()) {
            Ok(()) => element.into(),
            Err(_) => match web_sys::window() {
                Some(window) => window.into(),
                None => return,
            },
        };
        for kind in ["pointermove", "pointerup"] {
            let _ = target.add_event_listener_with_callback(kind, &self.listener);
        }
        let _ = self.slider.class_list().add_1("xterm-active");
        self.drag = Some(Drag {
            target,
            pointer_id: e.pointer_id(),
            buttons: e.buttons(),
            start_y: f64::from(e.page_y()),
            state: self.state,
        });
        // `_handleDragStart`.
        self.reveal();
    }

    fn stop_drag(&mut self, notify: bool) {
        let Some(drag) = self.drag.take() else { return };
        for kind in ["pointermove", "pointerup"] {
            let _ = drag
                .target
                .remove_event_listener_with_callback(kind, &self.listener);
        }
        if let Some(element) = drag.target.dyn_ref::<Element>() {
            let _ = element.release_pointer_capture(drag.pointer_id);
        }
        if notify {
            let _ = self.slider.class_list().remove_1("xterm-active");
            // `_handleDragEnd`.
            self.hide();
        }
    }

    /// `Scrollable.setScrollPositionNow` from the bar, then `Viewport._handleScroll`: the view follows the row
    /// nearest the new position on the next frame.
    fn set_scroll_now(&mut self, top: f64) {
        if !top.is_finite() {
            return;
        }
        if !self.set_scroll(Scroll::new(
            self.scroll.height,
            self.scroll.scroll_height,
            top,
        )) {
            return;
        }
        if self.cell_height <= 0.0 {
            return;
        }
        let row = js_round(self.scroll.top / self.cell_height).max(0.0) as u32;
        if row != self.request.unwrap_or(self.ydisp) {
            self.latest_ydisp = Some(row);
            self.request = Some(row);
            if let Some(shared) = self.wake.upgrade() {
                shared.schedule_frame();
            }
        }
    }

    /// `Scrollable._setState` and `SmoothScrollableElement._handleScroll`; false when nothing changed.
    fn set_scroll(&mut self, scroll: Scroll) -> bool {
        if scroll == self.scroll {
            return false;
        }
        self.scroll = scroll;
        for changed in [
            self.state.set_scroll_size(scroll.scroll_height),
            self.state.set_scroll_position(scroll.top),
            self.state.set_visible_size(scroll.height),
        ] {
            if changed {
                self.set_is_needed(self.state.is_needed);
                self.should_render = true;
            }
        }
        self.reveal();
        self.render();
        true
    }

    /// `Viewport._sync`: dimensions from the renderer and the buffer, then the position unless a drag set it.
    fn sync(&mut self, viewport: &Viewport, dimensions: &Dimensions) {
        let alternate = viewport.modes & mode::ALT_SCREEN != 0;
        let switched = alternate != self.alternate;
        if switched {
            self.alternate = alternate;
            self.latest_ydisp = None;
        }
        let (lines, ydisp) = if alternate {
            (viewport.rows, 0)
        } else {
            (
                viewport.history_length + viewport.rows,
                viewport
                    .history_length
                    .saturating_sub(viewport.display_offset),
            )
        };
        let cell_height = dimensions.css_cell.height;
        if cell_height <= 0.0 {
            return;
        }
        let size = (viewport.cols, viewport.rows);
        if !self.synced {
            let first = *self.first_size.get_or_insert(size);
            if !switched && lines == viewport.rows && first == size {
                return;
            }
            self.synced = true;
        }
        self.cell_height = cell_height;
        self.ydisp = ydisp;
        let scroll_height = cell_height * f64::from(lines);
        let dims = Scroll::new(dimensions.css_canvas.height, scroll_height, self.scroll.top);
        self.set_scroll(dims);
        if Some(ydisp) != self.latest_ydisp {
            let top = f64::from(ydisp) * cell_height;
            self.set_scroll(Scroll::new(
                self.scroll.height,
                self.scroll.scroll_height,
                top,
            ));
        }
    }

    /// `AbstractScrollbar.render` with `VerticalScrollbar._renderDomNode` and `_updateSlider`.
    fn render(&mut self) {
        if !self.should_render {
            return;
        }
        self.should_render = false;
        let bar = self.bar.style();
        let _ = bar.set_property("width", &px(self.state.scrollbar_size));
        let _ = bar.set_property("height", &px(self.state.available_size));
        let _ = bar.set_property("right", "0px");
        let _ = bar.set_property("top", "0px");
        let slider = self.slider.style();
        let _ = slider.set_property("top", &px(self.state.slider_position));
        let _ = slider.set_property("height", &px(self.state.slider_size));
    }

    /// `SmoothScrollableElement._reveal`.
    fn reveal(&mut self) {
        self.set_should_be_visible(true);
        if !self.mouse_over && self.drag.is_none() {
            let this = self.this.clone();
            self.hide_timer = Some(Timeout::new(HIDE_TIMEOUT_MS, move || {
                with_inner(&this, |inner| {
                    inner.hide_timer = None;
                    inner.hide();
                });
            }));
        }
    }

    /// `SmoothScrollableElement._hide`.
    fn hide(&mut self) {
        if !self.mouse_over && self.drag.is_none() {
            self.set_should_be_visible(false);
        }
    }

    fn set_should_be_visible(&mut self, visible: bool) {
        if self.should_be_visible != visible {
            self.should_be_visible = visible;
            self.ensure_visibility();
        }
    }

    fn set_is_needed(&mut self, needed: bool) {
        if self.is_needed != needed {
            self.is_needed = needed;
            self.ensure_visibility();
        }
    }

    /// `ScrollbarVisibilityController.ensureVisibility`; the class changes a task later when revealed. Hidden, it
    /// should never be visible (`_applyVisibilitySetting`).
    fn ensure_visibility(&mut self) {
        if !self.is_needed {
            self.hide_bar(false);
        } else if !self.should_be_visible || self.hidden {
            self.hide_bar(true);
        } else if !self.is_visible {
            self.is_visible = true;
            if self.reveal_timer.is_none() {
                let this = self.this.clone();
                self.reveal_timer = Some(Timeout::new(0, move || {
                    with_inner(&this, |inner| {
                        inner.reveal_timer = None;
                        inner.bar.set_class_name(VISIBLE);
                    });
                }));
            }
        }
    }

    fn hide_bar(&mut self, fade: bool) {
        self.reveal_timer = None;
        if !self.is_visible {
            return;
        }
        self.is_visible = false;
        self.bar.set_class_name(if fade {
            "xterm-invisible xterm-scrollbar xterm-vertical xterm-fade"
        } else {
            INVISIBLE
        });
    }
}

fn with_inner(weak: &Weak<RefCell<Inner>>, f: impl FnOnce(&mut Inner)) {
    if let Some(inner) = weak.upgrade() {
        if let Ok(mut inner) = inner.try_borrow_mut() {
            f(&mut inner);
        }
    }
}

fn px(value: f64) -> String {
    format!("{value}px")
}

/// xterm.js `isMac`: the wrapper gets `xterm-mac` there.
fn is_mac() -> bool {
    let platform = get(&get(&js_sys::global(), "navigator"), "platform").as_string();
    matches!(
        platform.as_deref(),
        Some("Macintosh" | "MacIntel" | "MacPPC" | "Mac68K")
    )
}

impl Shared {
    /// Builds or resizes the scrollbar for `options.scrollbar` (`{ showScrollbar, width }`); `showScrollbar: false`
    /// hides it and makes it 0 px wide (`Viewport._getChangeOptions`).
    pub(crate) fn apply_scrollbar(self: &Rc<Self>) {
        let mut core = self.core.borrow_mut();
        let Some(screen) = core.screen.clone() else {
            return;
        };
        let scrollbar = option(&core.options, "scrollbar");
        let show = get(&scrollbar, "showScrollbar").as_bool().unwrap_or(true);
        let width = if show {
            get(&scrollbar, "width").as_f64().unwrap_or(DEFAULT_WIDTH)
        } else {
            0.0
        };
        let created = match &core.scrollbar {
            None => {
                let theme = get(&core.options, "theme");
                if let Ok(bar) = Scrollbar::new(self, &screen, width) {
                    bar.set_theme(&theme);
                    bar.set_hidden(!show);
                    core.scrollbar = Some(bar);
                }
                true
            }
            Some(bar) => {
                bar.set_width(width);
                bar.set_hidden(!show);
                false
            }
        };
        drop(core);
        self.apply_background();
        if created {
            self.schedule_frame();
        }
    }

    pub(crate) fn apply_scrollbar_theme(&self) {
        {
            let core = self.core.borrow();
            if let Some(bar) = &core.scrollbar {
                bar.set_theme(&get(&core.options, "theme"));
            }
        }
        self.apply_background();
    }

    /// `Viewport`'s theme listener: the terminal element and the scrollable element take the theme background, so
    /// the strip beside the canvas (the scrollbar's) shows it rather than the page behind.
    fn apply_background(&self) {
        let core = self.core.borrow();
        let css = get(&get(&core.options, "theme"), "background")
            .as_string()
            .and_then(|c| css_to_color(&c))
            .map_or_else(|| "#000000".to_owned(), |c| c.css);
        if let Some(element) = &core.element {
            let _ = element.style().set_property("background-color", &css);
        }
        if let Some(bar) = &core.scrollbar {
            if let Ok(inner) = bar.inner.try_borrow() {
                let _ = inner.root.style().set_property("background-color", &css);
            }
        }
    }

    /// Scrolls the view to what the scrollbar and the accessibility tree asked for since the last frame.
    pub(crate) fn apply_scroll_requests(self: &Rc<Self>) {
        let scrolled = {
            let core = self.core.borrow();
            if core.disposed {
                return;
            }
            let row = core.scrollbar.as_ref().and_then(Scrollbar::take_request);
            let mut delta = core
                .a11y
                .as_ref()
                .map_or(0, |a11y| i64::from(a11y.take_scroll()));
            let e = core.view.engine();
            if let Some(row) = row {
                delta += i64::from(row)
                    - (i64::from(e.history_length()) - i64::from(e.display_offset()));
            }
            delta != 0 && e.scroll_display(-(delta as i32)).is_ok()
        };
        if scrolled {
            self.after_change();
        }
    }
}
