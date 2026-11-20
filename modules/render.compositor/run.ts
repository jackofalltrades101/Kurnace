// render.compositor — Kurnace's renderer (docs/v2-plan.md §3.1).
//
// stdin: one JSON request (same shape the core sends every renderer).
// stdout: NDJSON events (spec §9.2). Logs go to stderr.
//
// Serves the compositor render page, media, exact video frames, frame
// sequences and project scenes from a private loopback HTTP server, drives
// headless Chromium frame by frame, and streams the captured frames into
// ffmpeg, which also mixes the audio.
import { type ChildProcess, execFile, spawn } from 'node:child_process';
import { createReadStream, existsSync, mkdirSync, readFileSync, readdirSync, statSync } from 'node:fs';
import { mkdir, mkdtemp, rename, rm, writeFile } from 'node:fs/promises';
import http from 'node:http';
import { createRequire } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { type Timeline, clipEnd, framesToSeconds, fpsValue, validateTimeline } from '@kurnace/timeline';

const exec = promisify(execFile);
const here = path.dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);
const COMPOSITOR = path.dirname(require.resolve('@kurnace/compositor/scene-runtime.js'));

type Ev = Record<string, unknown> & { type: string };
const emit = (e: Ev) => process.stdout.write(JSON.stringify(e) + '\n');
const log = (msg: string, level = 'info') => emit({ type: 'log', level, msg });
for (const k of ['log', 'info', 'debug', 'warn'] as const) {
  console[k] = (...a: unknown[]) => process.stderr.write(a.map(String).join(' ') + '\n');
}

interface AssetIn {
  path: string; kind: string; ext?: string | null; width?: number | null; height?: number | null;
  fps_num?: number | null; fps_den?: number | null; duration_s?: number | null; sample_rate?: number | null;
}
interface Request {
  timeline: Timeline;
  assets: Record<string, AssetIn>;
  project_root?: string;
  mode?: 'still' | 'draft' | 'final';
  frame?: number;
  range?: string;
  scale?: number;
  out: string;
  background?: string;
}

const MIME: Record<string, string> = {
  html: 'text/html; charset=utf-8', js: 'text/javascript', mjs: 'text/javascript', css: 'text/css', json: 'application/json',
  png: 'image/png', jpg: 'image/jpeg', jpeg: 'image/jpeg', webp: 'image/webp', gif: 'image/gif', svg: 'image/svg+xml',
  mp4: 'video/mp4', mov: 'video/quicktime', webm: 'video/webm', wav: 'audio/wav', mp3: 'audio/mpeg', m4a: 'audio/aac',
  ogg: 'audio/ogg', woff: 'font/woff', woff2: 'font/woff2', ttf: 'font/ttf', otf: 'font/otf', glb: 'model/gltf-binary',
};
const mimeOf = (p: string) => MIME[path.extname(p).slice(1).toLowerCase()] ?? 'application/octet-stream';

async function readStdin(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const c of process.stdin) chunks.push(c as Buffer);
  return Buffer.concat(chunks).toString('utf8');
}

function parseRange(r: string | undefined, duration: number): [number, number] {
  if (!r) return [0, duration];
  const m = /^(\d+)-(\d+)$/.exec(r);
  if (!m) throw new Error(`bad range "${r}" (expected start-end in frames)`);
  const a = Number(m[1]), b = Math.min(Number(m[2]), duration);
  if (!(b > a)) throw new Error(`bad range "${r}": end must be > start`);
  return [a, b];
}

// ---------- video frame cache ----------

const CACHE = path.join(process.env.KURNACE_CACHE ?? path.join(os.homedir(), '.kurnace', 'cache'), 'frames');
const frameDir = (assetId: string, tl: Timeline) => path.join(CACHE, assetId, `${tl.fps.num}_${tl.fps.den}`);
const frameFile = (dir: string, index: number) => path.join(dir, `${String(index).padStart(8, '0')}.jpg`);

/** Decode source frames [first, first+count) at the timeline's fps, once, into the cache. */
async function extractRange(src: string, dir: string, tl: Timeline, first: number, count: number) {
  if (count <= 0) return;
  let missing = false;
  for (let i = first; i < first + count; i++) if (!existsSync(frameFile(dir, i))) { missing = true; break; }
  if (!missing) return;
  mkdirSync(dir, { recursive: true });
  const tmp = `${dir}.tmp-${process.pid}-${first}`;
  mkdirSync(tmp, { recursive: true });
  // -ss before -i: accurate seek; output frame k = source time (first + k) / fps.
  await exec('ffmpeg', ['-v', 'error', '-y', '-ss', framesToSeconds(first, tl.fps).toFixed(6), '-i', src,
    '-frames:v', String(count), '-vf', `fps=${tl.fps.num}/${tl.fps.den}`, '-q:v', '2', '-start_number', String(first),
    path.join(tmp, '%08d.jpg')], { maxBuffer: 1 << 24 });
  for (const f of readdirSync(tmp)) await rename(path.join(tmp, f), path.join(dir, f)).catch(() => {});
}

async function frameOnDemand(src: string, dir: string, tl: Timeline, index: number): Promise<string> {
  const f = frameFile(dir, index);
  if (!existsSync(f)) await extractRange(src, dir, tl, index, 1);
  if (!existsSync(f)) {
    // Past the end of the source: hold the last frame ffmpeg can produce.
    mkdirSync(dir, { recursive: true });
    await exec('ffmpeg', ['-v', 'error', '-y', '-sseof', '-0.1', '-i', src, '-frames:v', '1', '-q:v', '2', f]).catch(() => {});
  }
  return f;
}

// ---------- media server ----------

function sendFile(req: http.IncomingMessage, res: http.ServerResponse, file: string, type = mimeOf(file)) {
  const size = statSync(file).size;
  const m = /bytes=(\d*)-(\d*)/.exec(req.headers.range ?? '');
  res.setHeader('Accept-Ranges', 'bytes');
  res.setHeader('Content-Type', type);
  res.setHeader('Cache-Control', 'no-store');
  if (m) {
    const start = m[1] ? +m[1] : size - +m[2];
    const end = m[1] && m[2] ? Math.min(+m[2], size - 1) : size - 1;
    res.writeHead(206, { 'Content-Range': `bytes ${start}-${end}/${size}`, 'Content-Length': end - start + 1 });
    createReadStream(file, { start, end }).pipe(res);
  } else {
    res.writeHead(200, { 'Content-Length': size });
    createReadStream(file).pipe(res);
  }
}

function sceneEntry(dir: string): string {
  const toml = path.join(dir, 'scene.toml');
  if (existsSync(toml)) {
    const m = /^\s*entry\s*=\s*"([^"]+)"/m.exec(readFileSync(toml, 'utf8'));
    if (m) return m[1];
  }
  return 'index.html';
}

/** Serve one scene file; HTML gets the scene runtime injected before any scene script. */
export function serveScene(req: http.IncomingMessage, res: http.ServerResponse, scenesRoot: string, name: string, rel: string, runtimeUrl: string) {
  const dir = path.resolve(scenesRoot, name);
  if (!dir.startsWith(path.resolve(scenesRoot) + path.sep)) return void res.writeHead(403).end();
  const file = path.resolve(dir, rel || sceneEntry(dir));
  if (!file.startsWith(dir + path.sep) || !existsSync(file) || statSync(file).isDirectory()) {
    return void res.writeHead(404, { 'Content-Type': 'text/plain' }).end(`scene ${name}: ${rel || 'entry'} not found`);
  }
  if (file.endsWith('.html') || file.endsWith('.htm')) {
    let html = readFileSync(file, 'utf8');
    const tag = `<script src="${runtimeUrl}"></script>`;
    html = /<head[^>]*>/i.test(html) ? html.replace(/<head[^>]*>/i, h => `${h}${tag}`) : tag + html;
    res.writeHead(200, { 'Content-Type': MIME.html, 'Cache-Control': 'no-store' });
    return void res.end(html);
  }
  sendFile(req, res, file);
}

function startServer(rq: Request, tl: Timeline): Promise<{ url: string; close: () => void }> {
  const dist = path.join(COMPOSITOR, 'dist');
  if (!existsSync(path.join(dist, 'render.html'))) throw new Error(`compositor isn't built: run \`pnpm -C ${COMPOSITOR} build\``);
  const seqCache = new Map<string, string[]>();
  const server = http.createServer(async (req, res) => {
    try {
      const url = new URL(req.url ?? '/', 'http://x');
      const parts = url.pathname.split('/').filter(Boolean).map(decodeURIComponent);
      if (parts[0] !== 'media') {
        const file = path.join(dist, parts.join('/') || 'render.html');
        if (!file.startsWith(dist) || !existsSync(file)) return void res.writeHead(404).end();
        return sendFile(req, res, file);
      }
      const [, a, b, c, ...rest] = parts;
      if (a === '__kurnace' && b === 'scene-runtime.js') return sendFile(req, res, path.join(COMPOSITOR, 'scene-runtime.js'));
      if (a === 'scene') {
        if (!rq.project_root) return void res.writeHead(404).end('no project_root for scenes');
        return serveScene(req, res, path.join(rq.project_root, 'scenes'), b, [c, ...rest].filter(Boolean).join('/'), '/media/__kurnace/scene-runtime.js');
      }
      if (a === 'frame') {
        const asset = rq.assets[b];
        if (!asset) return void res.writeHead(404).end();
        const index = Math.round(Number(c) * fpsValue(tl.fps));
        return sendFile(req, res, await frameOnDemand(asset.path, frameDir(b, tl), tl, index), 'image/jpeg');
      }
      if (a === 'seq') {
        const asset = rq.assets[b];
        if (!asset) return void res.writeHead(404).end();
        let files = seqCache.get(b);
        if (!files) {
          files = readdirSync(asset.path).filter(f => /\.(png|jpe?g|webp)$/i.test(f)).sort();
          seqCache.set(b, files);
        }
        if (!files.length) return void res.writeHead(404).end('empty sequence');
        const i = Math.max(0, Math.min(files.length - 1, Number(c)));
        return sendFile(req, res, path.join(asset.path, files[i]));
      }
      const id = a.split('.')[0];
      const asset = rq.assets[id];
      if (!asset || !existsSync(asset.path)) return void res.writeHead(404).end();
      return sendFile(req, res, asset.path, MIME[asset.ext ?? ''] ?? mimeOf(asset.path));
    } catch (e) {
      log(`server: ${req.url}: ${String(e)}`, 'warn');
      if (!res.headersSent) res.writeHead(500).end(String(e));
    }
  });
  return new Promise(resolve => server.listen(0, '127.0.0.1', () => {
    const { port } = server.address() as { port: number };
    resolve({ url: `http://127.0.0.1:${port}`, close: () => server.close() });
  }));
}

// ---------- audio ----------

async function hasAudio(file: string) {
  const { stdout } = await exec('ffprobe', ['-v', 'error', '-select_streams', 'a', '-show_entries', 'stream=index', '-of', 'csv=p=0', file]).catch(() => ({ stdout: '' }));
  return stdout.trim().length > 0;
}

/** ffmpeg inputs + filter for the audio of frames [a, b). Muted tracks are skipped. */
async function audioGraph(rq: Request, tl: Timeline, a: number, b: number, firstInput: number) {
  const inputs: string[] = [];
  const chains: string[] = [];
  const muted = new Set(tl.tracks.filter(t => t.muted).map(t => t.id));
  for (const [id, c] of Object.entries(tl.clips)) {
    if (!c.asset || muted.has(c.track)) continue;
    const asset = rq.assets[c.asset];
    if (!asset || (asset.kind !== 'audio' && asset.kind !== 'video')) continue;
    const from = Math.max(c.start, a), to = Math.min(clipEnd(c), b);
    if (to <= from) continue;
    if (asset.kind === 'video' && !(await hasAudio(asset.path))) continue;
    const srcStart = framesToSeconds(c.in + (from - c.start), tl.fps);
    const len = framesToSeconds(to - from, tl.fps);
    const delayMs = Math.round(framesToSeconds(from - a, tl.fps) * 1000);
    const vol = typeof c.props?.volume === 'number' ? c.props.volume : 1;
    const n = firstInput + inputs.length / 2;
    inputs.push('-i', asset.path);
    const offset = c.offset_samples && asset.sample_rate ? c.offset_samples / asset.sample_rate : 0;
    chains.push(`[${n}:a]atrim=start=${(srcStart + offset).toFixed(6)}:duration=${len.toFixed(6)},asetpts=PTS-STARTPTS,`
      + `aformat=sample_rates=48000:channel_layouts=stereo,volume=${vol},adelay=${delayMs}|${delayMs}[a${id.replace(/\W/g, '_')}]`);
  }
  if (!chains.length) return null;
  const labels = chains.map(ch => ch.slice(ch.lastIndexOf('[')));
  const mix = `${chains.join(';')};${labels.join('')}amix=inputs=${labels.length}:normalize=0:dropout_transition=0,apad[aout]`;
  return { inputs, filter: mix };
}

// ---------- render ----------

async function main() {
  const rq = JSON.parse(await readStdin()) as Request;
  const tl = rq.timeline;
  const mode = rq.mode ?? 'final';
  const issues = validateTimeline(tl, new Set(Object.keys(rq.assets)));
  for (const i of issues) if (!i.msg.startsWith('transitions')) log(i.msg, i.level);
  if (issues.some(i => i.level === 'error')) throw new Error('timeline failed validation');

  const [a, b] = mode === 'still' ? [rq.frame ?? 0, (rq.frame ?? 0) + 1] : parseRange(rq.range, tl.duration);
  if (a < 0 || a >= Math.max(tl.duration, 1)) throw new Error(`frame ${a} is outside the timeline (0–${tl.duration - 1})`);
  const out = path.resolve(rq.out);
  await mkdir(path.dirname(out), { recursive: true });
  const scale = mode === 'draft' ? (rq.scale ?? 0.5) : (rq.scale ?? 1);

  // Pre-extract the video frames this range needs (one ffmpeg pass per clip).
  emit({ type: 'progress', pct: 0, msg: 'preparing media' });
  for (const c of Object.values(tl.clips)) {
    const asset = c.asset ? rq.assets[c.asset] : undefined;
    if (!asset || asset.kind !== 'video') continue;
    const from = Math.max(c.start, a), to = Math.min(clipEnd(c), b);
    if (to > from) await extractRange(asset.path, frameDir(c.asset!, tl), tl, c.in + (from - c.start), to - from);
  }

  const srv = await startServer(rq, tl);
  const require2 = createRequire(path.join(here, 'package.json'));
  const puppeteer = require2('puppeteer');
  // One browser per worker: screenshots are encoded in the browser process,
  // so pages sharing a browser serialize on it.
  const browsers: { close(): Promise<void>; newPage(): Promise<any> }[] = [];
  const launch = async () => {
    const b = await (puppeteer.default ?? puppeteer).launch({
      headless: true,
      args: ['--hide-scrollbars', '--mute-audio', '--disable-background-timer-throttling', '--disable-renderer-backgrounding', '--autoplay-policy=no-user-gesture-required'],
    });
    browsers.push(b);
    return b;
  };
  const procs: ChildProcess[] = [];
  const assetRefs = Object.fromEntries(Object.entries(rq.assets).map(([id, x]) => [id, {
    kind: x.kind, ext: x.ext, width: x.width, height: x.height, fps: x.fps_num && x.fps_den ? x.fps_num / x.fps_den : null,
  }]));
  const clip = { x: 0, y: 0, width: tl.width, height: tl.height };

  /** A compositor page in its own browser context (own renderer process). */
  async function openPage() {
    const page = await (await launch()).newPage();
    await page.setViewport({ width: tl.width, height: tl.height, deviceScaleFactor: scale });
    const pageErrors: string[] = [];
    page.on('pageerror', (e: Error) => pageErrors.push(e.message));
    await page.goto(`${srv.url}/render.html`, { waitUntil: 'load' });
    await page.waitForFunction(() => !!(window as unknown as { __kurn?: unknown }).__kurn);
    await page.evaluate((t: Timeline, as: unknown, base: string, bg: string) => (window as any).__kurn.load(t, as, base, bg),
      tl, assetRefs, `${srv.url}/media`, rq.background ?? '#000000');
    const seek = async (f: number) => {
      await page.evaluate((fr: number) => (window as any).__kurn.seek(fr), f);
      const errs: string[] = await page.evaluate(() => (window as any).__kurn.errors());
      if (errs.length || pageErrors.length) throw new Error([...errs, ...pageErrors.splice(0)].join('\n'));
    };
    return { page, seek };
  }

  const fps = `${tl.fps.num}/${tl.fps.den}`;
  const LOSSLESS = process.env.KURN_RENDER_LOSSLESS === '1';
  const encodeArgs = ['-vf', 'scale=out_color_matrix=bt709:out_range=tv,format=yuv420p', '-colorspace', 'bt709', '-color_primaries', 'bt709', '-color_trc', 'bt709',
    '-c:v', 'libx264', '-pix_fmt', 'yuv420p', '-crf', mode === 'draft' ? '26' : '17', '-preset', mode === 'draft' ? 'veryfast' : 'medium', '-r', fps];

  let done = 0;
  const t0 = Date.now();
  const tickProgress = (f: number) => {
    done += 1;
    if (done % 15 === 0 || done === b - a) {
      emit({ type: 'progress', pct: Math.round((done / (b - a)) * 100), msg: `frame ${f} · ${(done / ((Date.now() - t0) / 1000)).toFixed(1)} fps` });
    }
  };

  /** Render frames [from, to) into a video-only segment. */
  async function renderSegment(from: number, to: number, file: string) {
    const { page, seek } = await openPage();
    const ff = spawn('ffmpeg', ['-v', 'error', '-y', '-f', 'image2pipe', '-framerate', fps, '-c:v', LOSSLESS ? 'png' : 'mjpeg', '-i', '-',
      ...encodeArgs, '-an', file], { stdio: ['pipe', 'ignore', 'pipe'] });
    procs.push(ff);
    let ffErr = '';
    ff.stderr!.on('data', d => { ffErr = (ffErr + d).slice(-4000); });
    const ffDone = new Promise<void>((res, rej) => ff.on('close', code => (code === 0 ? res() : rej(new Error(`ffmpeg exited ${code}: ${ffErr.trim()}`)))));
    let tSeek = 0, tShot = 0;
    for (let f = from; f < to; f++) {
      const s0 = performance.now();
      await seek(f);
      const s1 = performance.now();
      tSeek += s1 - s0;
      // PNG capture costs ~4x JPEG and the output is 4:2:0 H.264 anyway, so finals
      // capture JPEG q95 (visually lossless). KURN_RENDER_LOSSLESS=1 forces PNG.
      const buf = await page.screenshot({ type: LOSSLESS ? 'png' : 'jpeg', quality: LOSSLESS ? undefined : mode === 'draft' ? 85 : 95, clip, optimizeForSpeed: true });
      tShot += performance.now() - s1;
      if (!ff.stdin!.write(buf)) await new Promise(r => ff.stdin!.once('drain', r));
      tickProgress(f);
    }
    if (process.env.KURN_RENDER_PROFILE) log(`segment ${from}-${to}: seek ${(tSeek / (to - from)).toFixed(0)} ms/frame, capture ${(tShot / (to - from)).toFixed(0)} ms/frame`);
    ff.stdin!.end();
    await ffDone;
    await page.browser().close().catch(() => {});
  }

  const work = await mkdtemp(path.join(os.tmpdir(), 'kurn-render-'));
  try {
    if (mode === 'still') {
      const { page, seek } = await openPage();
      await seek(a);
      await page.screenshot({ path: out, type: 'png', clip });
      emit({ type: 'progress', pct: 100 });
    } else {
      // Scenes are pure functions of the frame, so ranges render independently.
      const total = b - a;
      const workers = Math.max(1, Math.min(Number(process.env.KURN_RENDER_WORKERS ?? Math.max(1, Math.floor(os.availableParallelism() / 2))), 6, Math.ceil(total / 30)));
      const per = Math.ceil(total / workers);
      const segs: string[] = [];
      const jobs: Promise<void>[] = [];
      for (let i = 0; i < workers; i++) {
        const from = a + i * per, to = Math.min(b, from + per);
        if (to <= from) break;
        const file = path.join(work, `seg${i}.mp4`);
        segs.push(file);
        jobs.push(renderSegment(from, to, file));
      }
      emit({ type: 'log', level: 'info', msg: `rendering ${total} frames with ${segs.length} worker(s)` });
      await Promise.all(jobs);

      // Join segments (stream copy) and mix the audio in one pass.
      const list = path.join(work, 'segments.txt');
      await writeFile(list, segs.map(f => `file '${f.replace(/'/g, "'\\''")}'`).join('\n'));
      const audio = await audioGraph(rq, tl, a, b, 1);
      const durS = framesToSeconds(total, tl.fps).toFixed(6);
      await exec('ffmpeg', ['-v', 'error', '-y', '-f', 'concat', '-safe', '0', '-i', list,
        ...(audio?.inputs ?? []),
        ...(audio ? ['-filter_complex', audio.filter, '-map', '0:v', '-map', '[aout]', '-c:a', 'aac', '-b:a', '192k'] : []),
        '-c:v', 'copy', '-t', durS, '-movflags', '+faststart', out], { maxBuffer: 1 << 24 });
    }
    emit({ type: 'output', name: 'file', path: out });
    emit({ type: 'done' });
  } finally {
    for (const p of procs) p.kill();
    await Promise.all(browsers.map(b => b.close().catch(() => {})));
    srv.close();
    await rm(work, { recursive: true, force: true });
  }
}

main().then(() => process.exit(0), e => {
  emit({ type: 'error', msg: e instanceof Error ? e.message : String(e) });
  process.exit(1);
});
