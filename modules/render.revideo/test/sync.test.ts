// Frame-accuracy regression test for the TimelineRenderer (spec §7.2, §10.4).
// The source video encodes its own frame number as 8 black/white bars (binary),
// so we can read back exactly which source frame landed on each output frame.
import assert from 'node:assert/strict';
import { execFile } from 'node:child_process';
import { mkdir, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';
import { promisify } from 'node:util';

const exec = promisify(execFile);
const KURN = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../bin/kurn');
const BITS = 8;

async function makeBarsVideo(file: string, frames: number) {
  // bit k of N → bar k (MSB on the left) is white.
  const lum = `if(bitand(N\\,pow(2\\,${BITS - 1}-floor(X*${BITS}/W)))\\,235\\,16)`;
  await exec('ffmpeg', [
    '-y', '-v', 'error', '-f', 'lavfi', '-i', `color=black:size=640x360:rate=30:duration=${frames / 30}`,
    '-vf', `geq=lum='${lum}':cb=128:cr=128`, '-pix_fmt', 'yuv420p', '-c:v', 'libx264', '-g', '15', file,
  ]);
}

/** Decode the bar code from one image, or frame `n` of a video. */
async function decode(file: string, n?: number): Promise<number> {
  const sel = n === undefined ? [] : ['-vf', `select=eq(n\\,${n}),scale=${BITS}:1:flags=area`, '-fps_mode', 'passthrough'];
  const vf = n === undefined ? ['-vf', `scale=${BITS}:1:flags=area`] : sel;
  const { stdout } = await exec('ffmpeg', ['-v', 'error', '-i', file, ...vf, '-frames:v', '1', '-f', 'rawvideo', '-pix_fmt', 'gray', '-'],
    { encoding: 'buffer' });
  const px = stdout as unknown as Buffer;
  assert.equal(px.length, BITS);
  return [...px].reduce((acc, v) => (acc << 1) | (v > 128 ? 1 : 0), 0);
}

async function kurn(cwd: string, env: NodeJS.ProcessEnv, ...args: string[]) {
  const { stdout } = await exec(KURN, [...args, '--json'], { cwd, env, maxBuffer: 1 << 24 });
  const res = JSON.parse(stdout.trim().split('\n').pop()!);
  assert.notEqual(res.ok, false, stdout);
  return res;
}

test('stills and full renders sample the exact source frame', { timeout: 240_000 }, async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'kurn-sync-'));
  let teardown = async () => {};
  try {
    // Isolated core + library for this test (short socket path: macOS limit).
    const env = { ...process.env, KURNACE_HOME: path.join(dir, 'home'), KURN_SOCKET: `/tmp/kurn-sync-${process.pid}.sock` };
    await mkdir(path.join(dir, 'media'));
    await makeBarsVideo(path.join(dir, 'media/bars.mp4'), 120);
    await kurn(dir, env, 'project', 'new', 'p', '--no-git', '--width', '640', '--height', '360');
    const proj = path.join(dir, 'p');
    const [bars] = await kurn(proj, env, 'lib', 'add', path.join(dir, 'media/bars.mp4'));
    // Clip starts at timeline frame 10, 20 frames into the source.
    const start = 10, inF = 20;
    await kurn(proj, env, 'op', 'batch', '--args', JSON.stringify([
      { type: 'track.add', args: { id: 'v1', kind: 'video' } },
      { type: 'clip.add', args: { id: 'c', track: 'v1', asset: bars.id, start, in: inF, dur: 80 } },
    ]));
    teardown = async () => { await exec(KURN, ['stop'], { env }).catch(() => {}); };

    const expected = (f: number) => inF + (f - start);
    const probes = [10, 11, 25, 57, 89];

    for (const f of probes) {
      const r = await kurn(proj, env, 'render', 'still', '--frame', String(f));
      assert.equal(await decode(r.out), expected(f), `still frame ${f}`);
    }

    const r = await kurn(proj, env, 'render', 'final', '--out', path.join(dir, 'full.mp4'));
    const { stdout } = await exec('ffprobe', ['-v', 'error', '-count_frames', '-select_streams', 'v',
      '-show_entries', 'stream=nb_read_frames', '-of', 'csv=p=0', r.out]);
    assert.equal(Number(stdout.trim()), 90, 'full render frame count (timeline grows to fit: 10 + 80)');
    for (const f of probes) {
      assert.equal(await decode(r.out, f), expected(f), `full render frame ${f}`);
    }
  } finally {
    await teardown();
    await rm(dir, { recursive: true, force: true });
  }
});
