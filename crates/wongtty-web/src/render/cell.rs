//! Engine cells (`[codepoint, fg, bg, attrs]`) in the xterm.js render-model packing (fg/bg/ext words), which the
//! colour resolver, the glyph atlas and the rectangle renderer read.
//!
//! The packing and its accessors are ported from xterm.js 6.0.0 (commit f447274f430fd22513f6adbf9862d19524471c04,
//! MIT): `src/common/buffer/Constants.ts` lines 76-157 (`Attributes`, `FgFlags`, `BgFlags`, `ExtFlags`,
//! `UnderlineStyle`) and `src/common/buffer/AttributeData.ts` lines 9-211 (flag, colour and extended-attribute
//! accessors). The conversion replaces xterm.js `CellData`/`BufferLine.loadCell` (`CellData.ts` of the TypeScript
//! front end at 0510a7d).
use crate::engine::attr;

pub const CM_MASK: u32 = 0x300_0000;
pub const CM_DEFAULT: u32 = 0;
pub const CM_P16: u32 = 0x100_0000;
pub const CM_P256: u32 = 0x200_0000;
pub const CM_RGB: u32 = 0x300_0000;
pub const RGB_MASK: u32 = 0xff_ffff;
pub const PCOLOR_MASK: u32 = 0xff;

pub const FG_INVERSE: u32 = 0x400_0000;
pub const FG_BOLD: u32 = 0x800_0000;
pub const FG_UNDERLINE: u32 = 0x1000_0000;
pub const FG_BLINK: u32 = 0x2000_0000;
pub const FG_INVISIBLE: u32 = 0x4000_0000;
pub const FG_STRIKETHROUGH: u32 = 0x8000_0000;

pub const BG_ITALIC: u32 = 0x400_0000;
pub const BG_DIM: u32 = 0x800_0000;
pub const BG_HAS_EXTENDED: u32 = 0x1000_0000;
pub const BG_OVERLINE: u32 = 0x4000_0000;

pub const EXT_UNDERLINE_STYLE: u32 = 0x1c00_0000;
pub const EXT_VARIANT_OFFSET: u32 = 0xe000_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnderlineStyle {
    None = 0,
    Single = 1,
    Double = 2,
    Curly = 3,
    Dotted = 4,
    Dashed = 5,
}

impl UnderlineStyle {
    fn from_bits(bits: u32) -> Self {
        match bits {
            1 => Self::Single,
            2 => Self::Double,
            3 => Self::Curly,
            4 => Self::Dotted,
            5 => Self::Dashed,
            _ => Self::None,
        }
    }
}

// The engine's colour kinds (packages/wongtty/src/index.js ColorKind).
const COLOR_INDEXED: u32 = 2;
const COLOR_RGB: u32 = 3;

/// An engine colour in xterm.js packing. Named colours: 0–15 ANSI, 256 foreground, 257 background, 258 cursor,
/// 259–266 dim black … dim white, 267 bright foreground, 268 dim foreground.
pub fn to_xterm_color(packed: u32) -> u32 {
    let value = packed & 0xff_ffff;
    match packed >> 24 {
        COLOR_RGB => CM_RGB | value,
        COLOR_INDEXED if value < 16 => CM_P16 | value,
        COLOR_INDEXED => CM_P256 | (value & 0xff),
        _ if value < 16 => CM_P16 | value,
        _ if (259..=266).contains(&value) => CM_P16 | (value - 259),
        _ => CM_DEFAULT,
    }
}

/// The fg/bg/ext words of a cell and the accessors xterm.js `AttributeData` has on them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Attrs {
    pub fg: u32,
    pub bg: u32,
    pub ext: u32,
}

impl Attrs {
    pub fn is_inverse(&self) -> bool {
        self.fg & FG_INVERSE != 0
    }
    pub fn is_bold(&self) -> bool {
        self.fg & FG_BOLD != 0
    }
    pub fn is_underline(&self) -> bool {
        if self.has_extended() && self.underline_style() != UnderlineStyle::None {
            return true;
        }
        self.fg & FG_UNDERLINE != 0
    }
    pub fn is_invisible(&self) -> bool {
        self.fg & FG_INVISIBLE != 0
    }
    pub fn is_italic(&self) -> bool {
        self.bg & BG_ITALIC != 0
    }
    pub fn is_dim(&self) -> bool {
        self.bg & BG_DIM != 0
    }
    pub fn is_strikethrough(&self) -> bool {
        self.fg & FG_STRIKETHROUGH != 0
    }
    pub fn is_overline(&self) -> bool {
        self.bg & BG_OVERLINE != 0
    }
    pub fn fg_mode(&self) -> u32 {
        self.fg & CM_MASK
    }
    pub fn bg_mode(&self) -> u32 {
        self.bg & CM_MASK
    }
    pub fn fg_color(&self) -> i64 {
        color_value(self.fg)
    }
    pub fn bg_color(&self) -> i64 {
        color_value(self.bg)
    }
    pub fn has_extended(&self) -> bool {
        self.bg & BG_HAS_EXTENDED != 0
    }
    /// The extended underline style (`ExtendedAttrs.underlineStyle`).
    pub fn underline_style(&self) -> UnderlineStyle {
        UnderlineStyle::from_bits((self.ext & EXT_UNDERLINE_STYLE) >> 26)
    }
    fn ext_underline_color(&self) -> u32 {
        self.ext & (CM_MASK | RGB_MASK)
    }
    pub fn underline_color(&self) -> i64 {
        if self.has_extended() {
            let color = self.ext_underline_color();
            return match color & CM_MASK {
                CM_P16 | CM_P256 => (color & PCOLOR_MASK) as i64,
                CM_RGB => (color & RGB_MASK) as i64,
                _ => self.fg_color(),
            };
        }
        self.fg_color()
    }
    pub fn is_underline_color_rgb(&self) -> bool {
        if self.has_extended() {
            self.ext_underline_color() & CM_MASK == CM_RGB
        } else {
            self.fg_mode() == CM_RGB
        }
    }
    pub fn is_underline_color_default(&self) -> bool {
        if self.has_extended() {
            self.ext_underline_color() & CM_MASK == 0
        } else {
            self.fg_mode() == 0
        }
    }
    pub fn underline_variant_offset(&self) -> u32 {
        (self.ext & EXT_VARIANT_OFFSET) >> 29
    }
}

/// `AttributeData.getFgColor`: palette index, `0xRRGGBB`, or -1 for the default colour.
fn color_value(word: u32) -> i64 {
    match word & CM_MASK {
        CM_P16 | CM_P256 => (word & PCOLOR_MASK) as i64,
        CM_RGB => (word & RGB_MASK) as i64,
        _ => -1,
    }
}

/// `[r, g, b]` of `0xRRGGBB`.
pub fn to_color_rgb(value: u32) -> [u32; 3] {
    [(value >> 16) & 0xff, (value >> 8) & 0xff, value & 0xff]
}

/// One engine cell as the renderer reads it.
#[derive(Clone, Debug, Default)]
pub struct CellData {
    /// 0 for an empty cell and for the right half of a wide character.
    pub code: u32,
    /// 2 for a wide character, 0 for its right half.
    pub width: u32,
    /// The cell's text: its combining sequence, or the code point.
    pub chars: String,
    pub attrs: Attrs,
}

impl CellData {
    /// `combined` is the full text of a cell with combining marks (`CellExtra::text`), `underline_color` the
    /// SGR 58 colour in engine packing (0 for none).
    pub fn load(&mut self, cell: &[u32], combined: Option<&str>, underline_color: u32) {
        let [codepoint, fg, bg, attrs] = [0, 1, 2, 3].map(|i| cell.get(i).copied().unwrap_or(0));
        self.chars.clear();
        if attrs & attr::WIDE_SPACER != 0 {
            // Right half of a wide character: code 0 and width 0 like xterm.js, colours kept so the background
            // rectangle continues
            self.code = 0;
            self.width = 0;
        } else {
            self.code = codepoint;
            self.width = if attrs & attr::WIDE != 0 { 2 } else { 1 };
            match combined {
                Some(text) if !text.is_empty() => self.chars.push_str(text),
                _ => {
                    if let Some(c) = char::from_u32(codepoint).filter(|_| codepoint != 0) {
                        self.chars.push(c);
                    }
                }
            }
        }
        let mut fg = to_xterm_color(fg);
        let mut bg = to_xterm_color(bg);
        for (bit, flag) in [
            (attr::BOLD, FG_BOLD),
            (attr::INVERSE, FG_INVERSE),
            (attr::HIDDEN, FG_INVISIBLE),
            (attr::STRIKEOUT, FG_STRIKETHROUGH),
            (attr::BLINK, FG_BLINK),
        ] {
            if attrs & bit != 0 {
                fg |= flag;
            }
        }
        if attrs & attr::DIM != 0 {
            bg |= BG_DIM;
        }
        if attrs & attr::ITALIC != 0 {
            bg |= BG_ITALIC;
        }
        let style = if attrs & attr::CURLY_UNDERLINE != 0 {
            UnderlineStyle::Curly
        } else if attrs & attr::DOUBLE_UNDERLINE != 0 {
            UnderlineStyle::Double
        } else if attrs & attr::DOTTED_UNDERLINE != 0 {
            UnderlineStyle::Dotted
        } else if attrs & attr::DASHED_UNDERLINE != 0 {
            UnderlineStyle::Dashed
        } else if attrs & attr::UNDERLINE != 0 {
            UnderlineStyle::Single
        } else {
            UnderlineStyle::None
        };
        let mut ext = 0;
        if style != UnderlineStyle::None {
            fg |= FG_UNDERLINE;
            if style != UnderlineStyle::Single {
                bg |= BG_HAS_EXTENDED;
                ext |= ((style as u32) << 26) & EXT_UNDERLINE_STYLE;
            }
        }
        if underline_color != 0 {
            bg |= BG_HAS_EXTENDED;
            ext |= to_xterm_color(underline_color) & (CM_MASK | RGB_MASK);
        }
        self.attrs = Attrs { fg, bg, ext };
    }

    /// Length in UTF-16 units, as xterm.js measures `chars.length`.
    pub fn utf16_len(&self) -> usize {
        self.chars.encode_utf16().count()
    }
}
