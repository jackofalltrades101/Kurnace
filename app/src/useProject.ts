// Live project state: a Yjs replica of the core's timeline doc, kept in sync
// over the backend's subscription (spec §4: UI ⇄ core via Tauri IPC).
import { type Timeline, timelineFromDocJSON } from '@kurnace/timeline';
import { useEffect, useRef, useState } from 'react';
import * as Y from 'yjs';
import { type OpRecord, type PendingOp, type Presence, b64ToBytes, call, subscribeDoc } from './kurn';

export interface Change {
  /** Actor key from the transaction origin, e.g. "agent:claude". */
  origin: string | null;
  clips: string[];
  at: number;
}

export interface ProjectState {
  status: 'connecting' | 'live' | 'closed' | 'error';
  error?: string;
  timeline: Timeline | null;
  ops: OpRecord[];
  pending: PendingOp[];
  presence: Presence[];
  lastChange: Change | null;
  /** Bumps on every doc update (for tests and memo keys). */
  rev: number;
}

const ORIGIN_REMOTE = 'core';

function changedClips(prev: Timeline | null, next: Timeline | null): string[] {
  if (!next) return [];
  const out: string[] = [];
  for (const [id, c] of Object.entries(next.clips)) {
    const p = prev?.clips[id];
    if (!p || JSON.stringify(p) !== JSON.stringify(c)) out.push(id);
  }
  return out;
}

export function useProject(project: string | null): ProjectState & {
  refreshPending: () => void;
  boardDoc: Y.Doc | null;
  boardRev: number;
} {
  const [st, setSt] = useState<ProjectState>({ status: 'connecting', timeline: null, ops: [], pending: [], presence: [], lastChange: null, rev: 0 });
  const docRef = useRef<Y.Doc | null>(null);
  const boardDocRef = useRef<Y.Doc | null>(null);
  const [boardRev, setBoardRev] = useState(0);
  // Bumped to re-subscribe after the core restarts (stream closed).
  const [generation, setGeneration] = useState(0);

  const refreshPending = () => {
    if (!project) return;
    call<PendingOp[]>('op.pending', { project })
      .then(p => setSt(s => ({ ...s, pending: p.filter(x => x.state === 'pending') })))
      .catch(() => {});
  };

  useEffect(() => {
    if (!project) return;
    let un: (() => void) | undefined;
    let cancelled = false;
    const doc = new Y.Doc();
    docRef.current = doc;
    const boardDoc = new Y.Doc();
    boardDocRef.current = boardDoc;
    const root = doc.getMap('timeline');
    let prev: Timeline | null = null;
    let pendingOrigin: string | null = null;

    const recompute = () => {
      const tl = timelineFromDocJSON(root.toJSON());
      const clips = changedClips(prev, tl);
      const origin = pendingOrigin;
      prev = tl;
      setSt(s => ({
        ...s,
        timeline: tl,
        rev: s.rev + 1,
        // Initial state has no origin. Transactions that touch no clip (meta,
        // markers) keep the last clip highlight.
        lastChange: clips.length && origin ? { origin, clips, at: Date.now() } : s.lastChange,
      }));
    };
    // One recompute per applied transaction (not per nested event).
    doc.on('afterTransaction', tr => {
      if (tr.origin === ORIGIN_REMOTE) recompute();
    });

    setSt({ status: 'connecting', timeline: null, ops: [], pending: [], presence: [], lastChange: null, rev: 0 });
    call<OpRecord[]>('log', { project, limit: 200 }).then(ops => !cancelled && setSt(s => ({ ...s, ops: [...ops].reverse() }))).catch(() => {});
    refreshPending();

    subscribeDoc(project, e => {
      switch (e.kind) {
        case 'state':
          pendingOrigin = null;
          Y.applyUpdate(doc, b64ToBytes(e.event.state), ORIGIN_REMOTE);
          if (e.event.board_state) {
            Y.applyUpdate(boardDoc, b64ToBytes(e.event.board_state), ORIGIN_REMOTE);
            setBoardRev(r => r + 1);
          }
          setSt(s => ({
            ...s, status: 'live',
            presence: e.event.presence ?? s.presence,
            pending: (e.event.pending ?? s.pending).filter(x => x.state === 'pending'),
          }));
          break;
        case 'update':
          if (e.event.doc === 'board') {
            Y.applyUpdate(boardDoc, b64ToBytes(e.event.update), ORIGIN_REMOTE);
            setBoardRev(r => r + 1);
          } else {
            pendingOrigin = e.event.origin;
            Y.applyUpdate(doc, b64ToBytes(e.event.update), ORIGIN_REMOTE);
          }
          break;
        case 'op':
          setSt(s => ({ ...s, ops: [e.event.record, ...s.ops].slice(0, 500) }));
          refreshPending();
          break;
        case 'presence':
          setSt(s => ({ ...s, presence: e.event.presence }));
          break;
        case 'pending':
          setSt(s => ({ ...s, pending: e.event.pending.filter(x => x.state === 'pending') }));
          break;
        case 'error':
          setSt(s => ({ ...s, status: 'error', error: JSON.stringify(e.event) }));
          break;
        case 'closed':
          setSt(s => ({ ...s, status: 'closed' }));
          // The core restarted or crashed; its state is durable, so resubscribe from scratch.
          if (!cancelled) setTimeout(() => !cancelled && setGeneration(g => g + 1), 1000);
          break;
      }
    }).then(u => { if (cancelled) u(); else un = u; })
      .catch(err => setSt(s => ({ ...s, status: 'error', error: String(err) })));

    return () => {
      cancelled = true;
      un?.();
      doc.destroy();
      boardDoc.destroy();
      boardDocRef.current = null;
    };
  }, [project, generation]);

  return { ...st, refreshPending, boardDoc: boardDocRef.current, boardRev };
}
