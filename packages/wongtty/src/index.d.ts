/** Loads the WebAssembly module once. Defaults to the bundled `wongtty.wasm`. */
export function init(
  source?: URL | string | ArrayBuffer | ArrayBufferView | WebAssembly.Module | Response,
): Promise<void>;

export interface TerminalOptions {
  cols?: number;
  rows?: number;
  /** Lines of history to keep (default 1000, max 100000). */
  scrollback?: number;
  /** End a synchronized update the program never ended after this many ms (default 1000, as xterm.js). */
  syncTimeoutMs?: number;
  /** Called after such a timed end, so the renderer can redraw. */
  onSyncFlush?: () => void;
  /**
   * Called when the engine crashed (a trap in the shared WebAssembly
   * instance) and this terminal started over empty at the same size. The
   * call that hit the crash throws; the terminal works again afterwards.
   */
  onCrash?: (error: Error) => void;
}

export interface Cursor {
  /** Screen line; while scrolled back it is drawn on viewport row `line + displayOffset`. */
  line: number;
  col: number;
  visible: boolean;
  /** Set by the program with DECSCUSR (`CSI Ps SP q`). */
  shape: 'block' | 'underline' | 'bar';
  blink: boolean;
  /** The program chose `shape` and `blink`; when false they are the host's (`setCursorDefault`, block and steady by default). */
  styled: boolean;
}

export interface Damage {
  /** Redraw everything; `scroll` is 0 and `lines` is empty. */
  full: boolean;
  /** Move drawn rows up by this many first (the top ones fall off). */
  scroll: number;
  /** Viewport rows to redraw after scrolling, top to bottom. */
  lines: number[];
}

export interface Hyperlink {
  /** Shared by every cell of one link (generated when the program gave none). */
  id: string;
  uri: string;
}

/** What a cell carries beyond `rowCells`. */
export interface CellExtra {
  col: number;
  /** Full cell text when it has combining marks. */
  text?: string;
  /** Underline colour (SGR 58), packed like `rowCells` colours. */
  underlineColor?: number;
  /** OSC 8 hyperlink; cells of one link share the same object. */
  hyperlink?: Hyperlink;
}

/**
 * `payload` written to a fresh xterm.js of the same size and scrollback
 * rebuilds this screen; it ends with the unfinished escape sequence or UTF-8
 * character the output stopped in. During a synchronized update `CSI ? 2026 h`
 * comes before that unfinished end, so the receiver is inside the same update.
 * On refusal replay the raw output instead.
 */
export type AnsiSnapshot =
  | { ok: true; payload: Uint8Array }
  /** `"unsupported-wide-edge"`, `"margins"` or `"partial-tail-overflow"`. */
  | { ok: false; reason: string };

/** A hooked sequence parsing stopped at, before its default action (`Terminal.writeUntilHook`). */
export interface Hook {
  kind: 'csi' | 'osc' | 'dcs';
  /** As given to `setHook`. */
  id: number;
  /**
   * CSI and DCS parameters, each followed by its sub-parameters (an empty parameter is 0 and an empty sub-parameter
   * -1, as in xterm.js; values stop at 65535).
   */
  params: number[][];
  /** OSC: everything after the first `;`. DCS: the payload. */
  data: Uint8Array;
}

/** How `searchNext` and friends compare text. */
export interface SearchOptions {
  /**
   * Off: matched with the RegExp `i` flag, but by `searchRows` of a plain ASCII query, which the engine matches
   * with each line lowercased as the N-API binding does (U+212A KELVIN SIGN reads as `k`, U+0130 as `i` + U+0307).
   */
  caseSensitive?: boolean;
  /** A match needs a non-word character (`@xterm/addon-search`'s set, so not `_`) or a line end on both sides. */
  wholeWord?: boolean;
  /** `query` is a JavaScript regular expression (flag `g`); otherwise it is matched as plain text. Regular
   * expressions, and plain queries but for `searchRows` of an ASCII one, are matched in JS over each logical line's
   * text (`searchLine`). */
  regex?: boolean;
}

/** A match in cells: lines as in `rowText`, end exclusive and on `endLine` (`endCol` is 1…cols). */
export interface SearchMatch {
  startLine: number;
  startCol: number;
  endLine: number;
  endCol: number;
}

/** A logical line as search reads it: rows joined across soft wraps, blanks as spaces, empty cells at the end dropped. */
export interface SearchLine {
  /** Its first row. */
  first: number;
  rows: number;
  text: string;
  /** The cell of each UTF-16 unit of `text`: `row * cols + col` from `first`, bit 31 set for a wide character. */
  cells: Uint32Array;
}

/** What `ansiSnapshot` leaves out, like `@xterm/addon-serialize`'s `serialize` options. */
export interface AnsiSnapshotOptions {
  /** Keep only the newest this many history lines (0: the screen alone). All of them by default. */
  scrollback?: number;
  /**
   * Leave out the modes (cursor keys, keypad, mouse, focus reports, bracketed paste, insert, origin, autowrap off,
   * cursor visibility) and what continues the output (the unfinished sequence at the end, a synchronized update in
   * progress), so the bytes only paint the screen. Scroll margins are then not refused.
   */
  excludeModes?: boolean;
}

export class Terminal {
  constructor(options?: TerminalOptions);
  /** Feeds program output. Strings are encoded as UTF-8. */
  write(data: string | Uint8Array): void;
  resize(cols: number, rows: number): void;
  readonly cols: number;
  readonly rows: number;
  /** Scrollback lines; address them as rows -1 (newest) … -historyLength (oldest). */
  readonly historyLength: number;
  /**
   * Lines that have left the top of the scrollback so far (a full history dropping its oldest, ED3, RIS), like
   * xterm.js `onTrim` summed up. History row `-k` is line `trimmedLines + historyLength - k` counted from the first
   * line ever kept, until a resize reflows the history.
   */
  readonly trimmedLines: number;
  rowText(line: number): string | null;
  /**
   * `translateToString(trimRight)` of lines `first`…`last` (clamped to the buffer; by default all of it), each
   * followed by `\n`, in one call. `trimRight` defaults to true.
   */
  textRows(first?: number, last?: number, trimRight?: boolean): string;
  /** `[codepoint, fg, bg, attrs]` per cell. */
  rowCells(line: number): Uint32Array | null;
  /**
   * Like `rowCells`, but a view into WebAssembly memory instead of a copy:
   * valid only until the next call into any terminal. For renderers that read
   * every visible row each frame.
   */
  rowCellsView(line: number): Uint32Array | null;
  rowWrapped(line: number): boolean;
  /** Cell text including combining marks. */
  cellText(line: number, col: number): string | null;
  /** Cells with combining marks, an underline colour or a hyperlink (see `Attr`). */
  rowExtras(line: number): CellExtra[] | null;
  cursor(): Cursor;
  /** Rows the view is scrolled back into history (0 = following output). Viewport row `y` is line `y - displayOffset`. */
  readonly displayOffset: number;
  /** Positive `delta` scrolls back into history; clamped. Damage becomes full when the view moves. */
  scrollDisplay(delta: number): void;
  scrollToBottom(): void;
  /** Bitwise OR of {@link Mode} values. */
  modes(): number;
  /** Lets the program use the kitty keyboard protocol (off by default, its sequences ignored); switching drops the flags it pushed. */
  setKittyKeyboard(enabled: boolean): void;
  /** Drops the kitty keyboard flags the program pushed on both screens, keeping the protocol on or off. */
  resetKittyKeyboard(): void;
  /** Active kitty keyboard flags: 1 disambiguate, 2 event types, 4 alternate keys, 8 all keys as escapes, 16 associated text. */
  kittyFlags(): number;
  /**
   * The host's focus, for the report (`CSI I` or `CSI O`) that joins the replies, in order, each time the program turns
   * on focus events (`CSI ? 1004 h`); `null`, the default, sends none. Kept across a crash.
   */
  setFocus(focused: boolean | null): void;
  /**
   * The host's cursor style (xterm.js `cursorStyle` and `cursorBlink`; block, steady by default): `cursor()` reports it
   * until the program sets one, and DECRQSS (`DCS $ q SP q ST`) reports it, as xterm.js does. Kept across a crash.
   */
  setCursorDefault(shape: 'block' | 'underline' | 'bar', blink: boolean): void;
  takeDamage(): Damage;
  /** Bytes to write back to the PTY (status reports, …). */
  takeReplies(): Uint8Array;
  /** The same replies one by one, in order: xterm.js fires one `onData` per reply. */
  takeReplyList(): Uint8Array[];
  /** New window title, or null if unchanged ("" = reset). */
  takeTitle(): string | null;
  takeBells(): number;
  /** Full resets (RIS, `ESC c`) the program sent since the last call, so the host can drop its own screen state (selection). */
  takeResets(): number;
  /** Lets the program read and/or write the host clipboards with OSC 52 (both off by default). */
  setClipboardAccess(access?: { read?: boolean; write?: boolean }): void;
  /** Oldest text the program asked to copy, or null. */
  takeClipboardWrite(): { selection: 'c' | 'p'; text: string } | null;
  /** Oldest clipboard the program asked to read, or null. Answer it with replyClipboard. */
  takeClipboardRead(): 'c' | 'p' | null;
  /** Answers the oldest read of `selection`; the reply joins takeReplies(). False if none is waiting. */
  replyClipboard(selection: 'c' | 'p', text: string): boolean;
  /** Colours (0xRRGGBB, COLOR_SLOTS of them: palette 0–255, foreground, background, cursor) that answer OSC 4/10/11/12 queries; null stops answering. */
  setColors(colors: ArrayLike<number> | null): void;
  /**
   * A synchronized update (`CSI ? 2026 h`) is open. Its output is applied at once, as in xterm.js; a renderer
   * holds its frames (does not take damage) until it ends.
   */
  syncPending(): boolean;
  /** Ends a synchronized update, as its timeout does. False while a hook is pending (see `writeUntilHook`). */
  flushSync(): boolean;
  /**
   * Stops parsing at sequences of `kind` with `id`, or no longer does (`on` false). For CSI and DCS the id is the
   * intermediates (a private marker such as `?` first) and final byte, one byte each from the most significant;
   * for OSC the number before the first `;`. `write` runs their default action as usual.
   */
  setHook(kind: 'csi' | 'osc' | 'dcs', id: number, on: boolean): void;
  /**
   * Like `write`, but stops right after a hooked sequence, before its default action. True when all of `data` was
   * parsed; false when stopped: read `pendingHook()`, run or skip the default with `finishHook()`, then `resume()`.
   * Throws until then.
   */
  writeUntilHook(data: string | Uint8Array): boolean;
  pendingHook(): Hook | null;
  /** Runs the pending hook's default action (`runDefault`) or skips it; `resume()` parses on. */
  finishHook(runDefault: boolean): void;
  /** Parses the rest of the stopped write (or flush) after `finishHook`; false when it stopped at the next hook. */
  resume(): boolean;
  /**
   * Behaves as xterm.js 6.1 does with `windowsPty` (none by default): with a backend or build, more rows are added
   * blank at the bottom instead of bringing scrollback back; with a build that is not conpty 21376 or later,
   * resizing does not reflow; with conpty before 21376, a line feed or `CSI H` marks the cursor row as continuing
   * the row above when that row ends in a non-blank cell.
   */
  setWindowsPty(pty: { backend?: 'conpty' | 'winpty'; buildNumber?: number } | null | undefined): void;
  /**
   * The first match of `query` starting at or after cell (`line`, `col`), down to the bottom of the screen (no
   * wrap-around). Defaults to the top of the history. Matches never span logical lines (rows joined by soft wraps).
   */
  searchNext(query: string, options?: SearchOptions, line?: number, col?: number): SearchMatch | null;
  /** The last match ending at or before cell (`line`, `col`), up to the top of the history. Defaults to the end of the screen. */
  searchPrevious(query: string, options?: SearchOptions, line?: number, col?: number): SearchMatch | null;
  /** Every match in the logical lines rows `first`…`last` belong to (each search goes on from the end of the previous match), keeping those that touch the rows. */
  searchRows(query: string, options: SearchOptions | undefined, first: number, last: number): SearchMatch[];
  /**
   * The engine's matching of a plain `query` over rows `first`…`last` (`searchRows` without `regex`): at most
   * `limit` matches as `[startLine, startCol, endLine, endCol]` each, a view into WebAssembly memory valid only until
   * the next call into any terminal. Null for a query that is not ASCII and, with `keepLength`, when a line with a
   * match holds U+0130 (İ), which lowercases to two characters (for hosts that lowercase whole lines themselves).
   */
  searchRowsView(
    query: string,
    options: { caseSensitive?: boolean; wholeWord?: boolean; keepLength?: boolean } | undefined,
    first: number,
    last: number,
    limit?: number,
  ): Int32Array | null;
  /** The logical line row `line` belongs to, as search reads it, for matching the text yourself; null outside the buffer. */
  searchLine(line: number): SearchLine | null;
  /**
   * Puts a marker on buffer line `line` of the screen that is up (0 is the oldest history line, `historyLength` the
   * top screen row) and returns its id; 0 outside the buffer. The marker follows its line as output scrolls it into
   * the history and as scroll regions, IL and DL move it; it is dropped when its line leaves the scrollback or is
   * cleared whole (ED, a line IL or DL pushed out, RIS), and with the alternate screen when that is left. A resize
   * keeps it on its logical line.
   */
  addMarker(line: number): number;
  /** The marker's buffer line on its screen, -1 once it is gone. */
  markerLine(id: number): number;
  /** Drops a marker; it is not reported by `takeDisposedMarkers`. */
  removeMarker(id: number): void;
  /** Markers the engine dropped since the last call. */
  takeDisposedMarkers(): number[];
  /**
   * Covers `cols` × `rows` cells with the tiles of image `id` (a host that keeps the pixels, as @xterm/addon-image):
   * from the cursor, feeding lines as needed, or with `scrolling: false` from the top left of the screen with the
   * cursor staying. The cursor ends on the last row, at the image's left column (`'vt340'`) or right of it
   * (`'iip'`). Columns past the right edge are cut off. Returns the tiles written and the image id of each tile it
   * covered. Text or an erase over a cell removes its tile; tiles scroll and leave with their lines.
   */
  placeImage(id: number, cols: number, rows: number, options?: { scrolling?: boolean; cursor?: 'vt340' | 'iip' }): { tiles: number; covered: number[] };
  /** `[col, id, tile]` per cell of the row an image tile covers (`tile` is `row * cols + col` in the image), or null if out of range. */
  rowImages(line: number): Uint32Array | null;
  /** `[fg, bg, attrs]` that text written now gets, packed like `rowCells`. */
  pen(): Uint32Array;
  /**
   * Does not change the terminal, so it is safe while a renderer draws it; two calls give the same bytes.
   * `{ scrollback: 0, excludeModes: true }` paints just the screen it shows (a cold frame).
   */
  ansiSnapshot(options?: AnsiSnapshotOptions): AnsiSnapshot;
  dispose(): void;
}

/** Entries `setColors` takes: palette 0–255, then foreground, background, cursor. */
export declare const COLOR_SLOTS: 259;
export declare const Attr: Readonly<{
  BOLD: number; DIM: number; ITALIC: number; UNDERLINE: number;
  DOUBLE_UNDERLINE: number; CURLY_UNDERLINE: number; DOTTED_UNDERLINE: number; DASHED_UNDERLINE: number;
  INVERSE: number; HIDDEN: number; STRIKEOUT: number; BLINK: number;
  WIDE: number; WIDE_SPACER: number; COMBINING: number; UNDERLINE_COLOR: number; HYPERLINK: number;
  /** Blank end of a wrapped row whose wide character moved to the next row; not text. */
  LEADING_WIDE_SPACER: number;
  /** A space the program printed. Other blanks were erased or never written (xterm.js empty cells). */
  TYPED_SPACE: number;
  /** An image tile covers the cell; see `rowImages`. */
  IMAGE: number;
}>;

export declare const Mode: Readonly<{
  SHOW_CURSOR: number; APP_CURSOR: number; APP_KEYPAD: number; BRACKETED_PASTE: number;
  MOUSE_CLICK: number; MOUSE_DRAG: number; MOUSE_MOTION: number; SGR_MOUSE: number;
  FOCUS_EVENTS: number; ALT_SCREEN: number; ALTERNATE_SCROLL: number; LINE_WRAP: number;
  INSERT: number; ORIGIN: number; KITTY_KEYBOARD: number; MOUSE_X10: number; SGR_PIXELS_MOUSE: number;
}>;

export declare const ColorKind: Readonly<{ NAMED: 1; INDEXED: 2; RGB: 3 }>;

/** Named: 0–15 ANSI, 256 default fg, 257 default bg. Indexed: 0–255. RGB: 0xRRGGBB. */
export function decodeColor(packed: number): { kind: 'named' | 'indexed' | 'rgb'; value: number };
