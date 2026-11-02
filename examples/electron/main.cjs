// A shell in Electron: node-pty in the main process, wongtty/xterm in the renderer, IPC between them.
//   (cd packages/wongtty && npm run build) && cd examples/electron && npm install && npm start
const { app, BrowserWindow, ipcMain } = require('electron');
const { readFileSync } = require('node:fs');
const { dirname, join } = require('node:path');
const pty = require('node-pty');

app.whenReady().then(() => {
  const win = new BrowserWindow({ width: 900, height: 600, backgroundColor: '#1e1e1e', webPreferences: { preload: join(__dirname, 'preload.cjs') } });
  const shell = process.platform === 'win32' ? 'powershell.exe' : process.env.SHELL || 'bash';
  const term = pty.spawn(shell, [], { name: 'xterm-256color', cols: 80, rows: 24, cwd: app.getPath('home'), env: process.env });
  term.onData(data => win.webContents.send('pty:data', data));
  term.onExit(() => win.isDestroyed() || win.close());
  ipcMain.on('pty:input', (_, data) => term.write(data));
  ipcMain.on('pty:resize', (_, cols, rows) => term.resize(cols, rows));
  // The renderer cannot fetch file:// URLs, so it gets both modules' bytes over IPC and passes them to init().
  ipcMain.handle('wongtty:wasm', () => ['wongtty.wasm', 'xterm_web_bg.wasm'].map(file => readFileSync(join(dirname(require.resolve('wongtty')), file))));
  win.on('closed', () => term.kill());
  win.loadFile('index.html');
});

app.on('window-all-closed', () => app.quit());
