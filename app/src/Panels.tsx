/** @jsxImportSource react */
// Activity feed, pending-confirmation dialog and status bar (spec §15).
import { useEffect, useState } from 'react';
import { type OpRecord, type PendingOp, actorColor, call } from './kurn';

function summarize(r: OpRecord): string {
  const a = r.args as Record<string, unknown>;
  if (r.type === 'batch') return `batch of ${(a?.ops as unknown[] | undefined)?.length ?? 0} ops`;
  const subject = a?.clip ?? a?.track ?? a?.marker ?? a?.transition ?? a?.element ?? a?.frame ?? '';
  return `${r.type}${subject ? ` ${subject}` : ''}${r.derived ? ' · derived' : ''}`;
}

export function Activity({ ops, filter, setFilter }: { ops: OpRecord[]; filter: string; setFilter: (f: string) => void }) {
  const actors = [...new Set(ops.map(o => `${o.actor.type}:${o.actor.name}`))];
  const shown = filter === 'all' ? ops : ops.filter(o => `${o.actor.type}:${o.actor.name}` === filter);
  return (
    <section className="panel activity" aria-label="Activity">
      <header>
        <h2>Activity</h2>
        <select value={filter} onChange={e => setFilter(e.target.value)} aria-label="Filter by actor">
          <option value="all">everyone</option>
          {actors.map(a => <option key={a} value={a}>{a}</option>)}
        </select>
      </header>
      <ol className="feed">
        {shown.map(o => (
          <li key={o.id} title={JSON.stringify(o.args, null, 2)}>
            <span className="who" style={{ color: actorColor(o.actor) }}>{o.actor.name}</span>
            <span className="what">{summarize(o)}</span>
            <time className="muted">{o.ts.slice(11, 19)}</time>
          </li>
        ))}
        {shown.length === 0 && <li className="muted">No ops yet.</li>}
      </ol>
    </section>
  );
}

export function StatusBar({ project, status, pending }: {
  project: string; status: string; pending: PendingOp[];
}) {
  const [core, setCore] = useState<{ pid: number; uptime_s: number; library_warning?: string } | null>(null);
  const [budget, setBudget] = useState<{ spent_usd: number; limit_usd?: number | null; used_pct: number; warning: boolean; over: boolean } | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    const tick = () => {
      call('status').then(setCore).catch(e => setErr(String(e)));
      call('project.budget', { project }).then(setBudget).catch(() => {});
    };
    tick();
    const t = setInterval(tick, 10_000);
    return () => clearInterval(t);
  }, [project]);
  return (
    <footer className="status">
      <span className={`sync s-${status}`}>● {status}</span>
      <span className="muted" title={project}>{project.split('/').pop()}</span>
      {core && <span className="muted">core pid {core.pid}</span>}
      {budget?.limit_usd != null && (
        <span className={budget.over ? 'warn' : budget.warning ? 'warn' : 'muted'} title={`${budget.used_pct.toFixed(0)}% of API budget`}>
          ${budget.spent_usd.toFixed(2)} / ${budget.limit_usd.toFixed(2)}
        </span>
      )}
      {core?.library_warning && <span className="warn">{core.library_warning}</span>}
      {err && <span className="warn">{err}</span>}
      <span className="spacer" />
      {pending.length > 0 && <span className="pending" role="alert">{pending.length} pending approval{pending.length === 1 ? '' : 's'}</span>}
    </footer>
  );
}

/** Destructive ops from agents wait here until a human decides (spec §6.3, §15). */
export function PendingDialog({ project, pending, onDecided }: {
  project: string; pending: PendingOp[]; onDecided: () => void;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [now, setNow] = useState(Date.now() / 1000);
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now() / 1000), 1000);
    return () => clearInterval(t);
  }, []);
  if (pending.length === 0) return null;
  const decide = async (id: string, approve: boolean) => {
    const p = pending.find(x => x.id === id);
    setBusy(id);
    setErr(null);
    try {
      await call(p?.op.type.startsWith('module.') ? 'run.decide' : 'op.decide', { project, op: id, approve });
      onDecided();
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(null);
    }
  };
  return (
    <div className="pending-overlay" role="presentation">
      <div className="pending-dialog" role="dialog" aria-modal="true" aria-label="Pending confirmations">
        <header>
          <h2>Needs your approval</h2>
          <span className="muted">agents cannot apply these without you</span>
        </header>
        {err && <div className="note err">{err}</div>}
        <ul>
          {pending.map(p => (
            <li key={p.id}>
              <div className="pending-head">
                <b style={{ color: actorColor(p.actor) }}>{p.actor.name}</b>
                <code>{p.op.type}</code>
                <span className="muted">{Math.max(0, Math.round(p.expires_at - now))}s left</span>
              </div>
              <div className="pending-reason">{p.reason}</div>
              <pre>{JSON.stringify(p.op.args, null, 2)}</pre>
              <div className="pending-actions">
                <button className="approve" disabled={busy === p.id} onClick={() => void decide(p.id, true)}>Approve</button>
                <button className="reject" disabled={busy === p.id} onClick={() => void decide(p.id, false)}>Reject</button>
              </div>
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
