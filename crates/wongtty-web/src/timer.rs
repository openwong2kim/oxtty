//! Timers through the JS globals, so the terminal core also runs in Node (tests). Only macrotasks
//! (`setTimeout`) and animation frames: the IME and input ordering borrowed from xterm.js depends on it.
//!
//! A timer owns its callback and frees it when dropped, also when cancelled before it fires; dropping it from
//! inside its own callback is fine (wasm-bindgen frees a running closure once the call returns).
//!
//! [`schedule`] is the page-wide write scheduler: every terminal with queued writes takes turns in one run that
//! parses for a few times what the last frame took to draw and then waits for the terminals' next frame, so many
//! busy terminals share the time between frames instead of each taking a whole task.
use std::cell::RefCell;
use std::rc::Weak;

use js_sys::Function;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::terminal::Shared;

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

/// Parse time per run, shared by every terminal: this many times what the frame before it took, and at least
/// so long that the run ends after the next vsync, so that frame starts at once instead of after an idle wait.
const RUN_PER_DRAW: f64 = 6.0;
const VSYNC_MS: f64 = 1000.0 / 60.0;
/// What drawing the terminals a run gave a turn may cost the frame after it: when each costs more (truecolor
/// glyphs), fewer get a turn per run.
const DRAW_MS: f64 = 8.0;
/// The longest run while frames come, under the 50 ms of a long task.
const MAX_RUN_MS: f64 = 45.0;
/// The longest run per busy terminal: what each had in its own task before. One busy terminal gets a frame after
/// at most this much parsing; without frames (a hidden page wakes up about once a second) a run parses as long as
/// the terminals' own tasks did.
const TURN_MS: f64 = 12.0;
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
            frame_at: 0.0,
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
    /// When the frame the last run waited for started (0 when it did not wait or the frame did not come), how
    /// many turns that run gave and what drawing one cost.
    frame_at: f64,
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
            w.frame_at = at;
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
        // The frame from its start to this run counts as drawing what the last run parsed. The estimate rises at
        // once and falls slowly, so one cheap frame does not hand out turns a dear one cannot draw.
        let drawn = if w.frame_at > 0.0 {
            start - w.frame_at
        } else {
            0.0
        };
        if w.frame_at > 0.0 {
            w.draw_ms = (drawn / w.served.max(1) as f64).max(w.draw_ms * 0.75);
        }
        w.frame_at = 0.0;
        let jobs = w.jobs.len() as f64 * TURN_MS;
        let (quota, run) = if w.no_frames {
            (usize::MAX, jobs)
        } else {
            // Turns at most double from run to run: the first frame after a burst draws a few terminals, not all.
            let quota = ((DRAW_MS / w.draw_ms).max(1.0) as usize).min(w.served.max(2) * 2);
            (quota, (drawn * RUN_PER_DRAW).min(MAX_RUN_MS).min(jobs))
        };
        (quota, start + run.max(VSYNC_MS + 1.0 - drawn))
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
            return;
        }
        w.kick();
    });
}
