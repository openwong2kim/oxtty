//! Links: OSC 8 hyperlinks, `registerLinkProvider` and the built-in `WebLinksAddon` (`@xterm/addon-web-links`),
//! with the hover underline (the renderer's `"link"` highlight layer), the pointer cursor and activation on click.
//!
//! Ported from xterm.js @ 08829c7eb6279dda2a9e86df901353a1a89858ca (npm `@xterm/xterm` 6.1.0-beta.336 and
//! `@xterm/addon-web-links` 0.13.0-beta.330), MIT license (`crates/wongtty/LICENSE-XTERM`):
//! - `src/browser/Linkifier.ts` (Copyright (c) 2019 The xterm.js authors) lines 14-415: hovering, provider
//!   priority, overlapping links, activation, the underline and the pointer cursor;
//! - `src/browser/OscLinkProvider.ts` (Copyright (c) 2022 The xterm.js authors) lines 12-197, with its
//!   `defaultActivate`;
//! - `src/browser/services/LinkProviderService.ts` lines 5-27 and `src/browser/CoreBrowserTerminal.ts` lines 182
//!   and 747-749 (`registerLinkProvider`, the OSC 8 provider first);
//! - `addons/addon-web-links/src/WebLinksAddon.ts` lines 9-58 and `WebLinkProvider.ts` lines 14-200 (Copyright (c)
//!   2019 The xterm.js authors): URLs found with the addon's regular expression, a JS `RegExp` (no matcher in the
//!   wasm), checked with `URL`.
//!
//! Lines come from `buffer.active` like the addon reads them; OSC 8 links come from the last frame's cell extras
//! (`Viewport`, PRD D7) for rows in view and from the buffer API for the rows a wrapped link reaches beyond them. Unlike xterm.js, the `decorations` of a hovered link are read once
//! (later changes to them are not followed), a provider's late reply to an earlier hover is dropped, and a
//! `urlRegex` that matches empty text moves on instead of looping forever.
//!
//! Memory: nothing until the terminal is hovered or a provider is registered; then the providers and the links of
//! the hovered line, dropped with the terminal.
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use js_sys::{Array, Function, JsString, Object, Reflect, RegExp};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Element, MouseEvent, Url};

use crate::buffer::{self, BufferLine, BufferView};
use crate::emitter::Disposable;
use crate::engine::get;
use crate::events;
use crate::input::mouse;
use crate::options::option;
use crate::render::Highlight;
use crate::terminal::{Shared, Terminal};

/// `@xterm/addon-web-links` `strictUrlRegex`: http(s) URLs up to the first whitespace or quote, without trailing
/// punctuation or brackets.
const STRICT_URL_REGEX: &str =
    r#"(https?|HTTPS?):[/]{2}[^\s"'!*(){}|\\\^<>`]*[^\s"':,.!?{}|\\\^~\[\]`()<>]"#;
/// How far `@xterm/addon-web-links` follows a wrapped line up and down, in UTF-16 units.
const WRAPPED_LINE_LIMIT: usize = 2048;
const POINTER_CLASS: &str = "xterm-cursor-pointer";

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
/** xterm.js `IViewportRange`: 1-based cells, end inclusive. */
export interface IViewportRange {
  start: IViewportRangePosition;
  end: IViewportRangePosition;
}
export interface IViewportRangePosition {
  x: number;
  y: number;
}
/** xterm.js `ILinkDecorations`. */
export interface ILinkDecorations {
  pointerCursor: boolean;
  underline: boolean;
}
/** xterm.js `ILink`: `range` in 1-based buffer cells, end inclusive. */
export interface ILink {
  range: IBufferRange;
  text: string;
  decorations?: ILinkDecorations;
  activate(event: MouseEvent, text: string): void;
  hover?(event: MouseEvent, text: string): void;
  leave?(event: MouseEvent, text: string): void;
  dispose?(): void;
}
/** xterm.js `ILinkProvider`: `bufferLineNumber` is 1-based. */
export interface ILinkProvider {
  provideLinks(bufferLineNumber: number, callback: (links: ILink[] | undefined) => void): void;
}
/** xterm.js `ILinkHandler`, the `linkHandler` option for OSC 8 links. */
export interface ILinkHandler {
  activate(event: MouseEvent, text: string, range: IBufferRange): void;
  hover?(event: MouseEvent, text: string, range: IBufferRange): void;
  leave?(event: MouseEvent, text: string, range: IBufferRange): void;
  allowNonHttpProtocols?: boolean;
}
/** `@xterm/addon-web-links` `ILinkProviderOptions`. */
export interface ILinkProviderOptions {
  hover?(event: MouseEvent, text: string, location: IViewportRange): void;
  leave?(event: MouseEvent, text: string): void;
  urlRegex?: RegExp;
}
"#;

/// A link with its 1-based range `[start x, start y, end x, end y]`, end inclusive.
#[derive(Clone)]
struct Link {
    range: [f64; 4],
    text: String,
    source: Source,
}

#[derive(Clone)]
enum Source {
    /// An `ILink` from a registered provider.
    Js(JsValue),
    /// An OSC 8 hyperlink, with `linkHandler` as it was when the line was read.
    Osc(JsValue),
    /// A URL the built-in `WebLinksAddon` found.
    Web(Rc<WebState>),
}

enum Provider {
    Js(JsValue),
    Web(Rc<WebState>),
}

/// The hovered link (xterm.js `_currentLink` and its state).
struct Current {
    link: Link,
    underline: bool,
    pointer: bool,
}

/// The providers' replies for the hovered line (xterm.js `_activeProviderReplies`).
struct Replies {
    /// Which ask the replies answer.
    generation: u32,
    position: (f64, f64),
    /// The `linkProvided` of that ask, which its replies update.
    link_provided: bool,
    /// Per provider: not replied yet, replied `undefined`, or links.
    slots: Vec<Option<Option<Vec<Link>>>>,
}

impl Replies {
    fn size(&self) -> usize {
        self.slots.iter().filter(|s| s.is_some()).count()
    }
}

/// xterm.js `Linkifier` state.
struct Hover {
    current: Option<Current>,
    mouse_down_link: Option<(String, [f64; 4])>,
    last_event: Option<MouseEvent>,
    last_cell: Option<(f64, f64)>,
    mouse_out: bool,
    was_resized: bool,
    replies: Option<Replies>,
    active_line: f64,
    generation: u32,
}

impl Default for Hover {
    fn default() -> Self {
        Self {
            current: None,
            mouse_down_link: None,
            last_event: None,
            last_cell: None,
            mouse_out: true,
            was_resized: false,
            replies: None,
            active_line: -1.0,
            generation: 0,
        }
    }
}

#[derive(Default)]
struct Inner {
    /// Registered providers by id, after the OSC 8 provider.
    providers: Vec<(u32, Provider)>,
    next_id: u32,
    hover: Hover,
}

/// A terminal's links; empty until used.
#[derive(Default)]
pub(crate) struct Links {
    inner: RefCell<Option<Box<Inner>>>,
}

impl Links {
    fn with<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> Option<T> {
        let mut inner = self.inner.try_borrow_mut().ok()?;
        Some(f(inner.get_or_insert_with(Box::default)))
    }

    /// Like `with`, without creating the state.
    fn existing<T>(&self, f: impl FnOnce(&mut Inner) -> T) -> Option<T> {
        self.inner
            .try_borrow_mut()
            .ok()?
            .as_mut()
            .map(|inner| f(inner))
    }
}

/// A provider to ask, cloned out of the state so it can be called without a borrow.
enum Ask {
    Osc,
    Js(JsValue),
    Web(Rc<WebState>),
}

fn providers(shared: &Shared) -> Vec<Ask> {
    let mut list = vec![Ask::Osc];
    shared.links.existing(|inner| {
        list.extend(inner.providers.iter().map(|(_, p)| match p {
            Provider::Js(p) => Ask::Js(p.clone()),
            Provider::Web(state) => Ask::Web(state.clone()),
        }))
    });
    list
}

fn cols(shared: &Shared) -> f64 {
    shared
        .core
        .try_borrow()
        .map_or(0.0, |core| f64::from(core.size().0))
}

/// xterm.js `_linkAtPosition`.
fn link_at(link: &Link, (x, y): (f64, f64), cols: f64) -> bool {
    let [sx, sy, ex, ey] = link.range;
    let lower = sy * cols + sx;
    let upper = ey * cols + ex;
    let current = y * cols + x;
    lower <= current && current <= upper
}

/// xterm.js `_positionFromMouseEvent`: the 1-based cell, `y` a 1-based buffer line.
fn position_from_event(shared: &Rc<Shared>, event: &MouseEvent) -> Option<(f64, f64)> {
    let (x, y) = mouse::screen_coords(shared, event, false)?;
    let ydisp = buffer::active(shared).viewport_y();
    Some((f64::from(x), f64::from(y) + f64::from(ydisp)))
}

/// The hovered link's range, for a double click that selects it.
pub(crate) fn current_link_range(shared: &Shared) -> Option<[f64; 4]> {
    shared
        .links
        .existing(|inner| inner.hover.current.as_ref().map(|c| c.link.range))
        .flatten()
}

/// The screen's mousemove (xterm.js `_handleMouseMove`).
pub(crate) fn mouse_move(shared: &Rc<Shared>, event: &MouseEvent) {
    shared
        .links
        .with(|inner| inner.hover.last_event = Some(event.clone()));
    let Some(position) = position_from_event(shared, event) else {
        return;
    };
    shared.links.with(|inner| inner.hover.mouse_out = false);
    // Ignore the event if it's an embedder created hover widget
    for target in event.composed_path().iter() {
        let Some(element) = target.dyn_ref::<Element>() else {
            continue;
        };
        let classes = element.class_list();
        // Hit Terminal.element, break and continue
        if classes.contains("xterm") {
            break;
        }
        if classes.contains("xterm-hover") {
            return;
        }
    }
    let last = shared.links.with(|inner| inner.hover.last_cell).flatten();
    if last != Some(position) {
        handle_hover(shared, position);
        shared
            .links
            .with(|inner| inner.hover.last_cell = Some(position));
    }
}

fn handle_hover(shared: &Rc<Shared>, position: (f64, f64)) {
    let (active_line, resized) = shared
        .links
        .with(|inner| (inner.hover.active_line, inner.hover.was_resized))
        .unwrap_or((-1.0, false));
    // Check if we need to clear the link
    if active_line != position.1 || resized {
        clear_current_link(shared, None);
        ask_for_link(shared, position, false);
        shared.links.with(|inner| inner.hover.was_resized = false);
        return;
    }
    // Check the if the link is in the mouse position
    let cols = cols(shared);
    let in_position = shared
        .links
        .with(|inner| {
            inner
                .hover
                .current
                .as_ref()
                .is_some_and(|c| link_at(&c.link, position, cols))
        })
        .unwrap_or(false);
    if !in_position {
        clear_current_link(shared, None);
        ask_for_link(shared, position, true);
    }
}

/// xterm.js `_askForLink`.
fn ask_for_link(shared: &Rc<Shared>, position: (f64, f64), use_line_cache: bool) {
    let asks = providers(shared);
    let started = shared
        .links
        .with(|inner| {
            let hover = &mut inner.hover;
            if hover.replies.is_some() && use_line_cache {
                return None;
            }
            let old = hover.replies.take();
            hover.generation = hover.generation.wrapping_add(1);
            hover.replies = Some(Replies {
                generation: hover.generation,
                position,
                link_provided: false,
                slots: vec![None; asks.len()],
            });
            hover.active_line = position.1;
            Some((hover.generation, old))
        })
        .flatten();
    let generation = match started {
        Some((generation, old)) => {
            // The links of the line left behind are disposed
            for link in old
                .into_iter()
                .flat_map(|r| r.slots)
                .flatten()
                .flatten()
                .flatten()
            {
                if let Source::Js(link) = link.source {
                    if let Ok(dispose) = get(&link, "dispose").dyn_into::<Function>() {
                        if let Err(error) = dispose.call0(&link) {
                            web_sys::console::error_1(&error);
                        }
                    }
                }
            }
            Some(generation)
        }
        None => None,
    };
    let mut link_provided = false;
    for (i, ask) in asks.into_iter().enumerate() {
        let Some(generation) = generation else {
            // The line's replies are cached: look at the providers that answered
            let replied = shared
                .links
                .existing(|inner| {
                    inner
                        .hover
                        .replies
                        .as_ref()
                        .is_some_and(|r| matches!(r.slots.get(i), Some(Some(Some(_)))))
                })
                .unwrap_or(false);
            if replied {
                link_provided = check_result(shared, i, position, link_provided);
            }
            continue;
        };
        let y = position.1;
        match ask {
            Ask::Osc => reply(shared, generation, i, osc_links(shared, y)),
            Ask::Web(state) => reply(shared, generation, i, Some(web_links(shared, &state, y))),
            Ask::Js(provider) => {
                let callback = provider_callback(shared, generation, i);
                let result = get(&provider, "provideLinks")
                    .dyn_into::<Function>()
                    .and_then(|f| f.call2(&provider, &y.into(), &callback));
                if let Err(error) = result {
                    web_sys::console::error_1(&error);
                }
            }
        }
    }
}

thread_local! {
    /// The callback every `provideLinks` gets, bound to the terminal, the ask and the provider. Lives as long as
    /// the page; the GC owns the closure (`into_js_value`).
    static ON_LINKS: Function = Closure::<dyn Fn(u32, u32, u32, JsValue)>::new(on_links)
        .into_js_value()
        .unchecked_into();
}

fn provider_callback(shared: &Rc<Shared>, generation: u32, index: usize) -> JsValue {
    let id = events::terminal_id(shared);
    ON_LINKS.with(|f| {
        f.bind3(
            &JsValue::NULL,
            &id.into(),
            &generation.into(),
            &(index as u32).into(),
        )
        .into()
    })
}

fn on_links(terminal: u32, generation: u32, index: u32, links: JsValue) {
    if let Some(shared) = events::terminal_of(terminal) {
        reply(&shared, generation, index as usize, js_links(&links));
    }
}

/// An `ILink[]` a provider gave; entries without a numeric range are skipped.
fn js_links(links: &JsValue) -> Option<Vec<Link>> {
    if !Array::is_array(links) {
        return None;
    }
    Some(
        Array::from(links)
            .iter()
            .filter_map(|link| {
                let range = get(&link, "range");
                let n = |point: &str, key: &str| get(&get(&range, point), key).as_f64();
                Some(Link {
                    range: [
                        n("start", "x")?,
                        n("start", "y")?,
                        n("end", "x")?,
                        n("end", "y")?,
                    ],
                    text: get(&link, "text").as_string().unwrap_or_default(),
                    source: Source::Js(link),
                })
            })
            .collect(),
    )
}

/// A provider's reply (the `provideLinks` callback).
fn reply(shared: &Rc<Shared>, generation: u32, index: usize, links: Option<Vec<Link>>) {
    let state = shared
        .links
        .existing(|inner| {
            let hover = &mut inner.hover;
            if hover.mouse_out {
                return None;
            }
            let replies = hover.replies.as_mut()?;
            if replies.generation != generation {
                return None;
            }
            *replies.slots.get_mut(index)? = Some(links);
            Some((replies.position, replies.link_provided))
        })
        .flatten();
    let Some((position, link_provided)) = state else {
        return;
    };
    let link_provided = check_result(shared, index, position, link_provided);
    let providers = providers(shared).len();
    let cols = cols(shared);
    shared.links.existing(|inner| {
        let Some(replies) = inner.hover.replies.as_mut() else {
            return;
        };
        if replies.generation != generation {
            return;
        }
        replies.link_provided = link_provided;
        // If all providers have responded, remove lower priority links that intersect ranges of higher priority
        // links
        if replies.size() == providers {
            remove_intersecting_links(replies, cols);
        }
    });
}

/// xterm.js `_removeIntersectingLinks`: kept links are inclusive `[start, end]` offsets (`y * cols + x`), and a
/// lower priority link is dropped when it overlaps one.
fn remove_intersecting_links(replies: &mut Replies, cols: f64) {
    let mut kept: Vec<(f64, f64)> = Vec::new();
    let size = replies.size();
    for slot in replies.slots.iter_mut().take(size) {
        let Some(Some(links)) = slot else {
            continue;
        };
        links.retain(|link| {
            let [sx, sy, ex, ey] = link.range;
            let (start, end) = (sy * cols + sx, ey * cols + ex);
            if start > end {
                return true;
            }
            if kept.iter().any(|&(s, e)| start <= e && s <= end) {
                return false;
            }
            kept.push((start, end));
            true
        });
    }
}

/// xterm.js `_checkLinkProviderResult`: the first provider that has a link at the position wins, once every
/// provider before it came back empty.
fn check_result(
    shared: &Rc<Shared>,
    index: usize,
    position: (f64, f64),
    mut link_provided: bool,
) -> bool {
    let cols = cols(shared);
    let found = shared
        .links
        .existing(|inner| {
            let replies = inner.hover.replies.as_ref()?;
            // Check if every provider before this one has come back undefined
            let has_link_before =
                (0..index).any(|j| !matches!(replies.slots.get(j), Some(Some(None))));
            if has_link_before {
                return None;
            }
            match replies.slots.get(index) {
                Some(Some(Some(links))) => {
                    links.iter().find(|l| link_at(l, position, cols)).cloned()
                }
                _ => None,
            }
        })
        .flatten();
    if let Some(link) = found {
        link_provided = true;
        handle_new_link(shared, link);
    }
    // Once all the providers have responded, respect their order
    let fallback = shared
        .links
        .existing(|inner| {
            let replies = inner.hover.replies.as_ref()?;
            if link_provided || replies.size() != providers_len(inner) {
                return None;
            }
            (0..replies.size()).find_map(|j| match replies.slots.get(j) {
                Some(Some(Some(links))) => {
                    links.iter().find(|l| link_at(l, position, cols)).cloned()
                }
                _ => None,
            })
        })
        .flatten();
    if let Some(link) = fallback {
        link_provided = true;
        handle_new_link(shared, link);
    }
    link_provided
}

fn providers_len(inner: &Inner) -> usize {
    inner.providers.len() + 1
}

/// xterm.js `_handleNewLink`: hovers `link` if the mouse is still on it.
fn handle_new_link(shared: &Rc<Shared>, link: Link) {
    let Some(event) = shared
        .links
        .existing(|inner| inner.hover.last_event.clone())
        .flatten()
    else {
        return;
    };
    let Some(position) = position_from_event(shared, &event) else {
        return;
    };
    if !link_at(&link, position, cols(shared)) {
        return;
    }
    let (underline, pointer) = match &link.source {
        Source::Js(object) => {
            let decorations = get(object, "decorations");
            if decorations.is_undefined() {
                (true, true)
            } else {
                (
                    get(&decorations, "underline").is_truthy(),
                    get(&decorations, "pointerCursor").is_truthy(),
                )
            }
        }
        // OSC links always use underline and pointer decorations
        _ => (true, true),
    };
    shared.links.existing(|inner| {
        inner.hover.current = Some(Current {
            link: link.clone(),
            underline,
            pointer,
        })
    });
    link_hover(shared, &link, &event, underline, pointer);
}

/// xterm.js `_linkHover`.
fn link_hover(
    shared: &Rc<Shared>,
    link: &Link,
    event: &MouseEvent,
    underline: bool,
    pointer: bool,
) {
    if underline {
        show_underline(shared, Some(link.range));
    }
    if pointer {
        if let Some(screen) = mouse::screen(shared) {
            let _ = screen.class_list().add_1(POINTER_CLASS);
        }
    }
    call_link(link, "hover", event);
}

/// xterm.js `_linkLeave`.
fn link_leave(shared: &Rc<Shared>, current: &Current, event: &MouseEvent) {
    if current.underline {
        show_underline(shared, None);
    }
    if current.pointer {
        if let Some(screen) = mouse::screen(shared) {
            let _ = screen.class_list().remove_1(POINTER_CLASS);
        }
    }
    call_link(&current.link, "leave", event);
}

/// Calls a link's `activate`, `hover` or `leave` the way its provider defines them.
fn call_link(link: &Link, method: &str, event: &MouseEvent) {
    let text: JsValue = link.text.as_str().into();
    let result = match &link.source {
        Source::Js(object) => match get(object, method).dyn_into::<Function>() {
            Ok(f) => f.call2(object, event, &text),
            Err(_) => Ok(JsValue::UNDEFINED),
        },
        Source::Osc(handler) => {
            if method == "activate" && !handler.is_truthy() {
                default_activate(&link.text);
                Ok(JsValue::UNDEFINED)
            } else {
                match get(handler, method).dyn_into::<Function>() {
                    Ok(f) => {
                        let range = range_object(link.range);
                        f.call3(handler, event, &text, &range)
                    }
                    Err(_) => Ok(JsValue::UNDEFINED),
                }
            }
        }
        Source::Web(state) => match method {
            "activate" => match &state.handler {
                Some(handler) => handler.call2(&JsValue::UNDEFINED, event, &text),
                None => {
                    open_link(&link.text);
                    Ok(JsValue::UNDEFINED)
                }
            },
            "hover" => match &state.hover {
                Some(hover) => hover.call3(&state.options, event, &text, &range_object(link.range)),
                None => Ok(JsValue::UNDEFINED),
            },
            _ => match &state.leave {
                Some(leave) => leave.call2(&JsValue::UNDEFINED, event, &text),
                None => Ok(JsValue::UNDEFINED),
            },
        },
    };
    if let Err(error) = result {
        web_sys::console::error_1(&error);
    }
}

/// `{ start: { x, y }, end: { x, y } }`.
fn range_object([sx, sy, ex, ey]: [f64; 4]) -> JsValue {
    let point = |x: f64, y: f64| {
        let o = Object::new();
        let _ = Reflect::set(&o, &"x".into(), &x.into());
        let _ = Reflect::set(&o, &"y".into(), &y.into());
        o
    };
    let range = Object::new();
    let _ = Reflect::set(&range, &"start".into(), &point(sx, sy));
    let _ = Reflect::set(&range, &"end".into(), &point(ex, ey));
    range.into()
}

/// The underline of the hovered link in viewport cells (xterm.js `_fireUnderlineEvent` and the WebGL addon's
/// `LinkRenderLayer`), or none.
fn show_underline(shared: &Rc<Shared>, range: Option<[f64; 4]>) {
    let ydisp = f64::from(buffer::active(shared).viewport_y());
    let Ok(mut core) = shared.core.try_borrow_mut() else {
        return;
    };
    let (cols, rows) = core.size();
    let Some(renderer) = core.renderer.as_mut() else {
        return;
    };
    let mut highlights = Vec::new();
    if let Some([sx, sy, ex, ey]) = range {
        let (x1, y1, x2, y2) = (sx - 1.0, sy - ydisp - 1.0, ex, ey - ydisp - 1.0);
        let cols = f64::from(cols);
        let mut add = |row: f64, start: f64, end: f64| {
            if row >= 0.0 && row < f64::from(rows) && start < end {
                highlights.push(Highlight {
                    row: row as u32,
                    start: start.max(0.0) as u32,
                    end: end.min(cols) as u32,
                    underline: true,
                    ..Highlight::default()
                });
            }
        };
        if y1 == y2 {
            add(y1, x1, x2);
        } else {
            add(y1, x1, cols);
            let mut y = y1 + 1.0;
            while y < y2 && y < f64::from(rows) {
                add(y, 0.0, cols);
                y += 1.0;
            }
            add(y2, 0.0, x2);
        }
    }
    renderer.set_highlights("link", highlights);
    drop(core);
    shared.schedule_frame();
}

/// xterm.js `_clearCurrentLink`: lets go of the hovered link, only if it is within `rows` when given (a row of 0
/// counts as not given, like xterm.js).
fn clear_current_link(shared: &Rc<Shared>, rows: Option<(f64, f64)>) {
    let taken = shared
        .links
        .existing(|inner| {
            let hover = &mut inner.hover;
            let event = hover.last_event.clone()?;
            let current = hover.current.as_ref()?;
            if let Some((start, end)) = rows {
                let [_, sy, _, ey] = current.link.range;
                if start != 0.0 && end != 0.0 && !(sy >= start && ey <= end) {
                    return None;
                }
            }
            Some((hover.current.take()?, event))
        })
        .flatten();
    if let Some((current, event)) = taken {
        link_leave(shared, &current, &event);
    }
}

/// The screen's mousedown: the link it started on.
pub(crate) fn mouse_down(shared: &Rc<Shared>) {
    shared.links.existing(|inner| {
        inner.hover.mouse_down_link = inner
            .hover
            .current
            .as_ref()
            .map(|c| (c.link.text.clone(), c.link.range));
    });
}

/// The screen's mouseup: activates the hovered link when the press started on it and the mouse is still on it.
pub(crate) fn mouse_up(shared: &Rc<Shared>, event: &MouseEvent) {
    let Some(current) = shared
        .links
        .existing(|inner| {
            let hover = &inner.hover;
            let current = hover.current.as_ref()?;
            let same = hover.mouse_down_link.as_ref().is_some_and(|(text, range)| {
                *text == current.link.text && *range == current.link.range
            });
            same.then(|| current.link.clone())
        })
        .flatten()
    else {
        return;
    };
    let Some(position) = position_from_event(shared, event) else {
        return;
    };
    if link_at(&current, position, cols(shared)) {
        call_link(&current, "activate", event);
    }
}

pub(crate) fn mouse_leave(shared: &Rc<Shared>) {
    if shared
        .links
        .existing(|inner| inner.hover.mouse_out = true)
        .is_some()
    {
        clear_current_link(shared, None);
    }
}

/// A resize (or a restarted engine) lets go of the hovered link, and the next hover asks again.
pub(crate) fn after_resize(shared: &Rc<Shared>) {
    if shared.links.existing(|_| ()).is_none() {
        return;
    }
    clear_current_link(shared, None);
    shared
        .links
        .existing(|inner| inner.hover.was_resized = true);
}

/// Output changed viewport rows `start..=end`: a hovered link on them is let go and looked up again (xterm.js
/// `onRenderedViewportChange` while a link is hovered).
pub(crate) fn after_render(shared: &Rc<Shared>, start: u32, end: u32) {
    let hovered = shared
        .links
        .existing(|inner| inner.hover.current.as_ref().map(|c| c.link.range))
        .flatten();
    let Some([_, sy, _, ey]) = hovered else {
        return;
    };
    let ydisp = f64::from(buffer::active(shared).viewport_y());
    // When start is 0 a scroll most likely occurred, make sure links above the fold also get cleared.
    let start = if start == 0 {
        0.0
    } else {
        f64::from(start) + 1.0 + ydisp
    };
    let end = ydisp + 1.0 + f64::from(end);
    if sy >= start && ey <= end {
        clear_current_link(shared, Some((start, end)));
        let event = shared
            .links
            .existing(|inner| inner.hover.last_event.clone())
            .flatten();
        if let Some(position) = event.and_then(|e| position_from_event(shared, &e)) {
            ask_for_link(shared, position, false);
        }
    }
}

pub(crate) fn dispose(shared: &Shared) {
    let inner = shared
        .links
        .inner
        .try_borrow_mut()
        .ok()
        .and_then(|mut i| i.take());
    drop(inner);
}

/// xterm.js `defaultActivate` of OSC 8 links: asks first.
fn default_activate(uri: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let mut message = String::from("Do you want to navigate to ");
    message.push_str(uri);
    message.push_str("?\n\nWARNING: This link could potentially be dangerous");
    if window.confirm_with_message(&message).unwrap_or(false) {
        open_link(uri);
    }
}

/// Opens `uri` in a new window without an opener (`@xterm/addon-web-links` `handleLink`).
fn open_link(uri: &str) {
    let opened = web_sys::window().and_then(|w| w.open().ok().flatten());
    match opened {
        Some(new_window) => {
            // Electron can throw
            let _ = Reflect::set(&new_window, &"opener".into(), &JsValue::NULL);
            let _ = Reflect::set(&get(&new_window, "location"), &"href".into(), &uri.into());
        }
        None => {
            web_sys::console::warn_1(&"Opening link blocked as opener could not be cleared".into())
        }
    }
}

/// A line of `buffer.active` with its OSC 8 links by column.
struct OscLine {
    line: BufferLine,
    /// `(column, id, uri)` of the cells with a hyperlink.
    links: Vec<(u32, String, String)>,
}

impl OscLine {
    /// Buffer line `y` (0-based): its hyperlinks from the last frame while it is in view, else from the buffer API.
    fn get(shared: &Shared, view: &BufferView, y: f64) -> Option<OscLine> {
        let line = view.get_line(y)?;
        let visible = shared.core.try_borrow().ok().and_then(|core| {
            let v = &core.viewport;
            let row = y - (f64::from(v.history_length) - f64::from(v.display_offset));
            (row >= 0.0 && row < f64::from(v.rows)).then(|| {
                v.extras
                    .get(&(row as u32))
                    .into_iter()
                    .flatten()
                    .filter_map(|extra| {
                        let link = extra.hyperlink.as_ref()?;
                        Some((extra.col, link.id.clone(), link.uri.clone()))
                    })
                    .collect()
            })
        });
        let links = visible.unwrap_or_else(|| view.hyperlinks(y));
        Some(OscLine { line, links })
    }

    fn has_content(&self, x: u32) -> bool {
        self.line
            .get_cell(x.into(), None)
            .is_some_and(|c| !c.get_chars().is_empty())
    }

    /// xterm.js `getTrimmedLength`.
    fn trimmed_length(&self) -> u32 {
        (0..self.line.length())
            .rev()
            .find(|&x| self.has_content(x))
            .map_or(0, |x| {
                x + self.line.get_cell(x.into(), None).map_or(1, |c| c.width())
            })
    }

    /// The link of cell `x` (xterm.js `urlId`): its index in `links`. The right half of a wide character carries
    /// the link of its left half in the engine, as in xterm.js.
    fn url_id(&self, x: u32) -> Option<usize> {
        self.links.iter().position(|(col, _, _)| *col == x)
    }

    fn same_link(&self, x: u32, other: &OscLine, id: usize) -> bool {
        let Some(mine) = self.url_id(x) else {
            return false;
        };
        let (_, a, b) = &self.links[mine];
        let (_, c, d) = &other.links[id];
        a == c && b == d
    }
}

/// xterm.js `OscLinkProvider.provideLinks`: `y` is a 1-based buffer line.
fn osc_links(shared: &Rc<Shared>, y: f64) -> Option<Vec<Link>> {
    let view = buffer::active(shared);
    let line = OscLine::get(shared, &view, y - 1.0)?;
    let handler = shared
        .core
        .try_borrow()
        .map_or(JsValue::NULL, |core| option(&core.options, "linkHandler"));
    let allow_non_http = get(&handler, "allowNonHttpProtocols").is_truthy();
    let mut result = Vec::new();
    let length = line.trimmed_length();
    let mut current: Option<usize> = None;
    let mut start: Option<u32> = None;
    let mut finish = false;
    for x in 0..length {
        // Only check for content if there isn't a link, in case the link ends with a null cell
        if start.is_none() && !line.has_content(x) {
            continue;
        }
        let id = line.url_id(x);
        if let Some(id) = id {
            match (start, current) {
                (None, _) => {
                    start = Some(x);
                    current = Some(id);
                    continue;
                }
                (Some(_), Some(current)) => finish = !line.same_link(x, &line, current),
                (Some(_), None) => finish = true,
            }
        } else if start.is_some() {
            finish = true;
        }
        if finish || (start.is_some() && x == length - 1) {
            if let (Some(link_start), Some(link)) = (start, current) {
                let (_, _, uri) = &line.links[link];
                if !uri.is_empty() {
                    let end = x + u32::from(!finish && x == length - 1);
                    let range =
                        range_with_line_wrap(shared, &view, &line, y, link_start, end, link);
                    if allow_non_http || is_http(uri) {
                        result.push(Link {
                            range,
                            text: uri.clone(),
                            source: Source::Osc(handler.clone()),
                        });
                    }
                }
            }
            finish = false;
            // Clear link or start a new link if one starts immediately
            if id.is_some() {
                start = Some(x);
                current = id;
            } else {
                start = None;
                current = None;
            }
        }
    }
    Some(result)
}

/// Only `http:` and `https:` links, unless `linkHandler.allowNonHttpProtocols`; text that is not a URL is ignored.
fn is_http(uri: &str) -> bool {
    Url::new(uri).is_ok_and(|url| matches!(url.protocol().as_str(), "http:" | "https:"))
}

/// xterm.js `_getRangeWithLineWrap`: a link segment that starts a wrapped row or ends one grows over the same link
/// on the rows around it.
fn range_with_line_wrap(
    shared: &Shared,
    view: &BufferView,
    line: &OscLine,
    y: f64,
    start_x: u32,
    end_x: u32,
    link: usize,
) -> [f64; 4] {
    let get_line = |y: f64| OscLine::get(shared, view, y);
    let (mut start_y, mut final_start_x, mut end_y, mut final_end_x) = (y, start_x, y, end_x);
    // Expand upward only when this segment starts at column 0 and the current line is wrapped
    while final_start_x == 0 {
        if !get_line(start_y - 1.0).is_some_and(|l| l.line.is_wrapped()) {
            break;
        }
        let Some(previous) = get_line(start_y - 2.0) else {
            break;
        };
        let length = previous.trimmed_length();
        if length == 0 || !previous.same_link(length - 1, line, link) {
            break;
        }
        let mut previous_start = length - 1;
        while previous_start > 0 && previous.same_link(previous_start - 1, line, link) {
            previous_start -= 1;
        }
        start_y -= 1.0;
        final_start_x = previous_start;
    }
    // Expand downward only when this segment reaches the trimmed end of the line and the next line is wrapped
    while let Some(current) = get_line(end_y - 1.0) {
        if final_end_x != current.trimmed_length() {
            break;
        }
        let Some(next) = get_line(end_y).filter(|l| l.line.is_wrapped()) else {
            break;
        };
        let length = next.trimmed_length();
        if length == 0 || !next.same_link(0, line, link) {
            break;
        }
        let mut next_end = 1;
        while next_end < length && next.same_link(next_end, line, link) {
            next_end += 1;
        }
        end_y += 1.0;
        final_end_x = next_end;
    }
    [
        f64::from(final_start_x) + 1.0,
        start_y,
        f64::from(final_end_x),
        end_y,
    ]
}

/// The built-in `WebLinksAddon`'s handler and options.
struct WebState {
    id: u32,
    /// `None` opens the link in a new window.
    handler: Option<Function>,
    options: JsValue,
    hover: Option<Function>,
    leave: Option<Function>,
    regex: Option<RegExp>,
    /// The terminal it was loaded into and its provider id there.
    registration: RefCell<Option<(Weak<Shared>, u32)>>,
}

/// `@xterm/addon-web-links` `LinkComputer.computeLink`: `y` is a 1-based buffer line.
fn web_links(shared: &Rc<Shared>, state: &Rc<WebState>, y: f64) -> Vec<Link> {
    let (source, flags) = match &state.regex {
        Some(regex) => (regex.source(), String::from(regex.flags())),
        None => (JsString::from(STRICT_URL_REGEX), String::new()),
    };
    let flags = if flags.contains('g') {
        flags
    } else {
        flags + "g"
    };
    let source = String::from(source);
    let regex = RegExp::new(&source, &flags);
    let view = buffer::active(shared);
    let (lines, start_line) = windowed_line_strings(&view, y - 1.0);
    let line: String = lines.concat();
    let mut result = Vec::new();
    while let Some(found) = regex.exec(&line) {
        let text = found.get(0).as_string().unwrap_or_default();
        let index = get(&found, "index").as_f64().unwrap_or(0.0) as i64;
        let length = text.encode_utf16().count() as i64;
        if length == 0 {
            // An empty match would be found again forever
            regex.set_last_index(regex.last_index() + 1);
        }
        // check via URL if the matched text would form a proper url
        if !is_url(&text) {
            continue;
        }
        // map string positions back to buffer positions; values are 0-based right side excluding
        let (start_y, start_x) = map_string_index(&view, start_line, 0, index);
        let (end_y, end_x) = map_string_index(&view, start_y, start_x, length);
        if start_y == -1 || start_x == -1 || end_y == -1 || end_x == -1 {
            continue;
        }
        // range expects values 1-based right side including, thus +1 except for endX
        result.push(Link {
            range: [
                start_x as f64 + 1.0,
                start_y as f64 + 1.0,
                end_x as f64,
                end_y as f64 + 1.0,
            ],
            text,
            source: Source::Web(state.clone()),
        });
    }
    result
}

/// `@xterm/addon-web-links` `isUrl`: `URL` parses the text and it starts with what `URL` makes of its origin.
fn is_url(text: &str) -> bool {
    let Ok(url) = Url::new(text) else {
        return false;
    };
    let (username, password) = (url.username(), url.password());
    let mut base = url.protocol();
    base.push_str("//");
    if !username.is_empty() {
        base.push_str(&username);
        if !password.is_empty() {
            base.push(':');
            base.push_str(&password);
        }
        base.push('@');
    }
    base.push_str(&url.host());
    let lower = |s: &str| JsString::from(s).to_locale_lower_case(None);
    lower(text).starts_with(&String::from(lower(&base)), 0)
}

/// `@xterm/addon-web-links` `_getWindowedLineStrings`: the line with the wrapped lines around it, up and down until
/// a line with whitespace or past 2048 units; returns the lines and the index of the first.
fn windowed_line_strings(view: &BufferView, index: f64) -> (Vec<String>, i64) {
    let mut lines = Vec::new();
    let mut top = index as i64;
    let mut bottom = index as i64;
    let Some(line) = view.get_line(index) else {
        return (lines, top);
    };
    let current = line.translate_to_string(Some(true), None, None);
    // expand top, stop on whitespaces or length > 2048
    if line.is_wrapped() && !current.starts_with(' ') {
        let mut length = 0;
        let mut above = Vec::new();
        loop {
            top -= 1;
            let Some(line) = view.get_line(top as f64) else {
                break;
            };
            if length >= WRAPPED_LINE_LIMIT {
                break;
            }
            let content = line.translate_to_string(Some(true), None, None);
            length += content.encode_utf16().count();
            let stop = !line.is_wrapped() || content.contains(' ');
            above.push(content);
            if stop {
                break;
            }
        }
        above.reverse();
        lines.extend(above);
    }
    lines.push(current);
    // expand bottom, stop on whitespaces or length > 2048
    let mut length = 0;
    loop {
        bottom += 1;
        let Some(line) = view.get_line(bottom as f64) else {
            break;
        };
        if !line.is_wrapped() || length >= WRAPPED_LINE_LIMIT {
            break;
        }
        let content = line.translate_to_string(Some(true), None, None);
        length += content.encode_utf16().count();
        let stop = content.contains(' ');
        lines.push(content);
        if stop {
            break;
        }
    }
    (lines, top)
}

/// `@xterm/addon-web-links` `_mapStrIdx`: string index → `(line, column)`, 0-based, or `(-1, -1)` past the buffer.
fn map_string_index(
    view: &BufferView,
    mut line_index: i64,
    column: i64,
    mut index: i64,
) -> (i64, i64) {
    let mut start = column;
    while index != 0 {
        let Some(line) = view.get_line(line_index as f64) else {
            return (-1, -1);
        };
        let length = i64::from(line.length());
        for i in start.max(0)..length {
            let Some(cell) = line.get_cell(i as f64, None) else {
                continue;
            };
            let chars = cell.get_chars();
            if cell.width() != 0 {
                index -= (chars.encode_utf16().count() as i64).max(1);
                // correct the index for wide characters moved early to the next row: the last cell is empty and
                // the wrapped next row starts with a wide character
                if i == length - 1 && chars.is_empty() {
                    if let Some(next) = view.get_line((line_index + 1) as f64) {
                        if next.is_wrapped()
                            && next.get_cell(0.0, None).is_some_and(|c| c.width() == 2)
                        {
                            index += 1;
                        }
                    }
                }
            }
            if index < 0 {
                return (line_index, i);
            }
        }
        line_index += 1;
        start = 0;
    }
    (line_index, start)
}

thread_local! {
    /// For `loadAddon`, which gets the JS object: the addon is found by the id it carries.
    static ADDONS: RefCell<Vec<Weak<WebState>>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: Cell<u32> = const { Cell::new(1) };
}

/// `@xterm/addon-web-links`, built in: load it with `terminal.loadAddon(new WebLinksAddon(handler?, options?))`.
#[wasm_bindgen]
pub struct WebLinksAddon {
    state: Rc<WebState>,
}

#[wasm_bindgen]
impl WebLinksAddon {
    /// `handler` opens a link (a new window without an opener by default); `options.hover`/`leave` follow the mouse
    /// and `options.urlRegex` replaces the URL pattern.
    #[wasm_bindgen(constructor)]
    pub fn new(
        #[wasm_bindgen(unchecked_optional_param_type = "(event: MouseEvent, uri: string) => void")]
        handler: JsValue,
        #[wasm_bindgen(unchecked_optional_param_type = "ILinkProviderOptions")] options: JsValue,
    ) -> WebLinksAddon {
        let function = |key: &str| get(&options, key).dyn_into::<Function>().ok();
        let state = Rc::new(WebState {
            id: NEXT_ID.with(|n| n.replace(n.get().wrapping_add(1))),
            handler: handler.dyn_into().ok(),
            hover: function("hover"),
            leave: function("leave"),
            regex: get(&options, "urlRegex").dyn_into().ok(),
            options,
            registration: RefCell::new(None),
        });
        ADDONS.with(|a| a.borrow_mut().push(Rc::downgrade(&state)));
        WebLinksAddon { state }
    }

    /// `terminal.loadAddon(webLinksAddon)` calls this: registers the URL provider.
    pub fn activate(&self, terminal: &Terminal) {
        activate(&self.state, &terminal.shared);
    }

    /// Removes the URL provider.
    pub fn dispose(&self) {
        let registration = self.state.registration.borrow_mut().take();
        if let Some((shared, id)) = registration {
            if let Some(shared) = shared.upgrade() {
                unregister(&shared, id);
            }
        }
    }

    /// Internal: marks the built-in addon for `loadAddon`.
    #[wasm_bindgen(getter, js_name = __wongttyWebLinksAddon, skip_typescript)]
    pub fn marker(&self) -> u32 {
        self.state.id
    }
}

impl Drop for WebLinksAddon {
    fn drop(&mut self) {
        let state = Rc::downgrade(&self.state);
        ADDONS.with(|a| {
            if let Ok(mut a) = a.try_borrow_mut() {
                a.retain(|w| w.strong_count() > 0 && !Weak::ptr_eq(w, &state));
            }
        });
    }
}

fn activate(state: &Rc<WebState>, shared: &Rc<Shared>) {
    let id = register(shared, Provider::Web(state.clone()));
    *state.registration.borrow_mut() = Some((Rc::downgrade(shared), id));
}

/// `loadAddon` for the built-in `WebLinksAddon`: activates it on the terminal and returns true, or false for any
/// other addon.
pub fn load(terminal: &Terminal, addon: &JsValue) -> bool {
    let Some(id) = get(addon, "__wongttyWebLinksAddon").as_f64() else {
        return false;
    };
    let state = ADDONS.with(|a| {
        a.borrow()
            .iter()
            .filter_map(Weak::upgrade)
            .find(|s| f64::from(s.id) == id)
    });
    match state {
        Some(state) => {
            activate(&state, &terminal.shared);
            true
        }
        None => false,
    }
}

fn register(shared: &Shared, provider: Provider) -> u32 {
    shared
        .links
        .with(|inner| {
            inner.next_id = inner.next_id.wrapping_add(1);
            inner.providers.push((inner.next_id, provider));
            inner.next_id
        })
        .unwrap_or(0)
}

fn unregister(shared: &Shared, id: u32) {
    let removed = shared.links.existing(|inner| {
        let at = inner.providers.iter().position(|(i, _)| *i == id)?;
        Some(inner.providers.remove(at))
    });
    drop(removed);
}

#[wasm_bindgen]
impl Terminal {
    /// Adds a link provider, asked after the OSC 8 links and the providers registered before it; the earliest
    /// provider with a link under the mouse wins and later links that overlap its links are dropped.
    #[wasm_bindgen(js_name = registerLinkProvider)]
    pub fn register_link_provider(
        &self,
        #[wasm_bindgen(js_name = linkProvider, unchecked_param_type = "ILinkProvider")]
        link_provider: JsValue,
    ) -> Disposable {
        let id = register(&self.shared, Provider::Js(link_provider));
        let shared = Rc::downgrade(&self.shared);
        Disposable::new(move || {
            if let Some(shared) = shared.upgrade() {
                unregister(&shared, id);
            }
        })
    }
}
