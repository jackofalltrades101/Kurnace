# render.revideo

Kurnace's TimelineRenderer (spec §10). It reads a v2 `timeline.json` and renders a still, a draft or a final file with Revideo **0.11.0**. Revideo is pinned exactly; upgrading it should be a deliberate PR that re-runs `test/sync.test.ts`.

Protocol: one JSON request on stdin, NDJSON events on stdout (spec §9.2). Normally you call it through `kurn render still|draft|final`.

## Conventions
- Every frame's state is a pure function of the frame number (`@kurnace/timeline` `clipStateAt`). So a still at frame N is identical to frame N of a full render. `test/sync.test.ts` enforces this.
- Keyframe `t` is **clip-local** (0 = clip start).
- Track `order`: lower draws first, so V1 is underneath V2.
- Built-in components: `text`, `solid`.
- Image and video `props.fit` can be `contain` (default), `cover` or `none`.

## Revideo 0.11 issues we work around
| Issue | Workaround (where) |
|---|---|
| Uncaught page errors aren't handled, so a scene crash hangs the render forever | Patch the renderer's puppeteer to fail on `pageerror`, plus a progress-stall watchdog (`run.ts`, `KURN_RENDER_STALL_MS`) |
| mp4 seek drops the frame exactly at `t` because of float error, returning the next frame | Request `t − ¼ frame` for video (`scenes/timeline.ts`) |
| `play()`-driven media time drifted 1–2 frames, and differently for stills vs. full renders | Drive the `time` and `playing` signals explicitly every frame |
| `range` end is inclusive | Pass `[a, b−1]` for our end-exclusive `[a, b)` |
| Server-side audio and ffmpeg resolve `src` as `<outDir>/../public/<src>` | Stage assets as symlinks in `<work>/public` and set it as Vite's `publicDir` |
| `setVolume()` doesn't change the exported volume | Pass `volume` to the constructor and write the field each frame. Only one volume per clip is exported, so volume keyframes need `audio.mix` |
| `@revideo/vite-plugin` requires `@revideo/ui` but doesn't declare it | Added it as an explicit dependency |

## Preview (app) issues we work around — `app/src/Preview.tsx`
| Issue | Workaround |
|---|---|
| `@revideo/player-react` + React 19: `<revideo-player>` has getter-only `variables`/`width`/`height`/`fps`/`quality`, so every prop update after mount throws and the preview never updates | Add setters that write the observed attributes |
| `playing`/`looping` are plain fields; React writes the strings `"true"`/`"false"`, and `"false"` is truthy, so the preview autoplayed and the Play button never reached the element | Per-instance accessors that route React's strings through `setPlaying`/`toggleLoop` |
| Every `variables` change (each edit, late-resolving assets) reloads playback and the preview jumps to frame 0 | Remember the intended time and re-seek after each reload |
| The `currentTime` prop ignores changes under 0.05 s (one frame at 30 fps) | Seek with the element's `seekto` event; `seekAndSettle` waits for an actual render |
| WKWebView: video from a custom URL scheme, or a cross-origin one, stalls drawing after seeks | Serve media over loopback HTTP, same-origin with the page (Vite proxy in dev) |
| WKWebView: Revideo's detached `<video>` elements sometimes never fire `seeked`, freezing the preview after a seek (intermittent; Chrome is fine) | Attach media elements to a hidden in-document holder (helps, doesn't eliminate); stalls are detected and the preview offers "Reload preview" |

## Known limitations (Phase 0)
- Transitions are ignored (validation warns about them).
- The `ffmpeg` video decoder isn't tested; the default `web` decoder is used for mp4.
- Revideo bundles its own ffmpeg/ffprobe (`@ffmpeg-installer`, 4.4) for its audio step. Check that build's license before any distribution (spec §3.1).
- The mixed output sits about 3 dB below the source at volume 1, because of Revideo's mix stage. Final levels belong to `audio.mix`.
