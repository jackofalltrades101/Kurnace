/** @jsxImportSource react */
// Agents panel (spec §14, §15): terminal panes for agent sessions.
// Each pane is a real PTY in the backend. Panes get KURN_ACTOR so every op
// they make through `kurn` is attributed (spec §6.2).
import { FitAddon } from '@xterm/addon-fit';
import { Terminal } from '@xterm/xterm';
import '@xterm/xterm/css/xterm.css';
import { useEffect, useRef, useState } from 'react';
import { actorColor, call, onPtyData, onPtyExit, ptyKill, ptyResize, ptySpawn, ptyWrite, which } from './kurn';

interface Launcher { key: string; label: string; cmd: string[]; actor: string }
interface Pane { id: number; label: string; actor: string; alive: boolean }

const CANDIDATES: Launcher[] = [
  { key: 'claude', label: 'Claude', cmd: ['claude'], actor: 'agent:claude' },
  { key: 'opencode', label: 'OpenCode', cmd: ['opencode'], actor: 'agent:deepseek' },
  { key: 'herdr', label: 'Herdr', cmd: ['herdr'], actor: 'agent:herdr' },
];

function Term({ pane, active, project }: { pane: Pane; active: boolean; project: string }) {
  const el = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const term = new Terminal({
      fontFamily: 'ui-monospace, "SF Mono", Menlo, monospace',
      fontSize: 12,
      cursorBlink: true,
      theme: { background: '#0e0f11', foreground: '#d8d9dc' },
      allowProposedApi: false,
    });
    const fit = new FitAddon();
    term.loadAddon(fit);
    term.open(el.current!);
    const unData = onPtyData((id, bytes) => { if (id === pane.id) term.write(bytes); });
    const sub = term.onData(d => ptyWrite(pane.id, d));
    const ro = new ResizeObserver(() => {
      try {
        fit.fit();
        ptyResize(pane.id, term.cols, term.rows);
      } catch { /* hidden pane */ }
    });
    ro.observe(el.current!);
    return () => { ro.disconnect(); sub.dispose(); unData.then(f => f()); term.dispose(); };
  }, [pane.id, project]);
  return <div className="term" ref={el} hidden={!active} />;
}

export function Agents({ project }: { project: string }) {
  const [launchers, setLaunchers] = useState<Launcher[]>([]);
  const [panes, setPanes] = useState<Pane[]>([]);
  const [active, setActive] = useState<number | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    Promise.all(CANDIDATES.map(async l => ((await which(l.cmd[0])) ? l : null)))
      .then(ls => setLaunchers([...ls.filter((l): l is Launcher => !!l),
        { key: 'shell', label: 'Shell', cmd: ['/bin/zsh', '-l'], actor: 'human:shell' }]));
    const un = onPtyExit(id => setPanes(ps => ps.map(p => (p.id === id ? { ...p, alive: false } : p))));
    return () => { un.then(f => f()); };
  }, []);

  const launch = async (l: Launcher) => {
    setErr(null);
    try {
      // Refresh AGENTS.md/CLAUDE.md immediately before the agent starts, so its
      // project memory is current without injecting a prompt.
      if (l.actor.startsWith('agent:')) await call('agents.md', { project });
      const id = await ptySpawn(l.cmd, project, { KURN_ACTOR: l.actor }, 100, 30);
      setPanes(ps => [...ps, { id, label: l.label, actor: l.actor, alive: true }]);
      setActive(id);
    } catch (e) {
      setErr(String(e));
    }
  };

  const close = (id: number) => {
    ptyKill(id);
    setPanes(ps => ps.filter(p => p.id !== id));
    setActive(a => (a === id ? null : a));
  };

  return (
    <section className="panel agents" aria-label="Agents">
      <header>
        <h2>Agents</h2>
        <div className="launch">
          {launchers.map(l => <button key={l.key} onClick={() => launch(l)}>+ {l.label}</button>)}
        </div>
      </header>
      {panes.length > 0 && (
        <div className="tabs" role="tablist">
          {panes.map(p => (
            <div key={p.id} role="tab" aria-selected={active === p.id} className={`tab${active === p.id ? ' on' : ''}`} onClick={() => setActive(p.id)}>
              <span className="dot" style={{ background: p.alive ? actorColor(p.actor) : 'var(--muted)' }} aria-label={p.alive ? 'running' : 'exited'} />
              {p.label}
              <button className="x" onClick={e => { e.stopPropagation(); close(p.id); }} aria-label={`Close ${p.label}`}>×</button>
            </div>
          ))}
        </div>
      )}
      {err && <div className="note err">{err}</div>}
      <div className="terms">
        {panes.map(p => <Term key={p.id} pane={p} active={active === p.id} project={project} />)}
        {panes.length === 0 && (
          <div className="empty muted">
            Start an agent session. It runs in this project with <code>KURN_ACTOR</code> set, so its edits show up attributed in the timeline and activity feed.
          </div>
        )}
      </div>
    </section>
  );
}
