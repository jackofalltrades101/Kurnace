// UI harness: load the real app UI in headless Chrome with a mocked Tauri bridge
// that talks to the real core, then screenshot it. For layout checks only —
// the shipping webview is WKWebView/WebView2/WebKitGTK (spec §10.4).
//
//   node app/scripts/ui-shot.mjs <project-dir> <out.png> [--width 1400 --height 860 --wait 5000 --eval "<js>"]
// Requires the app's Vite dev server on :1420 (pnpm -C app dev).
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const req = createRequire(path.join(here, '../../modules/render.revideo/package.json'));
const puppeteer = req(req.resolve('puppeteer', { paths: [path.dirname(req.resolve('@revideo/renderer'))] }));
const [project, out, ...rest] = process.argv.slice(2);
const opt = Object.fromEntries(rest.reduce((a, v, i, xs) => (v.startsWith('--') ? [...a, [v.slice(2), xs[i + 1]]] : a), []));
const dashboard = rest.includes('--dashboard');
const SOCK = process.env.KURN_SOCKET ?? path.join(os.homedir(), '.kurnace/core.sock');
const ACTOR = `human:${os.userInfo().username}`;

function call(method, params, onEvent) {
  return new Promise((resolve, reject) => {
    const s = net.connect(SOCK);
    let buf = '';
    s.on('error', reject);
    s.on('data', d => {
      buf += d;
      let i;
      while ((i = buf.indexOf('\n')) >= 0) {
        const msg = JSON.parse(buf.slice(0, i));
        buf = buf.slice(i + 1);
        if (msg.event) { onEvent?.(msg.event); continue; }
        if (onEvent && method === 'doc.subscribe') { onEvent({ __reply: msg }); continue; }
        s.end();
        msg.ok ? resolve(msg.result) : reject(new Error(msg.error));
      }
    });
    s.write(JSON.stringify({ id: 1, method, actor: ACTOR, params }) + '\n');
  });
}

const browser = await (puppeteer.default ?? puppeteer).launch();
const page = await browser.newPage();
await page.setViewport({ width: +(opt.width ?? 1400), height: +(opt.height ?? 860), deviceScaleFactor: 1 });
page.on('pageerror', e => console.log('pageerror:', e.message));
page.on('console', m => m.type() === 'error' && console.log('console.error:', m.text()));
const emit = (event, payload) => page.evaluate((e, p) => window.__emit(e, p), event, payload).catch(() => {});

await page.exposeFunction('__kurnHost', async (cmd, args) => {
  switch (cmd) {
    case 'core_call': return call(args.method, args.params);
    case 'initial_project': return dashboard ? null : path.resolve(project);
    case 'asset_base': return 'http://kurnasset.test/t';
    case 'fish_status_cmd': return { configured: false, voice: '', model: 's2.1-pro', speed: 1 };
    case 'fish_save': return { configured: true, hint: '…test', voice: args.voice ?? '', model: args.model ?? 's2.1-pro', speed: args.speed ?? 1 };
    case 'fish_clear': return { configured: false, voice: '', model: 's2.1-pro', speed: 1 };
    case 'doc_subscribe':
      call('doc.subscribe', { project: args.project }, ev => {
        if (ev.__reply) emit('kurn-doc', { project: args.project, kind: 'state', event: ev.__reply.result });
        else emit('kurn-doc', { project: args.project, kind: ev.type, event: ev });
      });
      return null;
    case 'log_line': console.log('[webview]', args.line); return null;
    default: return null; // which, pty_*, remember_project, save_png
  }
});
await page.evaluateOnNewDocument(() => {
  const listeners = {};
  let n = 0;
  window.__emit = (event, payload) => (listeners[event] ?? []).forEach(h => window[`_${h}`]?.({ event, payload, id: 0 }));
  window.__TAURI_INTERNALS__ = {
    metadata: { currentWindow: { label: 'main' }, currentWebview: { windowLabel: 'main', label: 'main' } },
    transformCallback: cb => { const id = ++n; window[`_${id}`] = cb; return id; },
    convertFileSrc: (p, proto) => `http://${proto}.test/${p}`,
    invoke: async (cmd, args) => {
      if (cmd === 'plugin:event|listen') { (listeners[args.event] ??= []).push(args.handler); return args.handler; }
      if (cmd.startsWith('plugin:')) return null;
      return window.__kurnHost(cmd, args);
    },
  };
});
await page.setRequestInterception(true);
const assetCache = new Map();
page.on('request', async r => {
  const u = new URL(r.url());
  if (u.hostname !== 'kurnasset.test') return r.continue();
  try {
    const parts = u.pathname.split('/').filter(Boolean); // [token, (thumbs|peaks)?, id]
    const id = parts[parts.length - 1].split('.')[0];
    if (parts[parts.length - 3] === 'seq') {
      const sid = parts[parts.length - 2];
      const frame = parts[parts.length - 1];
      const a = await call('lib.get', { id: sid });
      if (!Array.isArray(a.frames) || !a.frames.includes(frame)) throw new Error('unknown frame');
      r.respond({ status: 200, contentType: frame.endsWith('.jpg') ? 'image/jpeg' : 'image/png', headers: { 'Access-Control-Allow-Origin': '*' }, body: readFileSync(`${a.path}/${frame}`) });
    } else if (parts[parts.length - 2] === 'thumbs') {
      const t = await call('lib.thumb', { id });
      r.respond({ status: 200, contentType: t.mime, headers: { 'Access-Control-Allow-Origin': '*' }, body: readFileSync(t.path) });
    } else if (parts[parts.length - 2] === 'peaks') {
      const p = await call('lib.peaks', { id });
      r.respond({ status: 200, contentType: 'application/json', headers: { 'Access-Control-Allow-Origin': '*' }, body: JSON.stringify(p) });
    } else {
      const a = assetCache.get(id) ?? await call('lib.get', { id });
      assetCache.set(id, a);
      r.respond({ status: 200, contentType: a.mime ?? 'application/octet-stream', headers: { 'Access-Control-Allow-Origin': '*' }, body: readFileSync(a.path) });
    }
  } catch (e) { r.respond({ status: 404, body: String(e) }); }
});
await page.goto('http://localhost:1420/kurnace.html', { waitUntil: 'domcontentloaded' });
await new Promise(r => setTimeout(r, +(opt.wait ?? 5000)));
if (opt.eval) { await page.evaluate(opt.eval); await new Promise(r => setTimeout(r, 1500)); }
if (opt.probe) console.log('probe:', JSON.stringify(await page.evaluate(opt.probe)));
await page.screenshot({ path: out });
console.log('saved', out);
await browser.close();
