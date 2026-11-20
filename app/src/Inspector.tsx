/** @jsxImportSource react */
// Inspector (docs/v2-plan.md §3.2): edit the selected clip — timing, transform
// (with keyframe toggles at the playhead), scene params from scene.toml, and
// text/solid props. Every change goes through the optimistic runner.
import { type Clip, type Timeline, clipStateAt } from '@kurnace/timeline';
import { useEffect, useState } from 'react';
import { call, revealPath } from './kurn';
import type { ResolvedAsset } from './Preview';

type Run = (op: { type: string; args: Record<string, unknown> }) => Promise<unknown>;
interface ParamSpec { type?: string; default?: unknown; min?: number; max?: number; description?: string }
interface SceneInfo { name: string; params?: Record<string, ParamSpec>; description?: string | null }

const TRANSFORM: { key: string; label: string; step: number; min?: number; max?: number }[] = [
  { key: 'x', label: 'X', step: 1 },
  { key: 'y', label: 'Y', step: 1 },
  { key: 'scale', label: 'Scale', step: 0.01, min: 0 },
  { key: 'rotation', label: 'Rotate', step: 0.5 },
  { key: 'opacity', label: 'Opacity', step: 0.01, min: 0, max: 1 },
];

/** A number field that commits on Enter/blur (not on every keystroke). */
function NumField({ value, onCommit, step = 1, min, max, label }: { value: number; onCommit: (v: number) => void; step?: number; min?: number; max?: number; label: string }) {
  const [text, setText] = useState(String(+value.toFixed(4)));
  useEffect(() => setText(String(+value.toFixed(4))), [value]);
  const commit = () => {
    let v = Number(text);
    if (!Number.isFinite(v)) return setText(String(value));
    if (min !== undefined) v = Math.max(min, v);
    if (max !== undefined) v = Math.min(max, v);
    if (v !== value) onCommit(v);
    else setText(String(+value.toFixed(4)));
  };
  return (
    <input type="number" aria-label={label} value={text} step={step} min={min} max={max}
      onChange={e => setText(e.target.value)} onBlur={commit}
      onKeyDown={e => { if (e.key === 'Enter') (e.target as HTMLInputElement).blur(); if (e.key === 'Escape') { setText(String(value)); (e.target as HTMLInputElement).blur(); } }} />
  );
}

function TextField({ value, onCommit, label, multiline }: { value: string; onCommit: (v: string) => void; label: string; multiline?: boolean }) {
  const [text, setText] = useState(value);
  useEffect(() => setText(value), [value]);
  const commit = () => { if (text !== value) onCommit(text); };
  return multiline
    ? <textarea aria-label={label} value={text} rows={2} onChange={e => setText(e.target.value)} onBlur={commit} />
    : <input aria-label={label} value={text} onChange={e => setText(e.target.value)} onBlur={commit} onKeyDown={e => { if (e.key === 'Enter') (e.target as HTMLInputElement).blur(); }} />;
}

export function Inspector({ project, tl, clipId, frame, assets, run, onSeek }: {
  project: string; tl: Timeline; clipId: string; frame: number; assets: Record<string, ResolvedAsset>; run: Run; onSeek: (f: number) => void;
}) {
  const clip = tl.clips[clipId] as (Clip & { scene?: string; params?: Record<string, unknown> }) | undefined;
  const [scene, setScene] = useState<SceneInfo | null>(null);
  useEffect(() => {
    if (!clip?.scene) { setScene(null); return; }
    call<SceneInfo[]>('scenes.list', { project }).then(list => setScene(list.find(s => s.name === clip.scene) ?? null)).catch(() => setScene(null));
  }, [project, clip?.scene]);
  if (!clip) return null;

  const len = clip.out - clip.in;
  const local = Math.max(0, Math.min(len - 1, frame - clip.start));
  const inside = frame >= clip.start && frame < clip.start + len;
  const st = clipStateAt(clip, clip.start + local);
  const asset = clip.asset ? assets[clip.asset] : undefined;
  const title = clip.scene ? `scene · ${clip.scene}` : clip.component ? `${clip.component}` : asset?.name ?? clip.asset ?? 'clip';
  const set = (args: Record<string, unknown>) => run({ type: 'clip.set', args: { clip: clipId, ...args } });

  /** Change a transform prop: keyframed lanes get a key at the playhead, others a static value. */
  const setProp = (key: string, v: number) => {
    if (clip.keyframes?.[key]?.length) void run({ type: 'keyframe.set', args: { clip: clipId, prop: key, t: local, v } });
    else void set({ props: { [key]: v } });
  };
  const keyAt = (key: string) => clip.keyframes?.[key]?.find(k => k.t === local);
  const toggleKey = (key: string) => {
    if (keyAt(key)) void run({ type: 'keyframe.remove', args: { clip: clipId, prop: key, t: local } });
    else void run({ type: 'keyframe.set', args: { clip: clipId, prop: key, t: local, v: Number(st.props[key] ?? 0) } });
  };

  const params = scene?.params ?? {};
  const paramValue = (k: string, spec: ParamSpec) => clip.params?.[k] ?? spec.default;

  return (
    <section className="panel inspector" aria-label="Inspector">
      <header>
        <h2>Inspector</h2>
        <span className="insp-title" title={clipId}>{title}</span>
      </header>
      <div className="insp-body">
        <fieldset>
          <legend>Timing <span className="muted">(frames)</span></legend>
          <label>Start <NumField label="Start" value={clip.start} min={0} onCommit={v => void run({ type: 'clip.move', args: { clip: clipId, start: Math.round(v) } })} /></label>
          <label>Length <NumField label="Length" value={len} min={1} onCommit={v => void run({ type: 'clip.trim', args: { clip: clipId, out: clip.in + Math.round(v) } })} /></label>
          <label>In <NumField label="In" value={clip.in} min={0} onCommit={v => void run({ type: 'clip.trim', args: { clip: clipId, in: Math.round(v), out: Math.round(v) + len } })} /></label>
          <div className="insp-row">
            <button onClick={() => onSeek(clip.start)}>Go to start</button>
            <button onClick={() => onSeek(clip.start + len - 1)}>Go to end</button>
          </div>
        </fieldset>

        {(
          <fieldset>
            <legend>Transform {inside ? <span className="muted">at frame {local} of clip</span> : <span className="muted">(playhead outside clip)</span>}</legend>
            {TRANSFORM.map(t => {
              const lane = clip.keyframes?.[t.key]?.length;
              return (
                <label key={t.key}>
                  <span className="lbl">{t.label}</span>
                  <NumField label={t.label} value={Number(st.props[t.key] ?? 0)} step={t.step} min={t.min} max={t.max} onCommit={v => setProp(t.key, v)} />
                  <button className={`kf-toggle${keyAt(t.key) ? ' on' : lane ? ' lane' : ''}`} onClick={() => toggleKey(t.key)}
                    title={keyAt(t.key) ? 'Remove keyframe here' : 'Add keyframe at playhead'} aria-pressed={!!keyAt(t.key)} disabled={!inside}>◆</button>
                </label>
              );
            })}
            {asset && asset.kind !== 'audio' && (
              <label><span className="lbl">Fit</span>
                <select value={String(clip.props?.fit ?? 'contain')} onChange={e => void set({ props: { fit: e.target.value } })}>
                  {['contain', 'cover', 'stretch', 'none'].map(f => <option key={f}>{f}</option>)}
                </select>
              </label>
            )}
            {(asset?.kind === 'audio' || asset?.kind === 'video') && (
              <label><span className="lbl">Volume</span>
                <NumField label="Volume" value={Number(st.props.volume ?? 1)} step={0.05} min={0} max={4} onCommit={v => setProp('volume', v)} />
                <button className={`kf-toggle${keyAt('volume') ? ' on' : ''}`} onClick={() => toggleKey('volume')} disabled={!inside} aria-pressed={!!keyAt('volume')}>◆</button>
              </label>
            )}
          </fieldset>
        )}

        {clip.scene && (
          <fieldset>
            <legend>Scene params</legend>
            {scene?.description && <p className="muted small">{scene.description}</p>}
            {Object.keys(params).length === 0 && <p className="muted small">This scene declares no params (add <code>[params]</code> to <code>scenes/{clip.scene}/scene.toml</code>).</p>}
            {Object.entries(params).map(([k, spec]) => {
              const v = paramValue(k, spec);
              const commit = (nv: unknown) => void set({ params: { [k]: nv } });
              return (
                <label key={k} title={spec.description}>
                  <span className="lbl">{k}</span>
                  {spec.type === 'number' || spec.type === 'integer'
                    ? <NumField label={k} value={Number(v ?? 0)} min={spec.min} max={spec.max} step={spec.type === 'integer' ? 1 : 0.01} onCommit={commit} />
                    : spec.type === 'color'
                      ? <input type="color" aria-label={k} value={String(v ?? '#000000').slice(0, 7)} onChange={e => commit(e.target.value)} />
                      : spec.type === 'bool' || spec.type === 'boolean'
                        ? <input type="checkbox" aria-label={k} checked={!!v} onChange={e => commit(e.target.checked)} />
                        : <TextField label={k} value={String(v ?? '')} onCommit={commit} multiline={String(v ?? '').length > 40} />}
                  {clip.params?.[k] !== undefined && <button className="reset" title="Back to default" onClick={() => commit(null)}>↺</button>}
                </label>
              );
            })}
            <div className="insp-row">
              <button onClick={() => void revealPath(`${project}/scenes/${clip.scene}`)}>Show scene files</button>
            </div>
          </fieldset>
        )}

        {clip.component === 'text' && (
          <fieldset>
            <legend>Text</legend>
            <label><span className="lbl">Text</span><TextField label="Text" value={String(clip.props?.text ?? '')} multiline onCommit={v => void set({ props: { text: v } })} /></label>
            <label><span className="lbl">Size</span><NumField label="Font size" value={Number(clip.props?.fontSize ?? 96)} min={1} onCommit={v => void set({ props: { fontSize: v } })} /></label>
            <label><span className="lbl">Weight</span><NumField label="Font weight" value={Number(clip.props?.fontWeight ?? 700)} step={100} min={100} max={900} onCommit={v => void set({ props: { fontWeight: v } })} /></label>
            <label><span className="lbl">Colour</span><input type="color" aria-label="Text colour" value={String(clip.props?.fill ?? '#ffffff').slice(0, 7)} onChange={e => void set({ props: { fill: e.target.value } })} /></label>
            <label><span className="lbl">Font</span><TextField label="Font family" value={String(clip.props?.fontFamily ?? 'sans-serif')} onCommit={v => void set({ props: { fontFamily: v } })} /></label>
          </fieldset>
        )}
        {clip.component === 'solid' && (
          <fieldset>
            <legend>Solid</legend>
            <label><span className="lbl">Colour</span><input type="color" aria-label="Fill" value={String(clip.props?.fill ?? '#000000').slice(0, 7)} onChange={e => void set({ props: { fill: e.target.value } })} /></label>
            <label><span className="lbl">Radius</span><NumField label="Radius" value={Number(clip.props?.radius ?? 0)} min={0} onCommit={v => void set({ props: { radius: v } })} /></label>
          </fieldset>
        )}
      </div>
    </section>
  );
}
