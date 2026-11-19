// Bridge to the Tauri backend (src-tauri/src/main.rs), which talks to the core.
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export interface Actor { type: 'human' | 'agent'; name: string; session?: string }
export interface OpRecord { id: string; actor: Actor; ts: string; doc: string; type: string; args: unknown; derived?: boolean }
export interface Asset {
  id: string; kind: string; name?: string; ext?: string; mime?: string;
  width?: number; height?: number; duration_s?: number; tags: string[]; deprecated: boolean;
  source: string; recipe_id?: string; parent_id?: string; created_by?: string; created_at?: string;
  frames?: string[]; fps_num?: number | null; fps_den?: number | null;
}
export interface PendingOp { id: string; actor: Actor; op: { type: string; args: unknown }; reason: string; state: string; expires_at: number }
export interface Presence { actor: Actor; playhead?: number; selection: string[]; updated_at: number }
export interface Peaks { sample_rate: number; buckets: number; peaks: number[] }

export class CoreError extends Error {}

export async function call<T = any>(method: string, params: Record<string, unknown> = {}): Promise<T> {
  try {
    return await invoke<T>('core_call', { method, params });
  } catch (e) {
    throw new CoreError(String(e));
  }
}

let assetBase = '';
/** Must run once before any assetUrl() call (App does this at startup). */
export async function initAssets() {
  assetBase = await invoke<string>('asset_base');
}

/** Loopback HTTP URL for a library asset (served by the backend with Range support). */
export function assetUrl(id: string, ext?: string | null): string {
  return `${assetBase}/${ext ? `${id}.${ext}` : id}`;
}

/** 10-frame strip (video) or the image itself. Generated lazily by the core. */
export const thumbUrl = (id: string) => `${assetBase}/thumbs/${id}`;
/** Waveform peaks JSON for audio/video assets. */
export const peaksUrl = (id: string) => `${assetBase}/peaks/${id}`;
/** Prefix for frame-sequence images: `${seqBase(id)}${frame}`. */
export const seqBase = (id: string) => `${assetBase}/seq/${id}/`;

export type DocEvent =
  | { project: string; kind: 'state'; event: { project: string; state: string; board_state?: string; presence?: Presence[]; pending?: PendingOp[] } }
  | { project: string; kind: 'update'; event: { doc: 'timeline' | 'board'; update: string; origin: string | null } }
  | { project: string; kind: 'op'; event: { record: OpRecord } }
  | { project: string; kind: 'presence'; event: { presence: Presence[] } }
  | { project: string; kind: 'pending'; event: { pending: PendingOp[] } }
  | { project: string; kind: 'error'; event: { error: string } }
  | { project: string; kind: 'closed' };

export async function subscribeDoc(project: string, on: (e: DocEvent) => void): Promise<UnlistenFn> {
  const un = await listen<DocEvent>('kurn-doc', e => {
    if (e.payload.project === project) on(e.payload);
  });
  await invoke('doc_subscribe', { project });
  return un;
}

export const b64ToBytes = (b64: string) => Uint8Array.from(atob(b64), c => c.charCodeAt(0));

export const log = (line: string) => invoke('log_line', { line }).catch(() => {});
export const savePng = (path: string, dataUrl: string) => invoke('save_png', { path, dataUrl });
export const revealPath = (path: string) => invoke('reveal_path', { path });

export interface FishStatus {
  configured: boolean;
  hint?: string;
  voice: string;
  model: string;
  speed: number;
}
export const fishStatus = () => invoke<FishStatus>('fish_status_cmd');
export const fishSave = (p: { apiKey?: string; voice?: string; model?: string; speed?: number }) =>
  invoke<FishStatus>('fish_save', p);
export const fishClear = () => invoke<FishStatus>('fish_clear');
export const onJob = (cb: (e: { call: number; method: string; event: { type: string; pct?: number; msg?: string } }) => void) =>
  listen<{ call: number; method: string; event: { type: string; pct?: number; msg?: string } }>('kurn-job', e => cb(e.payload));
export const onMenu = (cb: (id: string) => void) =>
  listen<string>('kurn-menu', e => cb(e.payload));
export const initialProject = () => invoke<string | null>('initial_project');
export const rememberProject = (project: string) => invoke('remember_project', { project });
export const which = (name: string) => invoke<string | null>('which', { name });

// ---- terminal panes ----
export const ptySpawn = (cmd: string[], cwd: string | undefined, env: Record<string, string>, cols: number, rows: number) =>
  invoke<number>('pty_spawn', { cmd, cwd, env, cols, rows });
export const ptyWrite = (id: number, data: string) => invoke('pty_write', { id, data });
export const ptyResize = (id: number, cols: number, rows: number) => invoke('pty_resize', { id, cols, rows });
export const ptyKill = (id: number) => invoke('pty_kill', { id });
export const onPtyData = (cb: (id: number, bytes: Uint8Array) => void) =>
  listen<{ id: number; data: string }>('pty-data', e => cb(e.payload.id, b64ToBytes(e.payload.data)));
export const onPtyExit = (cb: (id: number) => void) => listen<{ id: number }>('pty-exit', e => cb(e.payload.id));

/** Stable color per actor key, for attribution in the feed, timeline and presence. */
export function actorColor(a: Actor | string): string {
  const key = typeof a === 'string' ? a : `${a.type}:${a.name}`;
  if (key.startsWith('human:')) return 'var(--human)';
  return `oklch(0.72 0.14 ${actorHue(key)})`;
}

function actorHue(key: string): number {
  let h = 0;
  for (const c of key) h = (h * 31 + c.charCodeAt(0)) % 360;
  return h;
}

/** Canvas-safe variant: resolves CSS variables and uses HSL for WebKit. */
export function actorCanvasColor(a: Actor | string): string {
  const key = typeof a === 'string' ? a : `${a.type}:${a.name}`;
  if (key.startsWith('human:')) return '#e9b45c';
  return `hsl(${actorHue(key)} 65% 62%)`;
}
