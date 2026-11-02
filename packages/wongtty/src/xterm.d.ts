export * from './xterm_web.js';

type WasmSource = URL | string | ArrayBuffer | ArrayBufferView | WebAssembly.Module | Response;

/** Loads the engine (`wongtty.wasm`) and the front end (`xterm_web_bg.wasm`) once; call before `new Terminal()`. */
export function init(engine?: WasmSource, frontEnd?: WasmSource): Promise<void>;
