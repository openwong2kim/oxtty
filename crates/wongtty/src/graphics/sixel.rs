//! A sixel decoder: the payload of `DCS Ps ; Ps ; Ps q … ST` to RGBA pixels.
//!
//! Ported from the decoder `@xterm/addon-image` 0.10.0-beta.331 uses, Joerg Breitbart's `sixel` 0.16.0 (MIT):
//! `wasm/decoder.cpp` lines 1-540 of github.com/jerch/node-sixel at commit
//! 5cd50615e29f54b956188ec2a12d999add9099be (band decoding, colour commands and their quirks), and the pixel
//! assembly of `lib/Decoder.js` (`_initCanvas`, `_handle_band`, `width`, `height`, `data32`), built with the addon's
//! limits (16384 pixels wide, 4096 colours). Pixels are `0xAABBGGRR` (RGBA bytes in memory order).
//!
//! The finished rows go to a [`Sink`] one at a time, so a host can keep them outside its own memory. An image with
//! raster attributes (`"Pan;Pad;Ph;Pv`, as encoders write them) streams its rows as its bands end; one without keeps
//! its bands until the end, when its width is known.

/// Widest image (in pixels, as the addon's decoder is built).
pub const MAX_WIDTH: usize = 16384 - 4;
/// Colour registers.
pub const PALETTE_SIZE: usize = 4096;
/// The initial sixel colour: white.
pub const DEFAULT_FOREGROUND: u32 = 0xffff_ffff;

const ST_DATA: u8 = 0;
const ST_COMPRESSION: u8 = b'!';
const ST_ATTR: u8 = b'"';
const ST_COLOR: u8 = b'#';
const PARAMS: usize = 8;
/// Pixels the decoder of an image without raster attributes holds before it checks the limit.
const BLOCK: usize = 65536;
/// Most pixels an image without raster attributes may have, whatever the limit (its bands are held in memory).
const GROW_LIMIT: usize = 1 << 24;
/// Bytes the C decoder takes at a time (it matters for one quirk, see [`Decoder::decode`]).
const CHUNK: usize = 16384;

/// `0xAABBGGRR` of opaque 8-bit channels.
pub const fn rgba(r: u8, g: u8, b: u8) -> u32 {
    0xff00_0000 | (b as u32) << 16 | (g as u32) << 8 | r as u32
}

/// The addon's default palette: the 256 ANSI colours with the VT340 colours as the first 16, then zeros.
pub fn default_palette() -> Box<[u32; PALETTE_SIZE]> {
    let mut p = Box::new([0; PALETTE_SIZE]);
    p[..256].copy_from_slice(&DEFAULT_COLORS);
    p
}

/// VT340 colour registers (in percent) then the ANSI colour cube and grey ramp.
const DEFAULT_COLORS: [u32; 256] = {
    const VT340: [(u32, u32, u32); 16] = [
        (0, 0, 0),
        (20, 20, 80),
        (80, 13, 13),
        (20, 80, 20),
        (80, 20, 80),
        (20, 80, 80),
        (80, 80, 20),
        (53, 53, 53),
        (26, 26, 26),
        (33, 33, 60),
        (60, 26, 26),
        (33, 60, 33),
        (60, 33, 60),
        (33, 60, 60),
        (60, 60, 33),
        (80, 80, 80),
    ];
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    // Rounds these like the JS `normalizeRGB` of the sixel package
    const fn pct(v: u32) -> u8 {
        ((v * 255 + 50) / 100) as u8
    }
    let mut p = [0; 256];
    let mut i = 0;
    while i < 16 {
        let (r, g, b) = VT340[i];
        p[i] = rgba(pct(r), pct(g), pct(b));
        i += 1;
    }
    while i < 232 {
        let c = i - 16;
        p[i] = rgba(LEVELS[c / 36], LEVELS[c / 6 % 6], LEVELS[c % 6]);
        i += 1;
    }
    while i < 256 {
        let v = (8 + (i - 232) * 10) as u8;
        p[i] = rgba(v, v, v);
        i += 1;
    }
    p
};

/// Where decoded rows go.
pub trait Sink {
    /// The image is `width` × `height` pixels; false refuses it (the decoder stops). Called once, before the rows.
    fn start(&mut self, width: usize, height: usize) -> bool;
    /// The next row, `width` pixels.
    fn row(&mut self, pixels: &[u32]);
}

/// The rows in memory, for tests and hosts that want them there.
#[derive(Default)]
pub struct Pixels {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u32>,
}

impl Sink for Pixels {
    fn start(&mut self, width: usize, height: usize) -> bool {
        self.width = width;
        self.height = height;
        self.data = Vec::with_capacity(width * height);
        true
    }
    fn row(&mut self, pixels: &[u32]) {
        self.data.extend_from_slice(pixels);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Not decided until the first sixel data shows whether there are raster attributes.
    Undecided,
    /// No raster attributes: the width grows with the data, bands are kept until the end.
    Grow,
    /// Raster attributes: the image is truncated to them and rows stream out.
    Raster,
}

/// One image at a time; the palette carries over from image to image, like a terminal's colour registers.
pub struct Decoder {
    palette: Box<[u32; PALETTE_SIZE]>,
    palette_length: usize,
    fill: u32,
    /// Largest image in pixels.
    pixel_limit: usize,
    mode: Mode,
    abort: bool,
    state: u8,
    color: u32,
    /// Pixel column of the next sixel.
    cursor: usize,
    params: [i32; PARAMS],
    p_length: usize,
    /// Raster attributes.
    raster: [i32; 4],
    width: usize,
    height: usize,
    /// The band being painted: six rows of `stride` pixels. Grow mode clears it lazily, 128 pixels at a time.
    band: Vec<u32>,
    stride: usize,
    cleared: usize,
    real_width: usize,
    band_height: u8,
    /// Raster mode: rows given to the sink.
    rows_out: usize,
    /// Grow mode: finished bands (`6 × width` pixels each) and their widths.
    bands: Vec<u32>,
    band_widths: Vec<usize>,
    capacity: usize,
    scratch: Vec<u32>,
}

impl Decoder {
    pub fn new(palette: Box<[u32; PALETTE_SIZE]>) -> Self {
        Self {
            palette,
            palette_length: PALETTE_SIZE,
            fill: 0,
            pixel_limit: usize::MAX,
            mode: Mode::Undecided,
            abort: false,
            state: ST_DATA,
            color: DEFAULT_FOREGROUND,
            cursor: 0,
            params: [0; PARAMS],
            p_length: 1,
            raster: [0; 4],
            width: 0,
            height: 0,
            band: Vec::new(),
            stride: 0,
            cleared: 0,
            real_width: 0,
            band_height: 0,
            rows_out: 0,
            bands: Vec::new(),
            band_widths: Vec::new(),
            capacity: 0,
            scratch: Vec::new(),
        }
    }

    pub fn palette_mut(&mut self) -> &mut [u32; PALETTE_SIZE] {
        &mut self.palette
    }

    /// Starts an image: pixels not painted are `fill`; `palette_limit` colour registers are used, the image may have
    /// at most `pixel_limit` pixels.
    pub fn init(&mut self, fill: u32, palette_limit: usize, pixel_limit: usize) {
        self.release();
        self.fill = fill;
        self.palette_length = palette_limit.clamp(1, PALETTE_SIZE);
        self.pixel_limit = pixel_limit;
        self.mode = Mode::Undecided;
        self.abort = false;
        self.state = ST_DATA;
        self.color = DEFAULT_FOREGROUND;
        self.cursor = 0;
        self.params[0] = 0;
        self.p_length = 1;
        self.raster = [0; 4];
        self.width = 0;
        self.height = 0;
        self.real_width = 0;
        self.band_height = 0;
        self.rows_out = 0;
        self.capacity = 0;
    }

    /// Frees the pixel buffers (the palette stays).
    pub fn release(&mut self) {
        self.band = Vec::new();
        self.bands = Vec::new();
        self.band_widths = Vec::new();
        self.scratch = Vec::new();
        self.stride = 0;
        self.cleared = 0;
    }

    /// Whether the image was given up (too large).
    pub fn aborted(&self) -> bool {
        self.abort
    }

    pub fn decode(&mut self, data: &[u8], sink: &mut impl Sink) {
        for chunk in data.chunks(CHUNK) {
            let mut skip = 0;
            if self.mode == Mode::Undecided {
                for &byte in chunk {
                    self.decode_raster(byte & 0x7f, sink);
                    if self.mode != Mode::Undecided || self.abort {
                        break;
                    }
                }
                // The C decoder that takes over reads the chunk again from its start
                if self.mode == Mode::Undecided {
                    skip = chunk.len();
                }
            }
            for &byte in &chunk[skip..] {
                if self.abort {
                    return;
                }
                self.decode_byte(byte & 0x7f, sink);
            }
        }
    }

    /// Reads raster attributes before the first sixel data, then picks the mode (`decode_raster`).
    fn decode_raster(&mut self, code: u8, sink: &mut impl Sink) {
        let sixel_start = (63..127).contains(&code) || matches!(code, b'!' | b'#' | b'$' | b'-');
        if self.state == ST_DATA {
            if code == ST_ATTR {
                self.params[0] = 0;
                self.p_length = 1;
                self.state = ST_ATTR;
            } else if sixel_start {
                self.mode = Mode::Grow;
            }
        } else if code.is_ascii_digit() {
            let p = &mut self.params[self.p_length - 1];
            *p = p.wrapping_mul(10).wrapping_add((code - b'0') as i32);
        } else if code == b';' {
            if self.p_length < PARAMS {
                self.params[self.p_length] = 0;
                self.p_length += 1;
            }
        } else if self.p_length == 4 {
            self.raster = [
                self.params[0],
                self.params[1],
                self.params[2],
                self.params[3],
            ];
            self.state = ST_DATA;
            self.mode = Mode::Raster;
            self.width = (self.raster[2].max(0) as usize).min(MAX_WIDTH + 4);
            self.height = self.raster[3].max(0) as usize;
        } else if sixel_start {
            // Broken raster attributes: decode without them
            self.state = ST_DATA;
            self.mode = Mode::Grow;
        }
        match self.mode {
            Mode::Undecided => {}
            Mode::Raster => {
                let pixels = self.width.saturating_mul(self.height);
                if pixels > self.pixel_limit || !sink.start(self.width, self.height) {
                    self.abort = true;
                    return;
                }
                self.reset_band();
            }
            Mode::Grow => {
                self.capacity = BLOCK;
                self.reset_band();
            }
        }
    }

    fn decode_byte(&mut self, code: u8, sink: &mut impl Sink) {
        if code.is_ascii_digit() {
            let p = &mut self.params[self.p_length - 1];
            *p = p.wrapping_mul(10).wrapping_add((code - b'0') as i32);
            return;
        }
        if (63..127).contains(&code) {
            let bits = code - 63;
            if self.state != ST_DATA {
                let compressed = self.state == ST_COMPRESSION;
                if !compressed {
                    self.color = self.apply_color();
                }
                self.state = ST_DATA;
                if compressed {
                    let k = if self.params[0] != 0 {
                        self.params[0] as u32 as usize
                    } else {
                        1
                    };
                    self.paint(bits, k);
                    return;
                }
            }
            self.paint(bits, 1);
            return;
        }
        match code {
            ST_COMPRESSION | ST_COLOR => {
                if self.state == ST_COLOR {
                    self.color = self.apply_color();
                }
                self.params[0] = 0;
                self.p_length = 1;
                self.state = code;
            }
            b'$' => {
                self.note_width();
                self.cursor = 0;
            }
            b'-' => {
                let width = match self.mode {
                    Mode::Raster => self.width,
                    _ => {
                        self.note_width();
                        self.real_width
                    }
                };
                self.cursor = width;
                self.end_band(width, sink);
                if self.abort {
                    self.cursor = 0;
                    self.real_width = 0;
                    return;
                }
                self.reset_band();
                self.cursor = 0;
            }
            b';' if self.p_length < PARAMS => {
                self.params[self.p_length] = 0;
                self.p_length += 1;
            }
            _ => {}
        }
    }

    fn note_width(&mut self) {
        self.real_width = self.real_width.max(self.cursor).min(MAX_WIDTH);
    }

    /// Paints sixel `bits` `n` times from the cursor (`put`/`put_single`).
    fn paint(&mut self, bits: u8, n: usize) {
        let start = self.cursor;
        self.cursor = self.cursor.saturating_add(n);
        if self.mode == Mode::Grow {
            self.band_height |= bits;
            // A single sixel clears from where it is, a repeated one past its end (the C decoder's `clear_next`)
            let need = start.saturating_add(if n == 1 { 1 } else { n + 1 });
            while self.cleared < need && self.cleared < MAX_WIDTH {
                self.clear_next();
            }
        }
        if bits == 0 || start >= MAX_WIDTH {
            return;
        }
        let end = start.saturating_add(n).min(MAX_WIDTH).min(self.stride);
        if start >= end {
            return;
        }
        for y in 0..6 {
            if bits >> y & 1 != 0 {
                let row = y * self.stride;
                self.band[row + start..row + end].fill(self.color);
            }
        }
    }

    /// Grow mode: clears the next 128 pixels of the band.
    fn clear_next(&mut self) {
        let new = (self.cleared + 128).min(MAX_WIDTH + 4);
        if new > self.stride {
            let stride = new;
            let mut band = vec![self.fill; 6 * stride];
            for y in 0..6 {
                let (from, to) = (y * self.stride, y * stride);
                band[to..to + self.cleared].copy_from_slice(&self.band[from..from + self.cleared]);
            }
            self.band = band;
            self.stride = stride;
        } else {
            for y in 0..6 {
                let row = y * self.stride;
                self.band[row + self.cleared..row + new].fill(self.fill);
            }
        }
        self.cleared = new;
    }

    /// A new band of fill colour (`reset_line_m1`/`reset_line_m2`).
    fn reset_band(&mut self) {
        self.band_height = 0;
        match self.mode {
            Mode::Raster => {
                if self.stride != self.width {
                    self.stride = self.width;
                    self.band = vec![self.fill; 6 * self.width];
                } else {
                    self.band.fill(self.fill);
                }
            }
            _ => {
                self.real_width = 0;
                self.cleared = 0;
            }
        }
    }

    /// The band ended `width` pixels wide (`handle_band`).
    fn end_band(&mut self, width: usize, sink: &mut impl Sink) {
        match self.mode {
            Mode::Raster => {
                for y in 0..6 {
                    if self.rows_out >= self.height {
                        break;
                    }
                    sink.row(&self.band[y * self.stride..y * self.stride + width]);
                    self.rows_out += 1;
                }
            }
            _ => {
                let offset = self.bands.len();
                let pixels = offset + width * 6;
                if pixels > self.capacity {
                    if pixels > self.pixel_limit.min(GROW_LIMIT) {
                        self.abort = true;
                        self.release();
                        return;
                    }
                    self.capacity = pixels.div_ceil(BLOCK) * BLOCK;
                }
                for y in 0..6 {
                    let row = y * self.stride;
                    let painted = width.min(self.cleared);
                    self.bands.extend_from_slice(&self.band[row..row + painted]);
                    self.bands
                        .extend(std::iter::repeat_n(self.fill, width - painted));
                }
                self.band_widths.push(width);
            }
        }
    }

    /// Width of the band being painted (`current_width`).
    fn current_width(&mut self) -> usize {
        match self.mode {
            Mode::Grow => {
                self.note_width();
                self.real_width
            }
            Mode::Raster => self.width,
            Mode::Undecided => 0,
        }
    }

    /// Rows the band being painted has reached (`current_height`).
    fn current_height(&self) -> usize {
        8 - self.band_height.leading_zeros() as usize
    }

    /// Size of the image so far: the raster attributes, or what the data reached.
    pub fn size(&mut self) -> (usize, usize) {
        match self.mode {
            Mode::Undecided => (0, 0),
            Mode::Raster => (self.width, self.height),
            Mode::Grow => {
                let current = self.current_width();
                let widest = self.band_widths.iter().copied().max().unwrap_or(0);
                let bands = self.band_widths.len() * 6;
                let height = if current > 0 {
                    bands + self.current_height()
                } else {
                    bands
                };
                (widest.max(current), height)
            }
        }
    }

    /// Ends the image: the band being painted is part of it, rows the data did not reach are fill colour. Returns
    /// the size; nothing was given to `sink` (and its `start` was not called) when either side is 0.
    pub fn finish(&mut self, sink: &mut impl Sink) -> (usize, usize) {
        if self.abort {
            return (0, 0);
        }
        let (width, height) = self.size();
        match self.mode {
            Mode::Undecided => {}
            Mode::Raster => {
                if width > 0 {
                    for y in 0..6 {
                        if self.rows_out >= height {
                            break;
                        }
                        sink.row(&self.band[y * self.stride..y * self.stride + width]);
                        self.rows_out += 1;
                    }
                    if self.rows_out < height {
                        let fill = vec![self.fill; width];
                        while self.rows_out < height {
                            sink.row(&fill);
                            self.rows_out += 1;
                        }
                    }
                }
            }
            Mode::Grow => {
                let limit = self.pixel_limit.clamp(BLOCK, GROW_LIMIT);
                if width == 0 || height == 0 || width.saturating_mul(height) > limit {
                    self.release();
                    return (width, height);
                }
                if !sink.start(width, height) {
                    self.abort = true;
                    self.release();
                    return (0, 0);
                }
                let mut scratch = std::mem::take(&mut self.scratch);
                scratch.clear();
                scratch.resize(width, self.fill);
                let mut at = 0;
                for &bw in &self.band_widths {
                    for _ in 0..6 {
                        scratch[..bw].copy_from_slice(&self.bands[at..at + bw]);
                        scratch[bw..].fill(self.fill);
                        sink.row(&scratch);
                        at += bw;
                    }
                }
                let current = self.real_width;
                for y in 0..height - self.band_widths.len() * 6 {
                    let row = y * self.stride;
                    let painted = current.min(self.cleared);
                    scratch[..painted].copy_from_slice(&self.band[row..row + painted]);
                    scratch[painted..].fill(self.fill);
                    sink.row(&scratch);
                }
            }
        }
        self.release();
        (width, height)
    }

    /// A colour command (`#Pc` selects a register, `#Pc;Pu;Px;Py;Pz` defines it: Pu 1 HLS, 2 RGB), as the C decoder
    /// reads it, quirks included (`apply_color`).
    fn apply_color(&mut self) -> u32 {
        let p = self.params;
        let index = |v: i32, len: usize| {
            let v = v as u32 as usize;
            if v < len { v } else { v % len }
        };
        if self.p_length == 1 {
            return self.palette[index(p[0], self.palette_length)];
        }
        // As in C, `&&` binds tighter than `?:` here
        let valid = if self.p_length == 5 && p[1] == 1 {
            p[2] <= 360
        } else {
            p[2] <= 100 && p[3] <= 100 && p[4] <= 100
        };
        if !valid {
            return self.color;
        }
        let i = index(p[0], self.palette_length);
        match p[1] {
            1 => self.palette[i] = normalize_hls(p[2], p[3], p[4]),
            2 => self.palette[i] = normalize_rgb(p[2], p[3], p[4]),
            _ => {}
        }
        self.palette[i]
    }
}

/// Sixel RGB in percent to `0xAABBGGRR`, with the C decoder's integer rounding.
fn normalize_rgb(r: i32, g: i32, b: i32) -> u32 {
    let c = |v: i32| v.wrapping_mul(255).wrapping_add(50) / 100;
    (0xff00_0000u32 as i32 | c(b).wrapping_shl(16) | c(g).wrapping_shl(8) | c(r)) as u32
}

fn h2c(t1: f32, t2: f32, mut c: f32) -> f32 {
    if c < 0.0 {
        c += 1.0;
    } else if c > 1.0 {
        c -= 1.0;
    }
    if c < 1.0 / 6.0 {
        t2 + (t1 - t2) * 6.0 * c
    } else if c < 0.5 {
        t1
    } else if c < 2.0 / 3.0 {
        t2 + (t1 - t2) * (4.0 - c * 6.0)
    } else {
        t2
    }
}

/// Sixel HLS (hue turned by 240°, lightness and saturation in percent) to `0xAABBGGRR`, in `f32` like the C decoder.
fn normalize_hls(hi: i32, li: i32, si: i32) -> u32 {
    if si == 0 {
        return normalize_rgb(li, li, li);
    }
    let h = hi.wrapping_add(240) as f32 / 360.0;
    let l = li as f32 / 100.0;
    let s = si as f32 / 100.0;
    let t1 = if l < 0.5 {
        l * (1.0 + s)
    } else {
        l * (1.0 - s) + s
    };
    let t2 = l * 2.0 - t1;
    // As the C decoder's wasm build converts: truncated to an unsigned 32-bit integer, then its low byte (values out of
    // range wrap; HLS lightness and saturation are not checked)
    let channel = |c: f32| ((255.0 * h2c(t1, t2, c) + 0.5).max(0.0) as u32) & 0xff;
    0xff00_0000 | channel(h - 1.0 / 3.0) << 16 | channel(h) << 8 | channel(h + 1.0 / 3.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const R: u32 = rgba(255, 0, 0);
    const F: u32 = 0x1122_3344;

    fn decode(data: &str, fill: u32) -> Pixels {
        let mut d = Decoder::new(default_palette());
        let mut out = Pixels::default();
        d.init(fill, 256, 1 << 20);
        d.decode(data.as_bytes(), &mut out);
        let (w, h) = d.finish(&mut out);
        if w * h > 0 {
            assert_eq!((w, h), (out.width, out.height), "{data:?}");
            assert_eq!(out.data.len(), w * h);
        }
        out
    }

    #[test]
    fn raster_images_are_truncated_and_filled() {
        // 3x8: one sixel column per byte; `#1;2;100;0;0` defines red; the second band is cut at row 8
        let p = decode("\"1;1;3;8#1;2;100;0;0~~-~", F);
        assert_eq!((p.width, p.height), (3, 8));
        for y in 0..6 {
            assert_eq!(&p.data[y * 3..y * 3 + 3], &[R, R, F], "row {y}");
        }
        assert_eq!(&p.data[18..21], &[R, F, F]);
        assert_eq!(&p.data[21..24], &[R, F, F]);
    }

    #[test]
    fn images_without_raster_attributes_grow_to_their_widest_band() {
        // `!5?` repeats an empty sixel, `@` paints the top row only; bands of 2 and 5 pixels
        let p = decode("#1;2;100;0;0~~-!4?@", F);
        assert_eq!((p.width, p.height), (5, 7));
        assert_eq!(&p.data[0..5], &[R, R, F, F, F]);
        assert_eq!(&p.data[30..35], &[F, F, F, F, R]);
    }

    #[test]
    fn palette_registers_carry_over_and_colour_quirks_hold() {
        let mut d = Decoder::new(default_palette());
        let mut out = Pixels::default();
        d.init(0, 256, 1 << 20);
        // A colour definition applies at the next command
        d.decode(b"#7;2;0;0;100?", &mut out);
        d.finish(&mut out);
        let mut out = Pixels::default();
        d.init(0, 256, 1 << 20);
        // Register 7 is still blue; `#300` wraps around the 256-register limit to 44
        d.decode(b"\"1;1;2;1#7@#300@", &mut out);
        d.finish(&mut out);
        assert_eq!(out.data, vec![rgba(0, 0, 255), default_palette()[44]]);
        // HLS: hue 120 is red after the 240° turn
        assert_eq!(normalize_hls(120, 50, 100), rgba(255, 0, 0));
        assert_eq!(normalize_rgb(20, 80, 100), rgba(51, 204, 255));
        assert_eq!(default_palette()[2], rgba(204, 33, 33));
    }

    #[test]
    fn limits_and_empty_images() {
        let mut d = Decoder::new(default_palette());
        let mut out = Pixels::default();
        d.init(0, 256, 100);
        d.decode(b"\"1;1;20;20~", &mut out);
        assert!(d.aborted());
        assert_eq!(d.finish(&mut out), (0, 0));
        // Only a height: nothing is drawn, the size says how far the cursor moves
        let p = decode("----", 0);
        assert_eq!((p.width, p.height), (0, 0));
        let mut d = Decoder::new(default_palette());
        d.init(0, 256, 1 << 20);
        d.decode(b"----", &mut out);
        assert_eq!(d.size(), (0, 24));
        // Never panics on odd input
        let junk: Vec<u8> = (0..4000u32).map(|i| (i * 7919 % 251) as u8).collect();
        let mut d = Decoder::new(default_palette());
        d.init(0, 4096, 1 << 22);
        d.decode(&junk, &mut out);
        d.finish(&mut out);
        d.init(0, 4096, 1 << 22);
        d.decode(b"!99999999~!2147483647~#4294967295;2;1;1;1~", &mut out);
        d.finish(&mut out);
    }
}
