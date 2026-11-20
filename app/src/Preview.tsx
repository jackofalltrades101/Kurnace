/** @jsxImportSource react */
// Preview panel: the Kurnace compositor, live (docs/v2-plan.md §3.1).
// The same compositor renders finals in headless Chromium, so this is what renders.
// Scrubbing shows fast approximate frames; when the playhead stops, every layer
// settles on the exact frame. Scene files hot-reload when they change on disk.
import { type AssetRef, Compositor } from '@kurnace/compositor';
import type { Timeline } from '@kurnace/timeline';
import { useEffect, useMemo, useRef, useState } from 'react';
import { type Asset, assetBaseUrl, assetUrl, call, seqBase } from './kurn';

export interface ResolvedAsset {
  url: string; kind: string; ext?: string | null; name?: string; width?: number; height?: number; duration_s?: number;
  frames?: string[]; fps_num?: number | null; fps_den?: number | null;
}

/** Resolve the timeline's asset ids to kurnasset:// URLs (ids are immutable → cache forever). */
const assetCache = new Map<string, Promise<ResolvedAsset | null>>();
function resolve(id: string): Promise<ResolvedAsset | null> {
  if (!assetCache.has(id)) {
    assetCache.set(id, call<Asset>('lib.get', { id })
      .then(a => {
        const seq = a.kind === 'seq';
        return {
          url: seq ? seqBase(a.id) : assetUrl(a.id, a.ext),
          kind: a.kind,
          ext: a.ext,
          name: a.name,
          width: a.width ?? undefined,
          height: a.height ?? undefined,
          duration_s: a.duration_s,
          frames: seq ? a.frames : undefined,
          fps_num: a.fps_num,
          fps_den: a.fps_den,
        };
      })
      .catch(() => null));
  }
  return assetCache.get(id)!;
}

export function useAssets(tl: Timeline | null) {
  const [assets, setAssets] = useState<Record<string, ResolvedAsset>>({});
  const ids = useMemo(() => [...new Set(Object.values(tl?.clips ?? {}).map(c => c.asset).filter((x): x is string => !!x))].sort().join(','), [tl]);
  useEffect(() => {
    let live = true;
    Promise.all(ids ? ids.split(',').map(async id => [id, await resolve(id)] as const) : []).then(pairs => {
      if (live) setAssets(Object.fromEntries(pairs.filter(([, a]) => a)) as Record<string, ResolvedAsset>);
    });
    return () => { live = false; };
  }, [ids]);
  return assets;
}

export interface PreviewHandle {
  /** The preview is DOM, not a canvas; kept for callers that probe for one. */
  canvas(): HTMLCanvasElement | null;
  /** Seek; the frame settles exactly once the playhead stops. */
  seek(seconds: number): void;
  /** Show a frame immediately without waiting for exact media (scrubbing). */
  seekFast(seconds: number): void;
  /** Seek and resolve once every layer shows exactly `frame`. */
  seekAndSettle(frame: number, timeoutMs?: number): Promise<boolean>;
}

const toRefs = (assets: Record<string, ResolvedAsset>): Record<string, AssetRef> =>
  Object.fromEntries(Object.entries(assets).map(([id, a]) => [id, {
    kind: a.kind, ext: a.ext, url: a.kind === 'seq' ? undefined : a.url,
    width: a.width, height: a.height, fps: a.fps_num && a.fps_den ? a.fps_num / a.fps_den : null,
  }]));

const fmt = (frame: number, fps: number) => {
  const t = frame / fps;
  const m = Math.floor(t / 60), s = Math.floor(t % 60), f = Math.round(frame % fps);
  return `${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}:${String(f).padStart(2, '0')}`;
};

export function Preview(props: {
  timeline: Timeline;
  assets: Record<string, ResolvedAsset>;
  onTime: (t: number) => void;
  handle?: React.MutableRefObject<PreviewHandle | null>;
  /** Bumped per scene name when its files change (hot reload). */
  sceneRev?: Record<string, number>;
}) {
  const { timeline: tl, assets } = props;
  const fps = tl.fps.num / tl.fps.den;
  const viewport = useRef<HTMLDivElement>(null);
  const stageHost = useRef<HTMLDivElement>(null);
  const comp = useRef<Compositor | null>(null);
  const frameRef = useRef(0);
  const settleToken = useRef(0);
  const [frame, setFrame] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [scale, setScale] = useState(0.5);
  const [errors, setErrors] = useState<Record<string, string>>({});
  const refs = useMemo(() => toRefs(assets), [assets]);

  const report = (f: number) => {
    frameRef.current = f;
    setFrame(f);
    props.onTime(f / fps);
  };

  // One compositor per mounted preview; it diffs timelines into layers.
  useEffect(() => {
    const c = new Compositor(stageHost.current!, {
      mode: 'preview', base: assetBaseUrl(), assets: refs,
      onError: (clip, msg) => setErrors(e => (e[clip] === msg ? e : { ...e, [clip]: msg })),
      onFrame: f => report(f),
    });
    comp.current = c;
    return () => { c.destroy(); comp.current = null; };
  }, []);

  /** Show `f` now, then settle it exactly (latest call wins). */
  const settle = (f: number) => {
    const c = comp.current;
    if (!c) return Promise.resolve(false);
    const token = ++settleToken.current;
    c.show(f);
    report(f);
    return c.seek(f).then(() => token === settleToken.current, () => false);
  };

  useEffect(() => {
    const c = comp.current;
    if (!c) return;
    setErrors({});
    c.setTimeline(tl, refs);
    if (!c.playing) void settle(Math.min(frameRef.current, Math.max(0, tl.duration - 1)));
  }, [tl, refs]);

  // Hot reload: a scene's files changed on disk.
  const prevRev = useRef<Record<string, number>>({});
  useEffect(() => {
    const c = comp.current;
    if (!c || !props.sceneRev) return;
    for (const [name, rev] of Object.entries(props.sceneRev)) {
      if (prevRev.current[name] !== rev) {
        c.reloadScene(name);
        setErrors({});
      }
    }
    prevRev.current = { ...props.sceneRev };
    if (!c.playing) void settle(frameRef.current);
  }, [props.sceneRev]);

  // Fit the stage into the panel.
  useEffect(() => {
    const el = viewport.current!;
    const ro = new ResizeObserver(() => {
      const r = el.getBoundingClientRect();
      setScale(Math.max(0.05, Math.min((r.width - 24) / tl.width, (r.height - 24) / tl.height)));
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, [tl.width, tl.height]);

  const toggle = () => {
    const c = comp.current;
    if (!c) return;
    if (c.playing) { c.pause(); setPlaying(false); void settle(frameRef.current); }
    else { c.play(); setPlaying(true); }
  };
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (e.code !== 'Space' || t?.closest('input, textarea, select, [contenteditable], .xterm')) return;
      e.preventDefault();
      toggle();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  });
  // Stop at the end.
  useEffect(() => { if (playing && !comp.current?.playing) setPlaying(false); }, [frame]);

  if (props.handle) {
    props.handle.current = {
      canvas: () => null,
      seek: t => { void settle(Math.round(t * fps)); },
      seekFast: t => { const f = Math.round(t * fps); comp.current?.show(f); report(f); },
      seekAndSettle: async f => settle(f),
    };
  }

  const errList = Object.entries(errors);
  return (
    <div className="preview" ref={viewport}>
      <div className="stage-frame" style={{ width: tl.width * scale, height: tl.height * scale }}>
        <div ref={stageHost} style={{ transform: `scale(${scale})`, transformOrigin: '0 0', width: tl.width, height: tl.height }} />
        {tl.duration === 0 && <div className="empty overlay">Empty timeline — drop media or a scene on the timeline, or ask an agent.</div>}
      </div>
      <div className="transport" data-frame={frame}>
        <button onClick={toggle} aria-label={playing ? 'Pause' : 'Play'} title="Play/Pause (Space)">{playing ? '❚❚' : '▶'}</button>
        <span className="tc">{fmt(frame, fps)}</span>
        <span className="muted">/ {fmt(tl.duration, fps)}</span>
      </div>
      {errList.length > 0 && (
        <div className="scene-errors" role="alert">
          {errList.slice(0, 3).map(([clip, msg]) => <div key={clip}><b>{clip}</b> {msg.split('\n')[0]}</div>)}
          {errList.length > 3 && <div className="muted">+{errList.length - 3} more</div>}
        </div>
      )}
    </div>
  );
}
