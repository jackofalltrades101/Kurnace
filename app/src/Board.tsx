/** @jsxImportSource react */
// Board panel (spec §13): Excalidraw bound to the core's board doc through
// y-excalidraw. Local edits go straight into the replica and are relayed to
// the core as derived ops (§6.1); programmatic edits arrive as doc updates.
import { Excalidraw } from '@excalidraw/excalidraw';
import type { ExcalidrawImperativeAPI } from '@excalidraw/excalidraw/types';
import '@excalidraw/excalidraw/index.css';
import { ExcalidrawBinding, yjsToExcalidraw } from 'y-excalidraw';
import { useEffect, useMemo, useRef, useState } from 'react';
import * as Y from 'yjs';
import { call } from './kurn';
import type { ResolvedAsset } from './Preview';

export interface BoardProps {
  project: string;
  boardDoc: Y.Doc;
  assets: Record<string, ResolvedAsset>;
}

const enc = (bytes: Uint8Array) => {
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
};

/** y-excalidraw 2.0.12 treats `awareness` as optional but reads it
 *  unconditionally in its element observer. An empty stub is enough; the
 *  core's presence channel already provides agent cursors. */
const noAwareness = {
  clientID: 0,
  getStates: () => new Map(),
  setLocalStateField: () => {},
  on: () => {},
  off: () => {},
} as any;

export function Board({ project, boardDoc, assets }: BoardProps) {
  const yElements = useMemo(() => boardDoc.getArray<Y.Map<any>>('elements'), [boardDoc]);
  const yAssets = useMemo(() => boardDoc.getMap<any>('assets'), [boardDoc]);
  const [api, setApi] = useState<ExcalidrawImperativeAPI | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const host = useRef<HTMLDivElement>(null);
  const binding = useRef<ExcalidrawBinding | null>(null);
  const [initial] = useState(() => ({
    elements: yjsToExcalidraw(yElements),
    appState: { viewBackgroundColor: '#111214', theme: 'dark' as const },
  }));

  // y-excalidraw handles element/file diffs, remote scene updates and ordering.
  useEffect(() => {
    if (!api) return;
    const b = new ExcalidrawBinding(yElements, yAssets, api, noAwareness);
    binding.current = b;
    // initialData appState is not always applied before the first scene update.
    api.updateScene({ appState: { theme: 'dark', viewBackgroundColor: '#111214' } });
    return () => {
      binding.current = null;
      b.destroy();
    };
  }, [api, yElements, yAssets]);

  // Relay local replica edits to the core. Excalidraw fires onChange per
  // pointer move, so coalesce Yjs updates before crossing IPC.
  useEffect(() => {
    let pending: Uint8Array[] = [];
    let timer: ReturnType<typeof setTimeout> | undefined;
    const flush = () => {
      timer = undefined;
      if (pending.length === 0) return;
      const update = Y.mergeUpdates(pending);
      pending = [];
      call('board.derived', {
        project,
        update: enc(update),
        op: { type: 'el.update', args: { derived: true } },
      }).catch(e => setErr(String(e)));
    };
    const onUpdate = (update: Uint8Array, origin: unknown) => {
      // useProject applies core updates with the 'core' origin.
      if (origin === 'core' || origin === 'remote') return;
      pending.push(update);
      clearTimeout(timer);
      timer = setTimeout(flush, 120);
    };
    boardDoc.on('update', onUpdate);
    return () => {
      boardDoc.off('update', onUpdate);
      if (timer) clearTimeout(timer);
      flush();
    };
  }, [boardDoc, project]);

  const apply = async (op: unknown, label: string) => {
    setBusy(label);
    setErr(null);
    try {
      const r = await call<{ status?: string; reason?: string }>('board.apply', { project, op });
      if (r.status === 'pending') setErr(r.reason ?? 'waiting for approval');
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(null);
    }
  };

  const addFrame = () => {
    const n = yElements.length + 1;
    void apply(
      { type: 'frame.add', args: { name: `Scene ${n}`, x: 80 + n * 30, y: 80, width: 420, height: 260 } },
      'frame',
    );
  };

  const toTimeline = async () => {
    setBusy('rough cut');
    setErr(null);
    try {
      const r = await call<{ details?: { scenes?: number; clips?: number } }>('board.to-timeline', { project });
      setErr(`rough cut: ${r.details?.scenes ?? 0} scenes, ${r.details?.clips ?? 0} clips`);
    } catch (e) {
      setErr(String(e));
    } finally {
      setBusy(null);
    }
  };

  const onDrop = async (e: React.DragEvent) => {
    e.preventDefault();
    const id = e.dataTransfer.getData('text/plain');
    if (!id || !api) return;
    const rect = host.current?.getBoundingClientRect();
    const st = api.getAppState() as any;
    const zoom = st?.zoom?.value ?? 1;
    const x = ((e.clientX - (rect?.left ?? 0)) / zoom) - (st?.scrollX ?? 0);
    const y = ((e.clientY - (rect?.top ?? 0)) / zoom) - (st?.scrollY ?? 0);
    const name = assets[id]?.name ?? id;
    const a = assets[id];
    if (a?.kind === 'image') {
      try {
        const blob = await fetch(a.url).then(r => r.blob());
        const dataURL = await new Promise<string>((res, rej) => {
          const fr = new FileReader();
          fr.onload = () => res(String(fr.result));
          fr.onerror = () => rej(fr.error);
          fr.readAsDataURL(blob);
        });
        const file = {
          id, dataURL, mimeType: blob.type || 'image/png',
          created: Date.now(), lastRetrieved: Date.now(),
        };
        // Keep the binary with the board doc too; Excalidraw's own file store
        // is local to the webview and would lose it on reload.
        yAssets.set(id, file);
        api.addFiles([file as any]);
        const w = a.width ?? 320;
        const h = a.height ?? 240;
        const s = Math.min(260 / w, 180 / h, 1);
        const el = {
          id: `img_${Date.now().toString(36)}`,
          type: 'image', x, y, width: w * s, height: h * s, angle: 0,
          strokeColor: 'transparent', backgroundColor: 'transparent',
          fillStyle: 'solid', strokeWidth: 2, strokeStyle: 'solid',
          roughness: 1, opacity: 100, groupIds: [], frameId: null,
          roundness: null, seed: Math.floor(Math.random() * 2 ** 31),
          version: 1, versionNonce: Math.floor(Math.random() * 2 ** 31),
          updated: Date.now(), isDeleted: false, boundElements: null,
          link: null, locked: false, index: null,
          fileId: id, status: 'saved', scale: [1, 1], crop: null,
          customData: { asset: id },
        };
        api.updateScene({ elements: [...api.getSceneElements(), el as any] });
        return;
      } catch {
        // fall through to a labeled card
      }
    }
    await apply(
      {
        type: 'el.add',
        args: {
          element: {
            type: 'rectangle', x, y, width: 220, height: 130,
            strokeColor: '#7aa2ff', backgroundColor: '#1d1f23',
            label: { text: name }, customData: { asset: id },
          },
        },
      },
      'asset',
    );
  };

  return (
    <section className="panel board" aria-label="Board">
      <div className="board-bar">
        <span className="muted">Board</span>
        <span className="muted">{yElements.length} elements</span>
        <span className="spacer" />
        {busy && <span className="muted">{busy}…</span>}
        <button onClick={addFrame}>+ Frame</button>
        <button onClick={toTimeline} title="Build a rough cut from frames">To timeline</button>
      </div>
      {err && <div className={`note ${err.startsWith('rough cut') ? '' : 'err'}`}>{err}</div>}
      <div
        className="board-host"
        ref={host}
        onDragOver={e => { e.preventDefault(); e.dataTransfer.dropEffect = 'copy'; }}
        onDrop={onDrop}
      >
        <Excalidraw
          excalidrawAPI={api => { (window as any).__excalidrawApi = api; setApi(api); }}
          initialData={initial}
          theme="dark"
          name="Kurnace board"
          UIOptions={{
            canvasActions: {
              loadScene: false,
              saveToActiveFile: false,
              saveAsImage: false,
              export: false,
            },
          }}
        />
      </div>
    </section>
  );
}
