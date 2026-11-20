// Optimistic edits (docs/v2-plan.md §3.2).
//
// run(op) applies the op to a local overlay immediately, so the timeline and
// the preview show the edit on the next paint, then sends it to the core. An
// overlay entry is dropped once the core has acknowledged it AND a newer
// timeline has arrived from the core (the core's version now includes it).
// Rejected ops roll back and report why.
import type { Timeline } from '@kurnace/timeline';
import { type Op, applyOp } from '@kurnace/timeline/apply';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { call } from './kurn';

interface Entry { id: number; op: Op; acked: boolean; sentRev: number }
export interface OpResult { status?: string; reason?: string; op?: string; result?: unknown }

let seq = 0;

export function useOptimistic(project: string, tl: Timeline | null, onError: (msg: string) => void) {
  const [entries, setEntries] = useState<Entry[]>([]);
  const rev = useRef(0);
  const settle = (list: Entry[]) => list.filter(e => !(e.acked && rev.current > e.sentRev));

  useEffect(() => {
    rev.current += 1;
    setEntries(settle);
  }, [tl]);

  const view = useMemo(() => (tl ? entries.reduce((t, e) => applyOp(t, e.op), tl) : null), [tl, entries]);

  const run = useCallback(async (op: Op): Promise<OpResult | null> => {
    const id = ++seq;
    setEntries(list => [...list, { id, op, acked: false, sentRev: rev.current }]);
    const drop = () => setEntries(list => list.filter(e => e.id !== id));
    try {
      const r = await call<OpResult>('op.apply', { project, op });
      if (r?.status === 'pending') {
        // Destructive op waiting for a human: don't pretend it happened.
        drop();
        onError(r.reason ?? 'Waiting for approval');
        return r;
      }
      setEntries(list => settle(list.map(e => (e.id === id ? { ...e, acked: true } : e))));
      // Ops that change nothing never produce an update; don't keep them forever.
      setTimeout(drop, 2000);
      return r;
    } catch (e) {
      drop();
      onError(String(e).replace(/^(Error: )+/, '').replace(/^validation failed: /, "Can't do that: "));
      return null;
    }
  }, [project, onError]);

  return { view, run, pending: entries.length };
}
