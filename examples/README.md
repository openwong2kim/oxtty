# Examples

A real shell drawn by `wongtty/xterm`. Build the package first: `(cd packages/wongtty && npm run build)`.

**Not runnable right now:** `wongtty/xterm` is being rewritten in Rust (PRD v0.4) and these examples come back with it.

| Example | Run | What it shows |
| --- | --- | --- |
| [browser](browser) | `npm install && npm start`, open http://localhost:8080 | node-pty on a local server, the terminal in a web page, one WebSocket between them |
| [electron](electron) | `npm install && npm start` | node-pty in the main process, the terminal in the renderer; both wasm modules' bytes come over IPC because a `file://` page cannot fetch them |

Coming from xterm.js, the only new step is `await init()` before the first `new Terminal()`.
