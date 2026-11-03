//! The image layer: tiles of sixel and iTerm2 images over the text, as `@xterm/addon-image`'s top layer canvas
//! covers the WebGL canvas (so also the selection and the cursor). The program lives once per page
//! ([`ImageProgram`]); each pane keeps its quads ([`ImageLayer`]), rebuilt every frame from the viewport's image
//! tiles; textures are the shared context's ([`super::gpu`]), made for the images on screen.
//!
//! Tile geometry follows `ImageRenderer.draw` and `drawPlaceholder` of `@xterm/addon-image` 0.10.0-beta.331 (xterm.js
//! commit 08829c7eb6279dda2a9e86df901353a1a89858ca, MIT), `addons/addon-image/src/ImageRenderer.ts` lines 179-212
//! and 238-268, and the run merging of `ImageStorage.render` (`ImageStorage.ts` lines 340-384).
use web_sys::{
    WebGl2RenderingContext as Gl, WebGlBuffer, WebGlProgram, WebGlTexture, WebGlUniformLocation,
    WebGlVertexArrayObject,
};

use super::gl::{buffer_f32, create_program, PROJECTION_MATRIX};
use super::Dimensions;
use crate::engine::Viewport;
use crate::image::{self, Pixels};

const POSITION: u32 = 0;
const SIZE: u32 = 1;
const TEXPOS: u32 = 2;
const TEXSIZE: u32 = 3;
const UNIT_QUAD: u32 = 4;
const FLOATS_PER_QUAD: usize = 8;

fn vertex_shader() -> String {
    format!(
        "#version 300 es
layout (location = {POSITION}) in vec2 a_position;
layout (location = {SIZE}) in vec2 a_size;
layout (location = {TEXPOS}) in vec2 a_texpos;
layout (location = {TEXSIZE}) in vec2 a_texsize;
layout (location = {UNIT_QUAD}) in vec2 a_unitquad;

uniform mat4 u_projection;

out vec2 v_texcoord;

void main() {{
  vec2 zeroToOne = a_position + (a_unitquad * a_size);
  gl_Position = u_projection * vec4(zeroToOne, 0.0, 1.0);
  v_texcoord = a_texpos + a_unitquad * a_texsize;
}}"
    )
}

// The placeholder alternates black and white CSS pixels, white where x + y is odd (the addon's pattern)
const FRAGMENT_SHADER: &str = "#version 300 es
precision highp float;

in vec2 v_texcoord;

uniform sampler2D u_texture;
uniform bool u_placeholder;
uniform float u_height;
uniform float u_dpr;

out vec4 outColor;

void main() {
  if (u_placeholder) {
    vec2 css = floor(vec2(gl_FragCoord.x, u_height - gl_FragCoord.y) / u_dpr);
    outColor = vec4(vec3(mod(css.x + css.y, 2.0)), 1.0);
  } else {
    outColor = texture(u_texture, v_texcoord);
  }
}";

/// The image program and its buffers, once per context.
pub struct ImageProgram {
    program: WebGlProgram,
    vao: WebGlVertexArrayObject,
    buffers: [WebGlBuffer; 3],
    placeholder: WebGlUniformLocation,
    height: WebGlUniformLocation,
    dpr: WebGlUniformLocation,
}

impl ImageProgram {
    pub fn new(gl: &Gl) -> Option<Self> {
        let program = create_program(gl, &vertex_shader(), FRAGMENT_SHADER)?;
        let projection = gl.get_uniform_location(&program, "u_projection")?;
        let texture = gl.get_uniform_location(&program, "u_texture")?;
        let placeholder = gl.get_uniform_location(&program, "u_placeholder")?;
        let height = gl.get_uniform_location(&program, "u_height")?;
        let dpr = gl.get_uniform_location(&program, "u_dpr")?;
        let vao = gl.create_vertex_array()?;
        gl.bind_vertex_array(Some(&vao));

        let unit_quad = gl.create_buffer()?;
        gl.bind_buffer(Gl::ARRAY_BUFFER, Some(&unit_quad));
        buffer_f32(
            gl,
            Gl::ARRAY_BUFFER,
            &[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            Gl::STATIC_DRAW,
        );
        gl.enable_vertex_attrib_array(UNIT_QUAD);
        gl.vertex_attrib_pointer_with_i32(UNIT_QUAD, 2, Gl::FLOAT, false, 0, 0);

        let elements = gl.create_buffer()?;
        gl.bind_buffer(Gl::ELEMENT_ARRAY_BUFFER, Some(&elements));
        gl.buffer_data_with_u8_array(Gl::ELEMENT_ARRAY_BUFFER, &[0, 1, 2, 3], Gl::STATIC_DRAW);

        let attributes = gl.create_buffer()?;
        gl.bind_buffer(Gl::ARRAY_BUFFER, Some(&attributes));
        for (location, offset) in [(POSITION, 0), (SIZE, 2), (TEXPOS, 4), (TEXSIZE, 6)] {
            gl.enable_vertex_attrib_array(location);
            gl.vertex_attrib_pointer_with_i32(
                location,
                2,
                Gl::FLOAT,
                false,
                FLOATS_PER_QUAD as i32 * 4,
                offset * 4,
            );
            gl.vertex_attrib_divisor(location, 1);
        }
        gl.use_program(Some(&program));
        gl.uniform_matrix4fv_with_f32_array(Some(&projection), false, &PROJECTION_MATRIX);
        gl.uniform1i(Some(&texture), 0);
        Some(Self {
            program,
            vao,
            buffers: [unit_quad, elements, attributes],
            placeholder,
            height,
            dpr,
        })
    }

    pub fn dispose(&self, gl: &Gl) {
        gl.delete_program(Some(&self.program));
        gl.delete_vertex_array(Some(&self.vao));
        for buffer in &self.buffers {
            gl.delete_buffer(Some(buffer));
        }
    }

    /// Draws the layer; `texture` gives the texture of an image id (`None` skips its quads).
    pub fn draw(
        &self,
        gl: &Gl,
        layer: &ImageLayer,
        mut texture: impl FnMut(u32) -> Option<WebGlTexture>,
    ) {
        gl.use_program(Some(&self.program));
        gl.bind_vertex_array(Some(&self.vao));
        gl.uniform1f(Some(&self.height), layer.height as f32);
        gl.uniform1f(Some(&self.dpr), layer.dpr as f32);
        gl.bind_buffer(Gl::ARRAY_BUFFER, Some(&self.buffers[2]));
        gl.active_texture(Gl::TEXTURE0);
        for batch in &layer.batches {
            match batch.image {
                Some(id) => {
                    let Some(t) = texture(id) else { continue };
                    gl.bind_texture(Gl::TEXTURE_2D, Some(&t));
                    gl.uniform1i(Some(&self.placeholder), 0);
                }
                None => gl.uniform1i(Some(&self.placeholder), 1),
            }
            let data = &layer.attributes
                [batch.start * FLOATS_PER_QUAD..(batch.start + batch.count) * FLOATS_PER_QUAD];
            buffer_f32(gl, Gl::ARRAY_BUFFER, data, Gl::STREAM_DRAW);
            gl.draw_elements_instanced_with_i32(
                Gl::TRIANGLE_STRIP,
                4,
                Gl::UNSIGNED_BYTE,
                0,
                batch.count as i32,
            );
        }
    }
}

/// Uploads an image's pixels to a new texture; `None` if it is too large for the context.
pub fn upload(gl: &Gl, id: u32, max_size: f64) -> Option<WebGlTexture> {
    image::with_image(id, |image| {
        if image.width as f64 > max_size || image.height as f64 > max_size {
            return None;
        }
        let texture = gl.create_texture()?;
        gl.bind_texture(Gl::TEXTURE_2D, Some(&texture));
        for (key, value) in [
            (Gl::TEXTURE_WRAP_S, Gl::CLAMP_TO_EDGE),
            (Gl::TEXTURE_WRAP_T, Gl::CLAMP_TO_EDGE),
            (Gl::TEXTURE_MIN_FILTER, Gl::LINEAR),
            (Gl::TEXTURE_MAG_FILTER, Gl::LINEAR),
        ] {
            gl.tex_parameteri(Gl::TEXTURE_2D, key, value as i32);
        }
        let _ = match &image.pixels {
            Pixels::Rgba(bytes) => gl
                .tex_image_2d_with_i32_and_i32_and_i32_and_format_and_type_and_opt_array_buffer_view(
                    Gl::TEXTURE_2D,
                    0,
                    Gl::RGBA as i32,
                    image.width as i32,
                    image.height as i32,
                    0,
                    Gl::RGBA,
                    Gl::UNSIGNED_BYTE,
                    Some(bytes),
                ),
            Pixels::Bitmap(bitmap) => gl.tex_image_2d_with_u32_and_u32_and_image_bitmap(
                Gl::TEXTURE_2D,
                0,
                Gl::RGBA as i32,
                Gl::RGBA,
                Gl::UNSIGNED_BYTE,
                bitmap,
            ),
        };
        Some(texture)
    })
    .flatten()
}

/// Quads of one image (or of placeholders, `image: None`) in [`ImageLayer::attributes`].
struct Batch {
    image: Option<u32>,
    start: usize,
    count: usize,
}

/// One pane's image quads.
#[derive(Default)]
pub struct ImageLayer {
    /// Tiles of images no longer stored show the placeholder.
    pub placeholder: bool,
    attributes: Vec<f32>,
    batches: Vec<Batch>,
    height: f64,
    dpr: f64,
}

impl ImageLayer {
    pub fn is_empty(&self) -> bool {
        self.batches.is_empty()
    }

    /// Rebuilds the quads from the viewport's image tiles: runs of consecutive tiles of one image on a row are one
    /// quad, placeholders are drawn first.
    pub fn update(&mut self, viewport: &Viewport, dims: &Dimensions, dpr: f64) {
        self.attributes.clear();
        self.batches.clear();
        self.height = dims.device_canvas.height;
        self.dpr = dpr;
        let (canvas_w, canvas_h) = (dims.device_canvas.width, dims.device_canvas.height);
        let (cw, ch) = (dims.css_cell.width, dims.css_cell.height);
        if canvas_w < 1.0 || canvas_h < 1.0 || cw <= 0.0 || ch <= 0.0 {
            return;
        }
        let (sx_scale, sy_scale) = (dims.device_cell.width / cw, dims.device_cell.height / ch);
        // Runs of tiles, row by row (image, row, first column, first tile, count)
        let mut runs: Vec<(u32, u32, u32, u32, u32)> = Vec::new();
        let mut row_tiles: Vec<Option<(u32, u32)>> = Vec::new();
        for y in 0..viewport.rows {
            let Some(extras) = viewport.extras.get(&y) else {
                continue;
            };
            row_tiles.clear();
            row_tiles.resize(viewport.cols as usize, None);
            for e in extras {
                if let (Some(image), Some(slot)) = (e.image, row_tiles.get_mut(e.col as usize)) {
                    *slot = Some(image);
                }
            }
            for (col, tile) in row_tiles.iter().enumerate() {
                let Some((id, tile)) = *tile else { continue };
                let col = col as u32;
                match runs.last_mut() {
                    Some(run)
                        if run.0 == id
                            && run.1 == y
                            && run.2 + run.4 == col
                            && run.3 + run.4 == tile =>
                    {
                        run.4 += 1
                    }
                    _ => runs.push((id, y, col, tile, 1)),
                }
            }
        }
        // Placeholders (images no longer stored) first, then the images
        let mut images: Vec<(u32, [f32; 8])> = Vec::new();
        for (id, row, col, tile, count) in runs {
            let quad = image::with_image(id, |image| {
                let (ocw, och) = image.cell;
                // The image scaled to the current cell size (`_rescaleImage`)
                let aw = (image.width as f64 * cw / ocw).ceil();
                let ah = (image.height as f64 * ch / och).ceil();
                let cols = image.cols.max(1);
                let sx = (tile % cols) as f64 * cw;
                let sy = (tile / cols) as f64 * ch;
                let fw = if count as f64 * cw + sx > aw {
                    aw - sx
                } else {
                    count as f64 * cw
                };
                let fh = if sy + ch > ah { ah - sy } else { ch };
                if fw <= 0.0 || fh <= 0.0 {
                    return None;
                }
                let (fw, fh) = (fw.ceil(), fh.ceil());
                let (dx, dy) = ((col as f64 * cw).floor(), (row as f64 * ch).floor());
                Some([
                    (dx * sx_scale / canvas_w) as f32,
                    (dy * sy_scale / canvas_h) as f32,
                    (fw * sx_scale / canvas_w) as f32,
                    (fh * sy_scale / canvas_h) as f32,
                    (sx.floor() / aw) as f32,
                    (sy.floor() / ah) as f32,
                    (fw / aw) as f32,
                    (fh / ah) as f32,
                ])
            });
            match quad {
                Some(Some(q)) => images.push((id, q)),
                None if self.placeholder => {
                    let (dx, dy) = (col as f64 * cw * sx_scale, row as f64 * ch * sy_scale);
                    let w = count as f64 * cw * sx_scale;
                    let q = [
                        (dx / canvas_w) as f32,
                        (dy / canvas_h) as f32,
                        (w / canvas_w) as f32,
                        (ch * sy_scale / canvas_h) as f32,
                        0.0,
                        0.0,
                        0.0,
                        0.0,
                    ];
                    self.push(None, &q);
                }
                _ => {}
            }
        }
        for (id, quad) in images {
            self.push(Some(id), &quad);
        }
    }

    fn push(&mut self, image: Option<u32>, quad: &[f32; 8]) {
        match self.batches.last_mut() {
            Some(batch) if batch.image == image => batch.count += 1,
            _ => self.batches.push(Batch {
                image,
                start: self.attributes.len() / FLOATS_PER_QUAD,
                count: 1,
            }),
        }
        self.attributes.extend_from_slice(quad);
    }
}
