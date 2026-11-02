//! Draws cell backgrounds, the cursor and highlight outlines as instanced rectangles. The program lives once per
//! page ([`RectProgram`]); each pane keeps its rectangles ([`RectVertices`]).
//!
//! Ported from xterm.js 6.0.0 (commit f447274f430fd22513f6adbf9862d19524471c04, MIT):
//! `addons/addon-webgl/src/RectangleRenderer.ts` lines 19-382. The highlight outlines and underlines are oxtty's
//! (xterm.js draws decorations as DOM elements).
use web_sys::{
    WebGl2RenderingContext as Gl, WebGlBuffer, WebGlProgram, WebGlUniformLocation,
    WebGlVertexArrayObject,
};

use super::cell::{CM_MASK, CM_P16, CM_P256, CM_RGB, FG_INVERSE, PCOLOR_MASK, RGB_MASK};
use super::color::{Color, ColorSet};
use super::gl::{buffer_f32, create_program, PROJECTION_MATRIX};
use super::model::{CursorModel, RenderModel, RENDER_MODEL_BG_OFFSET, RENDER_MODEL_FG_OFFSET};
use super::{CursorInactiveStyle, Dimensions};

const POSITION: u32 = 0;
const SIZE: u32 = 1;
const COLOR: u32 = 2;
const UNIT_QUAD: u32 = 3;

const INDICES_PER_RECTANGLE: usize = 8;
const BYTES_PER_RECTANGLE: i32 = INDICES_PER_RECTANGLE as i32 * 4;
const INITIAL_BUFFER_RECTANGLE_CAPACITY: usize = 20 * INDICES_PER_RECTANGLE;

fn vertex_shader() -> String {
    format!(
        "#version 300 es
layout (location = {POSITION}) in vec2 a_position;
layout (location = {SIZE}) in vec2 a_size;
layout (location = {COLOR}) in vec4 a_color;
layout (location = {UNIT_QUAD}) in vec2 a_unitquad;

uniform mat4 u_projection;

out vec4 v_color;

void main() {{
  vec2 zeroToOne = a_position + (a_unitquad * a_size);
  gl_Position = u_projection * vec4(zeroToOne, 0.0, 1.0);
  v_color = a_color;
}}"
    )
}

const FRAGMENT_SHADER: &str = "#version 300 es
precision lowp float;

in vec4 v_color;

out vec4 outColor;

void main() {
  outColor = v_color;
}";

struct Vertices {
    attributes: Vec<f32>,
    count: usize,
}

impl Vertices {
    fn new() -> Self {
        Self {
            attributes: vec![0.0; INITIAL_BUFFER_RECTANGLE_CAPACITY],
            count: 0,
        }
    }
}

/// A rectangle in device pixels with an `[r, g, b, a]` colour (0–1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub color: [f32; 4],
}

pub fn color_to_floats(rgba: u32) -> [f32; 4] {
    [
        ((rgba >> 24) & 0xff) as f32 / 255.0,
        ((rgba >> 16) & 0xff) as f32 / 255.0,
        ((rgba >> 8) & 0xff) as f32 / 255.0,
        (rgba & 0xff) as f32 / 255.0,
    ]
}

/// The rectangle program and its buffers, once per context.
pub struct RectProgram {
    program: WebGlProgram,
    vao: WebGlVertexArrayObject,
    buffers: [WebGlBuffer; 3],
    projection_location: WebGlUniformLocation,
}

impl RectProgram {
    pub fn new(gl: &Gl) -> Option<Self> {
        let program = create_program(gl, &vertex_shader(), FRAGMENT_SHADER)?;
        let projection_location = gl.get_uniform_location(&program, "u_projection")?;
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
        for (location, size, offset) in [(POSITION, 2, 0), (SIZE, 2, 2), (COLOR, 4, 4)] {
            gl.enable_vertex_attrib_array(location);
            gl.vertex_attrib_pointer_with_i32(
                location,
                size,
                Gl::FLOAT,
                false,
                BYTES_PER_RECTANGLE,
                offset * 4,
            );
            gl.vertex_attrib_divisor(location, 1);
        }
        Some(Self {
            program,
            vao,
            buffers: [unit_quad, elements, attributes],
            projection_location,
        })
    }

    pub fn dispose(&self, gl: &Gl) {
        gl.delete_program(Some(&self.program));
        gl.delete_vertex_array(Some(&self.vao));
        for buffer in &self.buffers {
            gl.delete_buffer(Some(buffer));
        }
    }

    fn draw(&self, gl: &Gl, vertices: &Vertices) {
        if vertices.count == 0 {
            return;
        }
        gl.use_program(Some(&self.program));
        gl.bind_vertex_array(Some(&self.vao));
        gl.uniform_matrix4fv_with_f32_array(
            Some(&self.projection_location),
            false,
            &PROJECTION_MATRIX,
        );
        gl.bind_buffer(Gl::ARRAY_BUFFER, Some(&self.buffers[2]));
        let used = (vertices.count * INDICES_PER_RECTANGLE).min(vertices.attributes.len());
        buffer_f32(
            gl,
            Gl::ARRAY_BUFFER,
            &vertices.attributes[..used],
            Gl::DYNAMIC_DRAW,
        );
        gl.draw_elements_instanced_with_i32(
            Gl::TRIANGLE_STRIP,
            4,
            Gl::UNSIGNED_BYTE,
            0,
            vertices.count as i32,
        );
    }

    pub fn draw_backgrounds(&self, gl: &Gl, r: &RectVertices) {
        self.draw(gl, &r.vertices);
    }

    pub fn draw_cursor(&self, gl: &Gl, r: &RectVertices) {
        self.draw(gl, &r.vertices_cursor);
    }

    pub fn draw_overlay(&self, gl: &Gl, r: &RectVertices) {
        self.draw(gl, &r.vertices_overlay);
    }
}

/// One pane's rectangles (`RectangleRenderer` minus the GL objects).
pub struct RectVertices {
    bg_float: [f32; 4],
    cursor_float: [f32; 4],
    vertices: Vertices,
    vertices_cursor: Vertices,
    vertices_overlay: Vertices,
    cols: usize,
    rows: usize,
    dims: Dimensions,
}

impl RectVertices {
    pub fn new(cols: u32, rows: u32, dims: Dimensions, colors: &ColorSet) -> Self {
        let mut r = Self {
            bg_float: [0.0; 4],
            cursor_float: [0.0; 4],
            vertices: Vertices::new(),
            vertices_cursor: Vertices::new(),
            vertices_overlay: Vertices::new(),
            cols: cols as usize,
            rows: rows as usize,
            dims,
        };
        r.update_colors(colors);
        r
    }

    /// New size and cell metrics.
    pub fn resize(&mut self, cols: u32, rows: u32, dims: Dimensions) {
        self.cols = cols as usize;
        self.rows = rows as usize;
        self.dims = dims;
        self.update_viewport_rectangle();
    }

    /// The theme changed (`onChangeColors`).
    pub fn update_colors(&mut self, colors: &ColorSet) {
        self.bg_float = color_to_floats(colors.background.rgba);
        self.cursor_float = color_to_floats(colors.cursor.rgba);
        self.update_viewport_rectangle();
    }

    /// The first rectangle clears the screen.
    fn update_viewport_rectangle(&mut self) {
        let (w, h) = (
            self.cols as f64 * self.dims.device_cell.width,
            self.rows as f64 * self.dims.device_cell.height,
        );
        let color = self.bg_float;
        let dims = self.dims;
        add_rectangle(
            &mut self.vertices.attributes,
            0,
            &dims,
            0.0,
            0.0,
            w,
            h,
            color,
        );
    }

    pub fn update_backgrounds(&mut self, model: &RenderModel, colors: &ColorSet) {
        let (cols, rows) = (self.cols, self.rows);
        let mut count = 1;
        for y in 0..rows {
            let mut start_x = 0;
            let (mut current_bg, mut current_fg, mut current_inverse) = (0, 0, false);
            for x in 0..cols {
                let i = (y * cols + x) * 4;
                let bg = model
                    .cells
                    .get(i + RENDER_MODEL_BG_OFFSET)
                    .copied()
                    .unwrap_or(0);
                let fg = model
                    .cells
                    .get(i + RENDER_MODEL_FG_OFFSET)
                    .copied()
                    .unwrap_or(0);
                let inverse = fg & FG_INVERSE != 0;
                if bg != current_bg || (fg != current_fg && (current_inverse || inverse)) {
                    // A rectangle ends where a non-default colour changes
                    if current_bg != 0 || (current_inverse && current_fg != 0) {
                        self.update_rectangle(count, current_fg, current_bg, start_x, x, y, colors);
                        count += 1;
                    }
                    start_x = x;
                    current_bg = bg;
                    current_fg = fg;
                    current_inverse = inverse;
                }
            }
            if current_bg != 0 || (current_inverse && current_fg != 0) {
                self.update_rectangle(count, current_fg, current_bg, start_x, cols, y, colors);
                count += 1;
            }
        }
        self.vertices.count = count;
    }

    pub fn update_cursor(&mut self, cursor: Option<&CursorModel>) {
        let mut rects = Vec::new();
        if let Some(c) = cursor {
            let d = self.dims;
            let (cw, ch) = (d.device_cell.width, d.device_cell.height);
            let (x, y, w) = (c.x as f64 * cw, c.y as f64 * ch, c.width as f64 * cw);
            let color = self.cursor_float;
            let rect = |x, y, width, height| Rect {
                x,
                y,
                width,
                height,
                color,
            };
            match c.style {
                CursorInactiveStyle::Bar => rects.push(rect(x, y, c.dpr * c.cursor_width, ch)),
                CursorInactiveStyle::Underline => rects.push(rect(x, y + ch - c.dpr, w, c.dpr)),
                CursorInactiveStyle::Outline => {
                    rects.push(rect(x, y, c.dpr, ch));
                    rects.push(rect(x, y + ch - c.dpr, w, c.dpr));
                    rects.push(rect(x, y, w, c.dpr));
                    rects.push(rect(x + w - c.dpr, y, c.dpr, ch));
                }
                CursorInactiveStyle::Block | CursorInactiveStyle::None => {}
            }
        }
        let dims = self.dims;
        set_rects(&mut self.vertices_cursor, &dims, &rects);
    }

    /// Highlight outlines and underlines, drawn over the glyphs.
    pub fn update_overlay(&mut self, rects: &[Rect]) {
        let dims = self.dims;
        set_rects(&mut self.vertices_overlay, &dims, rects);
    }

    #[allow(clippy::too_many_arguments)]
    fn update_rectangle(
        &mut self,
        index: usize,
        fg: u32,
        bg: u32,
        start_x: usize,
        end_x: usize,
        y: usize,
        colors: &ColorSet,
    ) {
        let word = if fg & FG_INVERSE != 0 { fg } else { bg };
        let rgba = match word & CM_MASK {
            CM_P16 | CM_P256 => colors
                .ansi
                .get((word & PCOLOR_MASK) as usize)
                .map_or(0, |c| c.rgba),
            CM_RGB => (word & RGB_MASK) << 8,
            _ if fg & FG_INVERSE != 0 => colors.foreground.rgba,
            _ => colors.background.rgba,
        };
        let offset = index * INDICES_PER_RECTANGLE;
        let vertices = &mut self.vertices;
        if vertices.attributes.len() < offset + INDICES_PER_RECTANGLE {
            let max = (self.rows * self.cols + 1) * INDICES_PER_RECTANGLE;
            let len = (vertices.attributes.len() * 2)
                .min(max)
                .max(offset + INDICES_PER_RECTANGLE);
            vertices.attributes.resize(len, 0.0);
        }
        let d = self.dims;
        let mut color = color_to_floats(rgba);
        color[3] = 1.0;
        add_rectangle(
            &mut vertices.attributes,
            offset,
            &d,
            start_x as f64 * d.device_cell.width,
            y as f64 * d.device_cell.height,
            (end_x - start_x) as f64 * d.device_cell.width,
            d.device_cell.height,
            color,
        );
    }
}

fn set_rects(vertices: &mut Vertices, dims: &Dimensions, rects: &[Rect]) {
    let needed = rects.len() * INDICES_PER_RECTANGLE;
    if vertices.attributes.len() < needed {
        vertices.attributes.resize(needed, 0.0);
    }
    for (i, r) in rects.iter().enumerate() {
        add_rectangle(
            &mut vertices.attributes,
            i * INDICES_PER_RECTANGLE,
            dims,
            r.x,
            r.y,
            r.width,
            r.height,
            r.color,
        );
    }
    vertices.count = rects.len();
}

#[allow(clippy::too_many_arguments)]
fn add_rectangle(
    array: &mut [f32],
    offset: usize,
    dims: &Dimensions,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    color: [f32; 4],
) {
    let Some(slot) = array.get_mut(offset..offset + INDICES_PER_RECTANGLE) else {
        return;
    };
    let canvas = dims.device_canvas;
    slot[0] = (x / canvas.width) as f32;
    slot[1] = (y / canvas.height) as f32;
    slot[2] = (width / canvas.width) as f32;
    slot[3] = (height / canvas.height) as f32;
    slot[4..8].copy_from_slice(&color);
}

/// The rgba of a highlight colour as floats.
pub fn css_floats(color: &Color) -> [f32; 4] {
    color_to_floats(color.rgba)
}
