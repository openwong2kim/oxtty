//! Renderer options and the theme read from `term.options`, with xterm.js 6 defaults.
use js_sys::{Array, Object};
use wasm_bindgen::JsValue;

use super::{CursorInactiveStyle, CursorStyle, RendererOptions, Theme};
use crate::engine::get;
use crate::options::option;

/// The `term.options` keys that are renderer options.
pub const KEYS: [&str; 16] = [
    "fontFamily",
    "fontSize",
    "fontWeight",
    "fontWeightBold",
    "lineHeight",
    "letterSpacing",
    "theme",
    "drawBoldTextInBrightColors",
    "minimumContrastRatio",
    "customGlyphs",
    "allowTransparency",
    "rescaleOverlappingGlyphs",
    "cursorBlink",
    "cursorStyle",
    "cursorWidth",
    "cursorInactiveStyle",
];

const ANSI_NAMES: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "brightBlack",
    "brightRed",
    "brightGreen",
    "brightYellow",
    "brightBlue",
    "brightMagenta",
    "brightCyan",
    "brightWhite",
];

fn number(options: &Object, key: &str) -> Option<f64> {
    option(options, key).as_f64().filter(|v| v.is_finite())
}

fn boolean(options: &Object, key: &str, default: bool) -> bool {
    option(options, key).as_bool().unwrap_or(default)
}

const FONT_WEIGHTS: [&str; 11] = [
    "normal", "bold", "100", "200", "300", "400", "500", "600", "700", "800", "900",
];

/// A CSS font weight: `'normal'`, `'bold'`, `'100'`…`'900'` or a number 1–1000 (xterm.js `FontWeight`).
fn font_weight(value: &JsValue, default: &str) -> String {
    if let Some(n) = value.as_f64() {
        if (1.0..=1000.0).contains(&n) {
            return n.to_string();
        }
    }
    match value.as_string() {
        Some(s) if FONT_WEIGHTS.contains(&s.as_str()) => s,
        _ => default.into(),
    }
}

/// The renderer options in `options` (xterm.js defaults where unset); invalid values fall back to the defaults instead of throwing.
pub fn parse(options: &Object) -> RendererOptions {
    let d = RendererOptions::default();
    RendererOptions {
        font_family: option(options, "fontFamily")
            .as_string()
            .unwrap_or(d.font_family),
        font_size: number(options, "fontSize")
            .filter(|&v| v > 0.0)
            .unwrap_or(d.font_size),
        font_weight: font_weight(&option(options, "fontWeight"), &d.font_weight),
        font_weight_bold: font_weight(&option(options, "fontWeightBold"), &d.font_weight_bold),
        line_height: number(options, "lineHeight")
            .filter(|&v| v >= 1.0)
            .unwrap_or(d.line_height),
        letter_spacing: number(options, "letterSpacing").unwrap_or(d.letter_spacing),
        theme: parse_theme(&option(options, "theme")),
        draw_bold_text_in_bright_colors: boolean(
            options,
            "drawBoldTextInBrightColors",
            d.draw_bold_text_in_bright_colors,
        ),
        // xterm.js clamps to 1–21 in steps of 0.1
        minimum_contrast_ratio: number(options, "minimumContrastRatio")
            .map(|v| (super::color::js_round(v * 10.0) / 10.0).clamp(1.0, 21.0))
            .unwrap_or(d.minimum_contrast_ratio),
        custom_glyphs: boolean(options, "customGlyphs", d.custom_glyphs),
        allow_transparency: boolean(options, "allowTransparency", d.allow_transparency),
        rescale_overlapping_glyphs: boolean(
            options,
            "rescaleOverlappingGlyphs",
            d.rescale_overlapping_glyphs,
        ),
        cursor_blink: boolean(options, "cursorBlink", d.cursor_blink),
        cursor_style: match option(options, "cursorStyle").as_string().as_deref() {
            Some("underline") => CursorStyle::Underline,
            Some("bar") => CursorStyle::Bar,
            _ => CursorStyle::Block,
        },
        cursor_width: number(options, "cursorWidth")
            .filter(|&v| v >= 1.0)
            .unwrap_or(d.cursor_width),
        cursor_inactive_style: match option(options, "cursorInactiveStyle")
            .as_string()
            .as_deref()
        {
            Some("block") => CursorInactiveStyle::Block,
            Some("bar") => CursorInactiveStyle::Bar,
            Some("underline") => CursorInactiveStyle::Underline,
            Some("none") => CursorInactiveStyle::None,
            _ => CursorInactiveStyle::Outline,
        },
    }
}

/// An xterm.js `ITheme` object; anything that is not a string is ignored.
pub fn parse_theme(theme: &JsValue) -> Theme {
    if !theme.is_object() {
        return Theme::default();
    }
    let s = |key: &str| get(theme, key).as_string();
    let extended = get(theme, "extendedAnsi");
    Theme {
        foreground: s("foreground"),
        background: s("background"),
        cursor: s("cursor"),
        cursor_accent: s("cursorAccent"),
        selection_background: s("selectionBackground"),
        selection_foreground: s("selectionForeground"),
        selection_inactive_background: s("selectionInactiveBackground"),
        ansi: ANSI_NAMES.map(s),
        extended_ansi: if Array::is_array(&extended) {
            Array::from(&extended)
                .iter()
                .map(|c| c.as_string().unwrap_or_default())
                .collect()
        } else {
            Vec::new()
        },
    }
}
