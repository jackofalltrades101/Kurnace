// Kurnace scene runtime (docs/v2-plan.md §3.1).
//
// Injected into every HTML scene page. It makes the page a deterministic
// function of the frame number, and talks to the compositor over postMessage
// so the same scene works in the app's live preview and in headless renders.
//
// A scene can be any of:
//   1. Plain HTML + CSS animations / Web Animations — nothing to write; the
//      runtime pauses every animation and sets its time to the frame's time.
//   2. kurnace.scene({ duration, params, setup(ctx), render(frame, ctx) })
//   3. The legacy contract: window.META + window.renderFrame(frame[, params])
//
// Determinism: requestAnimationFrame, performance.now and Date.now run on a
// virtual clock, and Math.random is seeded (stable at load, re-seeded per frame).
// Keep render a pure function of (frame, params): no fetches or timers whose
// results change what's drawn.
//
// Protocol (window.parent ⇄ scene):
//   → { kurnace: 'seek', id, frame, params }
//   ← { kurnace: 'ready', meta }
//   ← { kurnace: 'drawn', id, frame }
//   ← { kurnace: 'error', id?, message, stack }
(() => {
  if (window.__kurnaceRuntime) return;
  window.__kurnaceRuntime = true;

  // ---------- virtual clock ----------
  let nowMs = 0;
  const EPOCH = Date.UTC(2026, 0, 1);
  const rafQueue = new Map();
  let rafSeq = 0;
  window.requestAnimationFrame = cb => { rafSeq += 1; rafQueue.set(rafSeq, cb); return rafSeq; };
  window.cancelAnimationFrame = id => { rafQueue.delete(id); };
  try { Object.defineProperty(performance, 'now', { value: () => nowMs, configurable: true }); } catch { /* read-only in some engines */ }
  Date.now = () => EPOCH + nowMs;

  // ---------- seeded randomness (mulberry32) ----------
  let seed = 1;
  const reseed = s => { seed = (s >>> 0) || 1; };
  Math.random = () => {
    seed = (seed + 0x6d2b79f5) >>> 0;
    let t = seed;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };

  // ---------- scene API ----------
  let def = null;
  let setupDone = null;
  const post = msg => { try { window.parent.postMessage({ kurnace: msg.kurnace, ...msg }, '*'); } catch { /* detached */ } };

  const kurnace = (window.kurnace = {
    /** Register a scene. render(frame, ctx) may return a promise. */
    scene(d) { def = d; },
    params: {},
    fps: 30,
    width: 0,
    height: 0,
    /** Deterministic noise in [0, 1) for (frame, salt). */
    random(frame, salt = 0) {
      let t = (Math.imul(frame + 1, 0x9e3779b1) ^ Math.imul(salt + 7, 0x85ebca6b)) >>> 0;
      t = Math.imul(t ^ (t >>> 16), 0x7feb352d);
      t = Math.imul(t ^ (t >>> 15), 0x846ca68b);
      return ((t ^ (t >>> 16)) >>> 0) / 4294967296;
    },
  });

  function meta() {
    const m = { ...(window.META || {}), ...(def || {}) };
    delete m.render; delete m.setup;
    return {
      duration: m.duration ?? null,
      fps: m.fps ?? null,
      width: m.width ?? null,
      height: m.height ?? null,
      params: m.params ?? {},
    };
  }

  function applyParams(params) {
    kurnace.params = params || {};
    // Expose params as CSS variables: --title, --accent, …
    const root = document.documentElement.style;
    for (const [k, v] of Object.entries(kurnace.params)) {
      if (typeof v === 'string' || typeof v === 'number') root.setProperty(`--${k}`, String(v));
    }
  }

  async function settleMedia() {
    // Fonts and images must be ready, or the first frames render with fallbacks.
    if (document.fonts?.ready) await document.fonts.ready;
    const imgs = [...document.images].filter(i => !i.complete);
    await Promise.all(imgs.map(i => i.decode().catch(() => {})));
  }

  async function seekVideos(timeS) {
    const vids = [...document.querySelectorAll('video')];
    await Promise.all(vids.map(v => new Promise(res => {
      v.pause();
      const target = Math.min(timeS, Number.isFinite(v.duration) ? v.duration : timeS);
      if (Math.abs(v.currentTime - target) < 1e-3) return res();
      const done = () => { v.removeEventListener('seeked', done); res(); };
      v.addEventListener('seeked', done);
      setTimeout(done, 2000); // never hang a render on a misbehaving element
      v.currentTime = target;
    })));
  }

  async function draw(frame, params) {
    if (params) applyParams(params);
    const fps = kurnace.fps || 30;
    nowMs = (frame / fps) * 1000;
    if (def?.setup && !setupDone) setupDone = Promise.resolve(def.setup(ctx(frame)));
    if (setupDone) await setupDone;
    reseed(Math.imul(frame + 1, 2654435761));

    // CSS animations / transitions / Web Animations: paused at exact time.
    if (document.getAnimations) {
      for (const a of document.getAnimations()) {
        a.pause();
        a.currentTime = nowMs;
      }
    }
    // rAF-driven code sees one tick at the frame's time.
    const cbs = [...rafQueue.values()];
    rafQueue.clear();
    for (const cb of cbs) cb(nowMs);

    if (def?.render) await def.render(frame, ctx(frame));
    else if (typeof window.renderFrame === 'function') await window.renderFrame(frame, kurnace.params);

    await seekVideos(nowMs / 1000);
    await settleMedia();
  }

  const ctx = frame => ({
    frame,
    time: frame / (kurnace.fps || 30),
    fps: kurnace.fps || 30,
    params: kurnace.params,
    width: kurnace.width || window.innerWidth,
    height: kurnace.height || window.innerHeight,
    random: salt => kurnace.random(frame, salt),
  });

  // Serialize seeks; a newer seek supersedes queued ones (preview scrubbing).
  let chain = Promise.resolve();
  let latest = null;
  window.addEventListener('message', e => {
    const m = e.data;
    if (!m || typeof m !== 'object' || m.kurnace !== 'seek') return;
    if (m.fps) kurnace.fps = m.fps;
    if (m.width) kurnace.width = m.width;
    if (m.height) kurnace.height = m.height;
    latest = m;
    chain = chain.then(async () => {
      if (latest !== m && !m.exact) return; // skipped in favour of a newer seek
      try {
        await draw(m.frame, m.params);
        post({ kurnace: 'drawn', id: m.id, frame: m.frame });
      } catch (err) {
        post({ kurnace: 'error', id: m.id, message: String(err?.message || err), stack: String(err?.stack || '') });
      }
    });
  });

  window.addEventListener('error', e => post({ kurnace: 'error', message: e.message, stack: String(e.error?.stack || `${e.filename}:${e.lineno}`) }));
  window.addEventListener('unhandledrejection', e => post({ kurnace: 'error', message: String(e.reason?.message || e.reason), stack: String(e.reason?.stack || '') }));

  const announce = async () => {
    // Pause animations immediately so nothing advances on the real clock.
    if (document.getAnimations) for (const a of document.getAnimations()) a.pause();
    await settleMedia();
    post({ kurnace: 'ready', meta: meta() });
  };
  if (document.readyState === 'complete') announce();
  else window.addEventListener('load', announce);
})();
