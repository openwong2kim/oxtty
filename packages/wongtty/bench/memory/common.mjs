// What bench.mjs and breakdown.mjs share: the static server for the page, Chrome on the real GPU, and process
// memory readers.
import { chromium } from 'playwright-core';
import { createServer } from 'node:http';
import { readFile } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import { dirname, extname, join, normalize } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..', '..');
const TYPES = { '.js': 'text/javascript', '.mjs': 'text/javascript', '.wasm': 'application/wasm', '.css': 'text/css' };
const IMPORTS = {
  wongtty: '/dist/index.js',
  'wongtty/xterm': '/dist/xterm.js',
  '@xterm/xterm': '/node_modules/@xterm/xterm/lib/xterm.mjs',
  '@xterm/addon-webgl': '/node_modules/@xterm/addon-webgl/lib/addon-webgl.mjs',
};
const html = `<!doctype html><meta charset="utf-8"><link rel="icon" href="data:,">
<link rel="stylesheet" href="/node_modules/@xterm/xterm/css/xterm.css">
<script type="importmap">${JSON.stringify({ imports: IMPORTS })}</script>
<body style="margin:0"><script type="module" src="/bench/memory/page.mjs"></script>`;

/** Serves the package root and the page; resolves to the server once it listens. */
export async function serve() {
  const server = createServer(async (req, res) => {
    const path = normalize(new URL(req.url, 'http://x').pathname);
    if (path === '/') return res.writeHead(200, { 'content-type': 'text/html' }).end(html);
    if (path.includes('..')) return res.writeHead(404).end();
    try {
      const body = await readFile(join(root, path));
      res.writeHead(200, { 'content-type': TYPES[extname(path)] ?? 'application/octet-stream' }).end(body);
    } catch {
      res.writeHead(404).end();
    }
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  return server;
}

/** Headless system Chrome on the real GPU (Metal through ANGLE, GPU blocklist ignored). */
export function launch() {
  return chromium.launch({ channel: 'chrome', headless: true, args: ['--use-angle=metal', '--enable-gpu', '--ignore-gpu-blocklist'] });
}

export function rss(pid) {
  try {
    return Number(execFileSync('ps', ['-o', 'rss=', '-p', String(pid)]).toString().trim()) * 1024;
  } catch {
    return 0;
  }
}

const UNITS = { B: 1, KB: 1024, MB: 1048576, GB: 1073741824 };
const bytes = (n, unit) => Number(n) * UNITS[unit];

/** Physical footprint in bytes (macOS `footprint`), 0 where it is missing. */
export function footprint(pid) {
  return footprintDetail(pid).total;
}

/** The footprint and its dirty bytes per region category (`MALLOC_SMALL`, `IOAccelerator`, ...). */
export function footprintDetail(pid) {
  let out;
  try {
    out = execFileSync('footprint', ['-p', String(pid)]).toString();
  } catch {
    return { total: 0, categories: {} };
  }
  const m = /Footprint: ([\d.]+) (B|KB|MB|GB)/.exec(out);
  const categories = {};
  for (const line of out.split('\n')) {
    const row = /^\s*([\d.]+) (B|KB|MB|GB)\s+[\d.]+ (?:B|KB|MB|GB)\s+[\d.]+ (?:B|KB|MB|GB)\s+\d+\s+(.+)$/.exec(line);
    if (row && row[3] !== 'TOTAL') categories[row[3].trim()] = bytes(row[1], row[2]);
  }
  return { total: m ? bytes(m[1], m[2]) : 0, categories };
}

/** Pids of the browser's processes by CDP type ('renderer', 'GPU', ...). */
export async function pids(browserCdp) {
  const out = {};
  for (const p of (await browserCdp.send('SystemInfo.getProcessInfo')).processInfo) (out[p.type] ??= []).push(p.id);
  return out;
}

export const mb = (b) => (b / 1048576).toFixed(0);
export const mb1 = (b) => (b / 1048576).toFixed(1);
