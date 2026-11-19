/** @jsxImportSource react */
// Render/export controls. All work goes through the core, so UI, CLI and
// agents use the same renderer/exporter. Headless output is the truth (§10.4).
import type { Timeline } from '@kurnace/timeline';
import { useEffect, useRef, useState } from 'react';
import { call, onJob, revealPath } from './kurn';

const PRESETS = ['youtube', 'vertical', 'square', 'web'] as const;

export function RenderBar({ project, tl, frame }: { project: string; tl: Timeline; frame: number }) {
  const [busy, setBusy] = useState<string | null>(null);
  const [pct, setPct] = useState(0);
  const [out, setOut] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [preset, setPreset] = useState<(typeof PRESETS)[number]>('youtube');
  const busyRef = useRef(false);
  busyRef.current = !!busy;

  useEffect(() => {
    let un: (() => void) | undefined;
    onJob(e => {
      if (!busyRef.current) return;
      if (e.event.type === 'progress') setPct(e.event.pct ?? 0);
      if (e.event.type === 'log' && (e.event as { level?: string }).level === 'error') {
        setError(e.event.msg ?? 'render error');
      }
    }).then(u => (un = u));
    return () => un?.();
  }, []);

  const run = async (label: string, method: string, params: Record<string, unknown>) => {
    busyRef.current = true;
    setBusy(label);
    setPct(0);
    setError(null);
    setOut(null);
    try {
      const r = await call<{ out?: string; png?: string; outputs?: Record<string, string>; assets?: Record<string, { path?: string }> }>(method, params);
      const file = r.out ?? r.png ?? r.outputs?.file ?? r.assets?.file?.path;
      setOut(file ?? null);
      setPct(100);
    } catch (e) {
      setError(String(e).replace(/^Error: /, ''));
    } finally {
      busyRef.current = false;
      setBusy(null);
    }
  };

  const still = () => run('still', 'render', {
    project, mode: 'still', frame, out: `${project}/renders/f${frame}.png`,
  });
  const draft = () => run('draft', 'render', {
    project, mode: 'draft', range: `0-${Math.max(tl.duration, 1)}`, scale: 0.5,
    out: `${project}/renders/draft.mp4`,
  });
  const final = () => run('final', 'render', {
    project, mode: 'final', out: `${project}/renders/final.mp4`,
  });
  const exportPreset = () => run('export', 'run', {
    project, module: 'export.mp4',
    params: { input: 'renders/final.mp4', preset, out: `renders/export-${preset}.mp4` },
  });

  const rel = out
    ? out.includes('/renders/')
      ? `renders/${out.split('/renders/').pop()}`
      : out.replace(`${project}/`, '')
    : null;
  return (
    <div className="render-bar" aria-label="Render and export">
      <span className="muted">Render</span>
      <button disabled={!!busy} onClick={still} title="Render the current frame with headless Chromium">Still · f{frame}</button>
      <button disabled={!!busy} onClick={draft} title="Half-scale draft MP4">Draft</button>
      <button disabled={!!busy} onClick={final} title="Full-quality MP4">Final</button>
      <span className="tl-sep" />
      <select value={preset} disabled={!!busy} onChange={e => setPreset(e.target.value as typeof preset)} aria-label="Export preset">
        {PRESETS.map(p => <option key={p} value={p}>{p}</option>)}
      </select>
      <button disabled={!!busy} onClick={exportPreset} title="Package renders/final.mp4 for a destination">Export</button>
      <span className="spacer" />
      {busy && <span className="muted">{busy}… {pct > 0 ? `${Math.round(pct)}%` : ''}</span>}
      {error && <span className="warn render-err" title={error}>{error}</span>}
      {rel && (
        <span className="render-out">
          <button onClick={() => revealPath(out!).catch(e => setError(String(e)))} title={out!}>Reveal</button>
          <code>{rel}</code>
        </span>
      )}
      {busy && <div className="render-progress" style={{ width: `${pct}%` }} />}
    </div>
  );
}
