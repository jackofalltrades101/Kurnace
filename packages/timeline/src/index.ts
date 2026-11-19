// Pure timeline model + evaluation. No Revideo imports, so the CLI, tests and
// the Revideo scene can all share it. Spec: §7 (data model, time units).

export interface Rational { num: number; den: number }

export type TrackKind = 'video' | 'audio' | 'component' | 'text' | '3d';

export interface Track {
  id: string;
  kind: TrackKind;
  name?: string;
  order: number;
  muted?: boolean;
  locked?: boolean;
}

export type Ease =
  | 'linear' | 'easeInQuad' | 'easeOutQuad' | 'easeInOutQuad'
  | 'easeInCubic' | 'easeOutCubic' | 'easeInOutCubic' | 'hold';

export interface Keyframe { t: number; v: number; ease?: Ease }

export interface Clip {
  track: string;
  asset?: string | null;
  component?: string | null;
  start: number;
  in: number;
  out: number;
  props?: Record<string, unknown>;
  keyframes?: Record<string, Keyframe[]>;
  offset_samples?: number;
}

export interface Timeline {
  version: 2;
  fps: Rational;
  width: number;
  height: number;
  duration: number;
  tracks: Track[];
  clips: Record<string, Clip>;
  markers?: Record<string, { t: number; label: string }>;
  transitions?: Record<string, { between: [string, string]; type: string; dur: number }>;
}

export const fpsValue = (r: Rational) => r.num / r.den;

/** Frames (at project fps) → seconds, exact rational math until the final divide. */
export const framesToSeconds = (frames: number, fps: Rational) => (frames * fps.den) / fps.num;

export const clipLength = (c: Clip) => c.out - c.in;
export const clipEnd = (c: Clip) => c.start + clipLength(c);

const EASES: Record<Ease, (x: number) => number> = {
  linear: x => x,
  hold: () => 0,
  easeInQuad: x => x * x,
  easeOutQuad: x => 1 - (1 - x) * (1 - x),
  easeInOutQuad: x => (x < 0.5 ? 2 * x * x : 1 - (-2 * x + 2) ** 2 / 2),
  easeInCubic: x => x ** 3,
  easeOutCubic: x => 1 - (1 - x) ** 3,
  easeInOutCubic: x => (x < 0.5 ? 4 * x ** 3 : 1 - (-2 * x + 2) ** 3 / 2),
};

/**
 * Sample a keyframe lane at clip-local frame `t` (0 = clip start).
 * A keyframe's `ease` shapes the segment that *leaves* it.
 */
export function sampleLane(lane: Keyframe[], t: number): number | undefined {
  if (lane.length === 0) return undefined;
  const ks = [...lane].sort((a, b) => a.t - b.t);
  if (t <= ks[0].t) return ks[0].v;
  const last = ks[ks.length - 1];
  if (t >= last.t) return last.v;
  for (let i = 0; i < ks.length - 1; i++) {
    const a = ks[i], b = ks[i + 1];
    if (t >= a.t && t < b.t) {
      const ease = EASES[a.ease ?? 'linear'] ?? EASES.linear;
      const x = ease((t - a.t) / (b.t - a.t));
      return a.v + (b.v - a.v) * x;
    }
  }
  return last.v;
}

export const DEFAULT_PROPS = { x: 0, y: 0, scale: 1, opacity: 1, rotation: 0, volume: 1 };

export interface ClipState {
  active: boolean;
  /** Clip-local frame (0 at clip start). */
  local: number;
  /** Source position in project frames (in + local). */
  source: number;
  props: Record<string, unknown>;
}

/** State of a clip at timeline frame `f`. Pure. */
export function clipStateAt(clip: Clip, f: number): ClipState {
  const local = f - clip.start;
  const active = local >= 0 && local < clipLength(clip);
  const props: Record<string, unknown> = { ...DEFAULT_PROPS, ...(clip.props ?? {}) };
  for (const [name, lane] of Object.entries(clip.keyframes ?? {})) {
    const v = sampleLane(lane, local);
    if (v !== undefined) props[name] = v;
  }
  return { active, local, source: clip.in + local, props };
}

export interface Issue { level: 'error' | 'warn'; msg: string }

/** Structural validation. Mirrors the invariants the core will enforce (§6.3). */
export function validateTimeline(tl: Timeline, knownAssets?: Set<string>): Issue[] {
  const issues: Issue[] = [];
  const err = (msg: string) => issues.push({ level: 'error', msg });
  const warn = (msg: string) => issues.push({ level: 'warn', msg });

  if (tl.version !== 2) err(`timeline.version must be 2 (got ${tl.version})`);
  if (!(tl.fps?.num > 0 && tl.fps?.den > 0)) err('fps must be a positive rational {num, den}');
  if (!Number.isInteger(tl.duration) || tl.duration <= 0) err('duration must be a positive integer frame count');

  const trackIds = new Set<string>();
  for (const t of tl.tracks ?? []) {
    if (trackIds.has(t.id)) err(`duplicate track id ${t.id}`);
    trackIds.add(t.id);
  }

  const byTrack = new Map<string, [string, Clip][]>();
  for (const [id, c] of Object.entries(tl.clips ?? {})) {
    for (const k of ['start', 'in', 'out'] as const) {
      if (!Number.isInteger(c[k])) err(`clip ${id}: ${k} must be an integer frame`);
    }
    if (c.in < 0) err(`clip ${id}: in < 0`);
    if (c.out <= c.in) err(`clip ${id}: out must be > in`);
    if (c.start < 0) err(`clip ${id}: start < 0`);
    if (!trackIds.has(c.track)) err(`clip ${id}: unknown track ${c.track}`);
    if (!c.asset && !c.component) err(`clip ${id}: needs asset or component`);
    if (c.asset && knownAssets && !knownAssets.has(c.asset)) err(`clip ${id}: unknown asset ${c.asset}`);
    if (clipEnd(c) > tl.duration) warn(`clip ${id}: extends past timeline duration`);
    if (!byTrack.has(c.track)) byTrack.set(c.track, []);
    byTrack.get(c.track)!.push([id, c]);
  }

  for (const [track, clips] of byTrack) {
    clips.sort((a, b) => a[1].start - b[1].start);
    for (let i = 1; i < clips.length; i++) {
      const [pid, p] = clips[i - 1], [cid, c] = clips[i];
      if (c.start < clipEnd(p)) err(`track ${track}: clips ${pid} and ${cid} overlap`);
    }
  }

  if (tl.transitions && Object.keys(tl.transitions).length > 0) {
    warn('transitions are not rendered yet (Phase 0); they are ignored');
  }
  return issues;
}

// ---------- Yjs layout (spec §7.3; mirrors crates/kurn-core/src/ydoc.rs) ----------

type J = Record<string, any>;
const num = (v: unknown, d = 0) => (typeof v === 'number' ? v : d);
const str = (v: unknown) => (typeof v === 'string' ? v : undefined);

/**
 * Materialize a Timeline from the `timeline` root map's JSON
 * (`doc.getMap('timeline').toJSON()`). Pure; tolerant of partial docs.
 */
export function timelineFromDocJSON(root: J): Timeline | null {
  const meta = root?.meta;
  if (!meta) return null;
  const tracks: Track[] = Object.entries((root.tracks ?? {}) as J).map(([id, t]: [string, J]) => ({
    id,
    kind: (str(t.kind) ?? 'video') as TrackKind,
    name: str(t.name),
    order: num(t.order),
    muted: t.muted === true,
    locked: t.locked === true,
  })).sort((a, b) => a.order - b.order || a.id.localeCompare(b.id));
  const clips: Record<string, Clip> = {};
  for (const [id, c] of Object.entries((root.clips ?? {}) as J) as [string, J][]) {
    const keyframes: Record<string, Keyframe[]> = {};
    for (const [lane, ks] of Object.entries((c.keyframes ?? {}) as J)) {
      keyframes[lane] = [...(ks as Keyframe[])].sort((a, b) => a.t - b.t);
    }
    clips[id] = {
      track: str(c.track) ?? '',
      asset: str(c.asset) ?? null,
      component: str(c.component) ?? null,
      start: num(c.start), in: num(c.in), out: num(c.out),
      props: (c.props ?? {}) as Record<string, unknown>,
      keyframes,
      ...(typeof c.offset_samples === 'number' ? { offset_samples: c.offset_samples } : {}),
    };
  }
  return {
    version: num(meta.version, 2) as 2,
    fps: { num: num(meta.fps_num, 30), den: num(meta.fps_den, 1) },
    width: num(meta.width, 1920),
    height: num(meta.height, 1080),
    duration: num(meta.duration),
    tracks,
    clips,
    markers: Object.fromEntries(Object.entries((root.markers ?? {}) as J).map(([id, m]: [string, J]) => [id, { t: num(m.t), label: str(m.label) ?? '' }])),
    transitions: Object.fromEntries(Object.entries((root.transitions ?? {}) as J).map(([id, t]: [string, J]) =>
      [id, { between: t.between as [string, string], type: str(t.type) ?? 'crossfade', dur: num(t.dur) }])),
  };
}
