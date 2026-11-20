// Local (optimistic) op application — mirrors crates/kurn-core/src/ops.rs.
//
// The UI applies an op here first so edits show instantly, sends it to the core,
// and drops the local copy once the core's update arrives (or rolls it back if the
// core rejects it). The core stays the authority: this only has to be right
// enough to look right for a few hundred milliseconds. Unknown op types are
// returned unchanged (the edit still goes to the core).
import type { Clip, Keyframe, Timeline, Track } from './index.ts';

export interface Op { type: string; args?: Record<string, any> }

const clone = <T>(x: T): T => structuredClone(x);
const end = (c: Clip) => c.start + (c.out - c.in);
const num = (v: unknown, d: number) => (typeof v === 'number' && Number.isFinite(v) ? v : d);

function setMap(target: Record<string, unknown>, patch: Record<string, unknown>) {
  for (const [k, v] of Object.entries(patch)) {
    if (v === null) delete target[k];
    else target[k] = v;
  }
}

/** Apply `op` to a copy of `tl`. Never throws: a bad op just doesn't change anything locally. */
export function applyOp(tl: Timeline, op: Op): Timeline {
  const next = clone(tl);
  try {
    applyInPlace(next, op);
    const contentEnd = Math.max(0, ...Object.values(next.clips).map(end));
    if (contentEnd > next.duration) next.duration = contentEnd;
    return next;
  } catch {
    return tl;
  }
}

function applyInPlace(tl: Timeline, op: Op) {
  const a = op.args ?? {};
  const clip = (id: string) => {
    const c = tl.clips[id];
    if (!c) throw new Error(`unknown clip ${id}`);
    return c;
  };
  switch (op.type) {
    case 'batch':
      for (const o of (a.ops ?? []) as Op[]) applyInPlace(tl, o);
      return;
    case 'timeline.set':
      for (const k of ['duration', 'width', 'height'] as const) if (typeof a[k] === 'number') tl[k] = a[k];
      if (a.fps) tl.fps = { num: a.fps.num, den: a.fps.den };
      return;
    case 'track.add': {
      const order = typeof a.order === 'number' ? a.order : Math.max(-1, ...tl.tracks.map(t => t.order)) + 1;
      const t: Track = { id: a.id ?? `t_local_${Math.random().toString(36).slice(2, 8)}`, kind: a.kind, name: a.name, order };
      tl.tracks = [...tl.tracks, t].sort((x, y) => x.order - y.order);
      return;
    }
    case 'track.remove':
      tl.tracks = tl.tracks.filter(t => t.id !== a.track);
      for (const [id, c] of Object.entries(tl.clips)) if (c.track === a.track) delete tl.clips[id];
      return;
    case 'track.reorder':
      tl.tracks = tl.tracks.map(t => (t.id === a.track ? { ...t, order: a.order } : t)).sort((x, y) => x.order - y.order);
      return;
    case 'track.set':
      tl.tracks = tl.tracks.map(t => (t.id === a.track ? {
        ...t,
        ...(a.name !== undefined ? { name: a.name } : {}),
        ...(a.muted !== undefined ? { muted: !!a.muted } : {}),
        ...(a.locked !== undefined ? { locked: !!a.locked } : {}),
      } : t));
      return;
    case 'clip.add': {
      const id = a.id ?? `c_local_${Math.random().toString(36).slice(2, 8)}`;
      const inF = num(a.in, 0);
      const out = typeof a.out === 'number' ? a.out : inF + num(a.dur, 0);
      tl.clips[id] = {
        track: a.track, asset: a.asset ?? null, component: a.component ?? null,
        ...(a.scene ? { scene: a.scene, params: a.params ?? {} } : {}),
        start: num(a.start, 0), in: inF, out, props: a.props ?? {}, keyframes: a.keyframes ?? {},
      };
      return;
    }
    case 'clip.remove':
      delete tl.clips[a.clip];
      return;
    case 'ripple.delete': {
      const gone = clip(a.clip);
      const len = gone.out - gone.in;
      delete tl.clips[a.clip];
      for (const c of Object.values(tl.clips)) if (c.track === gone.track && c.start >= end(gone)) c.start -= len;
      return;
    }
    case 'clip.move': {
      const c = clip(a.clip);
      if (typeof a.start === 'number') c.start = a.start;
      else if (typeof a.by === 'number') c.start += a.by;
      if (typeof a.track === 'string') c.track = a.track;
      return;
    }
    case 'clip.trim': {
      const c = clip(a.clip);
      for (const k of ['in', 'out', 'start'] as const) if (typeof a[k] === 'number') c[k] = a[k];
      return;
    }
    case 'clip.split': {
      const c = clip(a.clip);
      const at = a.at as number;
      if (at <= c.start || at >= end(c)) return;
      const shift = at - c.start;
      const cut = c.in + shift;
      const right: Clip = clone(c);
      right.start = at;
      right.in = cut;
      for (const [lane, ks] of Object.entries(right.keyframes ?? {})) {
        right.keyframes![lane] = ks.filter(k => k.t >= shift).map(k => ({ ...k, t: k.t - shift }));
      }
      c.out = cut;
      tl.clips[`c_local_split_${a.clip}_${at}`] = right;
      return;
    }
    case 'clip.set': {
      const c = clip(a.clip);
      if (a.props) { c.props = { ...(c.props ?? {}) }; setMap(c.props, a.props); }
      if (a.params) { c.params = { ...(c.params ?? {}) }; setMap(c.params, a.params); }
      for (const k of ['asset', 'component', 'scene', 'offset_samples'] as const) {
        if (a[k] !== undefined) (c as unknown as Record<string, unknown>)[k] = a[k];
      }
      return;
    }
    case 'keyframe.set': {
      const c = clip(a.clip);
      const lanes = { ...(c.keyframes ?? {}) };
      const key: Keyframe = { t: a.t, v: a.v, ...(a.ease ? { ease: a.ease } : {}) };
      lanes[a.prop] = [...(lanes[a.prop] ?? []).filter(k => k.t !== a.t), key].sort((x, y) => x.t - y.t);
      c.keyframes = lanes;
      return;
    }
    case 'keyframe.remove': {
      const c = clip(a.clip);
      const lanes = { ...(c.keyframes ?? {}) };
      if (typeof a.t !== 'number') delete lanes[a.prop];
      else lanes[a.prop] = (lanes[a.prop] ?? []).filter(k => k.t !== a.t);
      c.keyframes = lanes;
      return;
    }
    case 'marker.add':
      tl.markers = { ...(tl.markers ?? {}), [a.id ?? `m_local_${a.t}`]: { t: a.t, label: a.label } };
      return;
    case 'marker.remove': {
      const m = { ...(tl.markers ?? {}) };
      delete m[a.marker];
      tl.markers = m;
      return;
    }
    case 'transition.set': {
      const trs = { ...(tl.transitions ?? {}) };
      const found = Object.entries(trs).find(([, t]) => t.between[0] === a.between[0] && t.between[1] === a.between[1]);
      trs[found?.[0] ?? `tr_local_${a.between.join('_')}`] = { between: a.between, type: a.type, dur: a.dur };
      tl.transitions = trs;
      return;
    }
    case 'transition.remove': {
      const trs = { ...(tl.transitions ?? {}) };
      delete trs[a.transition];
      tl.transitions = trs;
      return;
    }
    default:
      return;
  }
}
