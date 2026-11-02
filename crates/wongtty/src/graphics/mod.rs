//! Decoders for inline images (sixel, iTerm2's OSC 1337), platform-neutral like [`crate::input`]: a host that draws
//! images uses them with [`crate::Engine::place_image`]. The engine itself does not call them, so its WebAssembly
//! build leaves them out.
pub mod iip;
pub mod sixel;
