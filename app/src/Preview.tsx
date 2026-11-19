/** @jsxImportSource react */
// Preview panel: the Revideo Player driven by the live timeline (spec §15).
// Agents never judge from here — headless `kurn render still` is the source of truth (§10.4).
import type { Timeline } from '@kurnace/timeline';
import { Vector2 } from '@revideo/core';
import { Player } from '@revideo/player-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import baseProject from '../../modules/render.revideo/src/project.ts';
import { type Asset, assetUrl, call, seqBase } from './kurn';

// @revideo/player-react 0.11 renders <revideo-player> with props like `variables`
// and `width`, but the element only defines getters for them (it reads its
// attributes). React 19 assigns custom-element props as properties, so every
// update after mount throws "Attempted to assign to readonly property" and the
// preview never updates. Add setters that write the observed attributes.
//
// `playing` and `looping` are plain instance fields instead: React writes the
// strings "false"/"true" into them, "false" is truthy, so the player autoplayed,
// and later writes (the Play button) never reached the element. Turn them into
// per-instance accessors that route React's strings through the element's own
// methods, while the element's internal boolean writes still just store.
function installBoolProp(el: any, key: string, apply: (el: any, v: boolean) => void) {
  const d = Object.getOwnPropertyDescriptor(el, key);
  if (d && 'get' in d) return;
  let value = d ? d.value === true || d.value === 'true' : false;
  Object.defineProperty(el, key, {
    configurable: true,
    get: () => value,
    set: (v: unknown) => {
      if (typeof v === 'boolean') { value = v; return; } // the element's own writes
      value = v === 'true';                               // React prop writes
      apply(el, value);
    },
  });
}

/** Patch one element instance (its class fields shadow prototype accessors). */
function patchPlayerElement(el: any) {
  installBoolProp(el, 'playing', (e, v) => e.setPlaying(v));
  installBoolProp(el, 'looping', (e, v) => e.player?.toggleLoop(v));
}

function patchRevideoPlayerElement() {
  const El = customElements.get('revideo-player');
  if (!El) return void customElements.whenDefined('revideo-player').then(patchRevideoPlayerElement);
  const connected = El.prototype.connectedCallback;
  El.prototype.connectedCallback = function (this: any) {
    patchPlayerElement(this);
    return connected?.call(this);
  };
  for (const key of ['fps', 'quality', 'width', 'height', 'variables']) {
    const d = Object.getOwnPropertyDescriptor(El.prototype, key);
    if (d?.get && !d.set) {
      Object.defineProperty(El.prototype, key, {
        ...d,
        set(this: HTMLElement, v: unknown) {
          if (v == null) this.removeAttribute(key);
          else this.setAttribute(key, typeof v === 'string' ? v : JSON.stringify(v));
        },
      });
    }
  }
  // The first <revideo-player> is upgraded while `customElements.define`
  // runs inside the dynamic import; its connectedCallback therefore ran
  // before we patched the prototype. Patch every instance that already exists.
  document.querySelectorAll('revideo-player').forEach(patchPlayerElement);
}
patchRevideoPlayerElement();

// Revideo's preview creates <video>/<audio> elements it never attaches to the
// document, and waits on their `seeked` events to draw a frame. WebKit (WKWebView)
// throttles detached media, so `seeked` sometimes never fires and the preview
// freezes after a seek. Park new media elements in a hidden in-document holder.
(function attachDetachedMedia() {
  const holder = document.createElement('div');
  holder.setAttribute('aria-hidden', 'true');
  holder.style.cssText = 'position:fixed;left:0;top:0;width:1px;height:1px;overflow:hidden;opacity:0;pointer-events:none;';
  const create = document.createElement.bind(document);
  document.createElement = ((tag: string, opts?: ElementCreationOptions) => {
    const el = create(tag, opts);
    if (/^(video|audio)$/i.test(tag)) {
      queueMicrotask(() => { if (!el.isConnected) (document.body ?? document.documentElement).appendChild(holder).appendChild(el); });
    }
    return el;
  }) as typeof document.createElement;
})();

export interface ResolvedAsset {
  url: string; kind: string; name?: string; width?: number; height?: number; duration_s?: number;
  frames?: string[]; fps_num?: number | null; fps_den?: number | null;
}

/** Resolve the timeline's asset ids to kurnasset:// URLs (ids are immutable → cache forever). */
const assetCache = new Map<string, Promise<ResolvedAsset | null>>();
function resolve(id: string): Promise<ResolvedAsset | null> {
  if (!assetCache.has(id)) {
    assetCache.set(id, call<Asset>('lib.get', { id })
      .then(a => {
        const seq = a.kind === 'seq';
        return {
          url: seq ? seqBase(a.id) : assetUrl(a.id, a.ext),
          kind: a.kind,
          name: a.name,
          width: a.width ?? undefined,
          height: a.height ?? undefined,
          duration_s: a.duration_s,
          frames: seq ? a.frames : undefined,
          fps_num: a.fps_num,
          fps_den: a.fps_den,
        };
      })
      .catch(() => null));
  }
  return assetCache.get(id)!;
}

export function useAssets(tl: Timeline | null) {
  const [assets, setAssets] = useState<Record<string, ResolvedAsset>>({});
  const ids = useMemo(() => [...new Set(Object.values(tl?.clips ?? {}).map(c => c.asset).filter((x): x is string => !!x))].sort().join(','), [tl]);
  useEffect(() => {
    let live = true;
    Promise.all(ids ? ids.split(',').map(async id => [id, await resolve(id)] as const) : []).then(pairs => {
      if (live) setAssets(Object.fromEntries(pairs.filter(([, a]) => a)) as Record<string, ResolvedAsset>);
    });
    return () => { live = false; };
  }, [ids]);
  return assets;
}

/** The shared Revideo project is built with default size/fps; adapt it to this timeline. */
function projectFor(tl: Timeline) {
  const s = baseProject.settings;
  const fps = tl.fps.num / tl.fps.den;
  return {
    ...baseProject,
    settings: {
      ...s,
      shared: { ...s.shared, size: new Vector2(tl.width, tl.height) },
      rendering: { ...s.rendering, fps },
      preview: { ...s.preview, fps },
    },
  } as typeof baseProject;
}

export interface PreviewHandle {
  canvas(): HTMLCanvasElement | null;
  /** Frame-exact seek. (The Player's `currentTime` prop ignores changes under 0.05 s.) */
  seek(seconds: number): void;
  /** Seek without waiting for a settled frame (used while scrubbing). */
  seekFast(seconds: number): void;
  /** Seek and resolve once the Player is showing exactly `frame` (false on timeout). */
  seekAndSettle(frame: number, timeoutMs?: number): Promise<boolean>;
}

const nextFrame = () => new Promise<void>(r => requestAnimationFrame(() => r()));

export function Preview(props: {
  timeline: Timeline;
  assets: Record<string, ResolvedAsset>;
  onTime: (t: number) => void;
  handle?: React.MutableRefObject<PreviewHandle | null>;
}) {
  const { timeline: tl, assets } = props;
  const host = useRef<HTMLDivElement>(null);
  // Re-create the project only when the canvas geometry changes, not on every edit.
  const project = useMemo(() => projectFor(tl), [tl.width, tl.height, tl.fps.num, tl.fps.den]);
  const variables = useMemo(() => ({ timeline: tl, assets, background: '#000000' }), [tl, assets]);
  const missing = Object.values(tl.clips).filter(c => c.asset && !assets[c.asset]).length;
  // Mount the Player once the first batch of assets has resolved (later edits
  // that add assets don't unmount it; the scene skips unresolved clips).
  const [mounted, setMounted] = useState(false);
  // WebKit can occasionally stall the preview after a seek (see attachDetachedMedia).
  // Offer a one-click reload instead of a frozen frame.
  const [stalled, setStalled] = useState(false);
  const [playerKey, setPlayerKey] = useState(0);
  useEffect(() => { if (missing === 0) setMounted(true); }, [missing]);
  // Belt and braces: the custom element can upgrade in the same tick as the
  // first React commit, before the module-level patch sees it.
  useEffect(() => {
    const el = host.current?.querySelector('revideo-player');
    if (el) patchPlayerElement(el);
  }, [mounted, playerKey]);

  // Where the user (or a test) wants the playhead. The element reloads on every
  // `variables` change — every edit, and when assets finish resolving — and the
  // preview would jump back to frame 0, so re-seek after each reload.
  const wantTime = useRef(0);
  const seek = (t: number) => {
    wantTime.current = t;
    host.current?.querySelector('revideo-player')?.dispatchEvent(new CustomEvent('seekto', { detail: t }));
  };
  useEffect(() => {
    const id = requestAnimationFrame(() => seek(wantTime.current));
    return () => cancelAnimationFrame(id);
  }, [variables]);

  const playerEl = () => host.current?.querySelector('revideo-player') as (HTMLElement & { player?: { playback: { frame: number } } }) | null;
  const seekAndSettle = async (frame: number, timeoutMs = 10_000) => {
    const fps = tl.fps.num / tl.fps.den;
    const t0 = performance.now();
    let lastIssue = -Infinity;
    // Wait for the player, (re)issue the seek until it sticks — a seek sent
    // before the project loads, or across a reload, is silently dropped.
    while (performance.now() - t0 < timeoutMs) {
      const el = playerEl();
      if (el?.player) {
        if (el.player.playback.frame === frame) {
          // The playhead being there isn't enough: the stage may still be waiting
          // on media for this frame. Ask for a render and wait until it's drawn.
          const p = el.player as unknown as { requestRender(): void; onRender: { subscribe(h: () => void): () => void } };
          const drawn = await new Promise<boolean>(res => {
            const timer = setTimeout(() => { un(); res(false); }, Math.max(500, timeoutMs - (performance.now() - t0)));
            const un = p.onRender.subscribe(() => { clearTimeout(timer); un(); res(true); });
            p.requestRender();
          });
          await nextFrame(); await nextFrame();
          if (drawn && playerEl()?.player?.playback.frame === frame) return true;
        } else if (performance.now() - lastIssue > 500) {
          seek(frame / fps);
          lastIssue = performance.now();
        }
      }
      await new Promise(r => setTimeout(r, 50));
    }
    return false;
  };

  /** User seeks: frame-exact, and flag a stall instead of freezing silently. */
  const userSeek = (t: number) => {
    seek(t);
    const frame = Math.round(t * tl.fps.num / tl.fps.den);
    seekAndSettle(frame, 5000).then(ok => setStalled(!ok));
  };

  if (props.handle) {
    props.handle.current = { canvas: () => findCanvas(host.current), seek: userSeek, seekFast: seek, seekAndSettle };
  }

  return (
    <div className="preview" ref={host}>
      {!mounted ? null : tl.duration === 0 ? (
        <div className="empty">Empty timeline — add clips with <code>kurn op clip.add</code> or ask an agent.</div>
      ) : (
        <Player
          key={playerKey}
          project={project}
          controls
          variables={variables}
          fps={tl.fps.num / tl.fps.den}
          onTimeUpdate={t => { wantTime.current = t; props.onTime(t); }}
          timeDisplayFormat="MM:SS.mm"
        />
      )}
      {missing > 0 && <div className="badge warn">resolving {missing} asset(s)…</div>}
      {stalled && (
        <div className="badge warn stalled" role="alert">
          Preview stalled on this frame.{' '}
          <button onClick={() => { setStalled(false); setPlayerKey(k => k + 1); setTimeout(() => seekAndSettle(Math.round(wantTime.current * tl.fps.num / tl.fps.den)), 500); }}>Reload preview</button>
        </div>
      )}
    </div>
  );
}

/** The Player renders inside <revideo-player>'s shadow root. */
export function findCanvas(root: ParentNode | null | undefined): HTMLCanvasElement | null {
  if (!root) return null;
  const direct = root.querySelector('canvas');
  if (direct) return direct;
  for (const el of root.querySelectorAll('*')) {
    const found = el.shadowRoot ? findCanvas(el.shadowRoot) : null;
    if (found) return found;
  }
  return null;
}
