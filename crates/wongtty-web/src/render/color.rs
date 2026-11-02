//! Colours: CSS parsing, blending and minimum contrast, and the colour set the renderer draws with.
//!
//! Ported from xterm.js 6.0.0 (commit f447274f430fd22513f6adbf9862d19524471c04, MIT):
//! `src/common/Color.ts` lines 9-376 (channels, color, css, rgb, rgba, contrastRatio),
//! `src/browser/services/ThemeService.ts` lines 23-122 and 186-198 (defaults, `_setTheme`, `parseColor`) and
//! `src/browser/Types.ts` lines 180-226 (`DEFAULT_ANSI_COLORS`).
use std::cell::RefCell;

use wasm_bindgen::JsCast;
use web_sys::{CanvasGradient, CanvasRenderingContext2d, HtmlCanvasElement};

use super::Theme;

/// A colour as xterm.js keeps it: the CSS string it came from and `0xRRGGBBAA`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Color {
    pub css: String,
    pub rgba: u32,
}

/// Transparent black (`NULL_COLOR`).
pub fn null_color() -> Color {
    Color {
        css: "#00000000".into(),
        rgba: 0,
    }
}

/// `Math.round`: halves round towards +∞ (Rust's `round` goes away from zero).
pub fn js_round(x: f64) -> f64 {
    (x + 0.5).floor()
}

pub fn to_css(r: u32, g: u32, b: u32, a: Option<u32>) -> String {
    match a {
        Some(a) => format!("#{r:02x}{g:02x}{b:02x}{a:02x}"),
        None => format!("#{r:02x}{g:02x}{b:02x}"),
    }
}

pub fn to_rgba(r: u32, g: u32, b: u32, a: u32) -> u32 {
    (r << 24) | (g << 16) | (b << 8) | a
}

pub fn channels_to_color(r: u32, g: u32, b: u32, a: Option<u32>) -> Color {
    Color {
        css: to_css(r, g, b, a),
        rgba: to_rgba(r, g, b, a.unwrap_or(0xff)),
    }
}

fn channels(rgba: u32) -> (u32, u32, u32, u32) {
    (
        rgba >> 24,
        (rgba >> 16) & 0xff,
        (rgba >> 8) & 0xff,
        rgba & 0xff,
    )
}

/// `color.blend`: `fg` over `bg` by `fg`'s alpha, opaque.
pub fn blend(bg: &Color, fg: &Color) -> Color {
    let a = (fg.rgba & 0xff) as f64 / 255.0;
    if a == 1.0 {
        return fg.clone();
    }
    let (r, g, b) = blend_channels(bg.rgba, fg.rgba, a);
    Color {
        css: to_css(r, g, b, None),
        rgba: to_rgba(r, g, b, 0xff),
    }
}

fn blend_channels(bg: u32, fg: u32, a: f64) -> (u32, u32, u32) {
    let (fr, fg_, fb, _) = channels(fg);
    let (br, bg_, bb, _) = channels(bg);
    let mix = |b: u32, f: u32| (b as f64 + js_round((f as f64 - b as f64) * a)) as u32;
    (mix(br, fr), mix(bg_, fg_), mix(bb, fb))
}

/// `rgba.blend`.
pub fn blend_rgba(bg: u32, fg: u32) -> u32 {
    let a = (fg & 0xff) as f64 / 255.0;
    if a == 1.0 {
        return fg;
    }
    let (r, g, b) = blend_channels(bg, fg, a);
    to_rgba(r, g, b, 0xff)
}

pub fn is_opaque(color: &Color) -> bool {
    color.rgba & 0xff == 0xff
}

pub fn opaque(color: &Color) -> Color {
    let rgba = color.rgba | 0xff;
    let (r, g, b, _) = channels(rgba);
    Color {
        css: to_css(r, g, b, None),
        rgba,
    }
}

pub fn opacity(color: &Color, opacity: f64) -> Color {
    let a = js_round(opacity * 255.0) as u32;
    let (r, g, b, _) = channels(color.rgba);
    channels_to_color(r, g, b, Some(a))
}

pub fn multiply_opacity(color: &Color, factor: f64) -> Color {
    let a = (color.rgba & 0xff) as f64;
    opacity(color, (a * factor) / 255.0)
}

/// Relative luminance of `0xRRGGBB` (WCAG 2.0).
pub fn relative_luminance(rgb: u32) -> f64 {
    relative_luminance2((rgb >> 16) & 0xff, (rgb >> 8) & 0xff, rgb & 0xff)
}

fn relative_luminance2(r: u32, g: u32, b: u32) -> f64 {
    let linear = |c: u32| {
        let s = c as f64 / 255.0;
        if s <= 0.03928 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    linear(r) * 0.2126 + linear(g) * 0.7152 + linear(b) * 0.0722
}

pub fn contrast_ratio(l1: f64, l2: f64) -> f64 {
    if l1 < l2 {
        (l2 + 0.05) / (l1 + 0.05)
    } else {
        (l1 + 0.05) / (l2 + 0.05)
    }
}

/// `rgba.ensureContrastRatio`: `fg` moved away from `bg` until `ratio` is met, or `None` if it already is.
pub fn ensure_contrast_ratio(bg: u32, fg: u32, ratio: f64) -> Option<u32> {
    let bg_l = relative_luminance(bg >> 8);
    let fg_l = relative_luminance(fg >> 8);
    if contrast_ratio(bg_l, fg_l) >= ratio {
        return None;
    }
    let ratio_of = |c: u32| contrast_ratio(bg_l, relative_luminance(c >> 8));
    type Adjust = fn(u32, u32, f64) -> u32;
    let (first, second): (Adjust, Adjust) = if fg_l < bg_l {
        (reduce_luminance, increase_luminance)
    } else {
        (increase_luminance, reduce_luminance)
    };
    let a = first(bg, fg, ratio);
    let a_ratio = ratio_of(a);
    if a_ratio < ratio {
        let b = second(bg, fg, ratio);
        return Some(if a_ratio > ratio_of(b) { a } else { b });
    }
    Some(a)
}

fn reduce_luminance(bg: u32, fg: u32, ratio: f64) -> u32 {
    let (br, bg_, bb, _) = channels(bg);
    let (mut r, mut g, mut b, _) = channels(fg);
    let bg_l = relative_luminance2(br, bg_, bb);
    let mut cr = contrast_ratio(relative_luminance2(r, g, b), bg_l);
    while cr < ratio && (r > 0 || g > 0 || b > 0) {
        let step = |c: u32| c - (c as f64 * 0.1).ceil() as u32;
        r = step(r);
        g = step(g);
        b = step(b);
        cr = contrast_ratio(relative_luminance2(r, g, b), bg_l);
    }
    to_rgba(r, g, b, 0xff)
}

fn increase_luminance(bg: u32, fg: u32, ratio: f64) -> u32 {
    let (br, bg_, bb, _) = channels(bg);
    let (mut r, mut g, mut b, _) = channels(fg);
    let bg_l = relative_luminance2(br, bg_, bb);
    let mut cr = contrast_ratio(relative_luminance2(r, g, b), bg_l);
    while cr < ratio && (r < 0xff || g < 0xff || b < 0xff) {
        let step = |c: u32| (c + ((255 - c) as f64 * 0.1).ceil() as u32).min(0xff);
        r = step(r);
        g = step(g);
        b = step(b);
        cr = contrast_ratio(relative_luminance2(r, g, b), bg_l);
    }
    to_rgba(r, g, b, 0xff)
}

/// `parseInt(s, 16)`: the leading hex digits, `None` if there are none.
fn parse_hex_prefix(s: &str) -> Option<u32> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    if digits.is_empty() {
        return None;
    }
    u64::from_str_radix(&digits, 16).ok().map(|v| v as u32)
}

/// `css.toColor`: `#rgb[a]`, `#rrggbb[aa]` and `rgb[a]()` are parsed directly; other CSS colours go through a
/// canvas (browser only) and must be opaque. `None` where xterm.js throws.
pub fn css_to_color(css: &str) -> Option<Color> {
    let hex = css.char_indices().any(|(i, c)| {
        c == '#'
            && css[i + 1..]
                .chars()
                .take_while(|c| c.is_ascii_hexdigit())
                .count()
                >= 3
    });
    if hex {
        let digit = |i: usize| parse_hex_prefix(&css.get(i..i + 1)?.repeat(2));
        match css.encode_utf16().count() {
            4 => return Some(channels_to_color(digit(1)?, digit(2)?, digit(3)?, None)),
            5 => {
                return Some(channels_to_color(
                    digit(1)?,
                    digit(2)?,
                    digit(3)?,
                    Some(digit(4)?),
                ))
            }
            7 => {
                let value = parse_hex_prefix(css.get(1..)?)?;
                return Some(Color {
                    css: css.into(),
                    rgba: (value << 8) | 0xff,
                });
            }
            9 => {
                return Some(Color {
                    css: css.into(),
                    rgba: parse_hex_prefix(css.get(1..)?)?,
                })
            }
            _ => {}
        }
    }
    if let Some((r, g, b, a)) = parse_rgb_function(css) {
        return Some(channels_to_color(r, g, b, Some(a)));
    }
    canvas_to_color(css)
}

/// `/rgba?\(\s*(\d{1,3})\s*,\s*(\d{1,3})\s*,\s*(\d{1,3})\s*(,\s*(0|1|\d?\.(\d+))\s*)?\)/` anywhere in `css`.
fn parse_rgb_function(css: &str) -> Option<(u32, u32, u32, u32)> {
    let bytes = css.as_bytes();
    (0..bytes.len()).find_map(|start| {
        let mut p = Parser { s: bytes, i: start };
        p.literal(b"rgb")?;
        p.optional(b'a');
        p.literal(b"(")?;
        p.spaces();
        let r = p.number(1, 3)?;
        p.spaces();
        p.literal(b",")?;
        p.spaces();
        let g = p.number(1, 3)?;
        p.spaces();
        p.literal(b",")?;
        p.spaces();
        let b = p.number(1, 3)?;
        p.spaces();
        let save = p.i;
        let alpha = (|| {
            p.literal(b",")?;
            p.spaces();
            let a = p.alpha()?;
            p.spaces();
            p.literal(b")")?;
            Some(a)
        })();
        let a = match alpha {
            Some(a) => a,
            None => {
                p.i = save;
                p.literal(b")")?;
                1.0
            }
        };
        Some((r, g, b, js_round(a * 255.0) as u32))
    })
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn literal(&mut self, text: &[u8]) -> Option<()> {
        let end = self.i + text.len();
        (self.s.get(self.i..end)? == text).then(|| self.i = end)
    }

    fn optional(&mut self, c: u8) {
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
        }
    }

    fn spaces(&mut self) {
        while self.s.get(self.i).is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn digits(&mut self, max: usize) -> usize {
        let start = self.i;
        while self.i - start < max && self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
            self.i += 1;
        }
        self.i - start
    }

    fn number(&mut self, min: usize, max: usize) -> Option<u32> {
        let start = self.i;
        if self.digits(max) < min {
            return None;
        }
        std::str::from_utf8(&self.s[start..self.i])
            .ok()?
            .parse()
            .ok()
    }

    /// `0|1|\d?\.(\d+)`, tried in that order like the regex (backtracking into the decimal form).
    fn alpha(&mut self) -> Option<f64> {
        let start = self.i;
        let decimal = (|| {
            self.digits(1);
            self.literal(b".")?;
            (self.digits(usize::MAX) > 0).then_some(())
        })();
        if decimal.is_some() {
            let text = std::str::from_utf8(&self.s[start..self.i]).ok()?;
            return text.parse().ok();
        }
        self.i = start;
        match self.s.get(self.i) {
            Some(b'0') => {
                self.i += 1;
                Some(0.0)
            }
            Some(b'1') => {
                self.i += 1;
                Some(1.0)
            }
            _ => None,
        }
    }
}

thread_local! {
    /// The 1×1 canvas `css.toColor` validates other CSS colours with; `None` outside a browser.
    static LITMUS: RefCell<Option<Option<(CanvasRenderingContext2d, CanvasGradient)>>> = const { RefCell::new(None) };
}

fn litmus() -> Option<(CanvasRenderingContext2d, CanvasGradient)> {
    let document = web_sys::window()?.document()?;
    let canvas: HtmlCanvasElement = document.create_element("canvas").ok()?.dyn_into().ok()?;
    canvas.set_width(1);
    canvas.set_height(1);
    let options = js_sys::Object::new();
    js_sys::Reflect::set(&options, &"willReadFrequently".into(), &true.into()).ok()?;
    let ctx: CanvasRenderingContext2d = canvas
        .get_context_with_context_options("2d", &options)
        .ok()??
        .dyn_into()
        .ok()?;
    ctx.set_global_composite_operation("copy").ok()?;
    let gradient = ctx.create_linear_gradient(0.0, 0.0, 1.0, 1.0);
    Some((ctx, gradient))
}

fn canvas_to_color(css: &str) -> Option<Color> {
    let litmus = LITMUS.with(|cell| {
        cell.borrow_mut()
            .get_or_insert_with(litmus)
            .as_ref()
            .cloned()
    })?;
    let (ctx, gradient) = litmus;
    ctx.set_fill_style_canvas_gradient(&gradient);
    ctx.set_fill_style_str(css);
    ctx.fill_style().as_string()?;
    ctx.fill_rect(0.0, 0.0, 1.0, 1.0);
    let data = ctx.get_image_data(0.0, 0.0, 1.0, 1.0).ok()?.data();
    let [r, g, b, a] = [0, 1, 2, 3].map(|i| data.get(i).copied().unwrap_or(0) as u32);
    (a == 0xff).then(|| Color {
        rgba: to_rgba(r, g, b, a),
        css: css.into(),
    })
}

/// 0xRRGGBB of a theme colour, `None` when unset or invalid.
pub fn css_to_rgb(css: Option<&str>) -> Option<u32> {
    css.and_then(css_to_color).map(|c| c.rgba >> 8)
}

fn parse_color(css: Option<&String>, fallback: &Color) -> Color {
    css.and_then(|c| css_to_color(c))
        .unwrap_or_else(|| fallback.clone())
}

/// xterm.js's default 256-colour palette.
pub fn default_ansi() -> Vec<Color> {
    const ANSI: [u32; 16] = [
        0x2e3436, 0xcc0000, 0x4e9a06, 0xc4a000, 0x3465a4, 0x75507b, 0x06989a, 0xd3d7cf, 0x555753,
        0xef2929, 0x8ae234, 0xfce94f, 0x729fcf, 0xad7fa8, 0x34e2e2, 0xeeeeec,
    ];
    const CUBE: [u32; 6] = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];
    let rgb = |c: u32| (c >> 16, (c >> 8) & 0xff, c & 0xff);
    let mut colors: Vec<Color> = ANSI
        .iter()
        .map(|&c| {
            let (r, g, b) = rgb(c);
            channels_to_color(r, g, b, None)
        })
        .collect();
    for i in 0..216 {
        colors.push(channels_to_color(
            CUBE[(i / 36) % 6],
            CUBE[(i / 6) % 6],
            CUBE[i % 6],
            None,
        ));
    }
    for i in 0..24 {
        let c = 8 + i * 10;
        colors.push(channels_to_color(c, c, c, None));
    }
    colors
}

/// The colours the renderer draws with (xterm.js `IColorSet`, without the scrollbar and overview ruler).
#[derive(Clone, Debug, PartialEq)]
pub struct ColorSet {
    pub foreground: Color,
    pub background: Color,
    pub cursor: Color,
    pub cursor_accent: Color,
    pub selection_foreground: Option<Color>,
    pub selection_background_transparent: Color,
    pub selection_background_opaque: Color,
    pub selection_inactive_background_transparent: Color,
    pub selection_inactive_background_opaque: Color,
    pub ansi: Vec<Color>,
}

impl ColorSet {
    /// `ThemeService._setTheme`.
    pub fn new(theme: &Theme) -> Self {
        let white = channels_to_color(0xff, 0xff, 0xff, None);
        let black = channels_to_color(0, 0, 0, None);
        let default_selection = Color {
            css: "rgba(255, 255, 255, 0.3)".into(),
            rgba: 0xffffff4d,
        };
        let foreground = parse_color(theme.foreground.as_ref(), &white);
        let background = parse_color(theme.background.as_ref(), &black);
        let cursor = blend(&background, &parse_color(theme.cursor.as_ref(), &white));
        let cursor_accent = blend(
            &background,
            &parse_color(theme.cursor_accent.as_ref(), &black),
        );
        let mut selection = parse_color(theme.selection_background.as_ref(), &default_selection);
        let selection_opaque = blend(&background, &selection);
        let mut inactive = parse_color(theme.selection_inactive_background.as_ref(), &selection);
        let inactive_opaque = blend(&background, &inactive);
        let selection_foreground = theme.selection_foreground.as_deref().and_then(css_to_color);
        // An opaque selection colour is drawn at 30% (xterm.js issue #2737)
        if is_opaque(&selection) {
            selection = opacity(&selection, 0.3);
        }
        if is_opaque(&inactive) {
            inactive = opacity(&inactive, 0.3);
        }
        let mut ansi = default_ansi();
        for (i, css) in theme.ansi.iter().enumerate() {
            ansi[i] = parse_color(css.as_ref(), &ansi[i]);
        }
        for (i, css) in theme.extended_ansi.iter().take(240).enumerate() {
            ansi[i + 16] = parse_color(Some(css), &ansi[i + 16]);
        }
        Self {
            foreground,
            background,
            cursor,
            cursor_accent,
            selection_foreground,
            selection_background_transparent: selection,
            selection_background_opaque: selection_opaque,
            selection_inactive_background_transparent: inactive,
            selection_inactive_background_opaque: inactive_opaque,
            ansi,
        }
    }
}

/// What the engine answers OSC 4/10/11/12 queries with: palette 0–255, foreground, background, cursor
/// (`themeColors` of the TypeScript front end at 0510a7d).
pub fn engine_colors(theme: &Theme) -> Vec<u32> {
    let defaults = default_ansi();
    let mut colors: Vec<u32> = (0..256)
        .map(|i| {
            let css = if i < 16 {
                theme.ansi[i].as_deref()
            } else {
                theme.extended_ansi.get(i - 16).map(String::as_str)
            };
            css_to_rgb(css).unwrap_or(defaults[i].rgba >> 8)
        })
        .collect();
    colors.push(css_to_rgb(theme.foreground.as_deref()).unwrap_or(0xffffff));
    colors.push(css_to_rgb(theme.background.as_deref()).unwrap_or(0x000000));
    colors.push(css_to_rgb(theme.cursor.as_deref()).unwrap_or(0xffffff));
    colors
}
