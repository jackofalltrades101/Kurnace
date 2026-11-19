/** @jsxImportSource react */
// Project dashboard: recent projects, open an existing folder, create a new one.
import { useEffect, useMemo, useState } from 'react';
import { call, revealPath } from './kurn';

interface Summary {
  path: string;
  name: string;
  fps: string;
  width: number;
  height: number;
  modified_at: number;
  recent_at: number;
}

const FPS = ['24/1', '25/1', '30/1', '60/1'];
const SIZES: Array<[string, number, number]> = [
  ['1080p · 1920×1080', 1920, 1080],
  ['Vertical · 1080×1920', 1080, 1920],
  ['Square · 1080×1080', 1080, 1080],
  ['720p · 1280×720', 1280, 720],
];

const slugify = (s: string) => s.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '') || 'untitled';
const fmt = (secs: number) => {
  if (!secs) return '';
  const d = new Date(secs * 1000);
  const days = Math.floor((Date.now() - d.getTime()) / 86_400_000);
  if (days <= 0) return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' });
  if (days === 1) return 'yesterday';
  if (days < 7) return `${days} days ago`;
  return d.toLocaleDateString();
};

export function Dashboard({ onOpen, onSettings, newSignal = 0 }: {
  onOpen: (project: string) => void;
  onSettings: () => void;
  newSignal?: number;
}) {
  const [root, setRoot] = useState('');
  const [projects, setProjects] = useState<Summary[]>([]);
  const [loading, setLoading] = useState(true);
  const [err, setErr] = useState<string | null>(null);
  const [newOpen, setNewOpen] = useState(false);
  const [name, setName] = useState('');
  const [dir, setDir] = useState('');
  const [dirTouched, setDirTouched] = useState(false);
  const [fps, setFps] = useState('30/1');
  const [size, setSize] = useState(0);
  const [openPath, setOpenPath] = useState('');
  const [busy, setBusy] = useState<string | null>(null);

  const load = async () => {
    setLoading(true);
    setErr(null);
    try {
      const r = await call<{ root: string; projects: Summary[] }>('project.list');
      setRoot(r.root);
      setProjects(r.projects);
      if (!dirTouched && !dir) setDir(`${r.root}/${slugify(name || 'my-project')}`);
    } catch (e) {
      setErr(String(e).replace(/^Error: /, ''));
    } finally {
      setLoading(false);
    }
  };
  useEffect(() => { void load(); }, []);
  useEffect(() => { if (newSignal > 0) setNewOpen(true); }, [newSignal]);
  useEffect(() => {
    if (!dirTouched) setDir(`${root}/${slugify(name || 'my-project')}`);
  }, [name, root, dirTouched]);

  const open = async (path: string) => {
    setBusy(path);
    setErr(null);
    try {
      const r = await call<{ project: string }>('project.open', { project: path });
      onOpen(r.project);
    } catch (e) {
      setErr(String(e).replace(/^Error: /, ''));
    } finally {
      setBusy(null);
    }
  };

  const create = async () => {
    setBusy('create');
    setErr(null);
    try {
      const [label, width, height] = SIZES[size];
      const r = await call<{ project: string }>('project.new', { dir, name: name || label, fps, width, height, git: true });
      onOpen(r.project);
    } catch (e) {
      setErr(String(e).replace(/^Error: /, ''));
    } finally {
      setBusy(null);
    }
  };

  const total = useMemo(() => projects.length, [projects]);

  return (
    <div className="dash">
      <header className="dash-head">
        <div>
          <h1>Kurnace</h1>
          <p className="muted">Projects on this Mac · {total}</p>
        </div>
        <div className="row">
          <button onClick={onSettings} title="Settings (⌘,)">Settings</button>
          <button onClick={() => void load()} disabled={loading}>Refresh</button>
          <button className="primary" onClick={() => setNewOpen(v => !v)}>+ New project</button>
        </div>
      </header>

      {err && <div className="note err">{err}</div>}

      <section className="dash-open">
        <input value={openPath} onChange={e => setOpenPath(e.target.value)} placeholder="Open another project folder…" aria-label="Project path" />
        <button disabled={!openPath || !!busy} onClick={() => void open(openPath)}>Open</button>
      </section>

      {newOpen && (
        <section className="new-project">
          <h2>New project</h2>
          <div className="form-grid">
            <label>Name<input value={name} onChange={e => setName(e.target.value)} placeholder="My video" autoFocus /></label>
            <label>Folder<input value={dir} onChange={e => { setDir(e.target.value); setDirTouched(true); }} /></label>
            <label>Frame rate
              <select value={fps} onChange={e => setFps(e.target.value)}>
                {FPS.map(f => <option key={f} value={f}>{f.split('/')[0]} fps</option>)}
              </select>
            </label>
            <label>Resolution
              <select value={size} onChange={e => setSize(Number(e.target.value))}>
                {SIZES.map(([label], i) => <option key={label} value={i}>{label}</option>)}
              </select>
            </label>
          </div>
          <div className="row">
            <button className="primary" disabled={!dir || !!busy} onClick={() => void create()}>Create project</button>
            <button onClick={() => setNewOpen(false)}>Cancel</button>
          </div>
          <p className="note muted">Creates the folder, git repo, AGENTS.md/CLAUDE.md and project snapshots.</p>
        </section>
      )}

      {loading ? <div className="empty">Loading projects…</div> : total === 0 ? (
        <div className="empty">
          No projects yet. Create one, or open an existing folder containing <code>project.toml</code>.
        </div>
      ) : (
        <section className="projects">
          {projects.map(p => (
            <article className="project-card" key={p.path}>
              <div className="project-art">▤</div>
              <div className="project-body">
                <h3>{p.name}</h3>
                <div className="project-meta muted">
                  {p.width}×{p.height} · {p.fps} · {fmt(p.recent_at || p.modified_at)}
                </div>
                <div className="project-path muted" title={p.path}>{p.path.replace(/^\/Users\/[^/]+/, '~')}</div>
              </div>
              <div className="project-actions">
                <button className="primary" disabled={!!busy} onClick={() => void open(p.path)}>Open</button>
                <button onClick={() => revealPath(p.path).catch(e => setErr(String(e)))}>Reveal</button>
              </div>
            </article>
          ))}
        </section>
      )}
    </div>
  );
}
