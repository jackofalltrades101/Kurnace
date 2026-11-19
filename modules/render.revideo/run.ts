// render.revideo module entry (spec §9.2 protocol).
// stdin: one JSON request. stdout: newline-delimited JSON events. Logs → stderr.
import { execFile } from 'node:child_process';
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';
import { copyFile, mkdir, mkdtemp, rm, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';
import { type Timeline, fpsValue, framesToSeconds, validateTimeline } from '@kurnace/timeline';

process.env.DISABLE_TELEMETRY = 'true'; // spec §10.3 — set before Revideo loads

const here = path.dirname(fileURLToPath(import.meta.url));

// Every dependency the browser side imports, pre-bundled when the server starts
// (see the optimizeDeps comment below). Regenerate after a Revideo upgrade:
//   rm -rf node_modules/.vite && (render once with KURN_DISCOVER_DEPS=1) &&
//   jq -r '.optimized | keys[]' node_modules/.vite/deps/_metadata.json
const OPTIMIZE_DEPS = process.env.KURN_DISCOVER_DEPS === '1'
  ? {}
  : { noDiscovery: true, include: JSON.parse(readFileSync(path.join(here, 'optimize-deps.json'), 'utf8')) as string[] };
const exec = promisify(execFile);

/** Windows symlinks need Developer Mode/admin; fall back to a copy. */
async function linkOrCopy(src: string, dst: string) {
  try {
    await symlink(src, dst);
  } catch {
    await copyFile(src, dst);
  }
}

type Ev = Record<string, unknown> & { type: string };
const emit = (e: Ev) => process.stdout.write(JSON.stringify(e) + '\n');
// Keep stdout clean for the protocol: Revideo/Vite/Puppeteer log via console.
for (const k of ['log', 'info', 'debug'] as const) {
  console[k] = (...a: unknown[]) => process.stderr.write(a.map(String).join(' ') + '\n');
}

interface AssetIn {
  id: string;
  path: string;
  kind: 'image' | 'video' | 'audio' | 'seq';
  ext?: string | null;
  width?: number;
  height?: number;
  frames?: string[];
  fps_num?: number | null;
  fps_den?: number | null;
}
interface Request {
  timeline: Timeline;
  assets: Record<string, AssetIn>;
  mode?: 'still' | 'draft' | 'final';
  frame?: number;
  range?: string;          // "a-b", frames, end exclusive
  scale?: number;
  out: string;
  background?: string;
}

async function readStdin(): Promise<string> {
  const chunks: Buffer[] = [];
  for await (const c of process.stdin) chunks.push(c as Buffer);
  return Buffer.concat(chunks).toString('utf8');
}

function parseRange(r: string | undefined, duration: number): [number, number] {
  if (!r) return [0, duration];
  const m = /^(\d+)-(\d+)$/.exec(r);
  if (!m) throw new Error(`bad range "${r}" (expected start-end in frames)`);
  const a = Number(m[1]), b = Number(m[2]);
  if (!(b > a)) throw new Error(`bad range "${r}": end must be > start`);
  return [a, Math.min(b, duration)];
}

/**
 * Revideo 0.11 never listens for uncaught page errors, so an exception in the
 * scene hangs the render forever. Patch the renderer's own puppeteer instance:
 * surface page errors / failed requests, and fail if progress stalls.
 * Returns a promise that only ever rejects.
 */
function instrumentPuppeteer(): Promise<never> {
  const req = createRequire(createRequire(import.meta.url).resolve('@revideo/renderer'));
  const pptr = req('puppeteer').default ?? req('puppeteer');
  const stallMs = Number(process.env.KURN_RENDER_STALL_MS ?? 60_000);
  let lastBeat = Date.now();
  heartbeat = () => { lastBeat = Date.now(); };

  return new Promise<never>((_, reject) => {
    const launch = pptr.launch.bind(pptr);
    pptr.launch = async (...args: unknown[]) => {
      const browser = await launch(...args);
      const newPage = browser.newPage.bind(browser);
      browser.newPage = async () => {
        const page = await newPage();
        page.on('pageerror', (e: Error) => {
          emit({ type: 'log', level: 'error', msg: `page error: ${e.message}` });
          reject(new Error(`scene crashed in browser: ${e.message}`));
          void browser.close().catch(() => {});
        });
        page.on('requestfailed', (r: { url(): string; failure(): { errorText: string } | null }) => {
          const why = r.failure()?.errorText ?? '';
          // Media elements routinely abort range requests; that's not an error.
          emit({ type: 'log', level: why === 'net::ERR_ABORTED' ? 'debug' : 'warn', msg: `request failed: ${r.url()} (${why})` });
        });
        page.on('response', (r: { status(): number; url(): string }) => {
          if (r.status() >= 400 && !r.url().endsWith('/favicon.ico')) emit({ type: 'log', level: 'warn', msg: `HTTP ${r.status()} ${r.url()}` });
        });
        page.on('console', (m: { type(): string; text(): string }) => {
          if (m.type() === 'error' && !m.text().includes('404 (Not Found)')) emit({ type: 'log', level: 'error', msg: `console: ${m.text()}` });
        });
        return page;
      };
      return browser;
    };
    const timer = setInterval(() => {
      if (Date.now() - lastBeat > stallMs) {
        clearInterval(timer);
        reject(new Error(`render stalled: no progress for ${stallMs / 1000}s`));
      }
    }, 1000);
    timer.unref();
  });
}
let heartbeat = () => {};

async function main() {
  const req = JSON.parse(await readStdin()) as Request;
  const tl = req.timeline;
  const mode = req.mode ?? 'final';

  const issues = validateTimeline(tl, new Set(Object.keys(req.assets)));
  for (const i of issues) emit({ type: 'log', level: i.level, msg: i.msg });
  if (issues.some(i => i.level === 'error')) throw new Error('timeline failed validation');

  const [a, b] = mode === 'still'
    ? [req.frame ?? 0, (req.frame ?? 0) + 1]
    : parseRange(req.range, tl.duration);
  if (a < 0 || a >= tl.duration) throw new Error(`frame ${a} outside timeline (0-${tl.duration - 1})`);

  const out = path.resolve(req.out);
  const work = await mkdtemp(path.join(tmpdir(), 'kurn-render-'));
  const scale = mode === 'draft' ? (req.scale ?? 0.5) : 1;

  // Stage assets as <work>/public/a/<id><ext> symlinks and serve <work>/public
  // as Vite's publicDir. Revideo's server-side ffmpeg (audio mix, ffmpeg video
  // decoder) resolves a src as <outDir>/../public/<src>, so with outDir =
  // <work>/out both the browser and the server see the same files.
  const pub = path.join(work, 'public');
  await mkdir(path.join(pub, 'a'), { recursive: true });
  const assets: Record<string, {
    url: string; kind: AssetIn['kind']; width?: number; height?: number;
    frames?: string[]; fps_num?: number | null; fps_den?: number | null;
  }> = {};
  for (const [id, x] of Object.entries(req.assets)) {
    if (x.kind === 'seq') {
      // Frame sequences are staged as a directory; the scene swaps <Img> src.
      await mkdir(path.join(pub, 'a', id), { recursive: true });
      const frames = x.frames ?? [];
      for (const f of frames) {
        await linkOrCopy(path.resolve(x.path, f), path.join(pub, 'a', id, f));
      }
      assets[id] = {
        url: `/a/${id}/`, kind: 'seq', width: x.width, height: x.height,
        frames, fps_num: x.fps_num, fps_den: x.fps_den,
      };
      continue;
    }
    // Library blobs have no extension; the browser and ffmpeg need one.
    const ext = x.ext ? `.${x.ext}` : path.extname(x.path).toLowerCase();
    const rel = `a/${id}${ext}`;
    await linkOrCopy(path.resolve(x.path), path.join(pub, rel));
    assets[id] = { url: `/${rel}`, kind: x.kind, width: x.width, height: x.height };
  }

  const failFast = instrumentPuppeteer();
  const { renderVideo } = await import('@revideo/renderer');
  emit({ type: 'progress', pct: 0, msg: `rendering ${mode} frames ${a}-${b - 1}` });

  try {
    const isStill = mode === 'still';
    const file = await Promise.race([failFast, renderVideo({
      // The renderer joins this onto process.cwd() (= module dir), so keep it relative.
      projectFile: './src/project.ts',
      variables: { timeline: tl, assets, background: req.background ?? '#000000' },
      settings: {
        outDir: path.join(work, 'out'),
        outFile: (isStill ? 'still.mov' : 'out.mp4') as `${string}.mp4`,
        logProgress: false,
        workers: 1,
        puppeteer: { dumpio: process.env.KURN_DEBUG === '1' },
        progressCallback: (_w, p) => { heartbeat(); emit({ type: 'progress', pct: Math.round(p * 100) }); },
        projectSettings: {
          // Revideo ranges are in seconds and the end frame is INCLUSIVE, so our
          // end-exclusive [a, b) becomes [a, b-1].
          range: [framesToSeconds(a, tl.fps), framesToSeconds(b - 1, tl.fps)],
          exporter: { name: '@revideo/core/ffmpeg', options: { format: isStill ? 'proRes' : 'mp4' } },
        },
        viteConfig: {
          root: here,
          publicDir: pub,
          // Without this, a cold Vite dep cache makes Vite discover deps on first
          // load, re-optimize and reload mid-render. That leaves two copies of
          // @revideo/core and the scene fails with "The scene is not available
          // in the current context". Serve deps as-is instead.
          optimizeDeps: OPTIMIZE_DEPS,
          logLevel: 'warn',
          define: {
            __KURN_FPS__: JSON.stringify(fpsValue(tl.fps)),
            __KURN_WIDTH__: JSON.stringify(tl.width),
            __KURN_HEIGHT__: JSON.stringify(tl.height),
            __KURN_SCALE__: JSON.stringify(scale),
          },
        },
      },
    })]);

    if (isStill) {
      // ProRes intermediate → PNG of the single rendered frame.
      await exec('ffmpeg', ['-y', '-v', 'error', '-i', file, '-frames:v', '1', '-update', '1', out]);
    } else {
      await exec('ffmpeg', ['-y', '-v', 'error', '-i', file, '-c', 'copy', out]);
    }
    emit({ type: 'output', name: 'file', path: out });
    emit({ type: 'done' });
  } finally {
    await rm(work, { recursive: true, force: true });
  }
}

main().then(() => process.exit(0), e => {
  emit({ type: 'error', msg: e instanceof Error ? e.message : String(e) });
  process.exit(1);
});

