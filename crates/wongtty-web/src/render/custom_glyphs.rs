//! Block elements, shades, box drawing and Powerline symbols drawn as shapes instead of font glyphs, so they line
//! up across cells whatever the font.
//!
//! Ported from xterm.js 6.0.0 (commit f447274f430fd22513f6adbf9862d19524471c04, MIT):
//! `addons/addon-webgl/src/CustomGlyphs.ts` lines 15-693 (definitions, `tryDrawCustomChar`, the draw functions
//! and `translateArgs`).
use std::cell::RefCell;
use std::collections::HashMap;

use wasm_bindgen::{Clamped, JsCast};
use web_sys::{CanvasPattern, CanvasRenderingContext2d, HtmlCanvasElement, ImageData, Path2d};

use super::color::js_round;

/// `(x, y, w, h)` in eighths of the cell.
type Block = (f64, f64, f64, f64);

fn block_element(c: char) -> Option<&'static [Block]> {
    Some(match c {
        // Block elements (0x2580-0x2590)
        '▀' => &[(0., 0., 8., 4.)],
        '▁' => &[(0., 7., 8., 1.)],
        '▂' => &[(0., 6., 8., 2.)],
        '▃' => &[(0., 5., 8., 3.)],
        '▄' => &[(0., 4., 8., 4.)],
        '▅' => &[(0., 3., 8., 5.)],
        '▆' => &[(0., 2., 8., 6.)],
        '▇' => &[(0., 1., 8., 7.)],
        '█' => &[(0., 0., 8., 8.)],
        '▉' => &[(0., 0., 7., 8.)],
        '▊' => &[(0., 0., 6., 8.)],
        '▋' => &[(0., 0., 5., 8.)],
        '▌' => &[(0., 0., 4., 8.)],
        '▍' => &[(0., 0., 3., 8.)],
        '▎' => &[(0., 0., 2., 8.)],
        '▏' => &[(0., 0., 1., 8.)],
        '▐' => &[(4., 0., 4., 8.)],
        // Block elements (0x2594-0x2595)
        '▔' => &[(0., 0., 8., 1.)],
        '▕' => &[(7., 0., 1., 8.)],
        // Terminal graphic characters (0x2596-0x259F)
        '▖' => &[(0., 4., 4., 4.)],
        '▗' => &[(4., 4., 4., 4.)],
        '▘' => &[(0., 0., 4., 4.)],
        '▙' => &[(0., 0., 4., 8.), (0., 4., 8., 4.)],
        '▚' => &[(0., 0., 4., 4.), (4., 4., 4., 4.)],
        '▛' => &[(0., 0., 4., 8.), (4., 0., 4., 4.)],
        '▜' => &[(0., 0., 8., 4.), (4., 0., 4., 8.)],
        '▝' => &[(4., 0., 4., 4.)],
        '▞' => &[(4., 0., 4., 4.), (0., 4., 4., 4.)],
        '▟' => &[(4., 0., 4., 8.), (0., 4., 8., 4.)],
        // VERTICAL ONE EIGHTH BLOCK-2 through VERTICAL ONE EIGHTH BLOCK-7
        '\u{1FB70}' => &[(1., 0., 1., 8.)],
        '\u{1FB71}' => &[(2., 0., 1., 8.)],
        '\u{1FB72}' => &[(3., 0., 1., 8.)],
        '\u{1FB73}' => &[(4., 0., 1., 8.)],
        '\u{1FB74}' => &[(5., 0., 1., 8.)],
        '\u{1FB75}' => &[(6., 0., 1., 8.)],
        // HORIZONTAL ONE EIGHTH BLOCK-2 through HORIZONTAL ONE EIGHTH BLOCK-7
        '\u{1FB76}' => &[(0., 1., 8., 1.)],
        '\u{1FB77}' => &[(0., 2., 8., 1.)],
        '\u{1FB78}' => &[(0., 3., 8., 1.)],
        '\u{1FB79}' => &[(0., 4., 8., 1.)],
        '\u{1FB7A}' => &[(0., 5., 8., 1.)],
        '\u{1FB7B}' => &[(0., 6., 8., 1.)],
        // LEFT AND LOWER ONE EIGHTH BLOCK … HORIZONTAL ONE EIGHTH BLOCK-1358
        '\u{1FB7C}' => &[(0., 0., 1., 8.), (0., 7., 8., 1.)],
        '\u{1FB7D}' => &[(0., 0., 1., 8.), (0., 0., 8., 1.)],
        '\u{1FB7E}' => &[(7., 0., 1., 8.), (0., 0., 8., 1.)],
        '\u{1FB7F}' => &[(7., 0., 1., 8.), (0., 7., 8., 1.)],
        '\u{1FB80}' => &[(0., 0., 8., 1.), (0., 7., 8., 1.)],
        '\u{1FB81}' => &[
            (0., 0., 8., 1.),
            (0., 2., 8., 1.),
            (0., 4., 8., 1.),
            (0., 7., 8., 1.),
        ],
        // UPPER ONE QUARTER BLOCK … UPPER SEVEN EIGHTHS BLOCK
        '\u{1FB82}' => &[(0., 0., 8., 2.)],
        '\u{1FB83}' => &[(0., 0., 8., 3.)],
        '\u{1FB84}' => &[(0., 0., 8., 5.)],
        '\u{1FB85}' => &[(0., 0., 8., 6.)],
        '\u{1FB86}' => &[(0., 0., 8., 7.)],
        // RIGHT ONE QUARTER BLOCK … RIGHT SEVEN EIGHTHS BLOCK
        '\u{1FB87}' => &[(6., 0., 2., 8.)],
        '\u{1FB88}' => &[(5., 0., 3., 8.)],
        '\u{1FB89}' => &[(3., 0., 5., 8.)],
        '\u{1FB8A}' => &[(2., 0., 6., 8.)],
        '\u{1FB8B}' => &[(1., 0., 7., 8.)],
        // CHECKER BOARD FILL
        '\u{1FB95}' => &[
            (0., 0., 2., 2.),
            (4., 0., 2., 2.),
            (2., 2., 2., 2.),
            (6., 2., 2., 2.),
            (0., 4., 2., 2.),
            (4., 4., 2., 2.),
            (2., 6., 2., 2.),
            (6., 6., 2., 2.),
        ],
        // INVERSE CHECKER BOARD FILL
        '\u{1FB96}' => &[
            (2., 0., 2., 2.),
            (6., 0., 2., 2.),
            (0., 2., 2., 2.),
            (4., 2., 2., 2.),
            (2., 4., 2., 2.),
            (6., 4., 2., 2.),
            (0., 6., 2., 2.),
            (4., 6., 2., 2.),
        ],
        // HEAVY HORIZONTAL FILL (upper middle and lower one quarter block)
        '\u{1FB97}' => &[(0., 2., 8., 2.), (0., 6., 8., 2.)],
        _ => return None,
    })
}

type Pattern = &'static [&'static [u8]];

/// Shade characters (0x2591-0x2593): a repeating pattern of filled (1) and empty (0) pixels.
fn pattern(c: char) -> Option<Pattern> {
    Some(match c {
        '░' => &[&[1, 0, 0, 0], &[0, 0, 0, 0], &[0, 0, 1, 0], &[0, 0, 0, 0]],
        '▒' => &[&[1, 0], &[0, 0], &[0, 1], &[0, 0]],
        '▓' => &[&[0, 1], &[1, 1], &[1, 0], &[1, 1]],
        _ => return None,
    })
}

// SVG path shapes (the `d` attribute) of the box drawing characters.
const TOP_TO_BOTTOM: &str = "M.5,0 L.5,1";
const LEFT_TO_RIGHT: &str = "M0,.5 L1,.5";
const TOP_TO_RIGHT: &str = "M.5,0 L.5,.5 L1,.5";
const TOP_TO_LEFT: &str = "M.5,0 L.5,.5 L0,.5";
const LEFT_TO_BOTTOM: &str = "M0,.5 L.5,.5 L.5,1";
const RIGHT_TO_BOTTOM: &str = "M0.5,1 L.5,.5 L1,.5";
const MIDDLE_TO_TOP: &str = "M.5,.5 L.5,0";
const MIDDLE_TO_LEFT: &str = "M.5,.5 L0,.5";
const MIDDLE_TO_RIGHT: &str = "M.5,.5 L1,.5";
const MIDDLE_TO_BOTTOM: &str = "M.5,.5 L.5,1";
const T_TOP: &str = "M0,.5 L1,.5 M.5,.5 L.5,0";
const T_LEFT: &str = "M.5,0 L.5,1 M.5,.5 L0,.5";
const T_RIGHT: &str = "M.5,0 L.5,1 M.5,.5 L1,.5";
const T_BOTTOM: &str = "M0,.5 L1,.5 M.5,.5 L.5,1";
const CROSS: &str = "M0,.5 L1,.5 M.5,0 L.5,1";
const TWO_DASHES_HORIZONTAL: &str = "M.1,.5 L.4,.5 M.6,.5 L.9,.5";
const THREE_DASHES_HORIZONTAL: &str = "M.0667,.5 L.2667,.5 M.4,.5 L.6,.5 M.7333,.5 L.9333,.5";
const FOUR_DASHES_HORIZONTAL: &str = "M.05,.5 L.2,.5 M.3,.5 L.45,.5 M.55,.5 L.7,.5 M.8,.5 L.95,.5";
const TWO_DASHES_VERTICAL: &str = "M.5,.1 L.5,.4 M.5,.6 L.5,.9";
const THREE_DASHES_VERTICAL: &str = "M.5,.0667 L.5,.2667 M.5,.4 L.5,.6 M.5,.7333 L.5,.9333";
const FOUR_DASHES_VERTICAL: &str = "M.5,.05 L.5,.2 M.5,.3 L.5,.45 L.5,.55 M.5,.7 L.5,.95";

const NORMAL: u32 = 1;
const BOLD: u32 = 3;

#[derive(Clone, Copy)]
enum Path {
    Static(&'static str),
    /// Built from `xp`, `yp`: 15% of the cell width, on the x and y axis.
    Dynamic(fn(f64, f64) -> String),
}

use Path::{Dynamic as D, Static as S};

/// Line weight (1 normal, 3 bold) and path of each stroke, in ascending weight order.
fn box_drawing(c: char) -> Option<&'static [(u32, Path)]> {
    Some(match c {
        // Uniform normal and bold
        '─' => &[(NORMAL, S(LEFT_TO_RIGHT))],
        '━' => &[(BOLD, S(LEFT_TO_RIGHT))],
        '│' => &[(NORMAL, S(TOP_TO_BOTTOM))],
        '┃' => &[(BOLD, S(TOP_TO_BOTTOM))],
        '┌' => &[(NORMAL, S(RIGHT_TO_BOTTOM))],
        '┏' => &[(BOLD, S(RIGHT_TO_BOTTOM))],
        '┐' => &[(NORMAL, S(LEFT_TO_BOTTOM))],
        '┓' => &[(BOLD, S(LEFT_TO_BOTTOM))],
        '└' => &[(NORMAL, S(TOP_TO_RIGHT))],
        '┗' => &[(BOLD, S(TOP_TO_RIGHT))],
        '┘' => &[(NORMAL, S(TOP_TO_LEFT))],
        '┛' => &[(BOLD, S(TOP_TO_LEFT))],
        '├' => &[(NORMAL, S(T_RIGHT))],
        '┣' => &[(BOLD, S(T_RIGHT))],
        '┤' => &[(NORMAL, S(T_LEFT))],
        '┫' => &[(BOLD, S(T_LEFT))],
        '┬' => &[(NORMAL, S(T_BOTTOM))],
        '┳' => &[(BOLD, S(T_BOTTOM))],
        '┴' => &[(NORMAL, S(T_TOP))],
        '┻' => &[(BOLD, S(T_TOP))],
        '┼' => &[(NORMAL, S(CROSS))],
        '╋' => &[(BOLD, S(CROSS))],
        '╴' => &[(NORMAL, S(MIDDLE_TO_LEFT))],
        '╸' => &[(BOLD, S(MIDDLE_TO_LEFT))],
        '╵' => &[(NORMAL, S(MIDDLE_TO_TOP))],
        '╹' => &[(BOLD, S(MIDDLE_TO_TOP))],
        '╶' => &[(NORMAL, S(MIDDLE_TO_RIGHT))],
        '╺' => &[(BOLD, S(MIDDLE_TO_RIGHT))],
        '╷' => &[(NORMAL, S(MIDDLE_TO_BOTTOM))],
        '╻' => &[(BOLD, S(MIDDLE_TO_BOTTOM))],
        // Double border
        '═' => &[(
            NORMAL,
            D(|_, yp| {
                format!(
                    "M0,{} L1,{} M0,{} L1,{}",
                    0.5 - yp,
                    0.5 - yp,
                    0.5 + yp,
                    0.5 + yp
                )
            }),
        )],
        '║' => &[(
            NORMAL,
            D(|xp, _| {
                format!(
                    "M{},0 L{},1 M{},0 L{},1",
                    0.5 - xp,
                    0.5 - xp,
                    0.5 + xp,
                    0.5 + xp
                )
            }),
        )],
        '╒' => &[(
            NORMAL,
            D(|_, yp| {
                format!(
                    "M.5,1 L.5,{} L1,{} M.5,{} L1,{}",
                    0.5 - yp,
                    0.5 - yp,
                    0.5 + yp,
                    0.5 + yp
                )
            }),
        )],
        '╓' => &[(
            NORMAL,
            D(|xp, _| {
                format!(
                    "M{},1 L{},.5 L1,.5 M{},.5 L{},1",
                    0.5 - xp,
                    0.5 - xp,
                    0.5 + xp,
                    0.5 + xp
                )
            }),
        )],
        '╔' => &[(
            NORMAL,
            D(|xp, yp| {
                format!(
                    "M1,{} L{},{} L{},1 M1,{} L{},{} L{},1",
                    0.5 - yp,
                    0.5 - xp,
                    0.5 - yp,
                    0.5 - xp,
                    0.5 + yp,
                    0.5 + xp,
                    0.5 + yp,
                    0.5 + xp
                )
            }),
        )],
        '╕' => &[(
            NORMAL,
            D(|_, yp| {
                format!(
                    "M0,{} L.5,{} L.5,1 M0,{} L.5,{}",
                    0.5 - yp,
                    0.5 - yp,
                    0.5 + yp,
                    0.5 + yp
                )
            }),
        )],
        '╖' => &[(
            NORMAL,
            D(|xp, _| {
                format!(
                    "M{},1 L{},.5 L0,.5 M{},.5 L{},1",
                    0.5 + xp,
                    0.5 + xp,
                    0.5 - xp,
                    0.5 - xp
                )
            }),
        )],
        '╗' => &[(
            NORMAL,
            D(|xp, yp| {
                format!(
                    "M0,{} L{},{} L{},1 M0,{} L{},{} L{},1",
                    0.5 + yp,
                    0.5 - xp,
                    0.5 + yp,
                    0.5 - xp,
                    0.5 - yp,
                    0.5 + xp,
                    0.5 - yp,
                    0.5 + xp
                )
            }),
        )],
        '╘' => &[(
            NORMAL,
            D(|_, yp| {
                format!(
                    "M.5,0 L.5,{} L1,{} M.5,{} L1,{}",
                    0.5 + yp,
                    0.5 + yp,
                    0.5 - yp,
                    0.5 - yp
                )
            }),
        )],
        '╙' => &[(
            NORMAL,
            D(|xp, _| {
                format!(
                    "M1,.5 L{},.5 L{},0 M{},.5 L{},0",
                    0.5 - xp,
                    0.5 - xp,
                    0.5 + xp,
                    0.5 + xp
                )
            }),
        )],
        '╚' => &[(
            NORMAL,
            D(|xp, yp| {
                format!(
                    "M1,{} L{},{} L{},0 M1,{} L{},{} L{},0",
                    0.5 - yp,
                    0.5 + xp,
                    0.5 - yp,
                    0.5 + xp,
                    0.5 + yp,
                    0.5 - xp,
                    0.5 + yp,
                    0.5 - xp
                )
            }),
        )],
        '╛' => &[(
            NORMAL,
            D(|_, yp| {
                format!(
                    "M0,{} L.5,{} L.5,0 M0,{} L.5,{}",
                    0.5 + yp,
                    0.5 + yp,
                    0.5 - yp,
                    0.5 - yp
                )
            }),
        )],
        '╜' => &[(
            NORMAL,
            D(|xp, _| {
                format!(
                    "M0,.5 L{},.5 L{},0 M{},.5 L{},0",
                    0.5 + xp,
                    0.5 + xp,
                    0.5 - xp,
                    0.5 - xp
                )
            }),
        )],
        '╝' => &[(
            NORMAL,
            D(|xp, yp| {
                format!(
                    "M0,{} L{},{} L{},0 M0,{} L{},{} L{},0",
                    0.5 - yp,
                    0.5 - xp,
                    0.5 - yp,
                    0.5 - xp,
                    0.5 + yp,
                    0.5 + xp,
                    0.5 + yp,
                    0.5 + xp
                )
            }),
        )],
        '╞' => &[(
            NORMAL,
            D(|_, yp| {
                format!(
                    "{TOP_TO_BOTTOM} M.5,{} L1,{} M.5,{} L1,{}",
                    0.5 - yp,
                    0.5 - yp,
                    0.5 + yp,
                    0.5 + yp
                )
            }),
        )],
        '╟' => &[(
            NORMAL,
            D(|xp, _| {
                format!(
                    "M{},0 L{},1 M{},0 L{},1 M{},.5 L1,.5",
                    0.5 - xp,
                    0.5 - xp,
                    0.5 + xp,
                    0.5 + xp,
                    0.5 + xp
                )
            }),
        )],
        '╠' => &[(
            NORMAL,
            D(|xp, yp| {
                format!(
                    "M{},0 L{},1 M1,{} L{},{} L{},1 M1,{} L{},{} L{},0",
                    0.5 - xp,
                    0.5 - xp,
                    0.5 + yp,
                    0.5 + xp,
                    0.5 + yp,
                    0.5 + xp,
                    0.5 - yp,
                    0.5 + xp,
                    0.5 - yp,
                    0.5 + xp
                )
            }),
        )],
        '╡' => &[(
            NORMAL,
            D(|_, yp| {
                format!(
                    "{TOP_TO_BOTTOM} M0,{} L.5,{} M0,{} L.5,{}",
                    0.5 - yp,
                    0.5 - yp,
                    0.5 + yp,
                    0.5 + yp
                )
            }),
        )],
        '╢' => &[(
            NORMAL,
            D(|xp, _| {
                format!(
                    "M0,.5 L{},.5 M{},0 L{},1 M{},0 L{},1",
                    0.5 - xp,
                    0.5 - xp,
                    0.5 - xp,
                    0.5 + xp,
                    0.5 + xp
                )
            }),
        )],
        '╣' => &[(
            NORMAL,
            D(|xp, yp| {
                format!(
                    "M{},0 L{},1 M0,{} L{},{} L{},1 M0,{} L{},{} L{},0",
                    0.5 + xp,
                    0.5 + xp,
                    0.5 + yp,
                    0.5 - xp,
                    0.5 + yp,
                    0.5 - xp,
                    0.5 - yp,
                    0.5 - xp,
                    0.5 - yp,
                    0.5 - xp
                )
            }),
        )],
        '╤' => &[(
            NORMAL,
            D(|_, yp| {
                format!(
                    "M0,{} L1,{} M0,{} L1,{} M.5,{} L.5,1",
                    0.5 - yp,
                    0.5 - yp,
                    0.5 + yp,
                    0.5 + yp,
                    0.5 + yp
                )
            }),
        )],
        '╥' => &[(
            NORMAL,
            D(|xp, _| {
                format!(
                    "{LEFT_TO_RIGHT} M{},.5 L{},1 M{},.5 L{},1",
                    0.5 - xp,
                    0.5 - xp,
                    0.5 + xp,
                    0.5 + xp
                )
            }),
        )],
        '╦' => &[(
            NORMAL,
            D(|xp, yp| {
                format!(
                    "M0,{} L1,{} M0,{} L{},{} L{},1 M1,{} L{},{} L{},1",
                    0.5 - yp,
                    0.5 - yp,
                    0.5 + yp,
                    0.5 - xp,
                    0.5 + yp,
                    0.5 - xp,
                    0.5 + yp,
                    0.5 + xp,
                    0.5 + yp,
                    0.5 + xp
                )
            }),
        )],
        '╧' => &[(
            NORMAL,
            D(|_, yp| {
                format!(
                    "M.5,0 L.5,{} M0,{} L1,{} M0,{} L1,{}",
                    0.5 - yp,
                    0.5 - yp,
                    0.5 - yp,
                    0.5 + yp,
                    0.5 + yp
                )
            }),
        )],
        '╨' => &[(
            NORMAL,
            D(|xp, _| {
                format!(
                    "{LEFT_TO_RIGHT} M{},.5 L{},0 M{},.5 L{},0",
                    0.5 - xp,
                    0.5 - xp,
                    0.5 + xp,
                    0.5 + xp
                )
            }),
        )],
        '╩' => &[(
            NORMAL,
            D(|xp, yp| {
                format!(
                    "M0,{} L1,{} M0,{} L{},{} L{},0 M1,{} L{},{} L{},0",
                    0.5 + yp,
                    0.5 + yp,
                    0.5 - yp,
                    0.5 - xp,
                    0.5 - yp,
                    0.5 - xp,
                    0.5 - yp,
                    0.5 + xp,
                    0.5 - yp,
                    0.5 + xp
                )
            }),
        )],
        '╪' => &[(
            NORMAL,
            D(|_, yp| {
                format!(
                    "{TOP_TO_BOTTOM} M0,{} L1,{} M0,{} L1,{}",
                    0.5 - yp,
                    0.5 - yp,
                    0.5 + yp,
                    0.5 + yp
                )
            }),
        )],
        '╫' => &[(
            NORMAL,
            D(|xp, _| {
                format!(
                    "{LEFT_TO_RIGHT} M{},0 L{},1 M{},0 L{},1",
                    0.5 - xp,
                    0.5 - xp,
                    0.5 + xp,
                    0.5 + xp
                )
            }),
        )],
        '╬' => {
            &[(
                NORMAL,
                D(|xp, yp| {
                    format!("M0,{} L{},{} L{},1 M1,{} L{},{} L{},1 M0,{} L{},{} L{},0 M1,{} L{},{} L{},0", 0.5 + yp, 0.5 - xp, 0.5 + yp, 0.5 - xp, 0.5 + yp, 0.5 + xp, 0.5 + yp, 0.5 + xp, 0.5 - yp, 0.5 - xp, 0.5 - yp, 0.5 - xp, 0.5 - yp, 0.5 + xp, 0.5 - yp, 0.5 + xp)
                }),
            )]
        }
        // Diagonal
        '╱' => &[(NORMAL, S("M1,0 L0,1"))],
        '╲' => &[(NORMAL, S("M0,0 L1,1"))],
        '╳' => &[(NORMAL, S("M1,0 L0,1 M0,0 L1,1"))],
        // Mixed weight
        '╼' => &[(NORMAL, S(MIDDLE_TO_LEFT)), (BOLD, S(MIDDLE_TO_RIGHT))],
        '╽' => &[(NORMAL, S(MIDDLE_TO_TOP)), (BOLD, S(MIDDLE_TO_BOTTOM))],
        '╾' => &[(NORMAL, S(MIDDLE_TO_RIGHT)), (BOLD, S(MIDDLE_TO_LEFT))],
        '╿' => &[(NORMAL, S(MIDDLE_TO_BOTTOM)), (BOLD, S(MIDDLE_TO_TOP))],
        '┍' => &[(NORMAL, S(MIDDLE_TO_BOTTOM)), (BOLD, S(MIDDLE_TO_RIGHT))],
        '┎' => &[(NORMAL, S(MIDDLE_TO_RIGHT)), (BOLD, S(MIDDLE_TO_BOTTOM))],
        '┑' => &[(NORMAL, S(MIDDLE_TO_BOTTOM)), (BOLD, S(MIDDLE_TO_LEFT))],
        '┒' => &[(NORMAL, S(MIDDLE_TO_LEFT)), (BOLD, S(MIDDLE_TO_BOTTOM))],
        '┕' => &[(NORMAL, S(MIDDLE_TO_TOP)), (BOLD, S(MIDDLE_TO_RIGHT))],
        '┖' => &[(NORMAL, S(MIDDLE_TO_RIGHT)), (BOLD, S(MIDDLE_TO_TOP))],
        '┙' => &[(NORMAL, S(MIDDLE_TO_TOP)), (BOLD, S(MIDDLE_TO_LEFT))],
        '┚' => &[(NORMAL, S(MIDDLE_TO_LEFT)), (BOLD, S(MIDDLE_TO_TOP))],
        '┝' => &[(NORMAL, S(TOP_TO_BOTTOM)), (BOLD, S(MIDDLE_TO_RIGHT))],
        '┞' => &[(NORMAL, S(RIGHT_TO_BOTTOM)), (BOLD, S(MIDDLE_TO_TOP))],
        '┟' => &[(NORMAL, S(TOP_TO_RIGHT)), (BOLD, S(MIDDLE_TO_BOTTOM))],
        '┠' => &[(NORMAL, S(MIDDLE_TO_RIGHT)), (BOLD, S(TOP_TO_BOTTOM))],
        '┡' => &[(NORMAL, S(MIDDLE_TO_BOTTOM)), (BOLD, S(TOP_TO_RIGHT))],
        '┢' => &[(NORMAL, S(MIDDLE_TO_TOP)), (BOLD, S(RIGHT_TO_BOTTOM))],
        '┥' => &[(NORMAL, S(TOP_TO_BOTTOM)), (BOLD, S(MIDDLE_TO_LEFT))],
        '┦' => &[(NORMAL, S(LEFT_TO_BOTTOM)), (BOLD, S(MIDDLE_TO_TOP))],
        '┧' => &[(NORMAL, S(TOP_TO_LEFT)), (BOLD, S(MIDDLE_TO_BOTTOM))],
        '┨' => &[(NORMAL, S(MIDDLE_TO_LEFT)), (BOLD, S(TOP_TO_BOTTOM))],
        '┩' => &[(NORMAL, S(MIDDLE_TO_BOTTOM)), (BOLD, S(TOP_TO_LEFT))],
        '┪' => &[(NORMAL, S(MIDDLE_TO_TOP)), (BOLD, S(LEFT_TO_BOTTOM))],
        '┭' => &[(NORMAL, S(RIGHT_TO_BOTTOM)), (BOLD, S(MIDDLE_TO_LEFT))],
        '┮' => &[(NORMAL, S(LEFT_TO_BOTTOM)), (BOLD, S(MIDDLE_TO_RIGHT))],
        '┯' => &[(NORMAL, S(MIDDLE_TO_BOTTOM)), (BOLD, S(LEFT_TO_RIGHT))],
        '┰' => &[(NORMAL, S(LEFT_TO_RIGHT)), (BOLD, S(MIDDLE_TO_BOTTOM))],
        '┱' => &[(NORMAL, S(MIDDLE_TO_RIGHT)), (BOLD, S(LEFT_TO_BOTTOM))],
        '┲' => &[(NORMAL, S(MIDDLE_TO_LEFT)), (BOLD, S(RIGHT_TO_BOTTOM))],
        '┵' => &[(NORMAL, S(TOP_TO_RIGHT)), (BOLD, S(MIDDLE_TO_LEFT))],
        '┶' => &[(NORMAL, S(TOP_TO_LEFT)), (BOLD, S(MIDDLE_TO_RIGHT))],
        '┷' => &[(NORMAL, S(MIDDLE_TO_TOP)), (BOLD, S(LEFT_TO_RIGHT))],
        '┸' => &[(NORMAL, S(LEFT_TO_RIGHT)), (BOLD, S(MIDDLE_TO_TOP))],
        '┹' => &[(NORMAL, S(MIDDLE_TO_RIGHT)), (BOLD, S(TOP_TO_LEFT))],
        '┺' => &[(NORMAL, S(MIDDLE_TO_LEFT)), (BOLD, S(TOP_TO_RIGHT))],
        '┽' => &[
            (NORMAL, S("M.5,0 L.5,1 M.5,.5 L1,.5")),
            (BOLD, S(MIDDLE_TO_LEFT)),
        ],
        '┾' => &[
            (NORMAL, S("M.5,0 L.5,1 M.5,.5 L0,.5")),
            (BOLD, S(MIDDLE_TO_RIGHT)),
        ],
        '┿' => &[(NORMAL, S(TOP_TO_BOTTOM)), (BOLD, S(LEFT_TO_RIGHT))],
        '╀' => &[
            (NORMAL, S("M0,.5 L1,.5 M.5,.5 L.5,1")),
            (BOLD, S(MIDDLE_TO_TOP)),
        ],
        '╁' => &[
            (NORMAL, S("M.5,.5 L.5,0 M0,.5 L1,.5")),
            (BOLD, S(MIDDLE_TO_BOTTOM)),
        ],
        '╂' => &[(NORMAL, S(LEFT_TO_RIGHT)), (BOLD, S(TOP_TO_BOTTOM))],
        '╃' => &[(NORMAL, S(RIGHT_TO_BOTTOM)), (BOLD, S(TOP_TO_LEFT))],
        '╄' => &[(NORMAL, S(LEFT_TO_BOTTOM)), (BOLD, S(TOP_TO_RIGHT))],
        '╅' => &[(NORMAL, S(TOP_TO_RIGHT)), (BOLD, S(LEFT_TO_BOTTOM))],
        '╆' => &[(NORMAL, S(TOP_TO_LEFT)), (BOLD, S(RIGHT_TO_BOTTOM))],
        '╇' => &[
            (NORMAL, S(MIDDLE_TO_BOTTOM)),
            (BOLD, S("M.5,.5 L.5,0 M0,.5 L1,.5")),
        ],
        '╈' => &[
            (NORMAL, S(MIDDLE_TO_TOP)),
            (BOLD, S("M0,.5 L1,.5 M.5,.5 L.5,1")),
        ],
        '╉' => &[
            (NORMAL, S(MIDDLE_TO_RIGHT)),
            (BOLD, S("M.5,0 L.5,1 M.5,.5 L0,.5")),
        ],
        '╊' => &[
            (NORMAL, S(MIDDLE_TO_LEFT)),
            (BOLD, S("M.5,0 L.5,1 M.5,.5 L1,.5")),
        ],
        // Dashed
        '╌' => &[(NORMAL, S(TWO_DASHES_HORIZONTAL))],
        '╍' => &[(BOLD, S(TWO_DASHES_HORIZONTAL))],
        '┄' => &[(NORMAL, S(THREE_DASHES_HORIZONTAL))],
        '┅' => &[(BOLD, S(THREE_DASHES_HORIZONTAL))],
        '┈' => &[(NORMAL, S(FOUR_DASHES_HORIZONTAL))],
        '┉' => &[(BOLD, S(FOUR_DASHES_HORIZONTAL))],
        '╎' => &[(NORMAL, S(TWO_DASHES_VERTICAL))],
        '╏' => &[(BOLD, S(TWO_DASHES_VERTICAL))],
        '┆' => &[(NORMAL, S(THREE_DASHES_VERTICAL))],
        '┇' => &[(BOLD, S(THREE_DASHES_VERTICAL))],
        '┊' => &[(NORMAL, S(FOUR_DASHES_VERTICAL))],
        '┋' => &[(BOLD, S(FOUR_DASHES_VERTICAL))],
        // Curved
        '╭' => &[(
            NORMAL,
            D(|_, yp| {
                let v = 0.5 + (yp / 0.15 * 0.5);
                format!("M.5,1 L.5,{v} C.5,{v},.5,.5,1,.5")
            }),
        )],
        '╮' => &[(
            NORMAL,
            D(|_, yp| {
                let v = 0.5 + (yp / 0.15 * 0.5);
                format!("M.5,1 L.5,{v} C.5,{v},.5,.5,0,.5")
            }),
        )],
        '╯' => &[(
            NORMAL,
            D(|_, yp| {
                let v = 0.5 - (yp / 0.15 * 0.5);
                format!("M.5,0 L.5,{v} C.5,{v},.5,.5,0,.5")
            }),
        )],
        '╰' => &[(
            NORMAL,
            D(|_, yp| {
                let v = 0.5 - (yp / 0.15 * 0.5);
                format!("M.5,0 L.5,{v} C.5,{v},.5,.5,1,.5")
            }),
        )],
        _ => return None,
    })
}

/// A Powerline symbol: path, filled or stroked, and left/right padding in half line widths.
struct Vector {
    d: &'static str,
    stroke: bool,
    left_padding: f64,
    right_padding: f64,
}

const fn fill(d: &'static str, left_padding: f64, right_padding: f64) -> Vector {
    Vector {
        d,
        stroke: false,
        left_padding,
        right_padding,
    }
}

const fn stroke(d: &'static str, left_padding: f64, right_padding: f64) -> Vector {
    Vector {
        d,
        stroke: true,
        left_padding,
        right_padding,
    }
}

/// Original symbols defined in https://github.com/powerline/fontpatcher.
fn powerline(c: char) -> Option<Vector> {
    Some(match c {
        // Git branch
        '\u{E0A0}' => fill("M.3,1 L.03,1 L.03,.88 C.03,.82,.06,.78,.11,.73 C.15,.7,.2,.68,.28,.65 L.43,.6 C.49,.58,.53,.56,.56,.53 C.59,.5,.6,.47,.6,.43 L.6,.27 L.4,.27 L.69,.1 L.98,.27 L.78,.27 L.78,.46 C.78,.52,.76,.56,.72,.61 C.68,.66,.63,.67,.56,.7 L.48,.72 C.42,.74,.38,.76,.35,.78 C.32,.8,.31,.84,.31,.88 L.31,1 M.3,.5 L.03,.59 L.03,.09 L.3,.09 L.3,.655", 0., 0.),
        // L N
        '\u{E0A1}' => fill("M.7,.4 L.7,.47 L.2,.47 L.2,.03 L.355,.03 L.355,.4 L.705,.4 M.7,.5 L.86,.5 L.86,.95 L.69,.95 L.44,.66 L.46,.86 L.46,.95 L.3,.95 L.3,.49 L.46,.49 L.71,.78 L.69,.565 L.69,.5", 0., 0.),
        // Lock
        '\u{E0A2}' => fill("M.25,.94 C.16,.94,.11,.92,.11,.87 L.11,.53 C.11,.48,.15,.455,.23,.45 L.23,.3 C.23,.25,.26,.22,.31,.19 C.36,.16,.43,.15,.51,.15 C.59,.15,.66,.16,.71,.19 C.77,.22,.79,.26,.79,.3 L.79,.45 C.87,.45,.91,.48,.91,.53 L.91,.87 C.91,.92,.86,.94,.77,.94 L.24,.94 M.53,.2 C.49,.2,.45,.21,.42,.23 C.39,.25,.38,.27,.38,.3 L.38,.45 L.68,.45 L.68,.3 C.68,.27,.67,.25,.64,.23 C.61,.21,.58,.2,.53,.2 M.58,.82 L.58,.66 C.63,.65,.65,.63,.65,.6 C.65,.58,.64,.57,.61,.56 C.58,.55,.56,.54,.52,.54 C.48,.54,.46,.55,.43,.56 C.4,.57,.39,.59,.39,.6 C.39,.63,.41,.64,.46,.66 L.46,.82 L.57,.82", 0., 0.),
        // Right triangle solid
        '\u{E0B0}' => fill("M0,0 L1,.5 L0,1", 0., 2.),
        // Right triangle line
        '\u{E0B1}' => stroke("M-1,-.5 L1,.5 L-1,1.5", 1., 1.),
        // Left triangle solid
        '\u{E0B2}' => fill("M1,0 L0,.5 L1,1", 2., 0.),
        // Left triangle line
        '\u{E0B3}' => stroke("M2,-.5 L0,.5 L2,1.5", 1., 1.),
        // Right semi-circle solid
        '\u{E0B4}' => fill("M0,0 L0,1 C0.552,1,1,0.776,1,.5 C1,0.224,0.552,0,0,0", 0., 1.),
        // Right semi-circle line
        '\u{E0B5}' => stroke("M.2,1 C.422,1,.8,.826,.78,.5 C.8,.174,0.422,0,.2,0", 0., 1.),
        // Left semi-circle solid
        '\u{E0B6}' => fill("M1,0 L1,1 C0.448,1,0,0.776,0,.5 C0,0.224,0.448,0,1,0", 1., 0.),
        // Left semi-circle line
        '\u{E0B7}' => stroke("M.8,1 C0.578,1,0.2,.826,.22,.5 C0.2,0.174,0.578,0,0.8,0", 1., 0.),
        // Lower left triangle
        '\u{E0B8}' => fill("M-.5,-.5 L1.5,1.5 L-.5,1.5", 0., 0.),
        // Backslash separator (and its redundant twin)
        '\u{E0B9}' | '\u{E0BF}' => stroke("M-.5,-.5 L1.5,1.5", 1., 1.),
        // Lower right triangle
        '\u{E0BA}' => fill("M1.5,-.5 L-.5,1.5 L1.5,1.5", 0., 0.),
        // Upper left triangle
        '\u{E0BC}' => fill("M1.5,-.5 L-.5,1.5 L-.5,-.5", 0., 0.),
        // Forward slash separator (and its redundant twin)
        '\u{E0BD}' | '\u{E0BB}' => stroke("M1.5,-.5 L-.5,1.5", 1., 1.),
        // Upper right triangle
        '\u{E0BE}' => fill("M-.5,-.5 L1.5,1.5 L1.5,-.5", 0., 0.),
        _ => return None,
    })
}

/// Draws `chars` as a custom glyph if it is one; returns whether it was.
#[allow(clippy::too_many_arguments)]
pub fn try_draw_custom_char(
    ctx: &CanvasRenderingContext2d,
    chars: &str,
    x_offset: f64,
    y_offset: f64,
    cell_width: f64,
    cell_height: f64,
    font_size: f64,
    dpr: f64,
) -> bool {
    let mut it = chars.chars();
    let (Some(c), None) = (it.next(), it.next()) else {
        return false;
    };
    if let Some(blocks) = block_element(c) {
        let (xe, ye) = (cell_width / 8.0, cell_height / 8.0);
        for &(x, y, w, h) in blocks {
            ctx.fill_rect(x_offset + x * xe, y_offset + y * ye, w * xe, h * ye);
        }
        return true;
    }
    if let Some(pattern) = pattern(c) {
        draw_pattern(ctx, c, pattern, x_offset, y_offset, cell_width, cell_height);
        return true;
    }
    if let Some(strokes) = box_drawing(c) {
        stroke_like_fill(ctx);
        for &(weight, path) in strokes {
            ctx.begin_path();
            ctx.set_line_width(dpr * weight as f64);
            let instructions = match path {
                Path::Static(d) => d.to_string(),
                Path::Dynamic(f) => f(0.15, 0.15 / cell_height * cell_width),
            };
            run_path(
                ctx,
                &instructions,
                cell_width,
                cell_height,
                x_offset,
                y_offset,
                true,
                dpr,
                0.0,
                0.0,
            );
            ctx.stroke();
            ctx.close_path();
        }
        return true;
    }
    if let Some(vector) = powerline(c) {
        // Clip the cell to make sure drawing doesn't occur beyond bounds
        if let Ok(clip) = Path2d::new() {
            clip.rect(x_offset, y_offset, cell_width, cell_height);
            ctx.clip_with_path_2d(&clip);
        }
        ctx.begin_path();
        // Scale the stroke with DPR and font size
        let css_line_width = font_size / 12.0;
        ctx.set_line_width(dpr * css_line_width);
        run_path(
            ctx,
            vector.d,
            cell_width,
            cell_height,
            x_offset,
            y_offset,
            false,
            dpr,
            vector.left_padding * (css_line_width / 2.0),
            vector.right_padding * (css_line_width / 2.0),
        );
        if vector.stroke {
            stroke_like_fill(ctx);
            ctx.stroke();
        } else {
            ctx.fill();
        }
        ctx.close_path();
        return true;
    }
    false
}

thread_local! {
    /// Shade patterns by character and fill style.
    static PATTERNS: RefCell<HashMap<(char, String), CanvasPattern>> = RefCell::new(HashMap::new());
}

fn draw_pattern(
    ctx: &CanvasRenderingContext2d,
    c: char,
    definition: Pattern,
    x_offset: f64,
    y_offset: f64,
    cell_width: f64,
    cell_height: f64,
) {
    let Some(fill_style) = ctx.fill_style().as_string() else {
        return;
    };
    let key = (c, fill_style);
    let cached = PATTERNS.with(|p| p.borrow().get(&key).cloned());
    let pattern = match cached {
        Some(pattern) => pattern,
        None => {
            let Some(pattern) = create_pattern(ctx, definition, &key.1) else {
                return;
            };
            PATTERNS.with(|p| p.borrow_mut().insert(key, pattern.clone()));
            pattern
        }
    };
    ctx.set_fill_style_canvas_pattern(&pattern);
    ctx.fill_rect(x_offset, y_offset, cell_width, cell_height);
}

fn create_pattern(
    ctx: &CanvasRenderingContext2d,
    definition: Pattern,
    fill_style: &str,
) -> Option<CanvasPattern> {
    let width = definition.first()?.len();
    let height = definition.len();
    let (r, g, b, a) = if let Some(hex) = fill_style.strip_prefix('#') {
        let part = |i: usize| {
            hex.get(i..i + 2)
                .and_then(|s| u32::from_str_radix(s, 16).ok())
                .unwrap_or(0) as f64
        };
        let a = if hex.len() > 6 && part(6) != 0.0 {
            part(6)
        } else {
            1.0
        };
        (part(0), part(2), part(4), a)
    } else if fill_style.starts_with("rgba") {
        let inner = fill_style.get(5..fill_style.len().saturating_sub(1))?;
        let v: Vec<f64> = inner
            .split(',')
            .map(|e| e.trim().parse().unwrap_or(0.0))
            .collect();
        (
            *v.first()?,
            *v.get(1)?,
            *v.get(2)?,
            v.get(3).copied().unwrap_or(1.0),
        )
    } else {
        return None;
    };
    let clamp = |v: f64| v.round_ties_even().clamp(0.0, 255.0) as u8;
    let mut data = vec![0u8; width * height * 4];
    for (y, row) in definition.iter().enumerate() {
        for (x, &filled) in row.iter().enumerate() {
            let i = (y * width + x) * 4;
            data[i] = clamp(r);
            data[i + 1] = clamp(g);
            data[i + 2] = clamp(b);
            data[i + 3] = clamp(filled as f64 * (a * 255.0));
        }
    }
    let document = ctx.canvas()?.owner_document()?;
    let tmp: HtmlCanvasElement = document.create_element("canvas").ok()?.dyn_into().ok()?;
    tmp.set_width(width as u32);
    tmp.set_height(height as u32);
    let tmp_ctx: CanvasRenderingContext2d = tmp.get_context("2d").ok()??.dyn_into().ok()?;
    let image =
        ImageData::new_with_u8_clamped_array_and_sh(Clamped(&data), width as u32, height as u32)
            .ok()?;
    tmp_ctx.put_image_data(&image, 0.0, 0.0).ok()?;
    ctx.create_pattern_with_html_canvas_element(&tmp, "")
        .ok()
        .flatten()
}

/// Runs SVG `d` instructions (`M`, `L`, `C`) on the canvas in cell coordinates.
#[allow(clippy::too_many_arguments)]
fn run_path(
    ctx: &CanvasRenderingContext2d,
    d: &str,
    cell_width: f64,
    cell_height: f64,
    x_offset: f64,
    y_offset: f64,
    clamp: bool,
    dpr: f64,
    left_padding: f64,
    right_padding: f64,
) {
    for instruction in d.split(' ') {
        let mut chars = instruction.chars();
        let Some(kind) = chars.next() else { continue };
        let args: Vec<&str> = chars.as_str().split(',').collect();
        if args.len() < 2 || args[0].is_empty() || args[1].is_empty() {
            continue;
        }
        let a = translate_args(
            &args,
            cell_width,
            cell_height,
            x_offset,
            y_offset,
            clamp,
            dpr,
            left_padding,
            right_padding,
        );
        match (kind, a.as_slice()) {
            ('C', [x1, y1, x2, y2, x, y, ..]) => ctx.bezier_curve_to(*x1, *y1, *x2, *y2, *x, *y),
            ('L', [x, y, ..]) => ctx.line_to(*x, *y),
            ('M', [x, y, ..]) => ctx.move_to(*x, *y),
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn translate_args(
    args: &[&str],
    cell_width: f64,
    cell_height: f64,
    x_offset: f64,
    y_offset: f64,
    clamp: bool,
    dpr: f64,
    left_padding: f64,
    right_padding: f64,
) -> Vec<f64> {
    let crisp = |v: f64, max: f64| (js_round(v + 0.5) - 0.5).min(max).max(0.0);
    args.iter()
        .enumerate()
        .map(|(i, e)| {
            let mut v: f64 = e.parse().unwrap_or(0.0);
            if i % 2 == 0 {
                // Translate from 0-1 to 0-cellWidth
                v *= cell_width - (left_padding * dpr) - (right_padding * dpr);
                // Keep the coordinate in the cell, at the nearest 0.5 for a crisp line at 100% devicePixelRatio
                if clamp && v != 0.0 {
                    v = crisp(v, cell_width);
                }
                v + x_offset + (left_padding * dpr)
            } else {
                v *= cell_height;
                if clamp && v != 0.0 {
                    v = crisp(v, cell_height);
                }
                v + y_offset
            }
        })
        .collect()
}

/// `ctx.strokeStyle = ctx.fillStyle`.
pub fn stroke_like_fill(ctx: &CanvasRenderingContext2d) {
    if let Some(style) = ctx.fill_style().as_string() {
        ctx.set_stroke_style_str(&style);
    }
}
