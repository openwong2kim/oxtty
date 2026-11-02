// A shell in the browser: node-pty on the server, wongtty/xterm on the page, one WebSocket between them.
//   (cd packages/wongtty && npm run build) && cd examples/browser && npm install && npm start
// then open http://localhost:8080. Listens on localhost only: anyone who can connect gets your shell.
import { readFile } from 'node:fs/promises';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { dirname, extname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import pty from 'node-pty';
import { WebSocketServer } from 'ws';

const here = dirname(fileURLToPath(import.meta.url));
const dist = dirname(createRequire(import.meta.url).resolve('wongtty'));
const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.wasm': 'application/wasm' };

const server = createServer(async (req, res) => {
  const url = new URL(req.url, 'http://localhost');
  const file = url.pathname === '/' ? join(here, 'index.html') : url.pathname.startsWith('/wongtty/') ? join(dist, url.pathname.slice(7)) : undefined;
  if (!file || file.includes('..')) return res.writeHead(404).end();
  try {
    res.writeHead(200, { 'content-type': TYPES[extname(file)] ?? 'application/octet-stream' }).end(await readFile(file));
  } catch {
    res.writeHead(404).end();
  }
});

new WebSocketServer({ server }).on('connection', socket => {
  const shell = process.platform === 'win32' ? 'powershell.exe' : process.env.SHELL || 'bash';
  const term = pty.spawn(shell, [], { name: 'xterm-256color', cols: 80, rows: 24, cwd: process.env.HOME, env: process.env });
  term.onData(data => socket.send(data));
  term.onExit(() => socket.close());
  socket.on('message', message => {
    const { input, resize } = JSON.parse(message);
    if (input) term.write(input);
    if (resize) term.resize(resize.cols, resize.rows);
  });
  socket.on('close', () => term.kill());
});

server.listen(8080, 'localhost', () => console.log('http://localhost:8080'));
