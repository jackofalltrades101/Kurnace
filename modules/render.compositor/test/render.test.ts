// render.compositor: frame accuracy, determinism and scene contract tests.
// Talks to the module directly over its stdin/stdout protocol (no core needed).
import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import { mkdir, mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const exec = promisify(execFile);
const MOD = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const BITS = 8;

function render(req: unknown): Promise<string> {
  return new Promise((resolve, reject) => {
    const p = spawn(process.execPath, ['--disable-warning=ExperimentalWarning', path.join(MOD, 'run.ts')], { cwd: MOD, env: { ...process.env, KURNACE_CACHE: CACHE } });
    let out = '', err = '';
    p.stdout.on('data', d => (out += d));
    p.stderr.on('data', d => (err += d));
    p.on('close', () => {
      const evs = out.trim().split('\n').filter(Boolean).map(l => JSON.parse(l));
      const e = evs.find(x => x.type === 'error');
      if (e) return reject(new Error(e.msg));
      const o = evs.find(x => x.type === 'output');
      o ? resolve(o.path) : reject(new Error(`no output\n${err.slice(-2000)}`));
    });
    p.stdin.end(JSON.stringify(req));
  });
}

async function barsVideo(file: string, frames: number) {
  const lum = `if(bitand(N\\,pow(2\\,${BITS - 1}-floor(X*${BITS}/W)))\\,235\\,16)`;
  await exec('ffmpeg', ['-y', '-v', 'error', '-f', 'lavfi', '-i', `color=black:size=640x360:rate=30:duration=${frames / 30}`,
    '-vf', `geq=lum='${lum}':cb=128:cr=128`, '-pix_fmt', 'yuv420p', '-c:v', 'libx264', '-g', '15', file]);
}

/** Read the 8-bar code from an image, or frame n of a video. Optional crop "w:h:x:y". */
async function decode(file: string, n?: number, crop?: string): Promise<number> {
  const vf = [...(n === undefined ? [] : [`select=eq(n\\,${n})`]), ...(crop ? [`crop=${crop}`] : []), `scale=${BITS}:1:flags=area`].join(',');
  const { stdout } = await exec('ffmpeg', ['-v', 'error', '-i', file, '-vf', vf, ...(n === undefined ? [] : ['-fps_mode', 'passthrough']),
    '-frames:v', '1', '-f', 'rawvideo', '-pix_fmt', 'gray', '-'], { encoding: 'buffer' });
  const px = stdout as unknown as Buffer;
  return [...px].reduce((acc, v) => (acc << 1) | (v > 128 ? 1 : 0), 0);
}

let CACHE = '';

test('video frames are exact in stills and full renders, in any order', { timeout: 300_000 }, async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'kurn-comp-'));
  CACHE = path.join(dir, 'cache');
  try {
    const src = path.join(dir, 'bars.mp4');
    await barsVideo(src, 120);
    const start = 10, inF = 20;
    const tl = {
      version: 2, fps: { num: 30, den: 1 }, width: 640, height: 360, duration: 90,
      tracks: [{ id: 'v1', kind: 'video', order: 0 }],
      clips: { c: { track: 'v1', asset: 'bars', start, in: inF, out: inF + 80, props: { fit: 'stretch' } } },
    };
    const assets = { bars: { path: src, kind: 'video', ext: 'mp4', width: 640, height: 360 } };
    const expected = (f: number) => inF + (f - start);
    for (const f of [57, 10, 89, 11, 25]) { // deliberately out of order
      const out = await render({ timeline: tl, assets, mode: 'still', frame: f, out: path.join(dir, `f${f}.png`) });
      assert.equal(await decode(out), expected(f), `still ${f}`);
    }
    const full = await render({ timeline: tl, assets, mode: 'final', out: path.join(dir, 'full.mp4') });
    const { stdout } = await exec('ffprobe', ['-v', 'error', '-count_frames', '-select_streams', 'v', '-show_entries', 'stream=nb_read_frames', '-of', 'csv=p=0', full]);
    assert.equal(Number(stdout.trim().split(',')[0]), 90, 'frame count');
    for (const f of [10, 11, 25, 57, 89]) assert.equal(await decode(full, f), expected(f), `full ${f}`);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('HTML scenes: CSS animations, kurnace.scene and legacy renderFrame are pure functions of the frame', { timeout: 300_000 }, async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'kurn-scene-'));
  CACHE = path.join(dir, 'cache');
  try {
    const scenes = path.join(dir, 'proj', 'scenes');
    // 1. Plain CSS animation: a white bar slides across in 1 s (30 frames), no JS.
    await mkdir(path.join(scenes, 'css'), { recursive: true });
    await writeFile(path.join(scenes, 'css', 'index.html'), `<!doctype html><html><head><style>
      html,body{margin:0;background:#000;width:640px;height:360px;overflow:hidden}
      #bar{position:absolute;top:0;left:0;width:80px;height:360px;background:#fff;animation:slide 1s linear forwards}
      @keyframes slide{from{transform:translateX(0)}to{transform:translateX(560px)}}</style></head><body><div id="bar"></div></body></html>`);
    // 2. kurnace.scene with params: draws the frame number as 8 bars, offset by params.add.
    await mkdir(path.join(scenes, 'api'), { recursive: true });
    await writeFile(path.join(scenes, 'api', 'scene.toml'), 'type = "html"\nentry = "main.html"\n');
    await writeFile(path.join(scenes, 'api', 'main.html'), `<!doctype html><html><head><style>html,body{margin:0;background:#000}</style></head><body>
      <canvas id="c" width="640" height="360"></canvas><script>
      const g = document.getElementById('c').getContext('2d');
      kurnace.scene({ duration: 90, render(frame, ctx) {
        const n = frame + (ctx.params.add || 0);
        g.fillStyle = '#000'; g.fillRect(0, 0, 640, 360);
        for (let b = 0; b < 8; b++) if (n & (1 << (7 - b))) { g.fillStyle = '#fff'; g.fillRect(b * 80, 0, 80, 360); }
      } });</script></body></html>`);
    // 3. Legacy contract (what the duckfight agent wrote): window.META + window.renderFrame(f).
    await mkdir(path.join(scenes, 'legacy'), { recursive: true });
    await writeFile(path.join(scenes, 'legacy', 'index.html'), `<!doctype html><html><head><style>html,body{margin:0;background:#000}</style></head><body>
      <canvas id="c" width="640" height="360"></canvas><script>
      window.META = { duration: 90, fps: 30 };
      const g = document.getElementById('c').getContext('2d');
      let rnd = 0;
      window.renderFrame = f => {
        rnd = Math.random(); // seeded per frame → deterministic
        g.fillStyle = '#000'; g.fillRect(0, 0, 640, 360);
        for (let b = 0; b < 8; b++) if (f & (1 << (7 - b))) { g.fillStyle = '#fff'; g.fillRect(b * 80, 0, 80, 360); }
        document.title = String(rnd);
      };</script></body></html>`);

    const mk = (scene: string, params: Record<string, unknown> = {}) => ({
      version: 2, fps: { num: 30, den: 1 }, width: 640, height: 360, duration: 60,
      tracks: [{ id: 's', kind: 'component', order: 0 }],
      clips: { c: { track: 's', scene, params, start: 0, in: 5, out: 65 } },
    });
    const req = (tl: unknown, frame: number, name: string) => ({ timeline: tl, assets: {}, project_root: path.join(dir, 'proj'), mode: 'still', frame, out: path.join(dir, name) });

    // CSS: at timeline frame 10 (scene frame 15 = 0.5 s) the bar's left edge is at 280 px.
    const css = await render(req(mk('css'), 10, 'css10.png'));
    const { stdout: row } = await exec('ffmpeg', ['-v', 'error', '-i', css, '-vf', 'crop=640:1:0:180', '-f', 'rawvideo', '-pix_fmt', 'gray', '-'], { encoding: 'buffer' });
    const px = [...(row as unknown as Buffer)];
    const left = px.findIndex(v => v > 128);
    assert.ok(Math.abs(left - 280) <= 1, `css bar at ${left}, want 280`);

    for (const [scene, params, add] of [['api', { add: 3 }, 3], ['legacy', {}, 0]] as const) {
      for (const f of [40, 7, 22]) {
        const out = await render(req(mk(scene, params), f, `${scene}${f}.png`));
        assert.equal(await decode(out), f + 5 + add, `${scene} frame ${f}`);
      }
    }
    // Determinism: the same frame twice is byte-identical in pixels.
    const a = await render(req(mk('legacy'), 30, 'det-a.png'));
    const b = await render(req(mk('legacy'), 30, 'det-b.png'));
    const { stdout: diff } = await exec('ffmpeg', ['-v', 'error', '-i', a, '-i', b, '-lavfi', 'psnr', '-f', 'null', '-']).catch(e => e);
    assert.ok(!/average:\d/.test(String(diff)) || /average:inf/.test(String(diff)), 'repeat render identical');
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('scene errors fail the render with the scene\'s message', { timeout: 120_000 }, async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'kurn-scene-err-'));
  CACHE = path.join(dir, 'cache');
  try {
    await mkdir(path.join(dir, 'proj', 'scenes', 'boom'), { recursive: true });
    await writeFile(path.join(dir, 'proj', 'scenes', 'boom', 'index.html'),
      `<!doctype html><html><head></head><body><script>kurnace.scene({ render(f) { if (f > 3) throw new Error('kaboom at ' + f); } });</script></body></html>`);
    const tl = { version: 2, fps: { num: 30, den: 1 }, width: 320, height: 180, duration: 10,
      tracks: [{ id: 's', kind: 'component', order: 0 }], clips: { c: { track: 's', scene: 'boom', start: 0, in: 0, out: 10 } } };
    await assert.rejects(render({ timeline: tl, assets: {}, project_root: path.join(dir, 'proj'), mode: 'still', frame: 5, out: path.join(dir, 'x.png') }), /kaboom at 5/);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('transitions: crossfade blends at the cut, wipe and slide split the frame, dip goes through black', { timeout: 300_000 }, async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'kurn-tr-'));
  CACHE = path.join(dir, 'cache');
  try {
    const mk = (type: string) => ({
      version: 2, fps: { num: 30, den: 1 }, width: 320, height: 180, duration: 60,
      tracks: [{ id: 'v', kind: 'video', order: 0 }],
      clips: {
        a: { track: 'v', component: 'solid', start: 0, in: 0, out: 30, props: { fill: '#ff0000', width: 320, height: 180 } },
        b: { track: 'v', component: 'solid', start: 30, in: 0, out: 30, props: { fill: '#0000ff', width: 320, height: 180 } },
      },
      transitions: { t: { between: ['a', 'b'], type, dur: 20 } },
    });
    /** RGB at (x, y) of a still at frame f. */
    const px = async (type: string, f: number, x: number, y = 90) => {
      const out = await render({ timeline: mk(type), assets: {}, mode: 'still', frame: f, out: path.join(dir, `${type}${f}.png`) });
      const { stdout } = await exec('ffmpeg', ['-v', 'error', '-i', out, '-vf', `crop=1:1:${x}:${y}`, '-f', 'rawvideo', '-pix_fmt', 'rgb24', '-'], { encoding: 'buffer' });
      return [...(stdout as unknown as Buffer)];
    };
    // Window = frames 20–39 (centred on the cut at 30).
    const [r0, , b0] = await px('crossfade', 15, 160);
    assert.ok(r0 > 240 && b0 < 15, `before the window: pure A (${r0},${b0})`);
    const [rm, , bm] = await px('crossfade', 30, 160);
    assert.ok(rm > 90 && rm < 170 && bm > 90 && bm < 170, `mid crossfade blends (${rm},${bm})`);
    const [r1, , b1] = await px('crossfade', 45, 160);
    assert.ok(b1 > 240 && r1 < 15, `after the window: pure B (${r1},${b1})`);
    const left = await px('wipe', 30, 20), right = await px('wipe', 30, 300);
    assert.ok(left[2] > 200 && right[0] > 200, `wipe: B on the left, A on the right (${left}) (${right})`);
    const sl = await px('slide', 30, 20), sr = await px('slide', 30, 300);
    assert.ok(sl[0] > 200 && sr[2] > 200, `slide: A leaving left, B entering right (${sl}) (${sr})`);
    const dip = await px('dip', 30, 160);
    assert.ok(Math.max(...dip) < 60, `dip: near black at the cut (${dip})`);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
