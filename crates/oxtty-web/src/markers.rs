//! xterm.js markers (`registerMarker`, `markers`, `IMarker`) on the engine's markers, which the buffer API may call
//! (PRD D7). The engine moves a marker with its line as output scrolls it into the history and as scroll regions,
//! inserted and deleted lines move it, and drops it when its line leaves the scrollback or is cleared (ED, RIS);
//! `onDispose` fires once the write that dropped it is parsed, before the write callback. Each screen has its own
//! markers, as each xterm.js buffer does; the alternate screen's are dropped when it is left.
//!
//! Memory: the terminal keeps the id of each live marker (4 bytes) and an emitter only for the markers whose
//! `onDispose` was read. An `IMarker` is a small object made when read that asks the engine for its line, so
//! `term.markers[0] !== term.markers[0]`, unlike xterm.js.
use std::rc::{Rc, Weak};

use js_sys::{Array, Function};
use wasm_bindgen::prelude::*;

use crate::emitter::Emitter;
use crate::engine::mode;
use crate::events;
use crate::terminal::{Shared, Terminal};

#[wasm_bindgen(typescript_custom_section)]
const TYPES: &str = r#"
export type IMarker = Marker;
"#;

/// The live markers of one terminal.
#[derive(Default)]
pub(crate) struct Markers {
    /// The primary screen's, oldest first.
    ids: Vec<u32>,
    /// The alternate screen's, oldest first.
    alt_ids: Vec<u32>,
    /// Bumped when the engine starts over (a crash): its ids begin again, and older `IMarker`s must not see them.
    generation: u32,
    /// `onDispose` of the markers whose event was read.
    emitters: Vec<(u32, Rc<Emitter>)>,
}

impl Markers {
    fn contains(&self, id: u32) -> bool {
        self.ids.contains(&id) || self.alt_ids.contains(&id)
    }

    fn remove(&mut self, gone: &[u32]) {
        self.ids.retain(|id| !gone.contains(id));
        self.alt_ids.retain(|id| !gone.contains(id));
    }

    fn take_emitter(&mut self, id: u32) -> Option<Rc<Emitter>> {
        let at = self.emitters.iter().position(|(i, _)| *i == id)?;
        Some(self.emitters.swap_remove(at).1)
    }
}

fn fire(emitters: Vec<Rc<Emitter>>) {
    for emitter in emitters {
        emitter.fire(&[]);
        emitter.clear();
    }
}

/// Fires `onDispose` of the markers the engine dropped since the last call. Runs after every parsed write.
pub(crate) fn sync(shared: &Rc<Shared>) {
    let none = {
        let markers = shared.markers.borrow();
        markers.ids.is_empty() && markers.alt_ids.is_empty()
    };
    if none && !crate::image::has_markers(shared) {
        return;
    }
    let gone: Vec<u32> = {
        let Ok(core) = shared.core.try_borrow() else {
            return;
        };
        if core.disposed {
            return;
        }
        let Ok(list) = core.view.engine().take_disposed_markers() else {
            return;
        };
        Array::from(&list)
            .iter()
            .filter_map(|id| id.as_f64())
            .map(|id| id as u32)
            .collect()
    };
    if gone.is_empty() {
        return;
    }
    // Hook: images go with their end markers.
    crate::image::markers_gone(shared, &gone);
    let emitters = {
        let mut markers = shared.markers.borrow_mut();
        markers.remove(&gone);
        gone.iter()
            .filter_map(|&id| markers.take_emitter(id))
            .collect()
    };
    fire(emitters);
}

/// Drops every marker (the terminal is disposed or its engine started over) and fires their `onDispose`.
pub(crate) fn drop_all(shared: &Rc<Shared>) {
    let emitters = {
        let mut markers = shared.markers.borrow_mut();
        markers.generation = markers.generation.wrapping_add(1);
        markers.ids.clear();
        markers.alt_ids.clear();
        std::mem::take(&mut markers.emitters)
    };
    fire(emitters.into_iter().map(|(_, emitter)| emitter).collect());
}

/// xterm.js `IMarker`.
#[wasm_bindgen]
pub struct Marker {
    shared: Weak<Shared>,
    id: u32,
    generation: u32,
}

impl Marker {
    /// The terminal, while this marker is live.
    fn live(&self) -> Option<Rc<Shared>> {
        let shared = self.shared.upgrade()?;
        let live = shared
            .markers
            .try_borrow()
            .is_ok_and(|m| m.generation == self.generation && m.contains(self.id));
        live.then_some(shared)
    }
}

#[wasm_bindgen]
impl Marker {
    /// Unique among the terminal's markers (xterm.js numbers markers across all terminals).
    #[wasm_bindgen(getter)]
    pub fn id(&self) -> u32 {
        self.id
    }

    /// The buffer line (0 is the oldest scrollback line), -1 once disposed.
    #[wasm_bindgen(getter)]
    pub fn line(&self) -> i32 {
        let Some(shared) = self.live() else {
            return -1;
        };
        let Ok(core) = shared.core.try_borrow() else {
            return -1;
        };
        if core.disposed {
            return -1;
        }
        core.view.engine().marker_line(self.id).unwrap_or(-1)
    }

    #[wasm_bindgen(getter = isDisposed)]
    pub fn is_disposed(&self) -> bool {
        self.live().is_none()
    }

    /// Removes the marker and fires `onDispose`, like xterm.js; again does nothing.
    pub fn dispose(&self) {
        let Some(shared) = self.live() else {
            return;
        };
        if let Ok(core) = shared.core.try_borrow() {
            if !core.disposed {
                let _ = core.view.engine().remove_marker(self.id);
            }
        }
        let emitter = {
            let mut markers = shared.markers.borrow_mut();
            markers.remove(&[self.id]);
            markers.take_emitter(self.id)
        };
        fire(emitter.into_iter().collect());
    }

    /// Fires once when the marker is disposed; a listener added after that is never called.
    #[wasm_bindgen(getter = onDispose, unchecked_return_type = "IEvent<void>")]
    pub fn on_dispose(&self) -> Function {
        let emitter = match self.live() {
            Some(shared) => {
                let mut markers = shared.markers.borrow_mut();
                match markers.emitters.iter().find(|(id, _)| *id == self.id) {
                    Some((_, emitter)) => emitter.clone(),
                    None => {
                        let emitter = Rc::new(Emitter::default());
                        markers.emitters.push((self.id, emitter.clone()));
                        emitter
                    }
                }
            }
            // Dropped on return, so subscribing does nothing.
            None => Rc::new(Emitter::default()),
        };
        events::emitter_event(&emitter)
    }
}

#[wasm_bindgen]
impl Terminal {
    /// A marker on the cursor line plus `cursorYOffset` of the active buffer. `undefined` for a line outside the
    /// buffer (xterm.js marks those too); non-integers throw like xterm.js.
    #[wasm_bindgen(js_name = registerMarker, unchecked_return_type = "IMarker")]
    pub fn register_marker(
        &self,
        #[wasm_bindgen(js_name = cursorYOffset)] cursor_y_offset: Option<f64>,
    ) -> Result<Option<Marker>, JsValue> {
        let offset = cursor_y_offset.unwrap_or(0.0);
        if !offset.is_finite() || offset.fract() != 0.0 {
            return Err(js_sys::Error::new("This API only accepts integers").into());
        }
        let (id, alternate) = {
            let Ok(core) = self.shared.core.try_borrow() else {
                return Ok(None);
            };
            if core.disposed {
                return Ok(None);
            }
            let engine = core.view.engine();
            let line =
                f64::from(engine.history_length()) + f64::from(core.view.cursor().line) + offset;
            if !(0.0..=f64::from(i32::MAX)).contains(&line) {
                return Ok(None);
            }
            let alternate = engine.modes().unwrap_or(0) & mode::ALT_SCREEN != 0;
            (engine.add_marker(line as i32).unwrap_or(0), alternate)
        };
        if id == 0 {
            return Ok(None);
        }
        let mut markers = self.shared.markers.borrow_mut();
        if alternate {
            markers.alt_ids.push(id);
        } else {
            markers.ids.push(id);
        }
        Ok(Some(Marker {
            shared: Rc::downgrade(&self.shared),
            id,
            generation: markers.generation,
        }))
    }

    /// The live markers of the active buffer, oldest first, like xterm.js.
    #[wasm_bindgen(getter, unchecked_return_type = "ReadonlyArray<IMarker>")]
    pub fn markers(&self) -> Array {
        let alternate = self.shared.core.try_borrow().ok().and_then(|core| {
            (!core.disposed)
                .then(|| core.view.engine().modes().unwrap_or(0) & mode::ALT_SCREEN != 0)
        });
        let list = Array::new();
        let Some(alternate) = alternate else {
            return list;
        };
        let markers = self.shared.markers.borrow();
        let ids = if alternate {
            &markers.alt_ids
        } else {
            &markers.ids
        };
        for &id in ids {
            list.push(
                &Marker {
                    shared: Rc::downgrade(&self.shared),
                    id,
                    generation: markers.generation,
                }
                .into(),
            );
        }
        list
    }
}
