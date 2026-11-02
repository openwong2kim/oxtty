const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('pty', {
  wasm: () => ipcRenderer.invoke('wongtty:wasm'),
  write: data => ipcRenderer.send('pty:input', data),
  resize: (cols, rows) => ipcRenderer.send('pty:resize', cols, rows),
  onData: listener => ipcRenderer.on('pty:data', (_, data) => listener(data)),
});
