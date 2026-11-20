// UI harness: load the real app UI in headless Chrome with a mocked Tauri bridge
// that talks to the real core, then screenshot it. For layout checks only —
// the shipping webview is WKWebView/WebView2/WebKitGTK (spec §10.4).
//
//   node app/scripts/ui-shot.mjs <project-dir> <out.png> [--width 1400 --height 860 --wait 5000 --eval "<js>"]
// Requires the app's Vite dev server on :1420 (pnpm -C app dev).
import { closeSync, existsSync, openSync, readFileSync, readSync, statSync } from 'node:fs';
import { createRequire } from 'node:module';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const req = createRequire(path.join(here, '../../modules/render.compositor/package.json'));
const puppeteer = req('puppeteer');
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

const browser = await (puppeteer.default ?? puppeteer).launch({ dumpio: !!process.env.DUMPIO });
const page = await browser.newPage();
await page.setViewport({ width: +(opt.width ?? 1400), height: +(opt.height ?? 860), deviceScaleFactor: 1 });
page.on('pageerror', e => console.log('pageerror:', e.message));
page.on('error', e => console.log('PAGE CRASH:', e.message));
page.on('close', () => console.log('PAGE CLOSED'));
page.on('console', m => m.type() === 'error' && console.log('console.error:', m.text()));
const emit = (event, payload) => page.evaluate((e, p) => window.__emit(e, p), event, payload).catch(() => {});

await page.exposeFunction('__kurnHost', async (cmd, args) => {
  switch (cmd) {
    case 'core_call': return call(args.method, args.params);
    case 'initial_project': return dashboard ? null : path.resolve(project);
    case 'asset_base': return 'http://kurnasset.test/t';
    case 'asset_set_project': return null;
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
    const segs = u.pathname.split('/').slice(2).map(decodeURIComponent); // after /t/
    if (segs[0] === '__kurnace') {
      return r.respond({ status: 200, contentType: 'text/javascript', body: readFileSync(path.join(here, '../../packages/compositor/scene-runtime.js')) });
    }
    if (segs[0] === 'scene') {
      const dir = path.join(path.resolve(project), 'scenes', segs[1]);
      let rel = segs.slice(2).join('/');
      if (!rel) { const t = existsSync(path.join(dir, 'scene.toml')) ? readFileSync(path.join(dir, 'scene.toml'), 'utf8') : ''; rel = (/^\s*entry\s*=\s*"([^"]+)"/m.exec(t) ?? [])[1] ?? 'index.html'; }
      const file = path.join(dir, rel);
      if (/\.html?$/.test(file)) {
        const depth = 2 + (rel.match(/\//g) ?? []).length;
        const tag = `<script src="${'../'.repeat(depth)}__kurnace/scene-runtime.js"></script>`;
        const html = readFileSync(file, 'utf8').replace(/<head[^>]*>/i, h => h + tag);
        return r.respond({ status: 200, contentType: 'text/html', body: html });
      }
      return r.respond({ status: 200, contentType: file.endsWith('.js') ? 'text/javascript' : file.endsWith('.css') ? 'text/css' : 'application/octet-stream', body: readFileSync(file) });
    }
    if (segs[0] === 'frame') {
      const a = assetCache.get(segs[1]) ?? await call('lib.get', { id: segs[1] });
      assetCache.set(segs[1], a);
      const { execFileSync } = await import('node:child_process');
      const jpg = execFileSync('ffmpeg', ['-v', 'error', '-ss', segs[2], '-i', a.path, '-frames:v', '1', '-f', 'image2', '-c:v', 'mjpeg', '-q:v', '3', '-'], { maxBuffer: 1 << 26 });
      return r.respond({ status: 200, contentType: 'image/jpeg', body: jpg });
    }
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
      // Serve byte ranges like the real asset server; CDP messages cap at 100 MB.
      const size = statSync(a.path).size;
      const m = /bytes=(\d*)-(\d*)/.exec(r.headers().range ?? '');
      const start = m && m[1] ? +m[1] : 0;
      const end = Math.min(m && m[2] ? +m[2] : size - 1, start + 4 * 1024 * 1024 - 1, size - 1);
      const fd = openSync(a.path, 'r'); const buf = Buffer.alloc(end - start + 1); readSync(fd, buf, 0, buf.length, start); closeSync(fd);
      r.respond({ status: m || size > buf.length ? 206 : 200, contentType: a.mime ?? 'application/octet-stream',
        headers: { 'Access-Control-Allow-Origin': '*', 'Accept-Ranges': 'bytes', 'Content-Range': `bytes ${start}-${end}/${size}` }, body: buf });
    }
  } catch (e) { r.respond({ status: 404, body: String(e) }); }
});
await page.goto('http://localhost:1420/kurnace.html', { waitUntil: 'domcontentloaded' });
if (opt.monitor) {
  for (let i = 0; i < +opt.monitor; i++) {
    await new Promise(r => setTimeout(r, 400));
    try {
      const m = await page.metrics();
      console.log(`t=${(i + 1) * 400}ms heap=${(m.JSHeapUsedSize / 1e6).toFixed(0)}MB nodes=${m.Nodes} listeners=${m.JSEventListeners} layouts=${m.LayoutCount} recalcs=${m.RecalcStyleCount} scripts=${m.ScriptDuration.toFixed(2)}s`);
    } catch (e) { console.log('dead:', e.message.slice(0, 80)); break; }
  }
}
await new Promise(r => setTimeout(r, +(opt.wait ?? 5000)));
if (opt.script) {
  // --script file.mjs — export default async (page, { sleep, kurn, project }) => { … }; throw to fail.
  const mod = await import(path.resolve(opt.script));
  const { execFileSync } = await import('node:child_process');
  const kurn = (...args) => JSON.parse(execFileSync(path.join(here, '../../bin/kurn'), [...args, '--project', path.resolve(project), '--json'], { encoding: 'utf8', env: process.env }));
  try {
    await mod.default(page, { sleep: ms => new Promise(r => setTimeout(r, ms)), kurn, project: path.resolve(project) });
  } catch (e) {
    console.log('SCRIPT FAIL:', e?.stack ?? e);
    await page.screenshot({ path: out });
    await browser.close();
    process.exit(1);
  }
}
if (opt.click) {
  // --click "selector@x,y" — a real mouse click at (x, y) inside the element.
  for (const spec of String(opt.click).split(';')) {
    const [sel, xy] = spec.split('@');
    const [dx, dy] = xy.split(',').map(Number);
    const box = await (await page.$(sel))?.boundingBox();
    if (!box) { console.log('click: no element', sel); continue; }
    await page.mouse.click(box.x + dx, box.y + dy);
    await new Promise(r => setTimeout(r, 600));
  }
}
if (opt.eval) { await page.evaluate(opt.eval); await new Promise(r => setTimeout(r, 1500)); }
if (opt.probe) console.log('probe:', JSON.stringify(await page.evaluate(opt.probe)));
await page.screenshot({ path: out });
console.log('saved', out);
await browser.close();
// The live doc subscription socket would keep Node alive.
process.exit(0);
