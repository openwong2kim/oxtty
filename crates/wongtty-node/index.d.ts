/* Generated from src/lib.rs by @napi-rs/cli generateTypeDef; do not edit by hand.
 * Regenerate: NAPI_TYPE_DEF_TMP_FOLDER=<dir> cargo build -p wongtty-node (touch src/lib.rs first), then
 * generateTypeDef({ typeDefDir: <dir>, cwd: "crates/wongtty-node" }) from @napi-rs/cli 3 and write its dts here. */
export declare class Terminal {
  constructor(cols: number, rows: number, scrollback?: number | undefined | null)
  /** Feeds program output (string as UTF-8, or raw bytes). */
  write(data: string | Buffer): void
  resize(cols: number, rows: number): void
  get cols(): number
  get rows(): number
  get historyLength(): number
  /** Lines that have left the top of the scrollback so far (a full history dropping rows, ED3, RIS). */
  get trimmedLines(): number
  rowText(line: number): string | null
  /** `[ch, fg, bg, attrs]` per cell, or `null` if out of range. */
  rowCells(line: number): Uint32Array | null
  /**
   * `rowCells` written into `out` instead of a new array: returns the words written, or -1 if out of range.
   * Throws if `out` is shorter than the row (`cols * 4` words).
   * A view over a `SharedArrayBuffer` is accepted but not synchronised: a write from another thread during
   * the call races with it and can leave torn cells.
   */
  rowCellsInto(line: number, out: Uint32Array): number
  /**
   * Every visible row (what `rowCells(y - displayOffset)` gives for `y` in `0..rows`) written into `out` at
   * `y * cols * 4`, in one call; returns the words written (`rows * cols * 4`). Throws if `out` is shorter.
   * The same `SharedArrayBuffer` caveat as `rowCellsInto` applies.
   */
  viewportCellsInto(out: Uint32Array): number
  rowWrapped(line: number): boolean
  cellText(line: number, col: number): string | null
  /**
   * Cells with combining marks, an underline colour or a hyperlink, or
   * `null` if out of range.
   */
  rowExtras(line: number): Array<CellExtra> | null
  cursor(): Cursor
  /** Rows the view is scrolled back into history (0 = following output). */
  get displayOffset(): number
  /** Positive `delta` scrolls back into history. */
  scrollDisplay(delta: number): void
  scrollToBottom(): void
  modes(): number
  /** Lets the program use the kitty keyboard protocol (off by default); switching drops its pushed flags. */
  setKittyKeyboard(enabled: boolean): void
  /** Drops the kitty keyboard flags the program pushed, keeping the protocol on or off. */
  resetKittyKeyboard(): void
  /** Active kitty keyboard flags (1 disambiguate, 2 event types, 4 alternate keys, 8 all keys, 16 associated text). */
  kittyFlags(): number
  /**
   * The host's focus, for the report the engine sends when the program turns on focus events (`CSI ? 1004 h`);
   * `null`, the default, sends none.
   */
  setFocus(focused?: boolean | undefined | null): void
  /**
   * The host's cursor style (`"block"`, `"underline"` or `"bar"`): `cursor()` reports it until the program sets
   * one, and DECRQSS reports it.
   */
  setCursorDefault(shape: string, blink: boolean): void
  takeDamage(): Damage
  takeReplies(): Buffer
  /** The same replies one by one, in order (xterm.js fires one `onData` per reply). */
  takeReplyList(): Array<Buffer>
  takeTitle(): string | null
  takeBells(): number
  /** Full resets (RIS) the program sent since the last call. */
  takeResets(): number
  /** Lets the program read and/or write the host clipboards with OSC 52 (both off by default). */
  setClipboardAccess(read: boolean, write: boolean): void
  /** Oldest text the program asked to copy. */
  takeClipboardWrite(): ClipboardWrite | null
  /** Oldest clipboard (`"c"` or `"p"`) the program asked to read; answer with `replyClipboard`. */
  takeClipboardRead(): string | null
  /** Answers the oldest read of `selection`; the reply joins `takeReplies()`. */
  replyClipboard(selection: string, text: string): boolean
  /**
   * Colours (`0xRRGGBB`, palette 0–255 then foreground, background, cursor) that answer
   * OSC 4/10/11/12 queries; `null` stops answering.
   */
  setColors(colors?: Uint32Array | undefined | null): void
  syncPending(): boolean
  flushSync(): void
  /** Stops parsing at sequences of `kind` (`"csi"`, `"osc"`, `"dcs"`) with `id`, or no longer does. */
  setHook(kind: string, id: number, on: boolean): void
  /** Like `write`, but stops right after a hooked sequence; returns the bytes taken. */
  feedUntilHook(data: Buffer): number
  pendingHook(): Hook | null
  /**
   * Runs the pending hook's default action (`run_default`) or drops it. Parsing goes on with the next
   * `feedUntilHook` (with the bytes it did not take, even none).
   */
  finishHook(runDefault: boolean): void
  /** xterm.js `windowsPty`: `backend` is `"conpty"` or `"winpty"`. */
  setWindowsPty(backend?: string | undefined | null, buildNumber?: number | undefined | null): void
  /** First match starting at or after cell (`line`, `col`), down to the bottom of the screen. */
  searchNext(query: string, options: SearchOptions | undefined | null, line: number, col: number): SearchMatch | null
  /** Last match ending at or before cell (`line`, `col`), up to the top of the history. */
  searchPrevious(query: string, options: SearchOptions | undefined | null, line: number, col: number): SearchMatch | null
  /** Every match in the logical lines rows `first`…`last` belong to, keeping those that touch the rows. */
  searchRows(query: string, options: SearchOptions | undefined | null, first: number, last: number): Array<SearchMatch>
  searchLine(line: number): SearchLine | null
  /**
   * Puts a marker on buffer line `line` of the screen that is up (0 is the oldest history line, or the alternate
   * screen's top row); null outside the buffer. The alternate screen's markers go when it is left.
   */
  addMarker(line: number): number | null
  /** The marker's buffer line on its screen; null once it is gone. */
  markerLine(id: number): number | null
  removeMarker(id: number): void
  /** Markers the engine dropped since the last call (trimmed, cleared, reset). */
  takeDisposedMarkers(): Array<number>
  /** Covers `cols` × `rows` cells with the tiles of image `id`. */
  placeImage(id: number, cols: number, rows: number, options?: PlaceImageOptions | undefined | null): ImagePlaced
  /** `[col, id, tile]` per cell an image tile covers, or `null` if out of range. */
  rowImages(line: number): Uint32Array | null
  /** `[fg, bg, attrs]` that text written now gets. */
  pen(): Uint32Array
  /** Bytes that rebuild this screen in a fresh xterm.js, or why they cannot. */
  ansiSnapshot(options?: AnsiSnapshotOptions | undefined | null): AnsiSnapshot
}

/** `{ ok: true, payload }` or `{ ok: false, reason }`. */
export interface AnsiSnapshot {
  ok: boolean
  payload?: Buffer
  /** `"unsupported-wide-edge"`, `"margins"` or `"partial-tail-overflow"`. */
  reason?: string
}

/** What `ansiSnapshot` leaves out, like `@xterm/addon-serialize` options. */
export interface AnsiSnapshotOptions {
  /** Keep only the newest this many history lines (0: the screen alone). */
  scrollback?: number
  /** Leave out the modes and what continues the output (unfinished sequence, synchronized update). */
  excludeModes?: boolean
}

export interface CellExtra {
  col: number
  text?: string
  underlineColor?: number
  hyperlink?: Hyperlink
}

export interface ClipboardWrite {
  /** `"c"` (clipboard) or `"p"` (primary selection). */
  selection: string
  text: string
}

export interface Cursor {
  line: number
  col: number
  visible: boolean
  /** `"block"`, `"underline"` or `"bar"`. */
  shape: string
  blink: boolean
  /** The program set `shape` and `blink` (DECSCUSR 1–6); otherwise they are defaults the host may replace. */
  styled: boolean
}

export interface Damage {
  /** Redraw everything; `scroll` is 0 and `lines` is empty. */
  full: boolean
  /** Move drawn rows up by this many first. */
  scroll: number
  lines: Array<number>
}

/** A hooked sequence parsing stopped at, before its default action. */
export interface Hook {
  /** `"csi"`, `"osc"` or `"dcs"`. */
  kind: string
  id: number
  /** Each parameter followed by its sub-parameters (an empty sub-parameter is -1). */
  params: Array<Array<number>>
  /** OSC: everything after the first `;`. DCS: the payload. */
  data: Buffer
}

export interface Hyperlink {
  id: string
  uri: string
}

/** What `placeImage` did: tiles written, and the image id of each tile it covered. */
export interface ImagePlaced {
  tiles: number
  covered: Array<number>
}

/** How `placeImage` moves the cursor. */
export interface PlaceImageOptions {
  /** Default true; false places the image at the top left of the screen and keeps the cursor. */
  scrolling?: boolean
  /** `"vt340"` (default) or `"iip"`. */
  cursor?: string
}

/** A logical line as search reads it; `cells[i]` is the cell of the i-th character (not UTF-16 unit) of `text`. */
export interface SearchLine {
  first: number
  rows: number
  text: string
  cells: Uint32Array
}

/** A match in cells; the end is exclusive and on `endLine`. */
export interface SearchMatch {
  startLine: number
  startCol: number
  endLine: number
  endCol: number
}

/** How the search methods compare text (no regex here: match `searchLine` text with your own). */
export interface SearchOptions {
  caseSensitive?: boolean
  wholeWord?: boolean
}
