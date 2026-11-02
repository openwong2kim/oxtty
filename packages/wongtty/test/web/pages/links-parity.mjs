// Page for links.test.mjs: an wongtty/xterm Terminal with its built-in WebLinksAddon next to the real xterm.js 6.1
// with @xterm/addon-web-links (WebGL and Unicode 11 addons, so both have the same cells). Both get the same
// linkHandler, web links handler and options, and the same link providers; every callback is logged, so the test
// can compare what each terminal hovers, leaves and activates under the same CDP mouse events.
import { init, Terminal, WebLinksAddon } from 'wongtty/xterm';
import { Terminal as XtermTerminal } from '@xterm/xterm';
import { WebglAddon } from '@xterm/addon-webgl';
import { Unicode11Addon } from '@xterm/addon-unicode11';
import { WebLinksAddon as XtermWebLinksAddon } from '@xterm/addon-web-links';

const COLS = 60;
const ROWS = 14;
const OPTIONS = { cols: COLS, rows: ROWS, fontFamily: 'Menlo, monospace', fontSize: 14, scrollback: 50 };

const frame = () => new Promise((resolve) => requestAnimationFrame(() => resolve()));
const range = (r) => r && [r.start.x, r.start.y, r.end.x, r.end.y];

/** Links for every match of `pattern` on a line, the way wmux's providers build them. */
function patternProvider(term, pattern, log, name, { async = false } = {}) {
  return {
    provideLinks(y, callback) {
      const line = term.buffer.active.getLine(y - 1);
      const text = line ? line.translateToString(true) : '';
      const links = [...text.matchAll(pattern)].map((m) => ({
        range: { start: { x: m.index + 1, y }, end: { x: m.index + m[0].length, y } },
        text: m[0],
        activate: (event, t) => log.push([`${name}-activate`, t, event.button]),
        hover: (event, t) => log.push([`${name}-hover`, t]),
        leave: (event, t) => log.push([`${name}-leave`, t]),
      }));
      log.push([`${name}-provide`, y]);
      const reply = links.length ? links : undefined;
      if (async) setTimeout(() => callback(reply), 0);
      else callback(reply);
    },
  };
}

async function setup() {
  await init();
  document.body.style.cssText = 'margin: 0; display: flex; gap: 40px; padding: 30px 10px';
  const [a, b] = [document.createElement('div'), document.createElement('div')];
  document.body.append(a, b);
  const wongtty = new Terminal(OPTIONS);
  wongtty.open(a);
  const xterm = new XtermTerminal({ ...OPTIONS, allowProposedApi: true });
  xterm.open(b);
  xterm.loadAddon(new Unicode11Addon());
  xterm.unicode.activeVersion = '11';
  xterm.loadAddon(new WebglAddon());
  const both = [wongtty, xterm];
  const logs = [[], []];
  const webLinks = [];
  const disposables = [[], []];
  both.forEach((t, k) => {
    const log = logs[k];
    t.options.linkHandler = {
      activate: (event, text, r) => log.push(['osc-activate', text, range(r), event.button]),
      hover: (event, text, r) => log.push(['osc-hover', text, range(r)]),
      leave: (event, text, r) => log.push(['osc-leave', text, range(r)]),
    };
    // Registered before the web links, like wmux's hard-wrapped provider: it wins where both have a link
    disposables[k].push(t.registerLinkProvider(patternProvider(t, /TICKET-\d+|OVERLAP https/g, log, 'ticket')));
    const handler = (event, uri) => log.push(['web-activate', uri, event.button]);
    const options = { hover: (event, uri, r) => log.push(['web-hover', uri, range(r)]), leave: (event, uri) => log.push(['web-leave', uri]) };
    const addon = k === 0 ? new WebLinksAddon(handler, options) : new XtermWebLinksAddon(handler, options);
    t.loadAddon(addon);
    webLinks.push(addon);
    disposables[k].push(t.registerLinkProvider(patternProvider(t, /ASYNC-\d+/g, log, 'async', { async: true })));
  });
  const screens = both.map((t) => t.element.querySelector('.xterm-screen'));
  Object.assign(window, {
    /** Writes to terminal `k` only and waits for it to parse and draw. */
    writeOne: async (k, text) => {
      await new Promise((resolve) => both[k].write(text, resolve));
      await frame();
      await frame();
    },
    write: async (text) => {
      await Promise.all(both.map((t) => new Promise((resolve) => t.write(text, resolve))));
      await frame();
      await frame();
    },
    /** Client coordinates of the centre of cell (col, row) of each terminal. */
    points: (col, row) => screens.map((screen, k) => {
      const rect = screen.getBoundingClientRect();
      return [rect.left + (col + 0.5) * rect.width / both[k].cols, rect.top + (row + 0.5) * rect.height / both[k].rows];
    }),
    /** Terminal `k`'s callbacks since the last call and whether its pointer cursor is on. */
    state: (k) => ({ log: logs[k].splice(0), pointer: screens[k].classList.contains('xterm-cursor-pointer') }),
    selection: () => both.map((t) => t.getSelectionPosition() ?? null),
    call: (name, args) => both.forEach((t) => t[name](...args)),
    allowNonHttpProtocols: (on) => both.forEach((t) => { t.options.linkHandler = { ...t.options.linkHandler, allowNonHttpProtocols: on }; }),
    disposeWebLinks: () => webLinks.forEach((addon) => addon.dispose()),
    disposeProviders: () => disposables.forEach((list) => list.splice(0).forEach((d) => d.dispose())),
    cells: () => screens.map((screen, k) => screen.getBoundingClientRect().width / both[k].cols),
    /** The bottom pixel row of wongtty's cells `start..end` on viewport row `row`: how many pixels are lit. */
    underlinePixels: (row, start, end) => {
      const canvas = [...wongtty.element.querySelectorAll('canvas')].find((c) => c.style.display !== 'none');
      const copy = document.createElement('canvas');
      [copy.width, copy.height] = [canvas.width, canvas.height];
      const ctx = copy.getContext('2d');
      ctx.drawImage(canvas, 0, 0);
      const cellWidth = canvas.width / COLS;
      const cellHeight = canvas.height / ROWS;
      const y = Math.floor((row + 1) * cellHeight) - 1;
      const pixels = ctx.getImageData(Math.floor(start * cellWidth), y, Math.max(1, Math.floor((end - start) * cellWidth)), 1).data;
      let lit = 0;
      for (let i = 0; i < pixels.length; i += 4) if (pixels[i] + pixels[i + 1] + pixels[i + 2] > 300) lit++;
      return lit;
    },
  });
}

/** Builds both terminals; resolves with an error message, or undefined. */
window.start = () => setup().then(() => undefined, (err) => String(err?.stack ?? err));
