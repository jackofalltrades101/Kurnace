// TimelineRenderer (spec §10.1): one generic scene that builds the scene graph
// from a timeline.json passed as a project variable, then steps frame by frame.
// Every frame's state is a pure function of the frame number (see timeline.ts),
// so `render still --frame N` and a full render agree.
import { Audio, Img, Node, Rect, Txt, Video, makeScene2D } from '@revideo/2d';
import { PlaybackState, useScene } from '@revideo/core';
import {
  type Clip, type Timeline, clipStateAt, framesToSeconds,
} from '@kurnace/timeline';

export interface ResolvedAsset {
  url: string;
  kind: 'image' | 'video' | 'audio' | 'seq';
  width?: number;
  height?: number;
  /** Frame-sequence files, relative to `url`. */
  frames?: string[];
  fps_num?: number | null;
  fps_den?: number | null;
}

type Fit = 'contain' | 'cover' | 'none';

interface Driver {
  update(f: number): void;
}

function fitSize(tl: Timeline, a: ResolvedAsset, fit: Fit): { width?: number; height?: number } {
  if (fit === 'none' || !a.width || !a.height) return { width: a.width, height: a.height };
  const sx = tl.width / a.width, sy = tl.height / a.height;
  const s = fit === 'cover' ? Math.max(sx, sy) : Math.min(sx, sy);
  return { width: a.width * s, height: a.height * s };
}

const num = (v: unknown, d: number) => (typeof v === 'number' ? v : d);
const str = (v: unknown, d: string) => (typeof v === 'string' ? v : d);

function applyTransform(node: Node, p: Record<string, unknown>) {
  node.position([num(p.x, 0), num(p.y, 0)]);
  node.scale(num(p.scale, 1));
  node.rotation(num(p.rotation, 0));
  node.opacity(num(p.opacity, 1));
}

function buildComponent(clip: Clip): Node {
  const p = clip.props ?? {};
  switch (clip.component) {
    case 'text':
      return new Txt({
        text: str(p.text, ''),
        fontSize: num(p.fontSize, 96),
        fontWeight: num(p.fontWeight, 700),
        fontFamily: str(p.fontFamily, 'sans-serif'),
        fill: str(p.fill, '#ffffff'),
      });
    case 'solid':
      return new Rect({
        width: num(p.width, 1920),
        height: num(p.height, 1080),
        fill: str(p.fill, '#000000'),
        radius: num(p.radius, 0),
      });
    default:
      throw new Error(`unknown component "${clip.component}" (Phase 0 built-ins: text, solid)`);
  }
}

function makeDriver(view: Node, tl: Timeline, id: string, clip: Clip, assets: Record<string, ResolvedAsset>, muted: boolean): Driver {
  const fps = tl.fps;
  const p = clip.props ?? {};

  // Component clip (title card, solid, …)
  if (clip.component) {
    const node = buildComponent(clip);
    node.opacity(0);
    view.add(node);
    return {
      update(f) {
        const s = clipStateAt(clip, f);
        if (!s.active) { node.opacity(0); return; }
        applyTransform(node, s.props);
        if (clip.component === 'text' && typeof s.props.text === 'string') (node as Txt).text(s.props.text);
      },
    };
  }

  const asset = assets[clip.asset!];
  if (!asset) {
    // Renders validate assets up front (run.ts); in the live preview an asset can
    // still be resolving. Skip the clip rather than crash the scene.
    console.warn(`clip ${id}: asset ${clip.asset} not resolved yet; skipping`);
    return { update() {} };
  }
  const inSec = framesToSeconds(clip.in, fps);

  if (asset.kind === 'seq') {
    const frames = asset.frames ?? [];
    if (frames.length === 0) {
      console.warn(`clip ${id}: sequence ${clip.asset} has no frames; skipping`);
      return { update() {} };
    }
    const seqFps = asset.fps_num && asset.fps_den ? asset.fps_num / asset.fps_den : fps.num / fps.den;
    const node = new Img({ src: asset.url + frames[0], ...fitSize(tl, asset, str(p.fit, 'contain') as Fit), opacity: 0 });
    view.add(node);
    let last = 0;
    return {
      update(f) {
        const s = clipStateAt(clip, f);
        if (!s.active) { node.opacity(0); return; }
        // Source seconds → sequence frame, independent of the project rate.
        const sec = framesToSeconds(s.source, fps);
        const idx = Math.min(frames.length - 1, Math.max(0, Math.floor(sec * seqFps + 1e-6)));
        if (idx !== last) {
          last = idx;
          (node as unknown as { src(v: string): void }).src(asset.url + frames[idx]);
        }
        applyTransform(node, s.props);
      },
    };
  }

  if (asset.kind === 'image') {
    const node = new Img({ src: asset.url, ...fitSize(tl, asset, str(p.fit, 'contain') as Fit), opacity: 0 });
    view.add(node);
    return {
      update(f) {
        const s = clipStateAt(clip, f);
        if (!s.active) { node.opacity(0); return; }
        applyTransform(node, s.props);
      },
    };
  }

  // Video / audio: drive `time` and `playing` explicitly every frame, so the
  // source position is a pure function of the frame (no reliance on play()'s
  // clock, which drifted 1–2 frames and differed between stills and full renders).
  // Revideo reads these same signals to build the audio track (Scene2D.getMediaAssets).
  // Export volume is read from a field set only by the constructor (Revideo 0.11
  // setVolume() updates the <video> element but not that field), so pass the
  // clip's starting volume here and also write the field each frame below.
  // Revideo exports ONE volume per clip; volume keyframes need audio.mix (§11).
  const startVol = muted ? 0 : num(clipStateAt(clip, clip.start).props.volume, 1);
  const media = asset.kind === 'video'
    ? new Video({ src: asset.url, time: inSec, volume: startVol, decoder: (p.decoder as 'web' | 'ffmpeg' | 'slow' | undefined) ?? null, ...fitSize(tl, asset, str(p.fit, 'contain') as Fit), opacity: 0 })
    : new Audio({ src: asset.url, time: inSec, volume: startVol });
  view.add(media);
  // `time` / `playing` are protected in Revideo's typings but are plain signals.
  const ctl = media as unknown as { time(v: number): void; playing(v: boolean): void; volume: number };
  return {
    update(f) {
      const s = clipStateAt(clip, f);
      ctl.playing(s.active);
      if (s.active) {
        // Where inside the source frame to aim:
        //  - Rendering uses Revideo's mp4 parser. Its seek drops frames with
        //    timestamp < t, and float error makes the frame exactly at t compare
        //    as smaller, so it returns the NEXT frame. Aim 1/4 frame early.
        //  - Preview seeks an HTMLVideoElement. Aim at the middle of the frame,
        //    the standard target that avoids landing on a boundary.
        // Audio nodes are not nudged.
        const rendering = (view as unknown as { playbackState(): PlaybackState }).playbackState() === PlaybackState.Rendering;
        const nudge = asset.kind !== 'video' ? 0 : rendering ? 0.25 : -0.5;
        ctl.time(Math.max(0, framesToSeconds(s.source - nudge, fps)));
        const vol = muted ? 0 : num(s.props.volume, 1);
        media.setVolume(vol); // preview
        ctl.volume = vol;     // export
      }
      if (asset.kind === 'video') {
        if (!s.active) { media.opacity(0); return; }
        applyTransform(media, s.props);
      }
    },
  };
}

export default makeScene2D('timeline', function* (view) {
  const tl = useScene().variables.get<Timeline | null>('timeline', null)();
  const assets = useScene().variables.get<Record<string, ResolvedAsset>>('assets', {})();
  if (!tl) throw new Error('TimelineRenderer: no "timeline" variable supplied');

  view.fill(String(useScene().variables.get('background', '#000000')()));

  // Bottom-to-top: lower `order` draws first (V1 under V2).
  const tracks = [...tl.tracks].sort((a, b) => a.order - b.order);
  const drivers: Driver[] = [];
  for (const track of tracks) {
    const clips = Object.entries(tl.clips)
      .filter(([, c]) => c.track === track.id)
      .sort((a, b) => a[1].start - b[1].start);
    for (const [id, clip] of clips) {
      drivers.push(makeDriver(view, tl, id, clip, assets, !!track.muted));
    }
  }

  for (let f = 0; f < tl.duration; f++) {
    for (const d of drivers) d.update(f);
    yield;
  }
});
