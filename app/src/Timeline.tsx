/** @jsxImportSource react */
// Canvas timeline (spec §7.4, §15): scroll/zoom, multi-select, drag/trim,
// snapping, ripple delete, split, keyframe lanes, markers and agent presence.
// The canvas is the source of interaction; operations always go through the
// core (`op.apply`) so the human and agents share one writer.
import { DEFAULT_PROPS, sampleLane, type Clip, type Ease, type Timeline as TTimeline, type Track } from '@kurnace/timeline';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { type Presence, actorCanvasColor, call, peaksUrl, revealPath, thumbUrl } from './kurn';
import { type ResolvedAsset } from './Preview';
import { chooseTickStep, clamp, clipEnd, clipLen, frameAtX, snapCandidates, snapFrame, timecode, trimEnd, trimStart } from './timelineEdit';
import type { Change } from './useProject';

const LABEL_W = 118;
const RULER_H = 24;
const TRACK_H = 34;
const KF_H = 20;
const ROW_GAP = 2;
const CLIP_INSET = 3;
const SCROLLBAR = 8;
const SNAP_PX = 8;
const EASES: Ease[] = ['linear', 'hold', 'easeInQuad', 'easeOutQuad', 'easeInOutQuad', 'easeInCubic', 'easeOutCubic', 'easeInOutCubic'];
const KEYS = ['opacity', 'x', 'y', 'scale', 'rotation', 'volume'];

type Row =
  | { kind: 'track'; id: string; track: Track; top: number; height: number }
  | { kind: 'kf'; clipId: string; prop: string; top: number; height: number };

type Op = { type: string; args: Record<string, unknown> };
type Hit =
  | { type: 'ruler' }
  | { type: 'clip'; id: string; edge: 'left' | 'right' | 'body' }
  | { type: 'kf'; clipId: string; prop: string; t: number }
  | { type: 'kf-lane'; clipId: string; prop: string; local: number }
  | { type: 'empty'; track: string | null }
  | { type: 'hscroll' };

type Drag =
  | { kind: 'scrub'; pointerId: number }
  | { kind: 'move'; pointerId: number; startX: number; startY: number; orig: Record<string, { start: number; track: string }>; delta: number; targetTrack: string | null; snapAt: number | null }
  | { kind: 'trim'; pointerId: number; clipId: string; edge: 'left' | 'right'; startX: number; orig: { start: number; in: number; out: number; track: string }; next: { start: number; in: number; out: number }; snapAt: number | null }
  | { kind: 'kf'; pointerId: number; clipId: string; prop: string; origT: number; t: number; v: number; ease?: Ease; startX: number; snapAt: number | null }
  | { kind: 'marquee'; pointerId: number; x0: number; y0: number; x1: number; y1: number; additive: boolean; base: Set<string> }
  | { kind: 'hscroll'; pointerId: number; startX: number; orig: number };

function buildRows(tl: TTimeline, selectedClipId: string | null): Row[] {
  // Lower `order` renders underneath, so draw it lower in the stack. The
  // highest order track therefore sits at the top, like a layer stack.
  const tracks = [...tl.tracks].sort((a, b) => b.order - a.order || b.id.localeCompare(a.id));
  const out: Row[] = [];
  let top = RULER_H;
  for (const track of tracks) {
    out.push({ kind: 'track', id: track.id, track, top, height: TRACK_H });
    top += TRACK_H + ROW_GAP;
    const c = selectedClipId ? tl.clips[selectedClipId] : undefined;
    if (c && c.track === track.id) {
      for (const prop of Object.keys(c.keyframes ?? {})) {
        out.push({ kind: 'kf', clipId: selectedClipId!, prop, top, height: KF_H });
        top += KF_H + ROW_GAP;
      }
    }
  }
  return out;
}

function trackColor(track: Track): string {
  switch (track.kind) {
    case 'audio': return '#3c7a5c';
    case 'text': case 'component': return '#7a4e8a';
    case '3d': return '#7a6338';
    default: return '#3d5a8a';
  }
}

function roundRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number) {
  const rr = Math.min(r, Math.abs(w) / 2, Math.abs(h) / 2);
  ctx.beginPath();
  ctx.moveTo(x + rr, y);
  ctx.arcTo(x + w, y, x + w, y + h, rr);
  ctx.arcTo(x + w, y + h, x, y + h, rr);
  ctx.arcTo(x, y + h, x, y, rr);
  ctx.arcTo(x, y, x + w, y, rr);
  ctx.closePath();
}

function assetKind(assets: Record<string, ResolvedAsset>, id?: string | null): string | undefined {
  return id ? assets[id]?.kind : undefined;
}

function propValue(clip: Clip, prop: string, local: number): number {
  const lane = clip.keyframes?.[prop];
  if (lane?.length) {
    const v = sampleLane(lane, local);
    if (typeof v === 'number') return v;
  }
  const p = clip.props?.[prop];
  if (typeof p === 'number') return p;
  const d = (DEFAULT_PROPS as Record<string, number>)[prop];
  return typeof d === 'number' ? d : 0;
}

export interface TimelineProps {
  project: string;
  tl: TTimeline;
  frame: number;
  assets: Record<string, ResolvedAsset>;
  presence: Presence[];
  lastChange: Change | null;
  onSeek: (frame: number) => void;
  onSeekEnd?: (frame: number) => void;
  /** Optimistic op runner (optimistic.ts); edits show before the core confirms. */
  run: (op: Op) => Promise<{ status?: string; reason?: string } | null>;
  /** Selection changes (for the inspector). */
  onSelectionChange?: (ids: string[]) => void;
}

export function Timeline(props: TimelineProps) {
  const { project, tl, frame, assets, presence, lastChange } = props;
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const wrapRef = useRef<HTMLDivElement>(null);
  const [size, setSize] = useState({ w: 0, h: 0 });
  const [pxPerFrame, setPxPerFrame] = useState(2);
  const [scrollX, setScrollX] = useState(0);
  const [scrollY, setScrollY] = useState(0);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [selectedKf, setSelectedKf] = useState<{ clip: string; prop: string; t: number } | null>(null);
  const [drag, setDragState] = useState<Drag | null>(null);
  const dragRef = useRef<Drag | null>(null);
  // Keep the ref in step synchronously: pointerup can fire before React renders
  // the last pointermove, and the drop must use the final position.
  const setDrag = useCallback((d: Drag | null) => { dragRef.current = d; setDragState(d); }, []);
  const [snapOn, setSnapOn] = useState(true);
  const [tool, setTool] = useState<'select' | 'razor'>('select');
  const [error, setError] = useState<string | null>(null);
  const [markerLabel, setMarkerLabel] = useState('');
  const [laneProp, setLaneProp] = useState('opacity');
  const [resTick, setResTick] = useState(0);
  const resRef = useRef(new Map<string, { img?: HTMLImageElement; peaks?: number[]; peakMax?: number; state: 'loading' | 'ok' | 'err' }>());
  const didFit = useRef(false);

  const single = selected.size === 1 ? [...selected][0] : null;
  const onSel = props.onSelectionChange;
  useEffect(() => { onSel?.([...selected]); }, [selected, onSel]);
  const rows = useMemo(() => buildRows(tl, single), [tl, single]);
  const contentH = rows.length ? rows[rows.length - 1].top + rows[rows.length - 1].height + 6 : RULER_H;
  const viewH = Math.max(0, size.h - SCROLLBAR);
  const fps = tl.fps.num / tl.fps.den;
  const duration = Math.max(tl.duration, 1);
  const livePresence = presence.filter(p => Date.now() / 1000 - (p.updated_at ?? 0) <= 60);

  // ---- resources (thumbnail strips / waveform peaks) ----
  useEffect(() => {
    for (const c of Object.values(tl.clips)) {
      const id = c.asset;
      if (!id || resRef.current.has(id)) continue;
      const kind = assetKind(assets, id);
      if (!kind) continue;
      const rec: { img?: HTMLImageElement; peaks?: number[]; peakMax?: number; state: 'loading' | 'ok' | 'err' } = { state: 'loading' };
      resRef.current.set(id, rec);
      if (kind === 'audio') {
        fetch(peaksUrl(id)).then(r => r.ok ? r.json() : Promise.reject(new Error(String(r.status))))
          .then((p: { peaks: number[] }) => {
            rec.peaks = p.peaks;
            rec.peakMax = Math.max(0.001, ...p.peaks);
            rec.state = 'ok';
            setResTick(t => t + 1);
          })
          .catch(() => { rec.state = 'err'; });
      } else if (kind === 'video' || kind === 'image') {
        const img = new Image();
        img.onload = () => { rec.img = img; rec.state = 'ok'; setResTick(t => t + 1); };
        img.onerror = () => { rec.state = 'err'; };
        img.src = thumbUrl(id);
      } else {
        rec.state = 'err';
      }
    }
  }, [tl, assets]);

  // ---- selection cleanup ----
  useEffect(() => {
    setSelected(prev => {
      const next = new Set([...prev].filter(id => !!tl.clips[id]));
      return next.size === prev.size ? prev : next;
    });
    setSelectedKf(prev => {
      if (!prev) return prev;
      const clip = tl.clips[prev.clip];
      return clip?.keyframes?.[prev.prop]?.some(k => k.t === prev.t) ? prev : null;
    });
  }, [tl]);

  useEffect(() => {
    if (!error) return;
    const t = setTimeout(() => setError(null), 6000);
    return () => clearTimeout(t);
  }, [error]);

  // The change glow expires on a timer; repaint once it does.
  useEffect(() => {
    if (!lastChange) return;
    const t = setTimeout(() => setResTick(x => x + 1), 2600);
    return () => clearTimeout(t);
  }, [lastChange]);

  // Expire offline agent cursors (the core uses the same 60 s TTL).
  useEffect(() => {
    const t = setInterval(() => setResTick(x => x + 1), 15_000);
    return () => clearInterval(t);
  }, []);

  // ---- viewport ----
  useEffect(() => {
    const el = wrapRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setSize({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    setSize({ w: el.clientWidth, h: el.clientHeight });
    return () => ro.disconnect();
  }, []);

  const fit = useCallback(() => {
    if (!size.w) return;
    const span = Math.max(tl.duration, 90);
    // Long timelines stay editable at a sane zoom instead of shrinking every
    // clip to a few pixels; users can still zoom out or scroll.
    setPxPerFrame(clamp((size.w - LABEL_W - 24) / span, 0.75, 30));
    setScrollX(0);
    setScrollY(0);
  }, [size.w, tl.duration]);

  useEffect(() => {
    if (size.w > 0 && !didFit.current) {
      didFit.current = true;
      fit();
    }
  }, [size.w, fit]);

  const zoomBy = (factor: number, anchorPx?: number) => {
    const ax = anchorPx ?? Math.max(0, (size.w - LABEL_W) / 2);
    const anchorFrame = frameAtX(ax, scrollX, pxPerFrame);
    const next = clamp(pxPerFrame * factor, 0.02, 40);
    setPxPerFrame(next);
    setScrollX(Math.max(0, anchorFrame - ax / next));
  };

  // ---- ops ----
  const runOp = props.run;

  const runOps = useCallback(async (ops: Op[]) => {
    if (ops.length === 0) return;
    return runOp(ops.length === 1 ? ops[0] : { type: 'batch', args: { ops } });
  }, [runOp]);

  const runMethod = useCallback(async (method: string) => {
    try { await call(method, { project }); } catch (e) { setError(String(e)); }
  }, [project]);

  // Test hook: screen geometry for scripted UI sessions (app/scripts/ui-shot.mjs --script).
  useEffect(() => {
    (window as unknown as { __kurnTimelineUI?: unknown }).__kurnTimelineUI = {
      /** Client x of frame `f` and client y of the middle of track `id`. */
      point(f: number, trackId: string) {
        const r = canvasRef.current?.getBoundingClientRect();
        const row = rows.find(x => x.kind === 'track' && x.id === trackId);
        if (!r || !row) return null;
        return { x: r.left + LABEL_W + (f - scrollX) * pxPerFrame, y: r.top + row.top - scrollY + row.height / 2 };
      },
      pxPerFrame,
    };
  }, [rows, scrollX, scrollY, pxPerFrame]);

  // ---- drop: library assets, project scenes, files from Finder ----
  type DropItem = { asset?: string; scene?: string };
  const [dropAt, setDropAt] = useState<{ x: number; frame: number; trackId: string | null } | null>(null);
  const trackRowAt = (y: number) => {
    const cy = y + scrollY;
    const row = rows.find(r => r.kind === 'track' && cy >= r.top && cy < r.top + r.height);
    return row && row.kind === 'track' ? row.track : null;
  };
  const placement = (x: number, y: number) => ({
    frame: clamp(Math.round(frameAtX(Math.max(0, x - LABEL_W), scrollX, pxPerFrame)), 0, Math.max(duration, 0) + 100000),
    track: trackRowAt(y),
  });
  const VISUAL = new Set(['video', 'component', 'text', '3d']);
  const placeItems = useCallback(async (items: DropItem[], x: number, y: number) => {
    if (!items.length) return;
    let { frame: at, track } = placement(x, y);
    const ops: Op[] = [];
    const newTracks = new Map<string, string>(); // kind → new track id (one per kind per drop)
    const occupied = (trackId: string, from: number, to: number) =>
      Object.values(tl.clips).some(c => c.track === trackId && c.start < to && clipEnd(c) > from);
    for (const it of items) {
      let kind = 'video', len = Math.round(5 * fps);
      if (it.asset) {
        const a = await call<{ kind: string; duration_s?: number; frames?: string[]; fps_num?: number; fps_den?: number }>('lib.get', { id: it.asset }).catch(() => null);
        if (!a) { setError(`unknown asset ${it.asset}`); continue; }
        kind = a.kind === 'audio' ? 'audio' : 'video';
        if ((a.kind === 'video' || a.kind === 'audio') && a.duration_s) len = Math.max(1, Math.round(a.duration_s * fps));
        if (a.kind === 'seq' && a.frames?.length) len = Math.max(1, Math.round(a.frames.length * fps / (a.fps_num && a.fps_den ? a.fps_num / a.fps_den : fps)));
      } else if (it.scene) {
        const sc = await call<{ name: string; duration?: number | null }[]>('scenes.list', { project }).catch(() => []);
        const d = sc.find(x => x.name === it.scene)?.duration;
        if (d) len = d;
        kind = 'component';
      }
      const fits = (t: Track | null) => !!t && !t.locked && (kind === 'audio' ? t.kind === 'audio' : VISUAL.has(t.kind)) && !occupied(t.id, at, at + len);
      let trackId: string;
      if (fits(track)) trackId = track!.id;
      else {
        const key = kind === 'audio' ? 'audio' : 'video';
        if (!newTracks.has(key)) {
          const id = `t_${Math.random().toString(36).slice(2, 8)}`;
          newTracks.set(key, id);
          ops.push({ type: 'track.add', args: { id, kind: key, ...(key === 'audio' ? { order: Math.min(0, ...tl.tracks.map(t => t.order)) - 1 } : {}) } });
        }
        trackId = newTracks.get(key)!;
      }
      ops.push({ type: 'clip.add', args: { track: trackId, start: at, dur: len, ...(it.asset ? { asset: it.asset } : { scene: it.scene }) } });
      at += len;
    }
    if (ops.length) await runOps(ops);
  }, [tl, rows, scrollX, scrollY, pxPerFrame, fps, project]);

  const dragItems = (dt: DataTransfer): DropItem[] => {
    const asset = dt.getData('application/x-kurn-asset') || (/^ast_/.test(dt.getData('text/plain')) ? dt.getData('text/plain') : '');
    const scene = dt.getData('application/x-kurn-scene');
    return asset ? [{ asset }] : scene ? [{ scene }] : [];
  };
  const onDragOver = (e: React.DragEvent<HTMLDivElement>) => {
    const types = [...e.dataTransfer.types];
    if (!types.some(t => t === 'application/x-kurn-asset' || t === 'application/x-kurn-scene' || t === 'text/plain')) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = 'copy';
    const r = e.currentTarget.getBoundingClientRect();
    const x = e.clientX - r.left, y = e.clientY - r.top;
    const pl = placement(x, y);
    setDropAt({ x: LABEL_W + (pl.frame - scrollX) * pxPerFrame, frame: pl.frame, trackId: pl.track?.id ?? null });
  };
  const onDrop = (e: React.DragEvent<HTMLDivElement>) => {
    e.preventDefault();
    setDropAt(null);
    const r = e.currentTarget.getBoundingClientRect();
    void placeItems(dragItems(e.dataTransfer), e.clientX - r.left, e.clientY - r.top);
  };
  // Files dropped from Finder are imported by the Library, which then asks us to place them.
  useEffect(() => {
    const onPlace = (ev: Event) => {
      const { ids, clientX, clientY } = (ev as CustomEvent<{ ids: string[]; clientX: number; clientY: number }>).detail;
      const r = wrapRef.current?.getBoundingClientRect();
      if (!r) return;
      void placeItems(ids.map(asset => ({ asset })), clientX - r.left, clientY - r.top);
    };
    window.addEventListener('kurn-place-assets', onPlace);
    return () => window.removeEventListener('kurn-place-assets', onPlace);
  }, [placeItems]);

  // ---- context menus ----
  type MenuItem = { label: string; run: () => void; danger?: boolean; disabled?: boolean } | 'sep';
  const [menu, setMenu] = useState<{ x: number; y: number; title?: string; items: MenuItem[] } | null>(null);
  useEffect(() => {
    if (!menu) return;
    const close = () => setMenu(null);
    const onKey = (e: KeyboardEvent) => { if (e.key === 'Escape') close(); };
    window.addEventListener('pointerdown', close);
    window.addEventListener('keydown', onKey);
    window.addEventListener('blur', close);
    return () => { window.removeEventListener('pointerdown', close); window.removeEventListener('keydown', onKey); window.removeEventListener('blur', close); };
  }, [menu]);

  const onContextMenu = (e: React.MouseEvent<HTMLCanvasElement>) => {
    e.preventDefault();
    const rect = e.currentTarget.getBoundingClientRect();
    const x = e.clientX - rect.left, y = e.clientY - rect.top;
    const hit = hitAt(x, y);
    const at = clamp(Math.round(frameAtX(Math.max(0, x - LABEL_W), scrollX, pxPerFrame)), 0, duration);
    const items: MenuItem[] = [];
    let title: string | undefined;
    if (hit.type === 'clip') {
      const id = hit.id;
      const c = tl.clips[id];
      if (!selected.has(id)) setSelected(new Set([id]));
      const ids = selected.has(id) ? [...selected] : [id];
      const sc = (c as Clip & { scene?: string }).scene;
      title = ids.length > 1 ? `${ids.length} clips` : sc ? `scene · ${sc}` : c.component ?? (c.asset ? assets[c.asset]?.name ?? c.asset : id);
      const inside = frame > c.start && frame < clipEnd(c);
      items.push(
        { label: 'Split at playhead', disabled: !inside, run: () => void runOps(ids.filter(i => tl.clips[i] && frame > tl.clips[i].start && frame < clipEnd(tl.clips[i])).map(i => ({ type: 'clip.split', args: { clip: i, at: frame } }))) },
        { label: 'Duplicate after', run: () => {
          const x = c as Clip & { scene?: string; params?: Record<string, unknown> };
          void runOp({ type: 'clip.add', args: {
            track: c.track, start: clipEnd(c), in: c.in, out: c.out, props: c.props ?? {}, keyframes: c.keyframes ?? {},
            ...(c.asset ? { asset: c.asset } : x.scene ? { scene: x.scene, params: x.params ?? {} } : { component: c.component }),
          } });
        } },
        { label: 'Move playhead to clip start', run: () => props.onSeek(c.start) },
        ...((): MenuItem[] => {
          // Transitions need a clip that starts exactly where this one ends, on the same track.
          const next = Object.entries(tl.clips).find(([oid, o]) => oid !== id && o.track === c.track && o.start === clipEnd(c));
          if (!next) return [];
          const existing = Object.entries(tl.transitions ?? {}).find(([, t]) => t.between[0] === id && t.between[1] === next[0]);
          if (existing) return [{ label: `Remove ${existing[1].type} into next clip`, run: () => void runOp({ type: 'transition.remove', args: { transition: existing[0] } }) }];
          const dur = Math.max(2, Math.round(fps / 2));
          return (['crossfade', 'dip', 'wipe', 'slide'] as const).map(type => ({
            label: `${type[0].toUpperCase()}${type.slice(1)} into next clip (${dur} f)`,
            run: () => void runOp({ type: 'transition.set', args: { between: [id, next[0]], type, dur } }),
          }));
        })(),
        'sep',
        ...(sc ? [{ label: 'Show scene files', run: () => void revealPath(`${project}/scenes/${sc}`) } as MenuItem] : []),
        { label: 'Copy clip id', run: () => void navigator.clipboard.writeText(ids.join(' ')) },
        'sep',
        { label: 'Delete', danger: true, run: () => { void runOps(ids.map(i => ({ type: 'clip.remove', args: { clip: i } }))); setSelected(new Set()); } },
        { label: 'Ripple delete (close gap)', danger: true, run: () => { void runOps(ids.map(i => ({ type: 'ripple.delete', args: { clip: i } }))); setSelected(new Set()); } },
      );
    } else if (hit.type === 'empty' && hit.track && x < LABEL_W) {
      const t = tl.tracks.find(t => t.id === hit.track)!;
      title = t.name ?? t.id;
      items.push(
        { label: t.muted ? 'Unmute' : 'Mute', run: () => void runOp({ type: 'track.set', args: { track: t.id, muted: !t.muted } }) },
        { label: t.locked ? 'Unlock' : 'Lock', run: () => void runOp({ type: 'track.set', args: { track: t.id, locked: !t.locked } }) },
        'sep',
        { label: 'Add video track above', run: () => void runOp({ type: 'track.add', args: { kind: 'video', order: t.order + 0.5 } }) },
        { label: 'Add audio track below', run: () => void runOp({ type: 'track.add', args: { kind: 'audio', order: t.order - 0.5 } }) },
        'sep',
        { label: 'Delete track and its clips', danger: true, disabled: t.locked, run: () => void runOp({ type: 'track.remove', args: { track: t.id } }) },
      );
    } else {
      items.push(
        { label: `Add marker at ${timecode(at, tl.fps)}`, run: () => void runOp({ type: 'marker.add', args: { t: at, label: `marker ${at}` } }) },
        { label: 'Add video track', run: () => void runOp({ type: 'track.add', args: { kind: 'video' } }) },
        { label: 'Add audio track', run: () => void runOp({ type: 'track.add', args: { kind: 'audio', order: Math.min(0, ...tl.tracks.map(t => t.order)) - 1 } }) },
      );
    }
    const wrap = wrapRef.current!.getBoundingClientRect();
    setMenu({ x: e.clientX - wrap.left, y: e.clientY - wrap.top, title, items });
  };

  // ---- editing actions ----
  const split = () => {
    const ops: Op[] = [];
    for (const id of selected) {
      const c = tl.clips[id];
      if (c && frame > c.start && frame < clipEnd(c)) ops.push({ type: 'clip.split', args: { clip: id, at: frame } });
    }
    if (ops.length) void runOps(ops);
  };

  const ripple = () => {
    const ops = [...selected].filter(id => tl.clips[id]).map(id => ({ type: 'ripple.delete', args: { clip: id } }));
    if (ops.length) { void runOps(ops); setSelected(new Set()); setSelectedKf(null); }
  };

  const remove = () => {
    const ops = [...selected].filter(id => tl.clips[id]).map(id => ({ type: 'clip.remove', args: { clip: id } }));
    if (ops.length) { void runOps(ops); setSelected(new Set()); setSelectedKf(null); }
  };

  const addMarker = () => {
    const label = markerLabel.trim() || `marker ${frame}`;
    void runOp({ type: 'marker.add', args: { t: frame, label } });
    setMarkerLabel('');
  };

  const addKeyframe = () => {
    if (!single) return;
    const c = tl.clips[single];
    if (!c) return;
    const local = clamp(Math.round(frame - c.start), 0, Math.max(0, clipLen(c) - 1));
    const v = propValue(c, laneProp, local);
    void runOp({ type: 'keyframe.set', args: { clip: single, prop: laneProp, t: local, v } });
    setSelectedKf({ clip: single, prop: laneProp, t: local });
  };

  const setKeyEase = (ease: Ease) => {
    if (!selectedKf) return;
    const c = tl.clips[selectedKf.clip];
    const k = c?.keyframes?.[selectedKf.prop]?.find(x => x.t === selectedKf.t);
    if (!c || !k) return;
    void runOp({ type: 'keyframe.set', args: { clip: selectedKf.clip, prop: selectedKf.prop, t: selectedKf.t, v: k.v, ease } });
  };

  // ---- drawing ----
  const draw = useCallback(() => {
    const canvas = canvasRef.current;
    if (!canvas || !size.w || !size.h) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = Math.round(size.w * dpr);
    canvas.height = Math.round(size.h * dpr);
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, size.w, size.h);
    ctx.fillStyle = '#17181b';
    ctx.fillRect(0, 0, size.w, size.h);

    const xOf = (f: number) => LABEL_W + (f - scrollX) * pxPerFrame;
    const viewportTop = RULER_H - scrollY;
    const now = Date.now();
    const fresh = lastChange?.origin && now - lastChange.at < 2500 ? new Set(lastChange.clips) : null;
    const freshColor = lastChange?.origin ? actorCanvasColor(lastChange.origin) : '#e9b45c';

    // gutter + ruler
    ctx.fillStyle = '#141518';
    ctx.fillRect(0, 0, LABEL_W, size.h);
    ctx.fillStyle = '#1b1d21';
    ctx.fillRect(LABEL_W, 0, size.w - LABEL_W, RULER_H);
    ctx.strokeStyle = '#2a2c31';
    ctx.beginPath(); ctx.moveTo(0, RULER_H - 0.5); ctx.lineTo(size.w, RULER_H - 0.5); ctx.stroke();
    ctx.beginPath(); ctx.moveTo(LABEL_W - 0.5, 0); ctx.lineTo(LABEL_W - 0.5, size.h); ctx.stroke();

    // ruler ticks / grid
    const step = chooseTickStep(pxPerFrame);
    const first = Math.floor(scrollX / step) * step;
    ctx.font = '10px ui-monospace, "SF Mono", Menlo, monospace';
    ctx.textBaseline = 'middle';
    for (let f = first; ; f += step) {
      const x = xOf(f);
      if (x > size.w) break;
      if (x < LABEL_W) continue;
      ctx.strokeStyle = '#2a2c31';
      ctx.beginPath(); ctx.moveTo(Math.round(x) + 0.5, RULER_H - 6); ctx.lineTo(Math.round(x) + 0.5, RULER_H); ctx.stroke();
      ctx.strokeStyle = 'rgba(255,255,255,0.035)';
      ctx.beginPath(); ctx.moveTo(Math.round(x) + 0.5, RULER_H); ctx.lineTo(Math.round(x) + 0.5, size.h - SCROLLBAR); ctx.stroke();
      ctx.fillStyle = '#8b8e96';
      ctx.fillText(timecode(f, tl.fps), x + 4, RULER_H / 2);
    }

    // rows, track labels and clips
    const rowByTrack = new Map(rows.filter(r => r.kind === 'track').map(r => [(r as Extract<Row, { kind: 'track' }>).id, r as Extract<Row, { kind: 'track' }>]));
    const trackById = new Map(tl.tracks.map(t => [t.id, t]));

    for (const row of rows) {
      const top = row.top - scrollY;
      if (top + row.height < RULER_H || top > size.h) continue;
      if (row.kind === 'kf') {
        const c = tl.clips[row.clipId];
        if (!c) continue;
        ctx.fillStyle = '#131417';
        ctx.fillRect(LABEL_W, top, size.w - LABEL_W, row.height);
        ctx.fillStyle = '#8b8e96';
        ctx.font = '10px ui-monospace, "SF Mono", Menlo, monospace';
        const lane = c.keyframes?.[row.prop] ?? [];
        ctx.fillText(`${row.prop} (${lane.length})`, 8, top + row.height / 2);
        const mid = top + row.height / 2;
        ctx.strokeStyle = '#26282d';
        ctx.beginPath(); ctx.moveTo(LABEL_W, mid + 0.5); ctx.lineTo(size.w, mid + 0.5); ctx.stroke();
        // Highlight the selected clip's span.
        ctx.fillStyle = 'rgba(122,162,255,0.05)';
        ctx.fillRect(xOf(c.start), top, clipLen(c) * pxPerFrame, row.height);
        for (const k of lane) {
          const kx = xOf(c.start + k.t);
          const on = selectedKf?.clip === row.clipId && selectedKf.prop === row.prop && selectedKf.t === k.t;
          ctx.fillStyle = on ? '#7aa2ff' : '#c9cbd1';
          ctx.beginPath();
          ctx.moveTo(kx, mid - 4); ctx.lineTo(kx + 4, mid); ctx.lineTo(kx, mid + 4); ctx.lineTo(kx - 4, mid);
          ctx.closePath(); ctx.fill();
        }
        continue;
      }

      const track = row.track;
      ctx.fillStyle = '#17181b';
      ctx.fillRect(LABEL_W, top, size.w - LABEL_W, row.height);
      ctx.fillStyle = track.kind === 'audio' ? '#1b2622' : '#181a1e';
      ctx.fillRect(LABEL_W, top, size.w - LABEL_W, row.height - ROW_GAP);

      // label gutter
      ctx.fillStyle = '#16171a';
      ctx.fillRect(0, top, LABEL_W, row.height);
      ctx.strokeStyle = '#24262b';
      ctx.beginPath(); ctx.moveTo(0, top + row.height - 0.5); ctx.lineTo(size.w, top + row.height - 0.5); ctx.stroke();
      ctx.fillStyle = '#d8d9dc';
      ctx.font = '11px -apple-system, BlinkMacSystemFont, system-ui, sans-serif';
      ctx.fillText(track.name ?? track.id, 8, top + row.height / 2 - 4);
      ctx.fillStyle = '#8b8e96';
      ctx.font = '9px ui-monospace, "SF Mono", Menlo, monospace';
      ctx.fillText(`${track.kind}${track.locked ? ' · L' : ''}${track.muted ? ' · M' : ''}`, 8, top + row.height / 2 + 8);

      const clips = Object.entries(tl.clips)
        .filter(([id, c]) => {
          // A single dragged clip previews on its target track, not the source.
          if (drag?.kind === 'move' && drag.orig[id] && Object.keys(drag.orig).length === 1 && drag.targetTrack) {
            return drag.targetTrack === track.id;
          }
          return c.track === track.id;
        })
        .sort((a, b) => a[1].start - b[1].start);
      for (const [id, c] of clips) {
        let start = c.start, inF = c.in, outF = c.out;
        if (drag?.kind === 'move' && drag.orig[id]) {
          start = Math.max(0, drag.orig[id].start + drag.delta);
        }
        if (drag?.kind === 'trim' && drag.clipId === id) {
          start = drag.next.start; inF = drag.next.in; outF = drag.next.out;
        }
        const x = xOf(start);
        const w = Math.max(2, (outF - inF) * pxPerFrame);
        const y = top + CLIP_INSET;
        const h = TRACK_H - CLIP_INSET * 2 - ROW_GAP;
        if (x + w < LABEL_W || x > size.w) continue;

        ctx.save();
        roundRect(ctx, x, y, w, h, 4);
        ctx.fillStyle = trackColor(track);
        ctx.fill();
        ctx.clip();

        const kind = assetKind(assets, c.asset);
        const rec = c.asset ? resRef.current.get(c.asset) : undefined;
        const sourceFrames = c.asset && assets[c.asset]?.duration_s ? assets[c.asset].duration_s! * fps : 0;
        if (kind === 'audio' && rec?.peaks?.length && sourceFrames > 0) {
          // Peaks cover the whole source; draw the portion under this clip.
          const peaks = rec.peaks;
          const maxPeak = rec.peakMax ?? Math.max(0.001, ...peaks);
          const mid = y + h / 2;
          const n = Math.max(1, Math.floor(w));
          ctx.strokeStyle = 'rgba(210,255,225,0.8)';
          ctx.beginPath();
          for (let i = 0; i < n; i++) {
            const srcFrame = inF + (i / n) * (outF - inF);
            const b = clamp(Math.floor(srcFrame / sourceFrames * peaks.length), 0, peaks.length - 1);
            const amp = (peaks[b] / maxPeak) * (h / 2 - 2);
            ctx.moveTo(x + i + 0.5, mid - amp);
            ctx.lineTo(x + i + 0.5, mid + amp);
          }
          ctx.stroke();
        } else if (rec?.img && (kind === 'video' || kind === 'image')) {
          ctx.globalAlpha = 0.5;
          if (kind === 'video' && sourceFrames > 0) {
            const sx = clamp(inF / sourceFrames, 0, 1) * rec.img.width;
            const sw = Math.max(1, ((outF - inF) / sourceFrames) * rec.img.width);
            ctx.drawImage(rec.img, sx, 0, sw, rec.img.height, x, y, w, h);
          } else {
            ctx.drawImage(rec.img, x, y, w, h);
          }
          ctx.globalAlpha = 1;
        }
        // subtle top sheen + label
        ctx.fillStyle = 'rgba(255,255,255,0.07)';
        ctx.fillRect(x, y, w, 1);
        ctx.font = '10.5px ui-monospace, "SF Mono", Menlo, monospace';
        ctx.textBaseline = 'middle';
        ctx.fillStyle = 'rgba(255,255,255,0.92)';
        const label = c.component
          ? String(c.props?.text ?? `<${c.component}>`)
          : c.asset ? (assets[c.asset]?.name ?? c.asset) : id;
        ctx.fillText(label, x + 5, y + h / 2, Math.max(0, w - 10));
        ctx.restore();

        if (selected.has(id)) {
          ctx.strokeStyle = '#7aa2ff';
          ctx.lineWidth = 2;
          roundRect(ctx, x + 1, y + 1, w - 2, h - 2, 4);
          ctx.stroke();
          ctx.lineWidth = 1;
        }
        if (fresh?.has(id)) {
          ctx.strokeStyle = freshColor;
          ctx.lineWidth = 2;
          ctx.shadowColor = freshColor;
          ctx.shadowBlur = 10;
          roundRect(ctx, x + 1, y + 1, w - 2, h - 2, 4);
          ctx.stroke();
          ctx.shadowBlur = 0;
          ctx.lineWidth = 1;
        }
      }
    }

    // markers
    for (const [, m] of Object.entries(tl.markers ?? {})) {
      const x = xOf(m.t);
      if (x < LABEL_W || x > size.w) continue;
      ctx.strokeStyle = 'rgba(233,180,92,0.7)';
      ctx.setLineDash([4, 4]);
      ctx.beginPath(); ctx.moveTo(x + 0.5, RULER_H); ctx.lineTo(x + 0.5, size.h - SCROLLBAR); ctx.stroke();
      ctx.setLineDash([]);
      ctx.fillStyle = '#e9b45c';
      ctx.font = '9px ui-monospace, "SF Mono", Menlo, monospace';
      ctx.fillText(m.label, x + 3, RULER_H + 8);
    }

    // agent presence: selections, then playheads
    let pi = 0;
    for (const p of presence) {
      if (!p.actor || p.actor.name === 'unknown') continue;
      if (now / 1000 - p.updated_at > 60) continue;
      const color = actorCanvasColor(p.actor);
      for (const id of p.selection) {
        const c = tl.clips[id];
        if (!c) continue;
        const row = rowByTrack.get(c.track);
        if (!row) continue;
        const x = xOf(c.start), w = clipLen(c) * pxPerFrame;
        const y = row.top - scrollY + CLIP_INSET;
        ctx.strokeStyle = color;
        ctx.lineWidth = 2;
        ctx.setLineDash([5, 3]);
        roundRect(ctx, x - 1, y - 1, w + 2, TRACK_H - CLIP_INSET * 2 - ROW_GAP + 2, 5);
        ctx.stroke();
        ctx.setLineDash([]);
        ctx.lineWidth = 1;
      }
      if (p.playhead !== undefined) {
        const x = xOf(p.playhead);
        ctx.strokeStyle = color;
        ctx.lineWidth = 1.5;
        ctx.beginPath(); ctx.moveTo(x + 0.5, RULER_H - 4); ctx.lineTo(x + 0.5, size.h - SCROLLBAR); ctx.stroke();
        ctx.fillStyle = color;
        ctx.beginPath();
        ctx.moveTo(x - 4, RULER_H - 10); ctx.lineTo(x + 4, RULER_H - 10); ctx.lineTo(x, RULER_H - 4);
        ctx.closePath(); ctx.fill();
        ctx.font = '9px -apple-system, BlinkMacSystemFont, system-ui, sans-serif';
        ctx.fillStyle = color;
        ctx.fillText(p.actor.name, x + 4, 8 + (pi % 2) * 10);
        pi++;
      }
    }

    // transitions: a bow-tie over the cut, spanning the transition window
    for (const t of Object.values(tl.transitions ?? {})) {
      const a = tl.clips[t.between[0]], b = tl.clips[t.between[1]];
      const row = a ? rowByTrack.get(a.track) : undefined;
      if (!a || !b || !row) continue;
      const from = b.start - Math.floor(t.dur / 2);
      const x0 = xOf(from), x1 = xOf(from + t.dur);
      const y0 = row.top - scrollY + CLIP_INSET, y1 = y0 + TRACK_H - CLIP_INSET * 2 - ROW_GAP;
      ctx.fillStyle = 'rgba(255, 255, 255, 0.22)';
      ctx.beginPath(); ctx.moveTo(x0, y0); ctx.lineTo(x1, y1); ctx.lineTo(x1, y0); ctx.lineTo(x0, y1); ctx.closePath(); ctx.fill();
      ctx.strokeStyle = 'rgba(255, 255, 255, 0.6)';
      ctx.strokeRect(x0 + 0.5, y0 + 0.5, x1 - x0 - 1, y1 - y0 - 1);
    }

    // snap line
    if (drag && 'snapAt' in drag && drag.snapAt !== null) {
      const x = xOf(drag.snapAt);
      ctx.strokeStyle = '#ffd166';
      ctx.setLineDash([3, 3]);
      ctx.beginPath(); ctx.moveTo(x + 0.5, RULER_H); ctx.lineTo(x + 0.5, size.h - SCROLLBAR); ctx.stroke();
      ctx.setLineDash([]);
    }

    // marquee
    if (drag?.kind === 'marquee') {
      const x = Math.min(drag.x0, drag.x1), y = Math.min(drag.y0, drag.y1);
      ctx.fillStyle = 'rgba(122,162,255,0.15)';
      ctx.strokeStyle = '#7aa2ff';
      ctx.fillRect(x, y, Math.abs(drag.x1 - drag.x0), Math.abs(drag.y1 - drag.y0));
      ctx.strokeRect(x + 0.5, y + 0.5, Math.abs(drag.x1 - drag.x0), Math.abs(drag.y1 - drag.y0));
    }

    // playhead
    const px = xOf(frame);
    if (px >= LABEL_W && px <= size.w) {
      ctx.strokeStyle = '#7aa2ff';
      ctx.lineWidth = 1.5;
      ctx.beginPath(); ctx.moveTo(px + 0.5, RULER_H - 8); ctx.lineTo(px + 0.5, size.h - SCROLLBAR); ctx.stroke();
      ctx.fillStyle = '#7aa2ff';
      ctx.beginPath();
      ctx.moveTo(px - 5, RULER_H - 14); ctx.lineTo(px + 5, RULER_H - 14); ctx.lineTo(px, RULER_H - 7);
      ctx.closePath(); ctx.fill();
      ctx.lineWidth = 1;
    }

    // horizontal scrollbar
    const barY = size.h - SCROLLBAR;
    const usable = Math.max(1, size.w - LABEL_W);
    const visibleFrames = usable / pxPerFrame;
    ctx.fillStyle = '#101113';
    ctx.fillRect(LABEL_W, barY, usable, SCROLLBAR);
    if (duration > visibleFrames) {
      const thumbW = Math.max(24, usable * (visibleFrames / duration));
      const maxScroll = Math.max(1, duration - visibleFrames);
      const thumbX = LABEL_W + (clamp(scrollX, 0, maxScroll) / maxScroll) * (usable - thumbW);
      ctx.fillStyle = '#3a3d44';
      roundRect(ctx, thumbX, barY + 2, thumbW, SCROLLBAR - 4, 3);
      ctx.fill();
    }

    // vertical scrollbar
    if (contentH > viewH) {
      const maxScroll = contentH - viewH;
      const trackH = Math.max(0, size.h - SCROLLBAR - RULER_H);
      const thumbH = Math.max(24, trackH * (viewH / contentH));
      const thumbY = RULER_H + (clamp(scrollY, 0, maxScroll) / Math.max(1, maxScroll)) * (trackH - thumbH);
      ctx.fillStyle = '#101113';
      ctx.fillRect(size.w - SCROLLBAR, RULER_H, SCROLLBAR, trackH);
      ctx.fillStyle = '#3a3d44';
      roundRect(ctx, size.w - SCROLLBAR + 2, thumbY, SCROLLBAR - 4, thumbH, 3);
      ctx.fill();
    }
  }, [size, pxPerFrame, scrollX, scrollY, tl, frame, selected, selectedKf, drag, presence, assets, lastChange, contentH, viewH, fps, resTick]);

  useEffect(() => { draw(); }, [draw]);

  // ---- hit testing ----
  const hitAt = useCallback((x: number, y: number): Hit => {
    if (y >= size.h - SCROLLBAR && x >= LABEL_W) return { type: 'hscroll' };
    if (y < RULER_H) return { type: 'ruler' };
    const cy = y + scrollY;
    const row = rows.find(r => cy >= r.top && cy < r.top + r.height);
    if (!row) return { type: 'empty', track: null };
    if (row.kind === 'kf') {
      const c = tl.clips[row.clipId];
      if (c) {
        const mid = row.top - scrollY + row.height / 2;
        if (Math.abs(y - mid) <= 7) {
          for (const k of c.keyframes?.[row.prop] ?? []) {
            const kx = LABEL_W + (c.start + k.t - scrollX) * pxPerFrame;
            if (Math.abs(x - kx) <= 6) return { type: 'kf', clipId: row.clipId, prop: row.prop, t: k.t };
          }
        }
        const local = Math.round(x < LABEL_W ? 0 : (x - LABEL_W) / pxPerFrame + scrollX - c.start);
        return { type: 'kf-lane', clipId: row.clipId, prop: row.prop, local };
      }
      return { type: 'empty', track: null };
    }
    if (x < LABEL_W) return { type: 'empty', track: row.id };
    const clips = Object.entries(tl.clips)
      .filter(([, c]) => c.track === row.id)
      .sort((a, b) => b[1].start - a[1].start);
    for (const [id, c] of clips) {
      const cx = LABEL_W + (c.start - scrollX) * pxPerFrame;
      const cw = clipLen(c) * pxPerFrame;
      if (x >= cx && x <= cx + cw) {
        const edge = x - cx <= 6 ? 'left' : cx + cw - x <= 6 ? 'right' : 'body';
        return { type: 'clip', id, edge };
      }
    }
    return { type: 'empty', track: row.id };
  }, [rows, tl, scrollX, scrollY, pxPerFrame, size.h]);

  const seekAt = (x: number) => {
    const f = clamp(frameAtX(Math.max(0, x - LABEL_W), scrollX, pxPerFrame), 0, duration);
    props.onSeek(f);
  };

  // ---- pointer ----
  const onPointerDown = (e: React.PointerEvent<HTMLCanvasElement>) => {
    if (e.button !== 0) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const x = e.clientX - rect.left, y = e.clientY - rect.top;
    e.currentTarget.focus();
    e.currentTarget.setPointerCapture(e.pointerId);
    const hit = hitAt(x, y);
    const additive = e.shiftKey || e.metaKey || e.ctrlKey;

    if (hit.type === 'hscroll') {
      setDrag({ kind: 'hscroll', pointerId: e.pointerId, startX: x, orig: scrollX });
      return;
    }
    if (hit.type === 'ruler') {
      const f = clamp(frameAtX(x - LABEL_W, scrollX, pxPerFrame), 0, duration);
      props.onSeek(f);
      setDrag({ kind: 'scrub', pointerId: e.pointerId });
      return;
    }
    if (hit.type === 'kf') {
      setSelectedKf({ clip: hit.clipId, prop: hit.prop, t: hit.t });
      const c = tl.clips[hit.clipId];
      const k = c?.keyframes?.[hit.prop]?.find(x => x.t === hit.t);
      if (c && k) setDrag({ kind: 'kf', pointerId: e.pointerId, clipId: hit.clipId, prop: hit.prop, origT: hit.t, t: hit.t, v: k.v, ease: k.ease, startX: x, snapAt: null });
      return;
    }
    if (hit.type === 'clip') {
      const c = tl.clips[hit.id];
      if (!c) return;
      const track = tl.tracks.find(t => t.id === c.track);
      if (tool === 'razor') {
        if (track?.locked) { setError(`track ${c.track} is locked`); return; }
        const at = clamp(Math.round(frameAtX(x - LABEL_W, scrollX, pxPerFrame)), c.start + 1, clipEnd(c) - 1);
        if (at > c.start && at < clipEnd(c)) void runOp({ type: 'clip.split', args: { clip: hit.id, at } });
        return;
      }
      let next: Set<string>;
      if (additive) {
        next = new Set(selected);
        next.has(hit.id) ? next.delete(hit.id) : next.add(hit.id);
      } else {
        next = selected.has(hit.id) ? new Set(selected) : new Set([hit.id]);
      }
      setSelected(next);
      setSelectedKf(null);
      if (additive && !next.has(hit.id)) return;
      if (track?.locked) { setError(`track ${c.track} is locked`); return; }

      if (hit.edge !== 'body' && (selected.size <= 1 || selected.has(hit.id))) {
        setDrag({ kind: 'trim', pointerId: e.pointerId, clipId: hit.id, edge: hit.edge, startX: x,
          orig: { start: c.start, in: c.in, out: c.out, track: c.track },
          next: { start: c.start, in: c.in, out: c.out }, snapAt: null });
        return;
      }
      const orig: Record<string, { start: number; track: string }> = {};
      for (const id of next) {
        const cc = tl.clips[id];
        if (cc) orig[id] = { start: cc.start, track: cc.track };
      }
      setDrag({ kind: 'move', pointerId: e.pointerId, startX: x, startY: y, orig, delta: 0,
        targetTrack: c.track, snapAt: null });
      return;
    }
    if (hit.type === 'kf-lane') {
      setSelected(new Set([hit.clipId]));
      setSelectedKf(null);
      // Double-click is handled below; a single click just seeks into the lane.
      if (!additive) setSelected(new Set([hit.clipId]));
      return;
    }
    // empty lane: marquee
    if (!additive) setSelected(new Set());
    setSelectedKf(null);
    setDrag({ kind: 'marquee', pointerId: e.pointerId, x0: x, y0: y, x1: x, y1: y, additive, base: additive ? new Set(selected) : new Set() });
  };

  const onPointerMove = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const d = dragRef.current;
    if (!d || d.pointerId !== e.pointerId) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const x = e.clientX - rect.left, y = e.clientY - rect.top;

    if (d.kind === 'scrub') {
      props.onSeek(clamp(frameAtX(x - LABEL_W, scrollX, pxPerFrame), 0, duration));
      return;
    }
    if (d.kind === 'hscroll') {
      const usable = Math.max(1, size.w - LABEL_W);
      const visibleFrames = usable / pxPerFrame;
      const max = Math.max(0, duration - visibleFrames);
      setScrollX(clamp(d.orig + ((x - d.startX) / usable) * Math.max(1, duration), 0, max));
      return;
    }
    if (d.kind === 'marquee') {
      const nx = clamp(x, LABEL_W, size.w), ny = clamp(y, RULER_H, size.h - SCROLLBAR);
      const box = { x0: Math.min(d.x0, nx), y0: Math.min(d.y0, ny), x1: Math.max(d.x0, nx), y1: Math.max(d.y0, ny) };
      const ids = new Set(d.base);
      for (const row of rows) {
        if (row.kind !== 'track') continue;
        const top = row.top - scrollY;
        if (top + row.height < box.y0 || top > box.y1) continue;
        for (const [id, c] of Object.entries(tl.clips)) {
          if (c.track !== row.id) continue;
          const cx = LABEL_W + (c.start - scrollX) * pxPerFrame;
          const cw = clipLen(c) * pxPerFrame;
          if (cx + cw >= box.x0 && cx <= box.x1) ids.add(id);
        }
      }
      setSelected(ids);
      setDrag({ ...d, x1: nx, y1: ny });
      return;
    }
    if (d.kind === 'move') {
      const anchorId = Object.keys(d.orig)[0];
      const anchor = d.orig[anchorId];
      if (!anchor) return;
      const raw = anchor.start + (x - d.startX) / pxPerFrame;
      let delta = Math.round(raw - anchor.start);
      let snapAt: number | null = null;
      if (snapOn && !e.altKey) {
        const candidates = snapCandidates(tl, frame, new Set(Object.keys(d.orig)));
        const anchorLen = (tl.clips[anchorId] ? clipLen(tl.clips[anchorId]) : 0);
        const startSnap = snapFrame(anchor.start + delta, candidates, SNAP_PX / pxPerFrame);
        const endSnap = snapFrame(anchor.start + delta + anchorLen, candidates, SNAP_PX / pxPerFrame);
        if (startSnap.at !== null && (endSnap.at === null || Math.abs(startSnap.frame - (anchor.start + delta)) <= Math.abs(endSnap.frame - (anchor.start + delta + anchorLen)))) {
          delta = startSnap.frame - anchor.start;
          snapAt = startSnap.at;
        } else if (endSnap.at !== null) {
          delta = endSnap.frame - anchorLen - anchor.start;
          snapAt = endSnap.at;
        }
      }
      const minStart = Math.min(...Object.values(d.orig).map(o => o.start));
      delta = Math.max(delta, -minStart);
      // Vertical movement changes the track for a single clip only.
      let targetTrack = d.targetTrack;
      if (Object.keys(d.orig).length === 1) {
        const cy = y + scrollY;
        const row = rows.find(r => r.kind === 'track' && cy >= r.top && cy < r.top + r.height) as Extract<Row, { kind: 'track' }> | undefined;
        if (row && !row.track.locked) targetTrack = row.id;
      }
      setDrag({ ...d, delta, targetTrack, snapAt });
      return;
    }
    if (d.kind === 'trim') {
      const rawDelta = (x - d.startX) / pxPerFrame;
      const c = tl.clips[d.clipId];
      if (!c) return;
      const sourceFrames = c.asset && assets[c.asset]?.duration_s ? assets[c.asset].duration_s! * fps : 0;
      let snapAt: number | null = null;
      if (d.edge === 'left') {
        let wanted = d.orig.start + rawDelta;
        if (snapOn && !e.altKey) {
          const s = snapFrame(wanted, snapCandidates(tl, frame, new Set([d.clipId])), SNAP_PX / pxPerFrame);
          wanted = s.frame; snapAt = s.at;
        }
        const next = trimStart(c, wanted, sourceFrames);
        setDrag({ ...d, next: { ...d.orig, ...next }, snapAt });
      } else {
        let wanted = d.orig.out + rawDelta;
        if (snapOn && !e.altKey) {
          const s = snapFrame(wanted, snapCandidates(tl, frame, new Set([d.clipId])), SNAP_PX / pxPerFrame);
          wanted = s.frame; snapAt = s.at;
        }
        const next = trimEnd(c, wanted, sourceFrames);
        setDrag({ ...d, next: { ...d.orig, ...next }, snapAt });
      }
      return;
    }
    if (d.kind === 'kf') {
      const c = tl.clips[d.clipId];
      if (!c) return;
      const wanted = (x - LABEL_W) / pxPerFrame + scrollX - c.start;
      let t = clamp(Math.round(wanted), 0, Math.max(0, clipLen(c) - 1));
      let snapAt: number | null = null;
      if (snapOn && !e.altKey) {
        // Snap retimes to other keys in the same lane.
        const other = (c.keyframes?.[d.prop] ?? []).map(k => c.start + k.t).filter(x => x !== c.start + d.origT);
        const s = snapFrame(c.start + t, other, SNAP_PX / pxPerFrame);
        if (s.at !== null) { t = clamp(s.at - c.start, 0, Math.max(0, clipLen(c) - 1)); snapAt = s.at; }
      }
      setDrag({ ...d, t, snapAt });
    }
  };

  const onPointerUp = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const d = dragRef.current;
    if (!d || d.pointerId !== e.pointerId) return;
    setDrag(null);
    if (d.kind === 'scrub') {
      const rect = e.currentTarget.getBoundingClientRect();
      const f = clamp(frameAtX(e.clientX - rect.left - LABEL_W, scrollX, pxPerFrame), 0, duration);
      props.onSeek(f);
      props.onSeekEnd?.(f);
      return;
    }
    if (d.kind === 'move') {
      const ops: Op[] = [];
      const one = Object.keys(d.orig).length === 1;
      for (const [id, o] of Object.entries(d.orig)) {
        const start = Math.max(0, o.start + d.delta);
        const track = one && d.targetTrack ? d.targetTrack : o.track;
        if (start !== o.start || track !== o.track) ops.push({ type: 'clip.move', args: { clip: id, start, ...(track !== o.track ? { track } : {}) } });
      }
      void runOps(ops);
      return;
    }
    if (d.kind === 'trim' && (d.next.start !== d.orig.start || d.next.in !== d.orig.in || d.next.out !== d.orig.out)) {
      void runOp({ type: 'clip.trim', args: { clip: d.clipId, start: d.next.start, in: d.next.in, out: d.next.out } });
      return;
    }
    if (d.kind === 'kf' && d.t !== d.origT) {
      void runOps([
        { type: 'keyframe.remove', args: { clip: d.clipId, prop: d.prop, t: d.origT } },
        { type: 'keyframe.set', args: { clip: d.clipId, prop: d.prop, t: d.t, v: d.v, ...(d.ease ? { ease: d.ease } : {}) } },
      ]);
      setSelectedKf({ clip: d.clipId, prop: d.prop, t: d.t });
    }
  };

  const onDoubleClick = (e: React.MouseEvent<HTMLCanvasElement>) => {
    const rect = e.currentTarget.getBoundingClientRect();
    const x = e.clientX - rect.left, y = e.clientY - rect.top;
    const hit = hitAt(x, y);
    if (hit.type === 'kf-lane') {
      const c = tl.clips[hit.clipId];
      if (!c) return;
      const local = clamp(hit.local, 0, Math.max(0, clipLen(c) - 1));
      const v = propValue(c, hit.prop, local);
      void runOp({ type: 'keyframe.set', args: { clip: hit.clipId, prop: hit.prop, t: local, v } });
      setSelectedKf({ clip: hit.clipId, prop: hit.prop, t: local });
    } else if (hit.type === 'ruler') {
      void runOp({ type: 'marker.add', args: { t: clamp(frameAtX(x - LABEL_W, scrollX, pxPerFrame), 0, duration), label: `marker ${Math.round(frameAtX(x - LABEL_W, scrollX, pxPerFrame))}` } });
    } else if (hit.type === 'clip') {
      const c = tl.clips[hit.id];
      if (c) props.onSeek(clamp(Math.round(frameAtX(x - LABEL_W, scrollX, pxPerFrame)), c.start, clipEnd(c)));
    }
  };

  // native wheel listener so preventDefault works (React's is passive)
  useEffect(() => {
    const el = canvasRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const rect = el.getBoundingClientRect();
      const x = e.clientX - rect.left;
      if (e.metaKey || e.ctrlKey) {
        zoomBy(Math.exp(-e.deltaY * 0.002), x - LABEL_W);
      } else if (e.shiftKey || contentH <= viewH) {
        setScrollX(s => clamp(s + (e.deltaY + e.deltaX) / pxPerFrame, 0, Math.max(0, duration - (size.w - LABEL_W) / pxPerFrame)));
      } else {
        setScrollY(s => clamp(s + e.deltaY, 0, Math.max(0, contentH - viewH)));
      }
    };
    el.addEventListener('wheel', onWheel, { passive: false });
    return () => el.removeEventListener('wheel', onWheel);
  }, [pxPerFrame, contentH, viewH, duration, size.w]);

  // ---- keyboard ----
  // Keep the latest closures in a ref so the listener is installed once, not
  // on every playhead update.
  const keyActions = useRef({ frame, duration, onSeek: props.onSeek, split, ripple, remove, addMarker, zoomBy, fit, runMethod });
  keyActions.current = { frame, duration, onSeek: props.onSeek, split, ripple, remove, addMarker, zoomBy, fit, runMethod };
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target instanceof Element ? e.target : null;
      if (t?.closest('input, textarea, select, [contenteditable="true"], .xterm')) return;
      const a = keyActions.current;
      const mod = e.metaKey || e.ctrlKey;
      if (mod && e.key.toLowerCase() === 'z') {
        e.preventDefault();
        void a.runMethod(e.shiftKey ? 'redo' : 'undo');
        return;
      }
      if (e.key === 'Escape') { setSelected(new Set()); setSelectedKf(null); return; }
      if (e.key === 'Delete' || e.key === 'Backspace') {
        e.preventDefault();
        e.shiftKey ? a.remove() : a.ripple();
        return;
      }
      if (e.key.toLowerCase() === 's') { e.preventDefault(); a.split(); return; }
      if (e.key.toLowerCase() === 'm') { e.preventDefault(); a.addMarker(); return; }
      if (e.key.toLowerCase() === 'b') { e.preventDefault(); setTool(t => t === 'razor' ? 'select' : 'razor'); return; }
      if (e.key === '+' || e.key === '=') { e.preventDefault(); a.zoomBy(1.25); return; }
      if (e.key === '-') { e.preventDefault(); a.zoomBy(0.8); return; }
      if (e.key === '0') { e.preventDefault(); a.fit(); return; }
      if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
        e.preventDefault();
        const step = (e.shiftKey ? 10 : 1) * (e.key === 'ArrowLeft' ? -1 : 1);
        const f = clamp(a.frame + step, 0, a.duration);
        a.onSeek(f);
        return;
      }
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, []);

  const keyEase = selectedKf ? tl.clips[selectedKf.clip]?.keyframes?.[selectedKf.prop]?.find(k => k.t === selectedKf.t)?.ease ?? 'linear' : 'linear';

  return (
    <section className="panel timeline" aria-label="Timeline">
      <header>
        <h2>Timeline</h2>
        <span className="muted tl-stat">frame {frame} / {tl.duration} · {tl.width}×{tl.height} @ {fps.toFixed(3)} fps</span>
        <div className="tl-tools">
          <button onClick={() => zoomBy(0.8)} title="Zoom out">−</button>
          <button onClick={() => zoomBy(1.25)} title="Zoom in">+</button>
          <button onClick={fit} title="Fit timeline (0)">Fit</button>
          <button className={snapOn ? 'on' : ''} onClick={() => setSnapOn(s => !s)} title="Snap to playhead, markers and clip edges (hold Alt to bypass)">Snap</button>
          <button className={tool === 'razor' ? 'on' : ''} onClick={() => setTool(t => t === 'razor' ? 'select' : 'razor')} title="Razor: click a clip to split at that point (B)">Razor</button>
          <span className="tl-sep" />
          <button onClick={split} disabled={![...selected].some(id => tl.clips[id] && frame > tl.clips[id].start && frame < clipEnd(tl.clips[id]))} title="Split selected at playhead (S)">Split</button>
          <button onClick={ripple} disabled={selected.size === 0} title="Ripple delete (Delete)">Ripple</button>
          <button onClick={remove} disabled={selected.size === 0} title="Delete without closing the gap (Shift+Delete)">Delete</button>
          <span className="tl-sep" />
          <input className="tl-marker" value={markerLabel} onChange={e => setMarkerLabel(e.target.value)} placeholder="marker" aria-label="Marker label" />
          <button onClick={addMarker} title="Add marker at playhead (M)">+ Marker</button>
          <button onClick={() => void runMethod('undo')} title="Undo (⌘Z)">Undo</button>
          <button onClick={() => void runMethod('redo')} title="Redo (⇧⌘Z)">Redo</button>
        </div>
      </header>
      <div className="tl-tools tl-tools-2">
        {single ? (
          <>
            <span className="muted">keyframes:</span>
            <select value={laneProp} onChange={e => setLaneProp(e.target.value)} aria-label="Keyframe property">
              {KEYS.map(k => <option key={k} value={k}>{k}</option>)}
            </select>
            <button onClick={addKeyframe} title="Add keyframe at playhead">+ key</button>
            {single && <span className="muted tl-clip-name">{single}</span>}
            {selectedKf && (
              <>
                <span className="tl-sep" />
                <span className="muted">{selectedKf.prop} @ {selectedKf.t}</span>
                <select value={keyEase} onChange={e => setKeyEase(e.target.value as Ease)} aria-label="Keyframe easing">
                  {EASES.map(k => <option key={k} value={k}>{k}</option>)}
                </select>
                <button onClick={() => { void runOp({ type: 'keyframe.remove', args: { clip: selectedKf.clip, prop: selectedKf.prop, t: selectedKf.t } }); setSelectedKf(null); }}>remove</button>
              </>
            )}
          </>
        ) : (
          <span className="muted">{selected.size > 1 ? `${selected.size} clips selected · batch edits` : 'Select a clip to edit keyframes'}</span>
        )}
        <span className="spacer" />
        {livePresence.filter(p => p.playhead !== undefined).map(p => (
          <span key={`${p.actor.type}:${p.actor.name}`} className="tl-who">
            <i style={{ background: actorCanvasColor(p.actor) }} />{p.actor.name}
          </span>
        ))}
        <span className="muted">{Object.keys(tl.clips).length} clips · {Object.keys(tl.markers ?? {}).length} markers</span>
      </div>
      <div className="tl-wrap" ref={wrapRef} onDragOver={onDragOver} onDragLeave={() => setDropAt(null)} onDrop={onDrop}>
        <canvas
          ref={canvasRef}
          className={`tl-canvas${tool === 'razor' ? ' tool-razor' : ''}`}
          tabIndex={0}
          role="application"
          data-drag={drag?.kind ?? ''}
          data-selected={selected.size}
          aria-label="Canvas timeline editor"
          onPointerDown={onPointerDown}
          onPointerMove={onPointerMove}
          onPointerUp={onPointerUp}
          onPointerCancel={onPointerUp}
          onDoubleClick={onDoubleClick}
          onContextMenu={onContextMenu}
        />
        {menu && (
          <div className="ctx-menu" role="menu" style={{ left: Math.min(menu.x, size.w - 220), top: Math.min(menu.y, Math.max(0, size.h - 24 - menu.items.length * 26)) }}
               onPointerDown={e => e.stopPropagation()}>
            {menu.title && <div className="ctx-title">{menu.title}</div>}
            {menu.items.map((it, i) => it === 'sep'
              ? <div key={i} className="ctx-sep" role="separator" />
              : <button key={i} role="menuitem" className={it.danger ? 'danger' : ''} disabled={it.disabled} onClick={() => { setMenu(null); it.run(); }}>{it.label}</button>)}
          </div>
        )}
        {dropAt && (
          <div className="tl-drop" style={{ left: dropAt.x }} aria-hidden>
            <span>{timecode(dropAt.frame, tl.fps)}{dropAt.trackId ? '' : ' · new track'}</span>
          </div>
        )}
        {error && <div className="tl-error" role="alert">{error}</div>}
      </div>
    </section>
  );
}
