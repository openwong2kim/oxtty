// wongtty — terminal emulator engine (Rust → WebAssembly).
// Works in browsers, bundlers (Vite/webpack resolve the .wasm URL) and Node ≥ 18.

const ABI = 14;
const encoder = new TextEncoder();
const decoder = new TextDecoder();
// Input buffer: smallest allocation, and the largest one kept between writes.
const IN_MIN = 4096;
const IN_KEEP = 256 * 1024;

/** @type {any} */
let wasm = null;
let initPromise = null;
// The compiled module, kept to rebuild the instance after a trap.
let wasmModule = null;
// Bumped each time the instance is rebuilt; terminals from an older one start over.
let generation = 0;

/**
 * Loads the WebAssembly module once. Call before creating terminals.
 * @param {URL | string | ArrayBuffer | ArrayBufferView | WebAssembly.Module | Response} [source]
 */
export function init(source) {
  initPromise ??= load(source).catch((err) => {
    initPromise = null;
    throw err;
  });
  return initPromise;
}

async function load(source) {
  source ??= new URL('./wongtty.wasm', import.meta.url);
  let module;
  let instance;
  if (source instanceof WebAssembly.Module) {
    module = source;
    instance = await WebAssembly.instantiate(source, {});
  } else if (source instanceof ArrayBuffer || ArrayBuffer.isView(source)) {
    ({ module, instance } = await WebAssembly.instantiate(source, {}));
  } else {
    const response = typeof Response !== 'undefined' && source instanceof Response
      ? source
      : await fetchWasm(source);
    if (response instanceof Uint8Array) {
      ({ module, instance } = await WebAssembly.instantiate(response, {}));
    } else if (WebAssembly.instantiateStreaming &&
      response.headers.get('content-type')?.startsWith('application/wasm')) {
      ({ module, instance } = await WebAssembly.instantiateStreaming(response, {}));
    } else {
      ({ module, instance } = await WebAssembly.instantiate(await response.arrayBuffer(), {}));
    }
  }
  const abi = instance.exports.wongtty_abi();
  if (abi !== ABI) throw new Error(`wongtty: wasm ABI ${abi} does not match JS wrapper ABI ${ABI}`);
  wasmModule = module;
  wasm = guard(instance.exports);
}

// A Rust panic aborts the whole instance (every terminal in it) with a
// WebAssembly.RuntimeError. Each export is wrapped so that a trap rebuilds the
// instance from the same module before the error reaches the caller; terminals
// notice the new generation on their next call and start over empty.
function guard(exports) {
  const guarded = { memory: exports.memory };
  for (const [name, fn] of Object.entries(exports)) {
    if (typeof fn !== 'function') continue;
    guarded[name] = (a, b, c, d) => {
      try {
        return fn(a, b, c, d);
      } catch (err) {
        if (err instanceof WebAssembly.RuntimeError) rebuild();
        throw err;
      }
    };
  }
  return guarded;
}

function rebuild() {
  wasm = guard(new WebAssembly.Instance(wasmModule, {}).exports);
  generation++;
}

async function fetchWasm(source) {
  const base = globalThis.location?.href ?? import.meta.url;
  const url = source instanceof URL ? source : new URL(String(source), base);
  if (url.protocol === 'file:') {
    // Keep the name in a variable so browser bundlers do not follow the node: module.
    const fsModule = 'node:fs/promises';
    const { readFile } = await import(/* @vite-ignore */ /* webpackIgnore: true */ fsModule);
    return new Uint8Array(await readFile(url));
  }
  const response = await fetch(url);
  if (!response.ok) throw new Error(`wongtty: failed to fetch ${url} (${response.status})`);
  return response;
}

function requireWasm() {
  if (!wasm) throw new Error('wongtty: call `await init()` before creating a Terminal');
  return wasm;
}

const bytesOut = (len) => new Uint8Array(wasm.memory.buffer, wasm.wongtty_out_ptr(), len).slice();
const textOut = (len) => (len <= 0 ? '' : decoder.decode(new Uint8Array(wasm.memory.buffer, wasm.wongtty_out_ptr(), len)));
const wordsOut = (n) => (n <= 0 ? new Uint32Array(0) : new Uint32Array(wasm.memory.buffer, wasm.wongtty_out32_ptr(), n).slice());

/** Rows `textRows` asks the engine for at a time. */
const TEXT_BLOCK = 1024;

/** `@xterm/addon-search` `NON_WORD_CHARACTERS`: what ends a word for `wholeWord`. */
const NON_WORD = ' ~!@#$%^&*()+`-=[]{}|\\;:"\',./<>?';
const CELL_WIDE = 0x80000000;

const isWholeWord = (text, at, len) =>
  (at === 0 || NON_WORD.includes(text[at - 1])) && (at + len === text.length || NON_WORD.includes(text[at + len]));

// The first regex match in `text` at or after unit `from`, as [index, length]; empty matches are skipped.
function regexNext(re, text, from, wholeWord) {
  for (let at = from; at <= text.length;) {
    re.lastIndex = at;
    const m = re.exec(text);
    if (!m) return null;
    if (m[0].length && (!wholeWord || isWholeWord(text, m.index, m[0].length))) return [m.index, m[0].length];
    at = m.index + 1;
  }
  return null;
}

// The last regex match in `text` that ends at or before unit `before`.
function regexPrevious(re, text, before, wholeWord) {
  let last = null;
  for (let at = 0; at < before;) {
    const found = regexNext(re, text, at, wholeWord);
    if (!found || found[0] + found[1] > before) {
      if (!found || found[0] >= before) break;
      at = found[0] + 1;
      continue;
    }
    last = found;
    at = found[0] + 1;
  }
  return last;
}

// [index, length] in a search line's units to a match in cells.
function lineMatch(line, [at, len], cols) {
  const start = line.cells[at] & ~CELL_WIDE;
  const lastCell = line.cells[at + len - 1];
  const end = (lastCell & ~CELL_WIDE) + (lastCell & CELL_WIDE ? 2 : 1) - 1;
  return { startLine: line.first + Math.floor(start / cols), startCol: start % cols, endLine: line.first + Math.floor(end / cols), endCol: end % cols + 1 };
}

const registry = typeof FinalizationRegistry === 'undefined'
  ? null
  : new FinalizationRegistry(({ h, gen }) => { if (gen === generation) wasm?.wongtty_drop(h); });

/** Cell attribute bits (`rowCells` word 3). */
export const Attr = Object.freeze({
  BOLD: 1 << 0, DIM: 1 << 1, ITALIC: 1 << 2, UNDERLINE: 1 << 3,
  DOUBLE_UNDERLINE: 1 << 4, CURLY_UNDERLINE: 1 << 5, DOTTED_UNDERLINE: 1 << 6, DASHED_UNDERLINE: 1 << 7,
  INVERSE: 1 << 8, HIDDEN: 1 << 9, STRIKEOUT: 1 << 10, BLINK: 1 << 11,
  WIDE: 1 << 12, WIDE_SPACER: 1 << 13, COMBINING: 1 << 14, UNDERLINE_COLOR: 1 << 15, HYPERLINK: 1 << 16, LEADING_WIDE_SPACER: 1 << 17,
  TYPED_SPACE: 1 << 18,
  IMAGE: 1 << 19,
});

/** Terminal mode bits (`modes()`). */
export const Mode = Object.freeze({
  SHOW_CURSOR: 1 << 0, APP_CURSOR: 1 << 1, APP_KEYPAD: 1 << 2, BRACKETED_PASTE: 1 << 3,
  MOUSE_CLICK: 1 << 4, MOUSE_DRAG: 1 << 5, MOUSE_MOTION: 1 << 6, SGR_MOUSE: 1 << 7,
  FOCUS_EVENTS: 1 << 8, ALT_SCREEN: 1 << 9, ALTERNATE_SCROLL: 1 << 10, LINE_WRAP: 1 << 11,
  INSERT: 1 << 12, ORIGIN: 1 << 13, KITTY_KEYBOARD: 1 << 14, MOUSE_X10: 1 << 15, SGR_PIXELS_MOUSE: 1 << 16,
});

const CURSOR_SHAPES = ['block', 'underline', 'bar'];
const HOOK_KINDS = ['csi', 'osc', 'dcs'];

/** Packed colour kinds (`rowCells` words 1 and 2). */
export const ColorKind = Object.freeze({ NAMED: 1, INDEXED: 2, RGB: 3 });

/** Entries `setColors` takes: palette 0–255, then foreground, background, cursor. */
export const COLOR_SLOTS = 259;

/** @internal Forces a trap in the instance, to test crash recovery. */
export function __trapForTest() {
  // A feed from far outside linear memory traps on the first read.
  wasm.wongtty_feed(wasm.wongtty_new(2, 1, 0), 0xfffffff0, 64);
}

/**
 * @param {number} packed
 * @returns {{ kind: 'named' | 'indexed' | 'rgb', value: number }}
 */
export function decodeColor(packed) {
  const kind = packed >>> 24;
  const value = packed & 0xffffff;
  return { kind: kind === ColorKind.RGB ? 'rgb' : kind === ColorKind.INDEXED ? 'indexed' : 'named', value };
}

export class Terminal {
  #h;
  // Renderers read it per cell, so keep the size here to avoid calling wasm (it changes only on resize)
  #cols = 0;
  #rows = 0;
  #inPtr = 0;
  #inCap = 0;
  #syncTimer = null;
  #syncTimeoutMs;
  #onSyncFlush;
  #onCrash;
  #gen;
  #scrollback;
  // Host settings re-applied when the engine starts over after a crash.
  #clipboard = { read: false, write: false };
  #kittyKeyboard = false;
  #focus = null;
  #colors = null;
  #hooks = new Set();
  #windowsPty = null;
  #cursorDefault = null;
  // Bytes of a write that stopped at a hook, fed by finishHook.
  #rest = null;

  /**
   * @param {{ cols?: number, rows?: number, scrollback?: number, syncTimeoutMs?: number, onSyncFlush?: () => void, onCrash?: (error: unknown) => void }} [options]
   */
  constructor({ cols = 80, rows = 24, scrollback = 1000, syncTimeoutMs = 1000, onSyncFlush, onCrash } = {}) {
    const w = requireWasm();
    this.#scrollback = scrollback;
    this.#syncTimeoutMs = syncTimeoutMs;
    this.#onSyncFlush = onSyncFlush;
    this.#onCrash = onCrash;
    this.#open(w, cols, rows);
  }

  #open(w, cols, rows) {
    this.#gen = generation;
    this.#h = w.wongtty_new(cols, rows, this.#scrollback);
    this.#readSize();
    registry?.register(this, { h: this.#h, gen: this.#gen }, this);
  }

  #handle() {
    if (!this.#h) throw new Error('wongtty: terminal is disposed');
    if (this.#gen !== generation) this.#restart();
    return this.#h;
  }

  // The instance was rebuilt after a trap: this terminal's state is gone. Start
  // over at the same size with the host's settings, and tell the host.
  #restart() {
    registry?.unregister(this);
    if (this.#syncTimer) clearTimeout(this.#syncTimer);
    this.#syncTimer = null;
    this.#inPtr = 0;
    this.#inCap = 0;
    this.#open(wasm, this.#cols, this.#rows);
    wasm.wongtty_set_clipboard_access(this.#h, this.#clipboard.read ? 1 : 0, this.#clipboard.write ? 1 : 0);
    if (this.#kittyKeyboard) wasm.wongtty_set_kitty_keyboard(this.#h, 1);
    if (this.#focus !== null) wasm.wongtty_set_focus(this.#h, this.#focus ? 1 : 0);
    if (this.#colors) this.setColors(this.#colors);
    this.#rest = null;
    if (this.#windowsPty) this.setWindowsPty(this.#windowsPty);
    if (this.#cursorDefault) this.setCursorDefault(...this.#cursorDefault);
    for (const key of this.#hooks) {
      const [kind, id] = key.split(':');
      wasm.wongtty_set_hook(this.#h, HOOK_KINDS.indexOf(kind), Number(id), 1);
    }
    this.#onCrash?.(new Error('wongtty: the engine crashed and this terminal was reset'));
  }

  /** Feeds program output. Strings are encoded as UTF-8. */
  write(data) {
    const h = this.#handle();
    if (typeof data === 'string') {
      if (!data.length) return;
      // UTF-8 is at most 3 bytes per UTF-16 unit: encode straight into linear memory.
      const ptr = this.#reserve(data.length * 3);
      const { written } = encoder.encodeInto(data, new Uint8Array(wasm.memory.buffer, ptr, data.length * 3));
      wasm.wongtty_feed(h, ptr, written);
    } else {
      if (!data.length) return;
      wasm.wongtty_feed(h, this.#stage(data), data.length);
    }
    this.#shrinkInput();
    this.#armSync();
  }

  /**
   * Stops parsing at sequences of `kind` with `id`, or no longer does (`on` false): for CSI and DCS the id is the
   * intermediates (a private marker such as `?` first) and final byte, one byte each from the most significant;
   * for OSC the number before the first `;`.
   * @param {'csi' | 'osc' | 'dcs'} kind
   */
  setHook(kind, id, on) {
    const k = HOOK_KINDS.indexOf(kind);
    if (k < 0) throw new RangeError(`wongtty: unknown hook kind ${kind}`);
    const h = this.#handle();
    if (on) this.#hooks.add(`${kind}:${id}`);
    else this.#hooks.delete(`${kind}:${id}`);
    wasm.wongtty_set_hook(h, k, id, on ? 1 : 0);
  }

  /**
   * Like `write`, but stops right after a hooked sequence, before its default action. Returns true when all of
   * `data` was parsed; false when stopped: read `pendingHook()`, run or skip the default with `finishHook()`, then
   * `resume()`. Throws until then.
   */
  writeUntilHook(data) {
    const h = this.#handle();
    if (this.#rest || wasm.wongtty_pending_hook(h) >= 0) throw new Error('wongtty: a hook is pending; call finishHook() first');
    let ptr;
    let len;
    if (typeof data === 'string') {
      ptr = this.#reserve(data.length * 3);
      len = encoder.encodeInto(data, new Uint8Array(wasm.memory.buffer, ptr, data.length * 3)).written;
    } else {
      ptr = this.#stage(data);
      len = data.length;
    }
    return this.#feedUntilHook(h, ptr, len);
  }

  /** @returns {{ kind: 'csi' | 'osc' | 'dcs', id: number, params: number[][], data: Uint8Array } | null} */
  pendingHook() {
    const kind = wasm.wongtty_pending_hook(this.#handle());
    if (kind < 0) return null;
    const out = new Uint32Array(wasm.memory.buffer, wasm.wongtty_out32_ptr());
    const [id, count] = out;
    const words = out.subarray(2);
    const params = [];
    for (let i = 0, at = 0; i < count; i++) {
      const n = words[at];
      params.push(Array.from(words.subarray(at + 1, at + 1 + n), (v) => v | 0));
      at += n + 1;
    }
    return { kind: HOOK_KINDS[kind], id, params, data: bytesOut(wasm.wongtty_out_len()) };
  }

  /** Runs the pending hook's default action (`runDefault`) or skips it; `resume()` parses on. */
  finishHook(runDefault) {
    wasm.wongtty_finish_hook(this.#handle(), runDefault ? 1 : 0);
  }

  /** Parses the rest of the stopped write (or flush) after `finishHook`; false when it stopped at the next hook. */
  resume() {
    const h = this.#handle();
    if (wasm.wongtty_pending_hook(h) >= 0) return false;
    const rest = this.#rest ?? new Uint8Array(0);
    this.#rest = null;
    return this.#feedUntilHook(h, this.#stage(rest), rest.length);
  }

  #feedUntilHook(h, ptr, len) {
    // Called with nothing too: the engine first goes on with what it stopped in.
    const n = wasm.wongtty_feed_until_hook(h, ptr, len);
    if (n < len) this.#rest = new Uint8Array(wasm.memory.buffer, ptr + n, len - n).slice();
    this.#shrinkInput();
    this.#armSync();
    return n === len && wasm.wongtty_pending_hook(h) < 0;
  }

  // The reusable input buffer in linear memory, grown to at least `len` bytes.
  #reserve(len) {
    if (len > this.#inCap) {
      if (this.#inPtr) wasm.wongtty_free(this.#inPtr, this.#inCap);
      this.#inCap = Math.max(len, IN_MIN);
      this.#inPtr = wasm.wongtty_alloc(this.#inCap);
    }
    return this.#inPtr;
  }

  // Copies bytes into the input buffer.
  #stage(bytes) {
    const ptr = this.#reserve(bytes.length);
    new Uint8Array(wasm.memory.buffer, ptr, bytes.length).set(bytes);
    return ptr;
  }

  // A buffer grown for one large write goes back, so the next small write
  // does not keep it allocated.
  #shrinkInput() {
    if (this.#inCap > IN_KEEP) {
      wasm.wongtty_free(this.#inPtr, this.#inCap);
      this.#inPtr = 0;
      this.#inCap = 0;
    }
  }

  // Synchronized output (CSI ? 2026 h) holds the renderer's frames until the
  // program ends it; if it never does, end it after a timeout like other terminals.
  #armSync() {
    const pending = wasm.wongtty_sync_pending(this.#h) === 1;
    if (pending && !this.#syncTimer) {
      this.#syncTimer = setTimeout(() => {
        this.#syncTimer = null;
        if (this.#h && wasm.wongtty_sync_pending(this.#h) === 1) {
          wasm.wongtty_flush_sync(this.#h);
          this.#onSyncFlush?.();
        }
      }, this.#syncTimeoutMs);
    } else if (!pending && this.#syncTimer) {
      clearTimeout(this.#syncTimer);
      this.#syncTimer = null;
    }
  }

  #readSize() {
    this.#cols = wasm.wongtty_cols(this.#h);
    this.#rows = wasm.wongtty_rows(this.#h);
  }

  resize(cols, rows) {
    wasm.wongtty_resize(this.#handle(), cols, rows);
    this.#readSize();
  }

  get cols() { this.#handle(); return this.#cols; }
  get rows() { this.#handle(); return this.#rows; }
  /** Scrollback lines; address them as rows -1 (newest) … -historyLength (oldest). */
  get historyLength() { return wasm.wongtty_history_len(this.#handle()); }

  /** Lines that have left the top of the scrollback (full history, ED3, RIS); history row -k is line trimmedLines + historyLength - k. */
  get trimmedLines() { return wasm.wongtty_trimmed_lines(this.#handle()); }

  /** @returns {string | null} */
  rowText(line) {
    const n = wasm.wongtty_row_text(this.#handle(), line);
    return n < 0 ? null : textOut(n);
  }

  /**
   * `translateToString(trimRight)` of lines `first`…`last` (clamped to the buffer), each followed by `\n`. The engine
   * reads a block of rows per call, so the text in its memory stays small. @returns {string}
   */
  textRows(first = -this.historyLength, last = this.rows - 1, trimRight = true) {
    const h = this.#handle();
    first = Math.max(first, -this.historyLength);
    last = Math.min(last, this.rows - 1);
    let text = '';
    for (let from = first; from <= last; from += TEXT_BLOCK) {
      text += textOut(wasm.wongtty_text_rows(h, from, Math.min(from + TEXT_BLOCK - 1, last), trimRight ? 1 : 0));
    }
    wasm.wongtty_out_release();
    return text;
  }

  /** `[codepoint, fg, bg, attrs]` per cell. @returns {Uint32Array | null} */
  rowCells(line) {
    const n = wasm.wongtty_row_cells(this.#handle(), line);
    return n < 0 ? null : wordsOut(n * 4);
  }

  /**
   * Like `rowCells`, but a view into WebAssembly memory instead of a copy:
   * valid only until the next call into any terminal. For renderers that read
   * every visible row each frame. @returns {Uint32Array | null}
   */
  rowCellsView(line) {
    const n = wasm.wongtty_row_cells(this.#handle(), line);
    return n < 0 ? null : new Uint32Array(wasm.memory.buffer, wasm.wongtty_out32_ptr(), n * 4);
  }

  rowWrapped(line) {
    return wasm.wongtty_row_wrapped(this.#handle(), line) === 1;
  }

  /** Cell text including combining marks. @returns {string | null} */
  cellText(line, col) {
    const n = wasm.wongtty_cell_text(this.#handle(), line, col);
    return n < 0 ? null : textOut(n);
  }

  /** Cells with combining marks, an underline colour or a hyperlink. */
  rowExtras(line) {
    const n = wasm.wongtty_row_extras(this.#handle(), line);
    if (n < 0) return null;
    if (n === 0) return [];
    const words = wordsOut(n * 4);
    const bytes = new Uint8Array(wasm.memory.buffer, wasm.wongtty_out_ptr(), wasm.wongtty_out_len()).slice();
    let at = 0;
    const texts = [];
    for (let i = 0; i < n; i++) {
      const len = words[i * 4 + 2];
      texts.push(len ? decoder.decode(bytes.subarray(at, at + len)) : undefined);
      at += len;
    }
    // After the text come this line's links, in `id \0 uri \0` order.
    const parts = decoder.decode(bytes.subarray(at)).split('\0');
    const links = [];
    for (let i = 0; i + 1 < parts.length; i += 2) links.push(Object.freeze({ id: parts[i], uri: parts[i + 1] }));
    const extras = [];
    for (let i = 0; i < n; i++) {
      const e = { col: words[i * 4] };
      if (texts[i] !== undefined) e.text = texts[i];
      if (words[i * 4 + 1]) e.underlineColor = words[i * 4 + 1];
      if (words[i * 4 + 3]) e.hyperlink = links[words[i * 4 + 3] - 1];
      extras.push(e);
    }
    return extras;
  }

  cursor() {
    const h = this.#handle();
    const c = wasm.wongtty_cursor(h);
    const style = wasm.wongtty_cursor_style(h);
    return {
      line: (c >>> 16) & 0x7fff,
      col: c & 0xffff,
      visible: c >>> 31 === 1,
      shape: CURSOR_SHAPES[style & 0xff] ?? 'block',
      blink: ((style >>> 8) & 1) === 1,
      styled: ((style >>> 9) & 1) === 1,
    };
  }

  /** Rows the view is scrolled back into history (0 = following output). */
  get displayOffset() { return wasm.wongtty_display_offset(this.#handle()); }

  /** Positive `delta` scrolls back into history; clamped. Damage becomes full when the view moves. */
  scrollDisplay(delta) {
    wasm.wongtty_scroll_display(this.#handle(), delta);
  }

  scrollToBottom() {
    wasm.wongtty_scroll_to_bottom(this.#handle());
  }

  modes() {
    return wasm.wongtty_modes(this.#handle());
  }

  /** Lets the program use the kitty keyboard protocol (off by default); switching drops its pushed flags. */
  setKittyKeyboard(enabled) {
    this.#kittyKeyboard = !!enabled;
    wasm.wongtty_set_kitty_keyboard(this.#handle(), enabled ? 1 : 0);
  }

  /** Drops the kitty keyboard flags the program pushed, keeping the protocol on or off. */
  resetKittyKeyboard() {
    wasm.wongtty_reset_kitty_keyboard(this.#handle());
  }

  /** Active kitty keyboard flags: 1 disambiguate, 2 event types, 4 alternate keys, 8 all keys as escapes, 16 associated text. */
  kittyFlags() {
    return wasm.wongtty_kitty_flags(this.#handle());
  }

  /** The host's focus, for the report sent when the program turns on focus events (`CSI ? 1004 h`); `null`, the default, sends none. */
  setFocus(focused) {
    this.#focus = focused == null ? null : !!focused;
    wasm.wongtty_set_focus(this.#handle(), this.#focus === null ? -1 : this.#focus ? 1 : 0);
  }

  /** The host's cursor style (xterm.js `cursorStyle`, `cursorBlink`): `cursor()` reports it until the program sets one, and DECRQSS reports it. */
  setCursorDefault(shape, blink) {
    this.#cursorDefault = [shape, !!blink];
    wasm.wongtty_set_cursor_default(this.#handle(), Math.max(0, CURSOR_SHAPES.indexOf(shape)), blink ? 1 : 0);
  }

  /** Viewport changes since the last call: move drawn rows up by `scroll`, then redraw `lines`. */
  takeDamage() {
    const n = wasm.wongtty_take_damage(this.#handle());
    if (n < 0) return { full: true, scroll: 0, lines: [] };
    const words = wordsOut(n + 1);
    return { full: false, scroll: words[0], lines: Array.from(words.subarray(1)) };
  }

  /** Bytes to write back to the PTY (status reports, …). */
  takeReplies() {
    const n = wasm.wongtty_take_replies(this.#handle());
    return n === 0 ? new Uint8Array(0) : bytesOut(n);
  }

  /** The same replies one by one, in order (xterm.js fires one `onData` per reply). */
  takeReplyList() {
    const n = wasm.wongtty_take_reply_list(this.#handle());
    if (n === 0) return [];
    const lens = wordsOut(n);
    const bytes = bytesOut(wasm.wongtty_out_len());
    const replies = [];
    let at = 0;
    for (const len of lens) {
      replies.push(bytes.subarray(at, at + len));
      at += len;
    }
    return replies;
  }

  /** @returns {string | null} new title, or null if unchanged */
  takeTitle() {
    const n = wasm.wongtty_take_title(this.#handle());
    return n < 0 ? null : textOut(n);
  }

  takeBells() {
    return wasm.wongtty_take_bells(this.#handle());
  }

  /** Full resets (RIS) the program sent since the last call. */
  takeResets() {
    return wasm.wongtty_take_resets(this.#handle());
  }

  /** Lets the program read and/or write the host clipboards with OSC 52 (both off by default). */
  setClipboardAccess({ read = false, write = false } = {}) {
    this.#clipboard = { read, write };
    wasm.wongtty_set_clipboard_access(this.#handle(), read ? 1 : 0, write ? 1 : 0);
  }

  /** @returns {{ selection: 'c' | 'p', text: string } | null} oldest text the program asked to copy */
  takeClipboardWrite() {
    const n = wasm.wongtty_take_clipboard_write(this.#handle());
    if (n < 0) return null;
    const out = bytesOut(n);
    return { selection: String.fromCharCode(out[0]), text: decoder.decode(out.subarray(1)) };
  }

  /** @returns {'c' | 'p' | null} oldest clipboard the program asked to read; answer with replyClipboard */
  takeClipboardRead() {
    const c = wasm.wongtty_take_clipboard_read(this.#handle());
    return c < 0 ? null : String.fromCharCode(c);
  }

  /** Answers the oldest read of `selection`; the reply joins takeReplies(). */
  replyClipboard(selection, text) {
    const h = this.#handle();
    const bytes = encoder.encode(text);
    return wasm.wongtty_reply_clipboard(h, selection.charCodeAt(0), this.#stage(bytes), bytes.length) === 1;
  }

  /** Colours (0xRRGGBB, COLOR_SLOTS of them) that answer OSC 4/10/11/12 queries; null stops answering. */
  setColors(colors) {
    const h = this.#handle();
    if (!colors) {
      this.#colors = null;
      return wasm.wongtty_set_colors(h, 0, 0);
    }
    if (colors.length !== COLOR_SLOTS) throw new RangeError(`wongtty: setColors takes ${COLOR_SLOTS} colours`);
    const words = Uint32Array.from(colors);
    this.#colors = words;
    wasm.wongtty_set_colors(h, this.#stage(new Uint8Array(words.buffer)), words.byteLength);
  }

  syncPending() {
    return wasm.wongtty_sync_pending(this.#handle()) === 1;
  }

  /** Ends a synchronized update, as its timeout does. Returns false while a hook is pending (see `writeUntilHook`). */
  flushSync() {
    const h = this.#handle();
    wasm.wongtty_flush_sync(h);
    this.#armSync();
    return wasm.wongtty_pending_hook(h) < 0;
  }

  /**
   * Behaves as xterm.js does with `windowsPty` (none by default): new rows blank at the bottom, no reflow before
   * conpty build 21376, and its wrapping heuristic for conpty before 21376.
   * @param {{ backend?: 'conpty' | 'winpty', buildNumber?: number } | null | undefined} pty
   */
  setWindowsPty(pty) {
    const h = this.#handle();
    this.#windowsPty = pty ? { backend: pty.backend, buildNumber: pty.buildNumber } : null;
    const backend = pty?.backend === 'conpty' ? 1 : pty?.backend === 'winpty' ? 2 : 0;
    wasm.wongtty_set_windows_pty(h, backend, pty?.buildNumber ?? NaN);
  }

  // Search runs here over the engine's logical lines (`searchLine`), with a RegExp for every query: a plain one is
  // escaped, and the `i` flag folds case without changing lengths. Matching in the wasm would cost the size budget.
  #matcher(query, { caseSensitive = false, regex = false } = {}) {
    return new RegExp(regex ? query : query.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'), caseSensitive ? 'g' : 'gi');
  }

  /** First match starting at or after cell (`line`, `col`), down to the bottom of the screen. @returns {object | null} */
  searchNext(query, options = {}, line = -this.historyLength, col = 0) {
    if (!query) return null;
    const re = this.#matcher(query, options);
    const cols = this.cols;
    line += Math.floor(col / cols);
    col %= cols;
    if (line < -this.historyLength) [line, col] = [-this.historyLength, 0];
    let found = this.searchLine(line);
    let target = found ? (line - found.first) * cols + col : 0;
    while (found) {
      let from = 0;
      while (from < found.text.length && (found.cells[from] & ~CELL_WIDE) < target) from++;
      const at = regexNext(re, found.text, from, options.wholeWord);
      if (at) return lineMatch(found, at, cols);
      found = this.searchLine(found.first + found.rows);
      target = 0;
    }
    return null;
  }

  /** Last match ending at or before cell (`line`, `col`), up to the top of the history; (`rows - 1`, `cols`) is the end. */
  searchPrevious(query, options = {}, line = this.rows - 1, col = this.cols) {
    if (!query) return null;
    const re = this.#matcher(query, options);
    const cols = this.cols;
    const top = -this.historyLength;
    line += Math.floor(col / cols);
    col %= cols;
    if (col === 0 && line > top) [line, col] = [line - 1, cols];
    if (line > this.rows - 1) [line, col] = [this.rows - 1, cols];
    let found = this.searchLine(line);
    let before = found ? (line - found.first) * cols + col : 0;
    while (found) {
      let bound = 0;
      while (bound < found.text.length && (found.cells[bound] & ~CELL_WIDE) + (found.cells[bound] & CELL_WIDE ? 2 : 1) <= before) bound++;
      const at = regexPrevious(re, found.text, bound, options.wholeWord);
      if (at) return lineMatch(found, at, cols);
      if (found.first <= top) return null;
      found = this.searchLine(found.first - 1);
      before = Infinity;
    }
    return null;
  }

  /** Every match in the logical lines rows `first`…`last` belong to (each from the end of the previous one), keeping those that touch the rows. */
  searchRows(query, options = {}, first, last) {
    const out = [];
    if (!query) return out;
    const re = this.#matcher(query, options);
    last = Math.min(last, this.rows - 1);
    let found = this.searchLine(Math.max(first, -this.historyLength));
    while (found && found.first <= last) {
      for (let at = 0, m; (m = regexNext(re, found.text, at, options.wholeWord)); at = m[0] + m[1]) {
        const match = lineMatch(found, m, this.cols);
        if (match.endLine >= first && match.startLine <= last) out.push(match);
      }
      found = this.searchLine(found.first + found.rows);
    }
    return out;
  }

  /**
   * The logical line row `line` belongs to as search reads it, or null outside the buffer. `cells[i]` is the cell of
   * the UTF-16 unit `text[i]`: `row * cols + col` from `first`, bit 31 set for a wide character.
   */
  searchLine(line) {
    const n = wasm.wongtty_search_line(this.#handle(), line);
    if (n < 0) return null;
    const text = textOut(wasm.wongtty_out_len());
    const words = new Uint32Array(wasm.memory.buffer, wasm.wongtty_out32_ptr(), n + 2);
    const cells = new Uint32Array(text.length);
    let unit = 0;
    let k = 2;
    for (const ch of text) {
      cells[unit++] = words[k];
      if (ch.length === 2) cells[unit++] = words[k];
      k++;
    }
    return { first: words[0] | 0, rows: words[1], text, cells };
  }

  /** Puts a marker on buffer line `line` of the primary screen (0 is the oldest history line); its id, or 0 outside the buffer or on the alternate screen. */
  addMarker(line) {
    return wasm.wongtty_add_marker(this.#handle(), line);
  }

  /** The marker's buffer line on the primary screen, or -1 once it is gone. */
  markerLine(id) {
    return wasm.wongtty_marker_line(this.#handle(), id);
  }

  removeMarker(id) {
    wasm.wongtty_remove_marker(this.#handle(), id);
  }

  /** Markers the engine dropped since the last call (trimmed, cleared, reset). @returns {number[]} */
  takeDisposedMarkers() {
    const n = wasm.wongtty_take_disposed_markers(this.#handle());
    return n > 0 ? Array.from(wordsOut(n)) : [];
  }

  /** Covers `cols` × `rows` cells with the tiles of image `id`. @returns {{ tiles: number, covered: number[] }} */
  placeImage(id, cols, rows, { scrolling = true, cursor = 'vt340' } = {}) {
    const size = (Math.min(rows, 0xffff) << 16 | Math.min(cols, 0xffff)) >>> 0;
    const n = wasm.wongtty_place_image(this.#handle(), id, size, (scrolling ? 1 : 0) | (cursor === 'iip' ? 2 : 0));
    const words = wordsOut(n + 1);
    return { tiles: words[0] ?? 0, covered: Array.from(words.subarray(1)) };
  }

  /** `[col, id, tile]` per cell an image tile covers, or null if out of range. @returns {Uint32Array | null} */
  rowImages(line) {
    const n = wasm.wongtty_row_images(this.#handle(), line);
    return n < 0 ? null : wordsOut(n * 3);
  }

  /** `[fg, bg, attrs]` that text written now gets, packed like `rowCells`. @returns {Uint32Array} */
  pen() {
    wasm.wongtty_pen(this.#handle());
    return wordsOut(3);
  }

  /** Bytes that rebuild this screen in a fresh xterm.js, or why they cannot. `options` as @xterm/addon-serialize's `scrollback` and `excludeModes`. */
  ansiSnapshot({ scrollback, excludeModes = false } = {}) {
    const n = wasm.wongtty_ansi_snapshot(this.#handle(), scrollback ?? -1, excludeModes ? 1 : 0);
    const result = n > 0 ? { ok: true, payload: bytesOut(n) } : { ok: false, reason: textOut(-n) };
    // The payload is copied out; free it in the engine's memory rather than at the next call
    wasm.wongtty_out_release();
    return result;
  }

  dispose() {
    if (!this.#h) return;
    if (this.#syncTimer) clearTimeout(this.#syncTimer);
    if (this.#gen === generation) {
      if (this.#inPtr) wasm.wongtty_free(this.#inPtr, this.#inCap);
      wasm.wongtty_drop(this.#h);
    }
    registry?.unregister(this);
    this.#h = 0;
    this.#inPtr = 0;
    this.#inCap = 0;
  }
}
