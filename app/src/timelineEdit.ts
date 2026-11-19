// Pure timeline-edit maths (spec §7.2, §7.4). No React or canvas imports, so
// the node test runner can exercise it without a webview.
import type { Clip, Rational, Timeline } from '@kurnace/timeline';

export interface SnapResult {
  frame: number;
  /** The frame we snapped to, or null when the input was kept. */
  at: number | null;
}

/** Snap `frame` to the nearest candidate within `threshold` frames. */
export function snapFrame(frame: number, candidates: readonly number[], threshold: number): SnapResult {
  let at: number | null = null;
  let best = threshold;
  for (const c of candidates) {
    const d = Math.abs(c - frame);
    if (d <= best) {
      best = d;
      at = c;
    }
  }
  return at === null ? { frame, at: null } : { frame: at, at };
}

/** Frames worth snapping to: timeline ends, markers, the playhead, clip edges. */
export function snapCandidates(tl: Timeline, playhead: number, exclude: ReadonlySet<string> = new Set()): number[] {
  const out = [0, tl.duration, playhead];
  for (const m of Object.values(tl.markers ?? {})) out.push(m.t);
  for (const [id, c] of Object.entries(tl.clips)) {
    if (exclude.has(id)) continue;
    out.push(c.start, c.start + clipLen(c));
  }
  return out;
}

export const clipLen = (c: Clip) => c.out - c.in;
export const clipEnd = (c: Clip) => c.start + clipLen(c);
export const clamp = (v: number, lo: number, hi: number) => Math.min(hi, Math.max(lo, v));

/** Pick a labelled ruler tick step (in frames) that leaves at least ~72 px. */
export function chooseTickStep(pxPerFrame: number): number {
  const steps = [1, 2, 5, 10, 15, 30, 60, 90, 150, 300, 600, 900, 1800, 3600, 7200, 18000, 36000];
  return steps.find(s => s * pxPerFrame >= 72) ?? steps[steps.length - 1];
}

/** `m:ss:ff` at the project rate. */
export function timecode(frame: number, fps: Rational): string {
  const rate = fps.num / fps.den;
  const seconds = frame * fps.den / fps.num;
  const m = Math.floor(seconds / 60);
  const s = Math.floor(seconds % 60);
  const f = Math.round(rate) > 0 ? Math.abs(frame) % Math.round(rate) : 0;
  return `${m}:${String(s).padStart(2, '0')}:${String(f).padStart(2, '0')}`;
}

/** Clips whose timeline span intersects [a, b). */
export function clipsInRange(tl: Timeline, a: number, b: number): string[] {
  const lo = Math.min(a, b), hi = Math.max(a, b);
  return Object.entries(tl.clips)
    .filter(([, c]) => clipEnd(c) > lo && c.start < hi)
    .map(([id]) => id);
}

/** Clamp a trim so a clip keeps at least one frame and never reads before source 0. */
export function trimStart(c: Clip, wantedStart: number, sourceFrames?: number): { start: number; in: number } {
  const minStart = Math.max(0, c.start - c.in); // in cannot go below 0
  const maxStart = c.start + clipLen(c) - 1;
  const start = clamp(Math.round(wantedStart), minStart, maxStart);
  return { start, in: c.in + (start - c.start) };
}

/** Clamp a trim so the right edge stays after `in` and inside the source. */
export function trimEnd(c: Clip, wantedOut: number, sourceFrames?: number): { out: number } {
  const max = sourceFrames && sourceFrames > 0 ? Math.max(c.in + 1, Math.floor(sourceFrames)) : Number.MAX_SAFE_INTEGER;
  return { out: clamp(Math.round(wantedOut), c.in + 1, max) };
}

/** Frame under an x coordinate (canvas-local px, after the label gutter). */
export function frameAtX(x: number, scrollX: number, pxPerFrame: number): number {
  return Math.round(scrollX + x / pxPerFrame);
}
