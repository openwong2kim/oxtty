//! Cursor blink timing, run by the renderer itself (macrotask timers and animation frames only).
//!
//! Ported from xterm.js 6.0.0 (commit f447274f430fd22513f6adbf9862d19524471c04, MIT):
//! `addons/addon-webgl/src/CursorBlinkStateManager.ts` lines 8-146. Its `setInterval` is a `setTimeout`
//! re-armed on every tick ([`crate::timer`] has no intervals).
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use crate::timer::{self, AnimationFrame, Timeout};

/// The time between cursor blinks.
const BLINK_INTERVAL: f64 = 600.0;

struct State {
    visible: Cell<bool>,
    start_timeout: RefCell<Option<Timeout>>,
    interval: RefCell<Option<Timeout>>,
    frame: RefCell<Option<AnimationFrame>>,
    /// When the animation was restarted; the next tick restarts the timers from there instead of every call
    /// doing it.
    restarted_at: Cell<Option<f64>>,
    /// Asks for the cursor row to be redrawn. Never called while a `State` slot is borrowed.
    render: Box<dyn Fn()>,
}

pub struct Blink {
    state: Rc<State>,
}

impl Blink {
    pub fn new(render: impl Fn() + 'static, focused: bool) -> Self {
        let blink = Self {
            state: Rc::new(State {
                visible: Cell::new(true),
                start_timeout: RefCell::new(None),
                interval: RefCell::new(None),
                frame: RefCell::new(None),
                restarted_at: Cell::new(None),
                render: Box::new(render),
            }),
        };
        if focused {
            restart_interval(&blink.state, BLINK_INTERVAL);
        }
        blink
    }

    pub fn is_cursor_visible(&self) -> bool {
        self.state.visible.get()
    }

    fn is_paused(&self) -> bool {
        is_paused(&self.state)
    }

    /// Shows the cursor at once and restarts the blink from now (the cursor moved).
    pub fn restart_blink_animation(&self) {
        if self.is_paused() {
            return;
        }
        self.state.restarted_at.set(Some(timer::now()));
        self.state.visible.set(true);
        request_frame(&self.state);
    }

    /// Stops blinking with the cursor shown (focus lost).
    pub fn pause(&self) {
        pause(&self.state);
    }

    pub fn resume(&self) {
        pause(&self.state);
        self.state.restarted_at.set(None);
        restart_interval(&self.state, BLINK_INTERVAL);
        self.restart_blink_animation();
    }
}

impl Drop for Blink {
    fn drop(&mut self) {
        self.state.interval.borrow_mut().take();
        self.state.start_timeout.borrow_mut().take();
        self.state.frame.borrow_mut().take();
    }
}

fn is_paused(state: &State) -> bool {
    state.start_timeout.borrow().is_none() && state.interval.borrow().is_none()
}

fn pause(state: &State) {
    state.visible.set(true);
    state.interval.borrow_mut().take();
    state.start_timeout.borrow_mut().take();
    state.frame.borrow_mut().take();
}

fn request_frame(state: &Rc<State>) {
    if state.frame.borrow().is_some() {
        return;
    }
    let weak = Rc::downgrade(state);
    let frame = AnimationFrame::new(move || {
        if let Some(state) = weak.upgrade() {
            state.frame.borrow_mut().take();
            (state.render)();
        }
    });
    *state.frame.borrow_mut() = frame;
}

/// A timer of the blink: the callback runs only while the blink exists.
fn after(state: &Rc<State>, ms: f64, f: impl FnOnce(&Rc<State>) + 'static) -> Timeout {
    let weak: Weak<State> = Rc::downgrade(state);
    Timeout::new(ms.max(0.0) as i32, move || {
        if let Some(state) = weak.upgrade() {
            f(&state);
        }
    })
}

fn restart_interval(state: &Rc<State>, time_to_start: f64) {
    state.interval.borrow_mut().take();
    // The first timeout hides the cursor; the interval starts after it, so a restart only moves this timeout
    let timeout = after(state, time_to_start, |state| {
        // Another restart was asked for while this one was starting
        if let Some(restarted) = state.restarted_at.take() {
            let time = BLINK_INTERVAL - (timer::now() - restarted);
            if time > 0.0 {
                restart_interval(state, time);
                return;
            }
        }
        state.visible.set(false);
        request_frame(state);
        arm_interval(state);
    });
    *state.start_timeout.borrow_mut() = Some(timeout);
}

fn arm_interval(state: &Rc<State>) {
    let timeout = after(state, BLINK_INTERVAL, |state| {
        arm_interval(state);
        if let Some(restarted) = state.restarted_at.take() {
            let time = BLINK_INTERVAL - (timer::now() - restarted);
            restart_interval(state, time);
            return;
        }
        state.visible.set(!state.visible.get());
        request_frame(state);
    });
    *state.interval.borrow_mut() = Some(timeout);
}
