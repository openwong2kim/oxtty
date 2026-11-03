//! WebGL2 helpers shared by the glyph and rectangle renderers.
//!
//! Ported from xterm.js 6.0.0 (commit f447274f430fd22513f6adbf9862d19524471c04, MIT):
//! `addons/addon-webgl/src/WebglUtils.ts` lines 8-63.
use web_sys::{WebGl2RenderingContext as Gl, WebGlProgram, WebGlShader};

/// Translates 0-1 coordinates (left to right, top to bottom) to clip space.
pub const PROJECTION_MATRIX: [f32; 16] = [
    2.0, 0.0, 0.0, 0.0, 0.0, -2.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, -1.0, 1.0, 0.0, 1.0,
];

/// A program whose compile and link were issued but not waited for: the GPU process (with
/// `KHR_parallel_shader_compile`, its worker threads) works on it until [`Linking::finish`] asks how it went.
pub struct Linking {
    program: WebGlProgram,
    shaders: [WebGlShader; 2],
}

pub fn start_program(gl: &Gl, vertex: &str, fragment: &str) -> Option<Linking> {
    let program = gl.create_program()?;
    let shaders = [
        create_shader(gl, Gl::VERTEX_SHADER, vertex)?,
        create_shader(gl, Gl::FRAGMENT_SHADER, fragment)?,
    ];
    for shader in &shaders {
        gl.attach_shader(&program, shader);
    }
    gl.link_program(&program);
    Some(Linking { program, shaders })
}

impl Linking {
    /// The linked program, once the link is done.
    pub fn finish(self, gl: &Gl) -> Option<WebGlProgram> {
        let program = self.program;
        if gl
            .get_program_parameter(&program, Gl::LINK_STATUS)
            .as_bool()
            .unwrap_or(false)
        {
            return Some(program);
        }
        // A lost context fails every link; only a real failure is worth reporting
        if !gl.is_context_lost() {
            for shader in &self.shaders {
                let log = gl.get_shader_info_log(shader).unwrap_or_default();
                if !log.is_empty() {
                    web_sys::console::error_1(&log.into());
                }
            }
            web_sys::console::error_1(
                &gl.get_program_info_log(&program).unwrap_or_default().into(),
            );
        }
        gl.delete_program(Some(&program));
        None
    }
}

fn create_shader(gl: &Gl, kind: u32, source: &str) -> Option<WebGlShader> {
    let shader = gl.create_shader(kind)?;
    gl.shader_source(&shader, source);
    gl.compile_shader(&shader);
    Some(shader)
}

/// Uploads `data` to the buffer bound at `target`.
pub fn buffer_f32(gl: &Gl, target: u32, data: &[f32], usage: u32) {
    // SAFETY: the view is used at once, before anything can allocate (and so move) wasm memory.
    let view = unsafe { js_sys::Float32Array::view(data) };
    gl.buffer_data_with_array_buffer_view(target, &view, usage);
}
