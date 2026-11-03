// Browser tests for wongtty/xterm: serves the package directory, opens a page module in headless system Chrome
// (SwiftShader WebGL2) and hands the page to the test. Page modules import 'wongtty/xterm' (the built dist) and
// real xterm.js as '@xterm/xterm' / '@xterm/addon-webgl' / '@xterm/addon-unicode11' / '@xterm/addon-web-links' through
// an import map, so both run side by side; 'wongtty' is the engine (index.js) for its crash trigger.
import { chromium } from 'playwright-core';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { dirname, extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.wasm': 'application/wasm', '.css': 'text/css', '.json': 'application/json' };
const IMPORTS = {
  wongtty: '/dist/index.js',
  'wongtty/xterm': '/dist/xterm.js',
  '@xterm/xterm': '/node_modules/@xterm/xterm/lib/xterm.mjs',
  '@xterm/addon-webgl': '/node_modules/@xterm/addon-webgl/lib/addon-webgl.mjs',
  '@xterm/addon-unicode11': '/node_modules/@xterm/addon-unicode11/lib/addon-unicode11.mjs',
  '@xterm/addon-web-links': '/node_modules/@xterm/addon-web-links/lib/addon-web-links.mjs',
  '@xterm/addon-image': '/node_modules/@xterm/addon-image/lib/addon-image.mjs',
};

/**
 * Opens `page` (a module under test/web/pages) and calls `fn(page)`. Any console error or page exception fails
 * the call. Options: `viewport`, `deviceScaleFactor`, `gpu` (the real GPU, Metal through ANGLE, instead of SwiftShader), `userAgent`.
 */
export async function withPage(page, fn, { viewport = { width: 1000, height: 700 }, deviceScaleFactor = 1, gpu = false, userAgent } = {}) {
  const html = `<!doctype html><meta charset="utf-8"><link rel="icon" href="data:,">
<link rel="stylesheet" href="/node_modules/@xterm/xterm/css/xterm.css">
<script type="importmap">${JSON.stringify({ imports: IMPORTS })}</script>
<body style="margin:0;background:#111"><div id="terminal"></div>
<script type="module" src="/test/web/pages/${page}"></script>`;
  const server = createServer(async (req, res) => {
    const path = normalize(new URL(req.url, 'http://x').pathname);
    if (path === '/') return res.writeHead(200, { 'content-type': 'text/html' }).end(html);
    if (path.includes('..')) return res.writeHead(404).end();
    try {
      // Read before sending headers, so a missing file is a plain 404 (e.g. a devDependency not installed yet).
      const body = await readFile(join(root, path));
      res.writeHead(200, { 'content-type': TYPES[extname(path)] ?? 'application/octet-stream' }).end(body);
    } catch {
      res.writeHead(404).end();
    }
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const errors = [];
  let browser;
  try {
    // Launched inside the try, so a failed launch still closes the server instead of keeping the process alive.
    const args = gpu ? ['--use-angle=metal', '--enable-gpu', '--ignore-gpu-blocklist'] : ['--enable-unsafe-swiftshader', '--use-angle=swiftshader'];
    browser = await chromium.launch({ channel: 'chrome', headless: true, args });
    const tab = await browser.newPage({ viewport, deviceScaleFactor, userAgent });
    tab.on('console', (m) => m.type() === 'error' && errors.push(`console: ${m.text()}`));
    tab.on('pageerror', (e) => errors.push(`pageerror: ${e.message}`));
    await tab.goto(`http://127.0.0.1:${server.address().port}/`);
    const result = await fn(tab);
    if (errors.length) throw new Error(errors.join('\n'));
    return result;
  } finally {
    await browser?.close();
    server.close();
  }
}

/** A Chrome DevTools Protocol session on `page`, for input (Input.*), heap and DOM-listener domains. */
export const cdp = (page) => page.context().newCDPSession(page);
