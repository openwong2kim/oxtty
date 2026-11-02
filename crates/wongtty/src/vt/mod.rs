//! Fork of Alacritty `alacritty_terminal` 0.26 (Apache-2.0). Changes are listed in `PATCHES.md`.
//!
//! A low-level API outside the wongtty version stability promise. Renderers use [`crate::Engine`].

pub mod event;
pub mod grid;
pub mod index;
pub mod selection;
pub mod term;
pub mod vi_mode;

pub use crate::vt::grid::Grid;
pub use crate::vt::term::Term;
pub mod vte;

mod wmux_unicode11;
