//! xterm.js 6 option defaults. A terminal stores only the options the user set (plus `cols`, `rows` and
//! `scrollback`, which the engine takes); `term.options` reads the rest from one static table through a `get`
//! trap shared by every terminal's options proxy, so no terminal holds a copy of the defaults.
use js_sys::{Function, Object, Reflect};
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use crate::engine::get;

enum Value {
    Bool(bool),
    Number(f64),
    Str(&'static str),
    Null,
    /// A new `{}` per read.
    Object,
    /// `{ showScrollbar: true }`.
    Scrollbar,
    /// xterm.js `isMac`: `navigator.platform` is a Mac one.
    IsMac,
}

/// xterm.js `DEFAULT_OPTIONS` (src/common/services/OptionsService.ts, as shipped in @xterm/xterm 6.1.0-beta.336),
/// in its order.
const DEFAULTS: [(&str, Value); 48] = [
    ("cols", Value::Number(80.0)),
    ("rows", Value::Number(24.0)),
    ("showCursorImmediately", Value::Bool(false)),
    ("cursorBlink", Value::Bool(false)),
    ("blinkIntervalDuration", Value::Number(0.0)),
    ("cursorStyle", Value::Str("block")),
    ("cursorWidth", Value::Number(1.0)),
    ("cursorInactiveStyle", Value::Str("outline")),
    ("drawBoldTextInBrightColors", Value::Bool(true)),
    ("documentOverride", Value::Null),
    ("fastScrollSensitivity", Value::Number(5.0)),
    ("fontFamily", Value::Str("monospace")),
    ("fontSize", Value::Number(15.0)),
    ("fontWeight", Value::Str("normal")),
    ("fontWeightBold", Value::Str("bold")),
    ("ignoreBracketedPasteMode", Value::Bool(false)),
    ("lineHeight", Value::Number(1.0)),
    ("letterSpacing", Value::Number(0.0)),
    ("linkHandler", Value::Null),
    ("logLevel", Value::Str("info")),
    ("logger", Value::Null),
    ("scrollback", Value::Number(1000.0)),
    ("scrollbar", Value::Scrollbar),
    ("scrollOnEraseInDisplay", Value::Bool(false)),
    ("scrollOnUserInput", Value::Bool(true)),
    ("scrollSensitivity", Value::Number(1.0)),
    ("screenReaderMode", Value::Bool(false)),
    ("smoothScrollDuration", Value::Number(0.0)),
    ("macOptionIsMeta", Value::Bool(false)),
    ("macOptionClickForcesSelection", Value::Bool(false)),
    ("minimumContrastRatio", Value::Number(1.0)),
    ("mouseEventsRequireAlt", Value::Bool(false)),
    ("disableStdin", Value::Bool(false)),
    ("allowProposedApi", Value::Bool(false)),
    ("allowTransparency", Value::Bool(false)),
    ("tabStopWidth", Value::Number(8.0)),
    ("theme", Value::Object),
    ("reflowCursorLine", Value::Bool(false)),
    ("rescaleOverlappingGlyphs", Value::Bool(false)),
    ("rightClickSelectsWord", Value::IsMac),
    ("windowOptions", Value::Object),
    ("windowsPty", Value::Object),
    ("wordSeparator", Value::Str(" ()[]{}',\"`")),
    ("altClickMovesCursor", Value::Bool(true)),
    ("convertEol", Value::Bool(false)),
    ("termName", Value::Str("xterm")),
    ("quirks", Value::Object),
    ("vtExtensions", Value::Object),
];

/// The xterm.js default of option `key`, or `undefined` for keys xterm.js does not know.
pub(crate) fn default_option(key: &str) -> JsValue {
    let Some((_, value)) = DEFAULTS.iter().find(|(k, _)| *k == key) else {
        return JsValue::UNDEFINED;
    };
    match value {
        Value::Bool(b) => (*b).into(),
        Value::Number(n) => (*n).into(),
        Value::Str(s) => (*s).into(),
        Value::Null => JsValue::NULL,
        Value::Object => Object::new().into(),
        Value::Scrollbar => {
            let scrollbar = Object::new();
            let _ = Reflect::set(&scrollbar, &"showScrollbar".into(), &true.into());
            scrollbar.into()
        }
        Value::IsMac => {
            let platform = get(&get(&js_sys::global(), "navigator"), "platform").as_string();
            matches!(
                platform.as_deref(),
                Some("Macintosh" | "MacIntel" | "MacPPC" | "Mac68K")
            )
            .into()
        }
    }
}

/// Option `key` of a terminal's options object: what the user set, else the xterm.js default. Setting an option
/// to `undefined` brings its default back, like xterm.js.
pub(crate) fn option(options: &JsValue, key: &str) -> JsValue {
    let value = get(options, key);
    if value.is_undefined() {
        default_option(key)
    } else {
        value
    }
}

thread_local! {
    /// Lives as long as the page; the GC owns the closure (`into_js_value`).
    static GET_TRAP: Function = Closure::<dyn Fn(JsValue, JsValue) -> JsValue>::new(|target: JsValue, key: JsValue| {
        match key.as_string() {
            Some(key) => option(&target, &key),
            // Symbols (`Symbol.toPrimitive`, inspection) read the target.
            None => Reflect::get(&target, &key).unwrap_or(JsValue::UNDEFINED),
        }
    })
    .into_js_value()
    .unchecked_into();
}

/// The `get` trap of the options proxy, the same function for every terminal.
pub(crate) fn get_trap() -> Function {
    GET_TRAP.with(Function::clone)
}
