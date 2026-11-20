import assert from 'node:assert/strict';
import { test } from 'node:test';
import { type Timeline, clipStateAt, framesToSeconds, sampleLane, validateTimeline } from '../src/index.ts';

const base = (): Timeline => ({
  version: 2, fps: { num: 30, den: 1 }, width: 1920, height: 1080, duration: 300,
  tracks: [{ id: 'v1', kind: 'video', order: 0 }],
  clips: { a: { track: 'v1', asset: 'x', start: 0, in: 0, out: 100 } },
});

test('framesToSeconds uses rational fps', () => {
  assert.equal(framesToSeconds(30, { num: 30, den: 1 }), 1);
  assert.equal(framesToSeconds(30000, { num: 30000, den: 1001 }), 1001);
});

test('sampleLane: clamps, interpolates, eases', () => {
  const lane = [{ t: 0, v: 0 }, { t: 10, v: 100 }];
  assert.equal(sampleLane(lane, -5), 0);
  assert.equal(sampleLane(lane, 5), 50);
  assert.equal(sampleLane(lane, 99), 100);
  assert.equal(sampleLane([{ t: 0, v: 0, ease: 'hold' }, { t: 10, v: 1 }], 9), 0);
  assert.equal(sampleLane([{ t: 0, v: 0, ease: 'easeInOutCubic' }, { t: 120, v: 1 }], 30), 0.0625);
  assert.equal(sampleLane([], 3), undefined);
});

test('clipStateAt: keyframe t is clip-local; source = in + local', () => {
  const s = clipStateAt({ track: 'v1', asset: 'x', start: 30, in: 30, out: 150,
    keyframes: { opacity: [{ t: 0, v: 0 }, { t: 10, v: 1 }] } }, 35);
  assert.equal(s.active, true);
  assert.equal(s.local, 5);
  assert.equal(s.source, 35);
  assert.equal(s.props.opacity, 0.5);
  assert.equal(clipStateAt({ track: 'v1', asset: 'x', start: 30, in: 0, out: 10 }, 40).active, false);
});

test('validateTimeline catches overlap, bad ranges, unknown refs', () => {
  assert.deepEqual(validateTimeline(base(), new Set(['x'])), []);
  const t = base();
  t.clips.b = { track: 'v1', asset: 'x', start: 50, in: 0, out: 10 };
  t.clips.c = { track: 'nope', asset: 'y', start: 0, in: 5, out: 5 };
  const msgs = validateTimeline(t, new Set(['x'])).map(i => i.msg).join('\n');
  assert.match(msgs, /a and b overlap/);
  assert.match(msgs, /c: out must be > in/);
  assert.match(msgs, /c: unknown track nope/);
  assert.match(msgs, /c: unknown asset y/);
});

test('timelineFromDocJSON reads the Rust core layout', async () => {
  const { timelineFromDocJSON } = await import('../src/index.ts');
  const tl = timelineFromDocJSON({
    meta: { version: 2, fps_num: 30000, fps_den: 1001, width: 1920, height: 1080, duration: 90 },
    tracks: { v2: { kind: 'video', order: 1 }, v1: { kind: 'video', order: 0, muted: true } },
    clips: { c: { track: 'v1', asset: 'ast_x', start: 0, in: 5, out: 50, props: { opacity: 0.5 },
                  keyframes: { x: [{ t: 10, v: 1 }, { t: 0, v: 0 }] } } },
    markers: { m: { t: 3, label: 'hi' } },
  })!;
  assert.deepEqual(tl.fps, { num: 30000, den: 1001 });
  assert.deepEqual(tl.tracks.map(t => t.id), ['v1', 'v2']);
  assert.equal(tl.tracks[0].muted, true);
  assert.equal(tl.clips.c.in, 5);
  assert.deepEqual(tl.clips.c.keyframes!.x.map(k => k.t), [0, 10]);
  assert.equal(timelineFromDocJSON({}), null);
});

test('applyOp mirrors the core for the ops the UI makes', async () => {
  const { applyOp } = await import('../src/apply.ts');
  let t: any = {
    version: 2, fps: { num: 30, den: 1 }, width: 1920, height: 1080, duration: 100,
    tracks: [{ id: 'v1', kind: 'video', order: 0 }],
    clips: { a: { track: 'v1', asset: 'x', start: 0, in: 10, out: 110, keyframes: { opacity: [{ t: 0, v: 0 }, { t: 60, v: 1 }] } },
             b: { track: 'v1', asset: 'x', start: 100, in: 0, out: 50 } },
  };
  const before = JSON.stringify(t);
  t = applyOp(t, { type: 'clip.split', args: { clip: 'a', at: 40 } });
  const right = Object.keys(t.clips).find(k => k.startsWith('c_local_split'))!;
  assert.deepEqual([t.clips.a.out, t.clips[right].start, t.clips[right].in], [50, 40, 50], "in=10 → cut at source 50");
  assert.deepEqual(t.clips[right].keyframes.opacity, [{ t: 20, v: 1 }]);
  t = applyOp(t, { type: 'ripple.delete', args: { clip: right } });
  assert.equal(t.clips.b.start, 40);
  t = applyOp(t, { type: 'batch', args: { ops: [
    { type: 'clip.move', args: { clip: 'b', by: 300 } },
    { type: 'clip.set', args: { clip: 'b', props: { opacity: 0.5 } } },
    { type: 'keyframe.set', args: { clip: 'b', prop: 'x', t: 5, v: 9, ease: 'easeOutCubic' } },
  ] } });
  assert.equal(t.clips.b.start, 340);
  assert.equal(t.duration, 390, 'duration grows to fit');
  assert.equal(t.clips.b.props.opacity, 0.5);
  assert.deepEqual(t.clips.b.keyframes.x, [{ t: 5, v: 9, ease: 'easeOutCubic' }]);
  assert.equal(JSON.stringify(applyOp(t, { type: 'clip.move', args: { clip: 'nope', start: 1 } })), JSON.stringify(t), 'bad op is a no-op');
  assert.notEqual(JSON.stringify(t), before);
});
