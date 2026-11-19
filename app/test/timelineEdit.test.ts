import assert from 'node:assert/strict';
import { test } from 'node:test';
import type { Clip, Timeline } from '@kurnace/timeline';
import { chooseTickStep, clipsInRange, frameAtX, snapFrame, timecode, trimEnd, trimStart } from '../src/timelineEdit.ts';

const clip = (start: number, inF: number, outF: number): Clip => ({ track: 'v1', asset: 'a', start, in: inF, out: outF });
const tl: Timeline = {
  version: 2, fps: { num: 30, den: 1 }, width: 1920, height: 1080, duration: 300,
  tracks: [{ id: 'v1', kind: 'video', order: 0 }],
  clips: { a: clip(10, 0, 20), b: clip(100, 0, 30) },
  markers: { m: { t: 75, label: 'beat' } },
};

test('snapFrame picks the closest candidate inside the threshold', () => {
  assert.deepEqual(snapFrame(47, [30, 50], 4), { frame: 50, at: 50 });
  assert.deepEqual(snapFrame(47, [30, 50], 2), { frame: 47, at: null });
  // ties pick the later candidate (deterministic)
  assert.deepEqual(snapFrame(40, [30, 50], 10).at, 50);
});

test('ruler ticks stay readable at every zoom', () => {
  for (const px of [0.02, 0.1, 0.5, 1, 2, 5, 20]) {
    const step = chooseTickStep(px);
    assert.ok(step * px >= 72, `step ${step} at ${px}px/frame`);
  }
  assert.equal(timecode(0, { num: 30, den: 1 }), '0:00:00');
  assert.equal(timecode(95, { num: 30, den: 1 }), '0:03:05');
});

test('trims keep at least one frame and stay inside the source', () => {
  const c = clip(100, 30, 90);
  assert.deepEqual(trimStart(c, 150), { start: 150, in: 80 });
  assert.deepEqual(trimStart(c, -50), { start: 70, in: 0 });
  assert.deepEqual(trimEnd(c, 10), { out: 31 });
  assert.deepEqual(trimEnd(c, 500, 120), { out: 120 });
});

test('range and coordinate helpers', () => {
  assert.deepEqual(clipsInRange(tl, 15, 25), ['a']);
  assert.deepEqual(clipsInRange(tl, 115, 140), ['b']);
  assert.equal(frameAtX(50, 10, 2), 35);
});
