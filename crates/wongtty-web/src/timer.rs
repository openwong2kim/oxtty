//! Timers through the JS globals, so the terminal core also runs in Node (tests). Only macrotasks
//! (`setTimeout`) and animation frames: the IME and input ordering borrowed from xterm.js depends on it.
//!
//! A timer owns its callback and frees it when dropped, also when cancelled before it fires; dropping it from
//! inside its own callback is fine (wasm-bindgen frees a running closure once the call returns).
//!
//! [`schedule`] is the page-wide write scheduler: every terminal with queued writes takes turns in one task that
//! stops [`FRAME_BUDGET_MS`] after the frame started and goes on after the next frame, so many busy terminals
//! share the time between frames instead of each taking a whole task.
use std::cell::RefCell;
use std::rc::Weak;

use js_sys::Function;

use crate::terminal::Shared;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = setTimeout)]
    fn set_timeout(callback: &Function, ms: i32) -> JsValue;
    #[wasm_bindgen(js_name = clearTimeout)]
    fn clear_timeout(id: &JsValue);
    #[wasm_bindgen(js_name = requestAnimationFrame, catch)]
    fn request_animation_frame(callback: &Function) -> Result<JsValue, JsValue>;
    #[wasm_bindgen(js_name = cancelAnimationFrame)]
    fn cancel_animation_frame(id: &JsValue);
    #[wasm_bindgen(js_namespace = performance, js_name = now)]
    fn performance_now() -> f64;
}

/// Milliseconds, monotonic.
pub fn now() -> f64 {
    performance_now()
}

/// Runs `f` on a later task.
pub fn queue(f: impl FnOnce() + 'static) {
    set_timeout(Closure::once_into_js(f).unchecked_ref(), 0);
}

/// A `setTimeout` cleared when dropped.
pub struct Timeout {
    id: JsValue,
    _callback: Closure<dyn FnMut()>,
}

impl Timeout {
    pub fn new(ms: i32, f: impl FnOnce() + 'static) -> Self {
        let callback = once(f);
        Self {
            id: set_timeout(callback.as_ref().unchecked_ref(), ms),
            _callback: callback,
        }
    }
}

impl Drop for Timeout {
    fn drop(&mut self) {
        clear_timeout(&self.id);
    }
}

/// A `requestAnimationFrame` cancelled when dropped; `None` where there are no frames (Node).
pub struct AnimationFrame {
    id: JsValue,
    _callback: Closure<dyn FnMut()>,
}

impl AnimationFrame {
    pub fn new(f: impl FnOnce() + 'static) -> Option<Self> {
        let callback = once(f);
        let Ok(id) = request_animation_frame(callback.as_ref().unchecked_ref()) else {
            // No frames here: the write scheduler does not wait for any.
            WRITES.with(|w| w.borrow_mut().no_frames = true);
            return None;
        };
        Some(Self {
            id,
            _callback: callback,
        })
    }
}

impl Drop for AnimationFrame {
    fn drop(&mut self) {
        cancel_animation_frame(&self.id);
    }
}

fn once(f: impl FnOnce() + 'static) -> Closure<dyn FnMut()> {
    let mut f = Some(f);
    Closure::new(move || {
        if let Some(f) = f.take() {
            f();
        }
    })
}

/// Parse time per run, shared by every terminal. With the drawing after it, longer than a vsync interval: the
/// frame starts as soon as the run ends instead of after an idle wait for vsync.
const BUDGET_MS: f64 = 22.0;
/// What drawing the terminals a run gave a turn may cost the frame after it: when each costs more (truecolor
/// glyphs), fewer get a turn per run.
const DRAW_MS: f64 = 8.0;
/// A run parses at least as long as the frame before it took, up to this (under the 50 ms of a long task).
const MAX_RUN_MS: f64 = 45.0;
/// The shortest turn: each ends with the terminal's events, `onWriteParsed` and a frame request.
const MIN_TURN_MS: f64 = 0.25;
/// What frame callbacks may parse in all before the scheduler runs again.
const FRAME_DRAIN_MS: f64 = 4.0;
/// No frame this long after yielding (a hidden page): runs follow each other on plain tasks until frames return.
const FRAME_WAIT_MS: i32 = 100;

thread_local! {
    static WRITES: RefCell<Writes> = const {
        RefCell::new(Writes {
            jobs: Vec::new(),
            task: None,
            running: false,
            fallback: None,
            no_frames: false,
            yielded_at: 0.0,
            resumed: false,
            served: 0,
            draw_ms: 0.0,
            frame_until: 0.0,
        })
    };
}

struct Writes {
    /// Terminals with queued writes, in turn order.
    jobs: Vec<Weak<Shared>>,
    /// The next run.
    task: Option<Timeout>,
    running: bool,
    /// After yielding for the terminals' next frame: the run for when it does not come.
    fallback: Option<Timeout>,
    /// Frames do not come (hidden page, Node): runs follow each other on tasks, each up to the budget.
    no_frames: bool,
    /// When the last run yielded (0 when it did not wait for a frame), whether that frame came, how many turns
    /// the run gave and what drawing one cost.
    yielded_at: f64,
    resumed: bool,
    served: usize,
    draw_ms: f64,
    /// Until when frame callbacks may parse ([`frame_started`]), 0 until one asks.
    frame_until: f64,
}

impl Writes {
    fn kick(&mut self) {
        if !self.running && self.task.is_none() && self.fallback.is_none() && !self.jobs.is_empty()
        {
            self.task = Some(Timeout::new(0, run));
        }
    }
}

/// Queues a terminal for a turn ([`Shared::drain_turn`]), at most once until its turn returns `false`.
pub(crate) fn schedule(job: Weak<Shared>) {
    WRITES.with(|w| {
        let mut w = w.borrow_mut();
        w.jobs.push(job);
        w.kick();
    });
}

/// Every terminal frame starts here. A scheduler waiting for the frame goes on in a task after it (which runs after
/// the frame's rendering). Returns until when this frame callback may parse its queued writes (`writes`), so a
/// frame shows what came just before it: frame callbacks share [`FRAME_DRAIN_MS`] until the scheduler's next run,
/// and none parse in a frame the scheduler waited for.
pub(crate) fn frame_started(writes: bool) -> Option<f64> {
    WRITES.with(|w| {
        let mut w = w.borrow_mut();
        let at = now();
        w.no_frames = false;
        if w.fallback.take().is_some() {
            w.resumed = true;
            w.frame_until = at;
            w.kick();
        }
        if writes && w.frame_until == 0.0 {
            w.frame_until = at + FRAME_DRAIN_MS;
        }
        (writes && !w.running && at < w.frame_until).then_some(w.frame_until)
    })
}

fn run() {
    let start = now();
    let (quota, deadline) = WRITES.with(|w| {
        let mut w = w.borrow_mut();
        w.task = None;
        w.running = true;
        w.frame_until = 0.0;
        // Run by the fallback: the frame did not come.
        if w.fallback.take().is_some() {
            w.no_frames = true;
        }
        // Everything between the last run and this one counts as drawing what that run parsed. The estimate
        // rises at once and falls slowly, so one cheap frame does not hand out turns a dear one cannot draw.
        let drawn = if w.resumed { start - w.yielded_at } else { 0.0 };
        if w.resumed {
            w.draw_ms = (drawn / w.served.max(1) as f64).max(w.draw_ms * 0.75);
        }
        w.resumed = false;
        // Turns at most double from run to run: the first frame after a burst draws a few terminals, not all.
        let quota = ((DRAW_MS / w.draw_ms).max(1.0) as usize).min(w.served.max(2) * 2);
        (quota, start + drawn.clamp(BUDGET_MS, MAX_RUN_MS))
    });
    let mut served = 0;
    loop {
        let at = now();
        if at >= deadline || served >= quota {
            break;
        }
        let Some((job, left)) = WRITES.with(|w| {
            let mut w = w.borrow_mut();
            (!w.jobs.is_empty()).then(|| (w.jobs.remove(0), w.jobs.len()))
        }) else {
            break;
        };
        // The time left is shared by the jobs that may still get a turn. No borrow while a job runs: its write
        // callbacks may write to, open or dispose any terminal.
        let share = (left + 1).min(quota - served) as f64;
        let until = (at + ((deadline - at) / share).max(MIN_TURN_MS)).min(deadline);
        served += 1;
        if job.upgrade().is_some_and(|job| job.drain_turn(until)) {
            WRITES.with(|w| w.borrow_mut().jobs.push(job));
        }
    }
    WRITES.with(|w| {
        let mut w = w.borrow_mut();
        w.running = false;
        w.served = served;
        if w.jobs.is_empty() {
            return;
        }
        // The terminals that parsed asked for a frame; the next run waits for it.
        if !w.no_frames {
            w.fallback = Some(Timeout::new(FRAME_WAIT_MS, run));
            w.yielded_at = now();
            return;
        }
        w.kick();
    });
}
