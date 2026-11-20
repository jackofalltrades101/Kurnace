// Kurnace compositor (docs/v2-plan.md §3.1).
//
// Renders a timeline as stacked DOM layers. The same code runs in the app's
// live preview and, inside headless Chromium, for final renders, so what you
// preview is what renders.
//
//   const comp = new Compositor(host, { mode: 'preview', base, assets });
//   comp.setTimeline(tl);
//   await comp.seek(120);          // exact: resolves once every layer has drawn frame 120
//   comp.play(); comp.pause();     // preview only
//
// URL layout under `base` (served by the app's asset server and the render server):
//   <base>/<assetId>.<ext>           original media
//   <base>/frame/<assetId>/<sec>     exact video frame (JPEG)
//   <base>/seq/<assetId>/<index>     frame of an image-sequence asset
//   <base>/scene/<name>/<path>       scene files (runtime injected into HTML)
import { type Clip, type Timeline, type Track, clipStateAt, framesToSeconds, fpsValue } from '@kurnace/timeline';

export interface AssetRef {
  kind: string; // image | video | audio | seq | …
  /** Full URL of the original media; defaults to <base>/<id>.<ext>. */
  url?: string;
  ext?: string | null;
  width?: number | null;
  height?: number | null;
  /** Native fps for video / seq assets, when known. */
  fps?: number | null;
}

export interface SceneMeta { duration?: number | null; fps?: number | null; width?: number | null; height?: number | null; params?: Record<string, unknown> }

export interface CompositorOptions {
  mode: 'preview' | 'render';
  base: string;
  assets: Record<string, AssetRef>;
  /** Called when a layer fails (scene error, missing media). */
  onError?: (clipId: string, message: string) => void;
  /** Preview: called every displayed frame while playing. */
  onFrame?: (frame: number) => void;
  /** Cache-buster per scene name, bumped by reloadScene(). */
  sceneVersion?: Record<string, number>;
  /** How long a scene may take to draw one frame before it's reported. */
  sceneTimeoutMs?: number;
}

/** Per-frame transition effect on a layer (docs/v2-plan.md §3.2). */
interface Fx { opacity?: number; dx?: number; clip?: string; /** draw above the other clip */ above?: boolean }

/**
 * Transitions between two clips, centred on the cut (where A ends / B starts):
 * both clips run `dur/2` past their edges into their source material.
 *   crossfade — A fades out while B fades in
 *   dip       — A fades to black, then B fades up
 *   wipe      — B is revealed left to right over A
 *   slide     — B pushes A out to the left
 */
function transitionFx(tl: Timeline, frame: number): Map<string, Fx> {
  const out = new Map<string, Fx>();
  for (const tr of Object.values(tl.transitions ?? {})) {
    const [aId, bId] = tr.between;
    const a = tl.clips[aId], b = tl.clips[bId];
    if (!a || !b || !(tr.dur > 0)) continue;
    const cut = b.start;
    const from = cut - Math.floor(tr.dur / 2), to = from + tr.dur;
    if (frame < from || frame >= to) continue;
    const p = (frame - from + 0.5) / tr.dur; // 0 → 1 across the transition
    const ease = p * p * (3 - 2 * p);
    switch (tr.type) {
      case 'dip':
        out.set(aId, { opacity: p < 0.5 ? 1 - ease * 2 : 0 });
        out.set(bId, { opacity: p < 0.5 ? 0 : (ease - 0.5) * 2 });
        break;
      case 'wipe':
        out.set(aId, {});
        out.set(bId, { above: true, clip: `inset(0 ${((1 - ease) * 100).toFixed(3)}% 0 0)` });
        break;
      case 'slide':
        out.set(aId, { dx: -ease * tl.width });
        out.set(bId, { dx: (1 - ease) * tl.width });
        break;
      default: // crossfade: A stays solid underneath while B fades in on top (no mid-dip in brightness)
        out.set(aId, {});
        out.set(bId, { opacity: ease, above: true });
    }
  }
  return out;
}

const num = (v: unknown, d: number) => (typeof v === 'number' && Number.isFinite(v) ? v : d);
const str = (v: unknown, d: string) => (typeof v === 'string' ? v : d);
type Fit = 'contain' | 'cover' | 'none' | 'stretch';

interface Ctx {
  tl: Timeline;
  opts: CompositorOptions;
  frame: number;
  exact: boolean;
  playing: boolean;
}

abstract class Layer {
  readonly el: HTMLDivElement;
  /** Track stacking order (set by the compositor). */
  z = 0;
  constructor(readonly id: string, public clip: Clip) {
    this.el = document.createElement('div');
    this.el.className = 'kc-layer';
    this.el.dataset.clip = id;
    Object.assign(this.el.style, {
      position: 'absolute', left: '50%', top: '50%', display: 'none', transformOrigin: '0 0', willChange: 'transform, opacity',
    });
  }
  /** Size of the layer's content in stage pixels (before scale). */
  abstract size(ctx: Ctx): { w: number; h: number };
  /** Update for this frame. Exact updates resolve once the frame is drawn. */
  abstract update(ctx: Ctx, local: number, source: number): Promise<void> | void;
  hide() { this.el.style.display = 'none'; }
  pausePlayback() {}
  dispose() { this.el.remove(); }

  place(ctx: Ctx, props: Record<string, unknown>, fx?: Fx) {
    // Show before measuring: hidden elements measure as 0×0.
    this.el.style.display = 'block';
    const { w, h } = this.size(ctx);
    const x = num(props.x, 0) + (fx?.dx ?? 0), y = num(props.y, 0);
    const s = num(props.scale, 1), r = num(props.rotation, 0);
    // Center-origin coordinates, like v1: (0,0) is the stage center, +y is down.
    this.el.style.width = `${w}px`;
    this.el.style.height = `${h}px`;
    this.el.style.transform = `translate(${x}px, ${y}px) rotate(${r}deg) scale(${s}) translate(${-w / 2}px, ${-h / 2}px)`;
    this.el.style.transformOrigin = '0 0';
    this.el.style.opacity = String(num(props.opacity, 1) * (fx?.opacity ?? 1));
    this.el.style.clipPath = fx?.clip ?? '';
    this.el.style.zIndex = String(this.z * 2 + (fx?.above ? 1 : 0));
  }
}

function fitSize(tl: Timeline, a: AssetRef | undefined, fit: Fit) {
  const w = a?.width || tl.width, h = a?.height || tl.height;
  if (fit === 'none') return { w, h };
  if (fit === 'stretch') return { w: tl.width, h: tl.height };
  const sx = tl.width / w, sy = tl.height / h;
  const s = fit === 'cover' ? Math.max(sx, sy) : Math.min(sx, sy);
  return { w: w * s, h: h * s };
}

function loadImage(img: HTMLImageElement, src: string): Promise<void> {
  if (img.dataset.src === src && img.complete && img.naturalWidth > 0) return Promise.resolve();
  img.dataset.src = src;
  img.src = src;
  return img.decode().catch(() => {
    throw new Error(`couldn't load ${src}`);
  });
}

class ImageLayer extends Layer {
  img = Object.assign(document.createElement('img'), { draggable: false });
  constructor(id: string, clip: Clip, private asset: AssetRef | undefined, private url: string) {
    super(id, clip);
    Object.assign(this.img.style, { width: '100%', height: '100%', display: 'block' });
    this.el.append(this.img);
  }
  size(ctx: Ctx) { return fitSize(ctx.tl, this.asset, str(this.clip.props?.fit, 'contain') as Fit); }
  update() { return loadImage(this.img, this.url); }
}

class SeqLayer extends Layer {
  img = Object.assign(document.createElement('img'), { draggable: false });
  constructor(id: string, clip: Clip, private asset: AssetRef | undefined, private assetId: string) {
    super(id, clip);
    Object.assign(this.img.style, { width: '100%', height: '100%', display: 'block' });
    this.el.append(this.img);
  }
  size(ctx: Ctx) { return fitSize(ctx.tl, this.asset, str(this.clip.props?.fit, 'contain') as Fit); }
  update(ctx: Ctx, _local: number, source: number) {
    const tlFps = fpsValue(ctx.tl.fps);
    const index = Math.round((source / tlFps) * (this.asset?.fps || tlFps));
    const p = loadImage(this.img, `${ctx.opts.base}/seq/${this.assetId}/${index}`);
    return ctx.exact ? p : void p.catch(() => {});
  }
}

/**
 * Video: exact frames (decoded by ffmpeg, served as images) when paused or
 * rendering; a <video> element while the preview plays.
 */
class VideoLayer extends Layer {
  img = Object.assign(document.createElement('img'), { draggable: false });
  video: HTMLVideoElement | null = null;
  constructor(id: string, clip: Clip, private asset: AssetRef | undefined, private assetId: string, private url: string, private muted: boolean) {
    super(id, clip);
    Object.assign(this.img.style, { width: '100%', height: '100%', display: 'block', position: 'absolute', inset: '0' });
    this.el.append(this.img);
  }
  size(ctx: Ctx) { return fitSize(ctx.tl, this.asset, str(this.clip.props?.fit, 'contain') as Fit); }
  private ensureVideo() {
    if (this.video) return this.video;
    const v = document.createElement('video');
    v.src = this.url;
    v.preload = 'auto';
    v.playsInline = true;
    Object.assign(v.style, { width: '100%', height: '100%', display: 'none', position: 'absolute', inset: '0' });
    this.el.append(v);
    return (this.video = v);
  }
  update(ctx: Ctx, _local: number, source: number) {
    const t = framesToSeconds(source, ctx.tl.fps);
    if (ctx.opts.mode === 'preview' && ctx.playing) {
      const v = this.ensureVideo();
      const st = clipStateAt(this.clip, ctx.frame);
      v.muted = this.muted;
      v.volume = Math.max(0, Math.min(1, num(st.props.volume, 1)));
      if (Math.abs(v.currentTime - t) > 0.25) v.currentTime = t;
      if (v.paused) v.play().catch(() => {});
      v.style.display = 'block';
      this.img.style.display = 'none';
      return;
    }
    this.pausePlayback();
    // Quantize to the timeline frame so cached frames are reused.
    const p = loadImage(this.img, `${ctx.opts.base}/frame/${this.assetId}/${t.toFixed(6)}`);
    this.img.style.display = 'block';
    if (this.video) this.video.style.display = 'none';
    return ctx.exact ? p : void p.catch(() => {});
  }
  pausePlayback() { this.video?.pause(); }
  hide() { super.hide(); this.pausePlayback(); }
  dispose() { this.video?.pause(); this.video?.removeAttribute('src'); super.dispose(); }
}

class AudioLayer extends Layer {
  audio: HTMLAudioElement | null = null;
  constructor(id: string, clip: Clip, private url: string, private muted: boolean) { super(id, clip); }
  size() { return { w: 0, h: 0 }; }
  update(ctx: Ctx, _local: number, source: number) {
    // Renders mix audio with ffmpeg; only the preview plays it.
    if (ctx.opts.mode !== 'preview' || !ctx.playing) { this.pausePlayback(); return; }
    if (!this.audio) { this.audio = new Audio(this.url); this.audio.preload = 'auto'; }
    const a = this.audio;
    const t = framesToSeconds(source, ctx.tl.fps);
    a.muted = this.muted;
    a.volume = Math.max(0, Math.min(1, num(clipStateAt(this.clip, ctx.frame).props.volume, 1)));
    if (Math.abs(a.currentTime - t) > 0.25) a.currentTime = t;
    if (a.paused) a.play().catch(() => {});
  }
  place() {}
  pausePlayback() { this.audio?.pause(); }
  hide() { this.pausePlayback(); }
  dispose() { this.audio?.pause(); }
}

class TextLayer extends Layer {
  span = document.createElement('div');
  constructor(id: string, clip: Clip) {
    super(id, clip);
    // inline-block + pre: the span is as wide as its text, whatever the layer's width.
    Object.assign(this.span.style, { display: 'inline-block', whiteSpace: 'pre', lineHeight: '1.15', textAlign: 'center' });
    this.el.append(this.span);
  }
  private style(p: Record<string, unknown>) {
    this.span.textContent = String(p.text ?? '');
    Object.assign(this.span.style, {
      fontSize: `${num(p.fontSize, 96)}px`,
      fontWeight: String(num(p.fontWeight, 700)),
      fontFamily: str(p.fontFamily, 'sans-serif'),
      color: str(p.fill, '#ffffff'),
      letterSpacing: p.letterSpacing != null ? `${num(p.letterSpacing, 0)}px` : '',
    });
  }
  size() { return { w: this.span.offsetWidth, h: this.span.offsetHeight }; }
  update(ctx: Ctx) {
    this.style(clipStateAt(this.clip, ctx.frame).props);
    return document.fonts?.ready.then(() => {});
  }
}

class SolidLayer extends Layer {
  size(ctx: Ctx) {
    const p = this.clip.props ?? {};
    return { w: num(p.width, ctx.tl.width), h: num(p.height, ctx.tl.height) };
  }
  update(ctx: Ctx) {
    const p = clipStateAt(this.clip, ctx.frame).props;
    this.el.style.background = str(p.fill, '#000000');
    this.el.style.borderRadius = `${num(p.radius, 0)}px`;
  }
}

/** An HTML scene in an iframe, driven by the scene runtime over postMessage. */
class SceneLayer extends Layer {
  frame: HTMLIFrameElement;
  meta: SceneMeta = {};
  private ready: Promise<void>;
  private resolveReady!: () => void;
  private pending = new Map<number, { resolve: () => void; reject: (e: Error) => void; timer: ReturnType<typeof setTimeout> }>();
  private seq = 0;
  private lastError: string | null = null;
  constructor(id: string, clip: Clip, readonly scene: string, private src: string, private onError: (msg: string) => void) {
    super(id, clip);
    this.frame = document.createElement('iframe');
    this.frame.setAttribute('scrolling', 'no');
    this.frame.title = `scene ${scene}`;
    Object.assign(this.frame.style, { border: '0', width: '100%', height: '100%', display: 'block', background: 'transparent', pointerEvents: 'none' });
    this.ready = new Promise(r => (this.resolveReady = r));
    this.el.append(this.frame);
    window.addEventListener('message', this.onMessage);
    this.frame.src = src;
  }
  private onMessage = (e: MessageEvent) => {
    if (e.source !== this.frame.contentWindow) return;
    const m = e.data;
    if (!m || typeof m !== 'object') return;
    if (m.kurnace === 'ready') { this.meta = m.meta ?? {}; this.resolveReady(); }
    else if (m.kurnace === 'drawn') { const p = this.pending.get(m.id); if (p) { clearTimeout(p.timer); this.pending.delete(m.id); p.resolve(); } }
    else if (m.kurnace === 'error') {
      this.lastError = m.message;
      this.onError(`${m.message}${m.stack ? `\n${m.stack}` : ''}`);
      const p = m.id != null ? this.pending.get(m.id) : null;
      if (p) { clearTimeout(p.timer); this.pending.delete(m.id); p.reject(new Error(`scene ${this.scene}: ${m.message}`)); }
    }
  };
  size(ctx: Ctx) {
    return { w: this.meta.width || ctx.tl.width, h: this.meta.height || ctx.tl.height };
  }
  async update(ctx: Ctx, _local: number, source: number) {
    const waitReady = Promise.race([
      this.ready,
      new Promise<void>((_, rej) => setTimeout(() => rej(new Error(`scene ${this.scene} didn't load (is ${this.src} a scene page?)`)), ctx.opts.sceneTimeoutMs ?? 15000)),
    ]);
    if (!ctx.exact) { waitReady.then(() => this.post(ctx, source, false)).catch(() => {}); return; }
    await waitReady;
    await this.post(ctx, source, true);
  }
  private post(ctx: Ctx, frame: number, exact: boolean): Promise<void> {
    const id = ++this.seq;
    const { w, h } = this.size(ctx);
    const params = { ...((this.clip as Clip & { params?: Record<string, unknown> }).params ?? {}) };
    this.frame.contentWindow?.postMessage({ kurnace: 'seek', id, frame, params, fps: fpsValue(ctx.tl.fps), width: w, height: h, exact }, '*');
    if (!exact) return Promise.resolve();
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`scene ${this.scene} took over ${(ctx.opts.sceneTimeoutMs ?? 15000) / 1000}s to draw frame ${frame}${this.lastError ? ` (last error: ${this.lastError})` : ''}`));
      }, ctx.opts.sceneTimeoutMs ?? 15000);
      this.pending.set(id, { resolve, reject, timer });
    });
  }
  dispose() {
    window.removeEventListener('message', this.onMessage);
    for (const p of this.pending.values()) clearTimeout(p.timer);
    super.dispose();
  }
}

export class Compositor {
  readonly stage: HTMLDivElement;
  private tl: Timeline | null = null;
  private layers = new Map<string, Layer>();
  private frame = 0;
  private raf = 0;
  private playStart = 0;
  private playFrom = 0;
  private _playing = false;

  constructor(host: HTMLElement, readonly opts: CompositorOptions) {
    this.stage = document.createElement('div');
    this.stage.className = 'kc-stage';
    Object.assign(this.stage.style, { position: 'relative', overflow: 'hidden', background: '#000', transformOrigin: '0 0' });
    host.append(this.stage);
  }

  get playing() { return this._playing; }
  get currentFrame() { return this.frame; }

  /** Diff the timeline into layers: clips keep their layer (and loaded media) across edits. */
  setTimeline(tl: Timeline, assets?: Record<string, AssetRef>) {
    if (assets) this.opts.assets = assets;
    this.tl = tl;
    Object.assign(this.stage.style, { width: `${tl.width}px`, height: `${tl.height}px` });
    const order = new Map(tl.tracks.map((t, i) => [t.id, i]));
    const tracks = new Map<string, Track>(tl.tracks.map(t => [t.id, t]));
    for (const [id, layer] of this.layers) {
      const c = tl.clips[id];
      if (!c || this.identity(c) !== this.identity(layer.clip)) { layer.dispose(); this.layers.delete(id); }
    }
    for (const [id, clip] of Object.entries(tl.clips)) {
      let layer = this.layers.get(id);
      if (!layer) {
        const made = this.makeLayer(id, clip, !!tracks.get(clip.track)?.muted);
        if (!made) continue;
        layer = made;
        this.layers.set(id, layer);
        this.stage.append(layer.el);
      }
      layer.clip = clip;
      layer.z = order.get(clip.track) ?? 0;
      layer.el.style.zIndex = String(layer.z * 2);
    }
  }

  /** Bump a scene's version so its iframes reload (hot reload). */
  reloadScene(name: string) {
    this.opts.sceneVersion = { ...this.opts.sceneVersion, [name]: (this.opts.sceneVersion?.[name] ?? 0) + 1 };
    for (const [id, layer] of this.layers) {
      if (layer instanceof SceneLayer && layer.scene === name) { layer.dispose(); this.layers.delete(id); }
    }
    if (this.tl) this.setTimeline(this.tl);
  }

  private identity(c: Clip) {
    const x = c as Clip & { scene?: string };
    return `${c.asset ?? ''}|${c.component ?? ''}|${x.scene ?? ''}`;
  }

  private makeLayer(id: string, clip: Clip, muted: boolean): Layer | null {
    const x = clip as Clip & { scene?: string };
    const base = this.opts.base;
    if (x.scene) {
      const v = this.opts.sceneVersion?.[x.scene] ?? 0;
      return new SceneLayer(id, clip, x.scene, `${base}/scene/${encodeURIComponent(x.scene)}/?v=${v}`, msg => this.opts.onError?.(id, msg));
    }
    if (clip.component === 'text') return new TextLayer(id, clip);
    if (clip.component === 'solid') return new SolidLayer(id, clip);
    if (clip.component) { this.opts.onError?.(id, `unknown component "${clip.component}"`); return null; }
    const a = clip.asset ? this.opts.assets[clip.asset] : undefined;
    if (!clip.asset || !a) { this.opts.onError?.(id, `asset ${clip.asset} isn't available`); return null; }
    const url = a.url ?? `${base}/${clip.asset}${a.ext ? `.${a.ext}` : ''}`;
    switch (a.kind) {
      case 'image': return new ImageLayer(id, clip, a, url);
      case 'video': return new VideoLayer(id, clip, a, clip.asset, url, muted);
      case 'audio': return new AudioLayer(id, clip, url, muted);
      case 'seq': return new SeqLayer(id, clip, a, clip.asset);
      default: this.opts.onError?.(id, `can't composite ${a.kind} assets yet`); return null;
    }
  }

  private ctx(frame: number, exact: boolean): Ctx {
    return { tl: this.tl!, opts: this.opts, frame, exact, playing: this._playing };
  }

  private async apply(frame: number, exact: boolean) {
    if (!this.tl) return;
    const ctx = this.ctx(frame, exact);
    const work: Promise<void>[] = [];
    const fxs = transitionFx(this.tl, frame);
    for (const [id, layer] of this.layers) {
      const st = clipStateAt(layer.clip, frame);
      const fx = fxs.get(id);
      // Transitions keep a clip visible a little past its edges.
      if (!st.active && !fx) { layer.hide(); continue; }
      const source = Math.max(0, st.source);
      const r = (async () => {
        await layer.update(ctx, st.local, source);
        layer.place(ctx, st.props, fx);
      })().catch(e => {
        this.opts.onError?.(id, String(e?.message ?? e));
        if (exact && this.opts.mode === 'render') throw e;
      });
      // Size can depend on loaded content (text); place now too so nothing jumps.
      layer.place(ctx, st.props, fx);
      work.push(r as Promise<void>);
    }
    if (exact) await Promise.all(work);
  }

  /** Show `frame`. Resolves once every visible layer has drawn it exactly. */
  async seek(frame: number) {
    if (this._playing) this.pause();
    this.frame = Math.max(0, Math.round(frame));
    await this.apply(this.frame, true);
    await new Promise(r => requestAnimationFrame(() => requestAnimationFrame(r)));
  }

  /** Show `frame` without waiting for exact media (scrubbing). Follow with seek() when it settles. */
  show(frame: number) {
    if (this._playing) this.pause();
    this.frame = Math.max(0, Math.round(frame));
    void this.apply(this.frame, false);
  }

  /** Preview only: start playback from the current frame on the real clock. */
  play() {
    if (!this.tl || this._playing) return;
    this._playing = true;
    this.playFrom = this.frame >= this.tl.duration - 1 ? 0 : this.frame;
    this.playStart = performance.now();
    const fps = fpsValue(this.tl.fps);
    const tick = () => {
      if (!this._playing || !this.tl) return;
      const f = this.playFrom + Math.floor(((performance.now() - this.playStart) / 1000) * fps);
      if (f >= this.tl.duration) { this.pause(); void this.seek(this.tl.duration - 1); return; }
      if (f !== this.frame) {
        this.frame = f;
        void this.apply(f, false);
        this.opts.onFrame?.(f);
      }
      this.raf = requestAnimationFrame(tick);
    };
    void this.apply(this.playFrom, false);
    this.raf = requestAnimationFrame(tick);
  }

  pause() {
    this._playing = false;
    cancelAnimationFrame(this.raf);
    for (const l of this.layers.values()) l.pausePlayback();
  }

  destroy() {
    this.pause();
    for (const l of this.layers.values()) l.dispose();
    this.layers.clear();
    this.stage.remove();
  }
}
