/** @jsxImportSource react */
// Kurnace app shell (spec §15).
//   Library │ Preview │ Agents + Activity
//   Timeline
//   Status
import { useEffect, useRef, useState } from 'react';
import { createRoot } from 'react-dom/client';
import { Agents } from './Agents';
import { Board } from './Board';
import { Dashboard } from './Dashboard';
import { call, initAssets, initialProject, log, onMenu, rememberProject, savePng } from './kurn';
import { Library } from './Library';
import { Activity, PendingDialog, StatusBar } from './Panels';
import { Preview, useAssets, type PreviewHandle } from './Preview';
import { RenderBar } from './RenderBar';
import { Settings } from './Settings';
import { Timeline } from './Timeline';
import { useProject } from './useProject';
import './styles.css';

const params = new URLSearchParams(location.search);
const sleep = (ms: number) => new Promise(r => setTimeout(r, ms));

// Test runs: forward console warnings/errors (Revideo reports media problems there).
if (params.has('parity') || params.has('selftest')) {
  for (const level of ['warn', 'error'] as const) {
    const orig = console[level].bind(console);
    console[level] = (...a: unknown[]) => { orig(...a); log(`console.${level}: ${a.map(x => (x instanceof Error ? x.message : typeof x === 'object' ? JSON.stringify(x) : String(x))).join(' ').slice(0, 400)}`); };
  }
}

window.addEventListener('error', e => log(`error: ${e.message} @ ${e.filename}:${e.lineno}\n${e.error?.stack ?? ''}`));
window.addEventListener('unhandledrejection', e => log(`unhandled: ${String(e.reason)}`));

function ProjectPicker({ onOpen }: { onOpen: (p: string) => void }) {
  const [path, setPath] = useState('');
  const [err, setErr] = useState<string | null>(null);
  const open = async (create: boolean) => {
    setErr(null);
    try {
      const r = await call<{ project: string }>(create ? 'project.new' : 'project.open', create ? { dir: path } : { project: path });
      onOpen(r.project);
    } catch (e) {
      setErr(String(e));
    }
  };
  return (
    <div className="picker">
      <h1>Kurnace</h1>
      <p className="muted">Open a project folder (one with <code>project.toml</code>), or create a new one.</p>
      <input value={path} onChange={e => setPath(e.target.value)} placeholder="/Users/you/Kurnace/projects/my-video" aria-label="Project path" autoFocus />
      <div className="row">
        <button onClick={() => open(false)} disabled={!path}>Open</button>
        <button onClick={() => open(true)} disabled={!path}>Create</button>
      </div>
      {err && <div className="note err">{err}</div>}
    </div>
  );
}

function Workspace({ project, onProjects, onSettings }: { project: string; onProjects: () => void; onSettings: () => void }) {
  const p = useProject(project);
  const [time, setTime] = useState(0);
  const [view, setView] = useState<'preview' | 'board'>('preview');
  const [filter, setFilter] = useState('all');
  const preview = useRef<PreviewHandle | null>(null);
  const tl = p.timeline;
  const assets = useAssets(tl);
  const fps = tl ? tl.fps.num / tl.fps.den : 30;
  const frame = Math.round(time * fps);
  const seek = (f: number) => {
    setTime(f / fps);
    preview.current?.seekFast(f / fps);
  };

  // ---- test hooks (scripts/parity.sh, scripts/phase2-live.sh) ----
  const capture = async (f: number, name: string) => {
    const ok = await preview.current?.seekAndSettle(f);
    if (!ok) log(`capture: player never settled on frame ${f}`);
    // Media frames (video seeks) can land a moment after the playhead does.
    await sleep(Number(params.get('capwait') ?? 1000));
    const c = preview.current?.canvas();
    if (!c) return log(`capture: no canvas for frame ${f}`);
    const file = `${project}/renders/parity/${name}-f${f}.png`;
    await savePng(file, c.toDataURL('image/png'));
    log(`CAPTURED ${file}`);
  };
  const ready = p.status === 'live' && !!tl;
  useEffect(() => {
    const frames = params.get('parity')?.split(',').map(Number);
    if (!ready || !frames) return;
    (async () => {
      for (const f of frames) await capture(f, 'webview');
      log('PARITY_DONE');
    })();
  }, [ready]);
  useEffect(() => {
    if (!params.has('selftest') || !tl) return;
    const lc = p.lastChange;
    log(`TIMELINE rev=${p.rev} clips=${Object.keys(tl.clips).length} origin=${lc?.origin ?? '-'} changed=${lc?.clips.join(',') ?? ''}`);
    const f = Number(params.get('capture') ?? NaN);
    if (lc?.origin?.startsWith('agent:') && Number.isFinite(f)) capture(f, 'live');
  }, [p.rev]);

  return (
    <div className="app">
      <Library refreshKey={p.ops.length} />
      <main className="center">
        <div className="center-tabs" role="tablist" aria-label="View">
          <button className="projects-tab" onClick={onProjects} title="Back to projects">← Projects</button>
          <button role="tab" aria-selected={view === 'preview'} className={view === 'preview' ? 'on' : ''} onClick={() => setView('preview')}>Preview</button>
          <button role="tab" aria-selected={view === 'board'} className={view === 'board' ? 'on' : ''} onClick={() => setView('board')}>Board</button>
          <span className="spacer" />
          <button className="projects-tab" onClick={onSettings} title="Settings (⌘,)">Settings</button>
        </div>
        <div className={`center-pane${view === 'preview' ? ' on' : ''}`}>
          {tl ? (
            <>
              <RenderBar project={project} tl={tl} frame={frame} />
              <Preview timeline={tl} assets={assets} onTime={setTime} handle={preview} />
            </>
          ) : (
            <div className="empty">{p.status === 'error' ? `Couldn't load project: ${p.error}` : 'Connecting to core…'}</div>
          )}
        </div>
        <div className={`center-pane${view === 'board' ? ' on' : ''}`}>
          {p.boardDoc && p.boardRev > 0 ? (
            <Board project={project} boardDoc={p.boardDoc} assets={assets} />
          ) : (
            <div className="empty">{p.status === 'error' ? `Couldn't load board: ${p.error}` : 'Connecting to core…'}</div>
          )}
        </div>
      </main>
      <aside className="right">
        <Agents project={project} />
        <Activity ops={p.ops} filter={filter} setFilter={setFilter} />
      </aside>
      {tl && (
        <Timeline project={project} tl={tl} frame={frame} assets={assets} presence={p.presence} lastChange={p.lastChange}
          onSeek={seek} onSeekEnd={f => preview.current?.seek(f / fps)} />
      )}
      <StatusBar project={project} status={p.status} pending={p.pending} />
      <PendingDialog project={project} pending={p.pending} onDecided={p.refreshPending} />
    </div>
  );
}

function App() {
  const [project, setProject] = useState<string | null | undefined>(undefined);
  const [newSignal, setNewSignal] = useState(0);
  const [settingsOpen, setSettingsOpen] = useState(false);
  useEffect(() => { initAssets().then(initialProject).then(p => setProject(p ?? null)); }, []);
  useEffect(() => {
    let un: (() => void) | undefined;
    onMenu(id => {
      if (id === 'projects') setProject(null);
      if (id === 'new-project') { setProject(null); setNewSignal(s => s + 1); }
      if (id === 'settings') setSettingsOpen(true);
    }).then(u => (un = u)).catch(() => {});
    return () => un?.();
  }, []);
  useEffect(() => {
    if (project) {
      rememberProject(project);
      document.title = `Kurnace — ${project.split('/').pop()}`;
    } else {
      document.title = 'Kurnace';
    }
  }, [project]);
  if (project === undefined) return null;
  return (
    <>
      {project
        ? <Workspace key={project} project={project} onProjects={() => setProject(null)} onSettings={() => setSettingsOpen(true)} />
        : <Dashboard onOpen={setProject} onSettings={() => setSettingsOpen(true)} newSignal={newSignal} />}
      <Settings open={settingsOpen} onClose={() => setSettingsOpen(false)} />
    </>
  );
}

createRoot(document.getElementById('root')!).render(<App />);
