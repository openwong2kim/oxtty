// Page for gen-input-fixtures.mjs: one real xterm.js 6.1 Terminal whose input internals are wrapped to record what
// they get and what they send, for the parity fixtures of the Rust encoders (crates/wongtty/src/input).
// `?platform=` overrides navigator.platform before xterm.js reads it (isMac, isWindows).
const platform = new URL(import.meta.url).searchParams.get('platform');
if (platform) Object.defineProperty(Navigator.prototype, 'platform', { get: () => platform, configurable: true });
const { Terminal } = await import('@xterm/xterm');

const MODS = ev => (ev.shiftKey ? 1 : 0) | (ev.altKey ? 2 : 0) | (ev.ctrlKey ? 4 : 0) | (ev.metaKey ? 8 : 0);
const result = r => (r ? [r.type, r.cancel ? 1 : 0, r.key ?? null] : null);

let term;
let out = [];
let events = [];

/** Modes and sizes as the encoders read them. */
function state() {
  const core = term._core;
  const modes = core.coreService.decPrivateModes;
  const mouse = core._mouseService._mouseStateService;
  return {
    appCursor: !!modes.applicationCursorKeys,
    appKeypad: !!modes.applicationKeypad,
    bracketedPaste: !!modes.bracketedPasteMode,
    sendFocus: !!modes.sendFocus,
    kittyFlags: core.coreService.kittyKeyboard.flags,
    protocol: mouse.activeProtocol,
    encoding: mouse.activeEncoding,
    alt: term.buffer.active.type === 'alternate',
  };
}

/** The bytes sent while `fn` runs. */
function sending(fn) {
  const from = out.length;
  const value = fn();
  return [value, out.slice(from)];
}

function wrapKeys(core) {
  const key = (type, ev, extra) => events.push([type, ev.key, ev.code, ev.keyCode, ev.charCode, ev.location, MODS(ev),
    ev.repeat ? 1 : 0, ev.getModifierState?.('AltGraph') ? 1 : 0, ...extra]);
  const wrap = (name, record) => {
    const original = core[name];
    core[name] = function (ev) {
      record(ev);
      return original.call(this, ev);
    };
  };
  wrap('_keyDown', ev => key('keydown', ev, [result(core._keyboardService.evaluateKeyDown(ev))]));
  wrap('_keyUp', ev => key('keyup', ev, [result(core._keyboardService.evaluateKeyUp(ev))]));
  wrap('_keyPress', ev => key('keypress', ev, [ev.which ?? null]));
  wrap('_inputEvent', ev => events.push(['input', ev.data, ev.inputType, ev.composed ? 1 : 0]));
}

/**
 * The modes and options the mouse reads: [protocol, encoding, application cursor keys, alternate screen,
 * scrollSensitivity, fastScrollSensitivity, mouseEventsRequireAlt].
 */
function mouseState() {
  const s = state();
  const o = term.options;
  return [s.protocol, s.encoding, s.appCursor ? 1 : 0, s.alt ? 1 : 0, o.scrollSensitivity, o.fastScrollSensitivity, o.mouseEventsRequireAlt ? 1 : 0];
}

function wrapMouse(core) {
  const service = core._mouseService;
  const pos = (ctx, ev) => {
    const p = service._mouseCoordsService.getMouseReportCoords(ev, ctx.target.screenElement);
    return p ? [p.col, p.row, p.x, p.y] : null;
  };
  const sendEvent = service._sendEvent;
  service._sendEvent = function (ctx, ev) {
    if (ev.type === 'wheel') return sendEvent.call(this, ctx, ev);
    const at = pos(ctx, ev);
    const s = mouseState();
    const [sent, bytes] = sending(() => sendEvent.call(this, ctx, ev));
    events.push(['mouse', ev.type, ev.button, ev.buttons, MODS(ev), at, s, bytes]);
    return sent;
  };
  const wheel = (name, reporting) => {
    const original = service[name];
    service[name] = function (ctx, ev) {
      if (!!ctx.requestedEvents.wheel !== reporting) return original.call(this, ctx, ev);
      const at = pos(ctx, ev);
      const s = mouseState();
      const geometry = [term.cols, term.rows, core._renderService.dimensions.device.cell.height, window.devicePixelRatio];
      const [value, bytes] = sending(() => original.call(this, ctx, ev));
      events.push(['wheel', ev.deltaY, ev.deltaMode, MODS(ev), at, s, geometry, bytes, ev.defaultPrevented ? 1 : 0]);
      return value;
    };
  };
  wheel('_handleWheel', true);
  wheel('_handlePassiveWheel', false);
}

const api = {
  /** A fresh terminal with `options`. */
  async setup(options = {}) {
    term?.dispose();
    const parent = document.getElementById('terminal');
    parent.replaceChildren();
    term = new Terminal({ cols: 40, rows: 6, fontFamily: 'Menlo, monospace', fontSize: 14, ...options });
    term.open(parent);
    out = [];
    events = [];
    term.onData(d => out.push(['data', d]));
    term.onBinary(d => out.push(['binary', d]));
    wrapKeys(term._core);
    wrapMouse(term._core);
    await new Promise(resolve => requestAnimationFrame(resolve));
  },
  write: data => new Promise(resolve => term.write(data, resolve)),
  setOption(key, value) {
    term.options[key] = value;
  },
  focus: () => term.focus(),
  blur: () => term.blur(),
  paste: text => term.paste(text),
  state,
  /** What was recorded and sent since the last call. */
  take() {
    const taken = { events, out };
    events = [];
    out = [];
    return taken;
  },
  /** Client coordinates of the centre of a cell. */
  point(col, row) {
    const rect = term.element.querySelector('.xterm-screen').getBoundingClientRect();
    return [rect.left + (col + 0.5) * rect.width / term.cols, rect.top + (row + 0.5) * rect.height / term.rows];
  },
  /** xterm.js `evaluateKeyDown` (presses, repeats) or `evaluateKeyUp` (releases) for plain event objects. */
  evaluate(list) {
    const service = term._core._keyboardService;
    return list.map(([type, ev]) => result(type === 'release' ? service.evaluateKeyUp(ev) : service.evaluateKeyDown(ev)));
  },
  /** A script-built wheel (CDP sends pixel deltas only) at a cell. */
  syntheticWheel(col, row, init) {
    const [clientX, clientY] = api.point(col, row);
    term.element.querySelector('.xterm-screen').dispatchEvent(new WheelEvent('wheel', { ...init, clientX, clientY, bubbles: true, cancelable: true }));
  },
};

window.api = api;
window.ready = true;
