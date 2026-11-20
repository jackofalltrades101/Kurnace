// Entry for the headless render page (render.html). The render server drives it:
//   await window.__kurn.load(timeline, assets, base)
//   await window.__kurn.seek(frame)     // resolves when the frame is fully drawn
import type { Timeline } from '@kurnace/timeline';
import { type AssetRef, Compositor } from './compositor';

let comp: Compositor | null = null;
const errors: string[] = [];

declare global {
  interface Window { __kurn: unknown }
}

window.__kurn = {
  load(tl: Timeline, assets: Record<string, AssetRef>, base: string, background = '#000000') {
    document.body.style.background = background;
    comp?.destroy();
    comp = new Compositor(document.getElementById('stage')!, {
      mode: 'render', base, assets, sceneTimeoutMs: 60_000,
      onError: (clip, msg) => errors.push(`${clip}: ${msg}`),
    });
    comp.stage.style.background = background;
    comp.setTimeline(tl);
  },
  async seek(frame: number) {
    await comp!.seek(frame);
  },
  errors: () => errors.splice(0),
};
