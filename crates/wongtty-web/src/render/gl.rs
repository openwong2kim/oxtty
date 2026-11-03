//! WebGL2 helpers shared by the glyph and rectangle renderers.
//!
//! Ported from xterm.js 6.0.0 (commit f447274f430fd22513f6adbf9862d19524471c04, MIT):
//! `addons/addon-webgl/src/WebglUtils.ts` lines 8-63.
use web_sys::{WebGl2RenderingContext as Gl, WebGlProgram, WebGlShader};

/// Translates 0-1 coordinates (left to right, top to bottom) to clip space.
pub const PROJECTION_MATRIX: [f32; 16] = [
    2.0, 0.0, 0.0, 0.0, 0.0, -2.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, -1.0, 1.0, 0.0, 1.0,
];

pub fn create_program(gl: &Gl, vertex: &str, fragment: &str) -> Option<WebGlProgram> {
    let program = gl.create_program()?;
    gl.attach_shader(&program, &create_shader(gl, Gl::VERTEX_SHADER, vertex)?);
    gl.attach_shader(&program, &create_shader(gl, Gl::FRAGMENT_SHADER, fragment)?);
    gl.link_program(&program);
    if gl
        .get_program_parameter(&program, Gl::LINK_STATUS)
        .as_bool()
        .unwrap_or(false)
    {
        return Some(program);
    }
    // A lost context fails every link; only a real failure is worth reporting
    if !gl.is_context_lost() {
        web_sys::console::error_1(&gl.get_program_info_log(&program).unwrap_or_default().into());
    }
    gl.delete_program(Some(&program));
    None
}

fn create_shader(gl: &Gl, kind: u32, source: &str) -> Option<WebGlShader> {
    let shader = gl.create_shader(kind)?;
    gl.shader_source(&shader, source);
    gl.compile_shader(&shader);
    if gl
        .get_shader_parameter(&shader, Gl::COMPILE_STATUS)
        .as_bool()
        .unwrap_or(false)
    {
        return Some(shader);
    }
    if !gl.is_context_lost() {
        web_sys::console::error_1(&gl.get_shader_info_log(&shader).unwrap_or_default().into());
    }
    gl.delete_shader(Some(&shader));
    None
}

/// Uploads `data` to the buffer bound at `target`.
pub fn buffer_f32(gl: &Gl, target: u32, data: &[f32], usage: u32) {
    // SAFETY: the view is used at once, before anything can allocate (and so move) wasm memory.
    let view = unsafe { js_sys::Float32Array::view(data) };
    gl.buffer_data_with_array_buffer_view(target, &view, usage);
}
