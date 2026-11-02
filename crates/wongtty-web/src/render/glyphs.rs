//! Draws the glyphs of a render model from the atlas pages, one instanced quad per cell. The program and its
//! buffers live once per page ([`GlyphProgram`], owned by the shared context); each pane keeps only its vertex
//! data ([`GlyphVertices`]).
//!
//! Ported from xterm.js 6.0.0 (commit f447274f430fd22513f6adbf9862d19524471c04, MIT):
//! `addons/addon-webgl/src/GlyphRenderer.ts` lines 15-390, and `allowRescaling`/`isEmoji`/`isNerdFontGlyph` of
//! `src/browser/renderer/shared/RendererUtils.ts` lines 25-66. The double-buffered vertex copies are one scratch
//! buffer of the shared context instead of two per pane.
use std::cell::RefCell;
use std::rc::Rc;

use web_sys::{
    HtmlElement, WebGl2RenderingContext as Gl, WebGlBuffer, WebGlProgram, WebGlUniformLocation,
    WebGlVertexArrayObject,
};

use super::atlas::{is_powerline_glyph, TextureAtlas};
use super::gl::{buffer_f32, create_program, PROJECTION_MATRIX};
use super::model::RenderModel;
use super::Dimensions;

const UNIT_QUAD: u32 = 0;
const CELL_POSITION: u32 = 1;
const OFFSET: u32 = 2;
const SIZE: u32 = 3;
const TEXPAGE: u32 = 4;
const TEXCOORD: u32 = 5;
const TEXSIZE: u32 = 6;

const INDICES_PER_CELL: usize = 11;
const BYTES_PER_CELL: i32 = INDICES_PER_CELL as i32 * 4;
const CELL_POSITION_INDICES: usize = 2;

fn vertex_shader() -> String {
    format!(
        "#version 300 es
layout (location = {UNIT_QUAD}) in vec2 a_unitquad;
layout (location = {CELL_POSITION}) in vec2 a_cellpos;
layout (location = {OFFSET}) in vec2 a_offset;
layout (location = {SIZE}) in vec2 a_size;
layout (location = {TEXPAGE}) in float a_texpage;
layout (location = {TEXCOORD}) in vec2 a_texcoord;
layout (location = {TEXSIZE}) in vec2 a_texsize;

uniform mat4 u_projection;
uniform vec2 u_resolution;

out vec2 v_texcoord;
flat out int v_texpage;

void main() {{
  vec2 zeroToOne = (a_offset / u_resolution) + a_cellpos + (a_unitquad * a_size);
  gl_Position = u_projection * vec4(zeroToOne, 0.0, 1.0);
  v_texpage = int(a_texpage);
  v_texcoord = a_texcoord + a_unitquad * a_texsize;
}}"
    )
}

fn fragment_shader(units: u32) -> String {
    let mut conditionals = String::new();
    for i in 1..units {
        conditionals.push_str(&format!(
            " else if (v_texpage == {i}) {{ outColor = texture(u_texture[{i}], v_texcoord); }}"
        ));
    }
    format!(
        "#version 300 es
precision lowp float;

in vec2 v_texcoord;
flat in int v_texpage;

uniform sampler2D u_texture[{units}];

out vec4 outColor;

void main() {{
  if (v_texpage == 0) {{
    outColor = texture(u_texture[0], v_texcoord);
  }} {conditionals}
}}"
    )
}

fn is_emoji(c: u32) -> bool {
    (0x1f600..=0x1f64f).contains(&c)
        || (0x1f300..=0x1f5ff).contains(&c)
        || (0x1f680..=0x1f6ff).contains(&c)
        || (0x2600..=0x26ff).contains(&c)
        || (0x2700..=0x27bf).contains(&c)
        || (0xfe00..=0xfe0f).contains(&c)
        || (0x1f900..=0x1f9ff).contains(&c)
        || (0x1f1e6..=0x1f1ff).contains(&c)
}

fn allow_rescaling(code: u32, width: u32, glyph_width: f64, cell_width: f64) -> bool {
    width == 1
        // Overlapping by more than half a cell: barely overlapping glyphs stay readable as they are
        && glyph_width > (cell_width * 1.5).ceil()
        && code > 0xff
        && !is_emoji(code)
        && !is_powerline_glyph(code)
        && !(0xe000..=0xf8ff).contains(&code)
}

/// The glyph program and its buffers, once per context.
pub struct GlyphProgram {
    program: WebGlProgram,
    vao: WebGlVertexArrayObject,
    resolution_location: WebGlUniformLocation,
    buffers: [WebGlBuffer; 3],
}

impl GlyphProgram {
    /// `units` texture units hold the atlas pages.
    pub fn new(gl: &Gl, units: u32) -> Option<Self> {
        let program = create_program(gl, &vertex_shader(), &fragment_shader(units))?;
        let projection = gl.get_uniform_location(&program, "u_projection")?;
        let resolution_location = gl.get_uniform_location(&program, "u_resolution")?;
        let texture_location = gl.get_uniform_location(&program, "u_texture")?;

        let vao = gl.create_vertex_array()?;
        gl.bind_vertex_array(Some(&vao));

        // The 4 vertices of a rectangle
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

        // Two triangles from the 4 vertices, as a triangle strip
        let elements = gl.create_buffer()?;
        gl.bind_buffer(Gl::ELEMENT_ARRAY_BUFFER, Some(&elements));
        gl.buffer_data_with_u8_array(Gl::ELEMENT_ARRAY_BUFFER, &[0, 1, 2, 3], Gl::STATIC_DRAW);

        let attributes = gl.create_buffer()?;
        gl.bind_buffer(Gl::ARRAY_BUFFER, Some(&attributes));
        for (location, size, offset) in [
            (OFFSET, 2, 0),
            (SIZE, 2, 2),
            (TEXPAGE, 1, 4),
            (TEXCOORD, 2, 5),
            (TEXSIZE, 2, 7),
            (CELL_POSITION, 2, 9),
        ] {
            gl.enable_vertex_attrib_array(location);
            gl.vertex_attrib_pointer_with_i32(
                location,
                size,
                Gl::FLOAT,
                false,
                BYTES_PER_CELL,
                offset * 4,
            );
            gl.vertex_attrib_divisor(location, 1);
        }

        gl.use_program(Some(&program));
        let units: Vec<i32> = (0..units as i32).collect();
        gl.uniform1iv_with_i32_array(Some(&texture_location), &units);
        gl.uniform_matrix4fv_with_f32_array(Some(&projection), false, &PROJECTION_MATRIX);
        Some(Self {
            program,
            vao,
            resolution_location,
            buffers: [unit_quad, elements, attributes],
        })
    }

    pub fn dispose(&self, gl: &Gl) {
        gl.delete_program(Some(&self.program));
        gl.delete_vertex_array(Some(&self.vao));
        for buffer in &self.buffers {
            gl.delete_buffer(Some(buffer));
        }
    }

    /// Draws `data` (packed cells, see [`GlyphVertices::pack`]) onto a `width`×`height` drawing buffer with the
    /// atlas pages already bound.
    pub fn draw(&self, gl: &Gl, data: &[f32], width: f64, height: f64) {
        gl.use_program(Some(&self.program));
        gl.bind_vertex_array(Some(&self.vao));
        gl.uniform2f(Some(&self.resolution_location), width as f32, height as f32);
        // Transparent glyph texture
        gl.enable(Gl::BLEND);
        gl.blend_func(Gl::SRC_ALPHA, Gl::ONE_MINUS_SRC_ALPHA);
        gl.bind_buffer(Gl::ARRAY_BUFFER, Some(&self.buffers[2]));
        buffer_f32(gl, Gl::ARRAY_BUFFER, data, Gl::STREAM_DRAW);
        gl.draw_elements_instanced_with_i32(
            Gl::TRIANGLE_STRIP,
            4,
            Gl::UNSIGNED_BYTE,
            0,
            (data.len() / INDICES_PER_CELL) as i32,
        );
    }
}

/// One pane's glyph vertices (`GlyphRenderer` minus the GL objects).
pub struct GlyphVertices {
    atlas: Option<Rc<RefCell<TextureAtlas>>>,
    attributes: Vec<f32>,
    cols: usize,
    rows: usize,
    dims: Dimensions,
    pub rescale_overlapping_glyphs: bool,
}

impl GlyphVertices {
    pub fn new(cols: u32, rows: u32, dims: Dimensions) -> Self {
        let mut vertices = Self {
            atlas: None,
            attributes: Vec::new(),
            cols: cols as usize,
            rows: rows as usize,
            dims,
            rescale_overlapping_glyphs: false,
        };
        vertices.clear();
        vertices
    }

    pub fn atlas(&self) -> Option<&Rc<RefCell<TextureAtlas>>> {
        self.atlas.as_ref()
    }

    /// True when the model must be rebuilt (no atlas, or the atlas merged pages).
    pub fn begin_frame(&self) -> bool {
        self.atlas
            .as_ref()
            .is_none_or(|a| a.try_borrow().is_ok_and(|a| a.begin_frame()))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn update_cell(
        &mut self,
        x: usize,
        y: usize,
        code: u32,
        bg: u32,
        fg: u32,
        ext: u32,
        chars: &str,
        chars_len16: usize,
        width: u32,
        last_bg: u32,
        container: Option<&HtmlElement>,
    ) {
        let i = (y * self.cols + x) * INDICES_PER_CELL;
        let Some(cell) = self.attributes.get_mut(i..i + INDICES_PER_CELL) else {
            return;
        };
        // An empty cell draws nothing; a space continues (it may carry an underline or strikethrough)
        if code == 0 {
            cell[..INDICES_PER_CELL - 1 - CELL_POSITION_INDICES].fill(0.0);
            return;
        }
        let Some(atlas) = &self.atlas else { return };
        let Ok(mut atlas) = atlas.try_borrow_mut() else {
            return;
        };
        let glyph = if chars_len16 > 1 {
            atlas.get_rasterized_glyph_combined_char(chars, bg, fg, ext, container)
        } else {
            atlas.get_rasterized_glyph(code, bg, fg, ext, container)
        };
        let page_width = atlas.page_width(glyph.texture_page);
        let d = &self.dims;
        let left_cell_padding = ((d.device_cell.width - d.device_char.width) / 2.0).floor();
        let canvas = d.device_canvas;
        if bg != last_bg && glyph.offset.0 > left_cell_padding {
            // The glyph reaches into a cell with another background: clip its left side there
            let clipped = glyph.offset.0 - left_cell_padding;
            cell[0] = (-(glyph.offset.0 - clipped) + d.device_char.left) as f32;
            cell[1] = (-glyph.offset.1 + d.device_char.top) as f32;
            cell[2] = ((glyph.size.0 - clipped) / canvas.width) as f32;
            cell[3] = (glyph.size.1 / canvas.height) as f32;
            cell[4] = glyph.texture_page as f32;
            cell[5] = (glyph.texture_position_clip_space.0 + clipped / page_width) as f32;
            cell[6] = glyph.texture_position_clip_space.1 as f32;
            cell[7] = (glyph.size_clip_space.0 - clipped / page_width) as f32;
            cell[8] = glyph.size_clip_space.1 as f32;
        } else {
            cell[0] = (-glyph.offset.0 + d.device_char.left) as f32;
            cell[1] = (-glyph.offset.1 + d.device_char.top) as f32;
            cell[2] = (glyph.size.0 / canvas.width) as f32;
            cell[3] = (glyph.size.1 / canvas.height) as f32;
            cell[4] = glyph.texture_page as f32;
            cell[5] = glyph.texture_position_clip_space.0 as f32;
            cell[6] = glyph.texture_position_clip_space.1 as f32;
            cell[7] = glyph.size_clip_space.0 as f32;
            cell[8] = glyph.size_clip_space.1 as f32;
        }
        // a_cellpos only changes on resize

        // Narrow wide glyphs printed in one cell so they do not run into the next
        if self.rescale_overlapping_glyphs
            && allow_rescaling(code, width, glyph.size.0, d.device_cell.width)
        {
            // - 1 to improve readability
            cell[2] = ((d.device_cell.width - 1.0) / canvas.width) as f32;
        }
    }

    pub fn clear(&mut self) {
        let (cols, rows) = (self.cols, self.rows);
        let count = cols * rows * INDICES_PER_CELL;
        if self.attributes.len() != count {
            self.attributes = vec![0.0; count];
        } else {
            self.attributes.fill(0.0);
        }
        let mut i = 0;
        for y in 0..rows {
            for x in 0..cols {
                self.attributes[i + 9] = x as f32 / cols as f32;
                self.attributes[i + 10] = y as f32 / rows as f32;
                i += INDICES_PER_CELL;
            }
        }
    }

    /// New size and cell metrics; clears the vertices.
    pub fn resize(&mut self, cols: u32, rows: u32, dims: Dimensions) {
        self.cols = cols as usize;
        self.rows = rows as usize;
        self.dims = dims;
        self.clear();
    }

    /// Copies each row up to its last non-blank cell into `out`, so trailing whitespace sends no vertices.
    pub fn pack(&self, model: &RenderModel, out: &mut Vec<f32>) {
        out.clear();
        for (y, &line_length) in model.line_lengths.iter().enumerate() {
            let start = y * self.cols * INDICES_PER_CELL;
            let end = start + line_length as usize * INDICES_PER_CELL;
            if let Some(src) = self.attributes.get(start..end) {
                out.extend_from_slice(src);
            }
        }
    }

    pub fn set_atlas(&mut self, atlas: Option<Rc<RefCell<TextureAtlas>>>) {
        self.atlas = atlas;
    }
}
