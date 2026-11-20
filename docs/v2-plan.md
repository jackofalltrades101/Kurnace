# Kurnace v2 — rework plan

**Status:** proposal · **Date:** 2026-11-19 · supersedes the phase roadmap in `kurnace-spec.md` §17

Kurnace v1 does everything on the spec's checklist, and every automated test passes. It still isn't good to use: it's buggy, the UI fights you, files are opaque, too much is hard-coded, and agents and the person don't really share a workspace. This plan works from evidence (what actually happened in real projects) back to root causes, then lays out the rework.

---

## 1. Evidence

### 1.1 Agents route around Kurnace
In your three real projects (`~/Kurnace/projects`), agents built the actual video **outside** Kurnace and used it as a place to drop files:

| Project | Timeline | Where the video was actually made |
|---|---|---|
| `filly` | 2 clips: one pre-rendered video and its audio | ~1,900 lines of Python/numpy/PIL in `scenes/` (`build.py`, `shots.py`, `fx.py`, `consolidate.py`), with a private venv. It works around `gen.python` by re-launching itself in that venv. |
| `bs-with-mogger` | 2 clips: one baked frame sequence and its audio | `scenes/duckfight/`: a canvas animation in HTML/JS with a home-made `window.META` and `window.renderFrame(f)` contract, plus its own Puppeteer frame renderer and audio synth. After each change: re-import by hand and re-point clip IDs with `clip.set`. |
| `wideo-one` | 6 stills + 5 `solid` clips, one track per scene | Six HTML pages plus a `build.py` that screenshots them with the headless Chromium **inside the installed app bundle**, at a hard-coded path. |

The agents needed code-driven scenes: HTML/CSS/JS, canvas, Python. Kurnace's composition model offers media clips plus two built-in components (`text` and `solid`), and transitions aren't rendered. So each agent built a private pipeline, the recipe and variant system went unused, and you saw none of the work until a final file appeared.

### 1.2 The UI
From a session with the `wideo-one` project:
- The Library panel mixes all 80 assets from every project. Thumbnails collapse into thin strips with no names, so it's unusable.
- You can drag library items, but the timeline has **no drop handler**, so you can't drag an asset onto the timeline.
- There's no inspector, so clip properties, scene parameters and keyframe values have nowhere to be edited.
- The timeline structure fights you. The agent made one track per scene, and you then moved every clip onto "Scene 4" by hand (5 `clip.move` ops in the log), leaving the other tracks empty.
- Every timeline edit is an `op.apply` round trip, and the UI waits for the core's update before showing anything. Drags snap back, then jump.
- The toolbar packs everything into one row, including an inline text box for markers. There are no context menus.
- The 1,019-line `Timeline.tsx` is a single component with model, view and input handling mixed together.

### 1.3 Hard-coding and plugins
- **Module IDs referenced directly** (45 references across core, CLI and app):
  - `render.revideo` is hard-wired as the renderer.
  - Fish TTS has bespoke settings UI and backend commands.
  - Agent launchers (`claude`, `opencode`, `herdr`) are a constant list.
- **Fixed vocabularies:**
  - Track kinds are a hard-coded list in two languages (`ops.rs`, `packages/timeline`).
  - Renderer components are a `switch` with two cases.
- **Module runtimes aren't isolated.** Python modules run with the system `python3 -I` and can't declare dependencies (which is why `filly` hacked its own venv). Node modules share the repo's `node_modules`.
- **"Panel" modules (spec §9.4) don't exist.** No module can contribute UI, settings or agent tools.
- **There's no install, enable or disable.** Modules are directories found by walking up from the executable.

### 1.4 Agent ↔ person interaction
- Agents live in a terminal pane. The only way to point an agent at "this clip" is to type its ID.
- Agents can't see your selection, playhead, or what you're looking at. Presence exists in the core, but it isn't surfaced to agents as context.
- Agents can't ask you something in the UI, show you a frame, highlight a clip, or propose a change for you to accept. They print to a terminal.
- Agent work outside ops is invisible until it lands: scene code, generation jobs, renders. There's no jobs view and no live reload.
- Agents call `kurn` with hand-written JSON (`--args '{"clip":…}'`). There are no typed tools, so agents get schemas wrong.

### 1.5 Files
- **Everything lives in one global, content-addressed library.** Blobs have no extension or name on disk, projects reference opaque `ast_…` IDs, and nothing is organised per project or in folders.
- **Re-importing creates duplicates** (`fight.mp4` ×2, `gojo_void.mp4` ×2, `intro_vs.mp4` ×2) because changes don't flow back to the assets that use them.
- **No file watching.** A file an agent writes in `scenes/` or `renders/` doesn't exist to Kurnace until someone runs `kurn lib add`.
- **No rename, move, delete or "show in Finder"** from the app.

### 1.6 Revideo cost
Twelve documented workarounds: six in rendering, six in the preview, listed in `modules/render.revideo/README.md`. The WebKit seek stall can't be fully fixed from outside Revideo. And none of the agents chose to write Revideo; they wrote HTML, canvas and Python.

---

## 2. Root causes

1. **The composition model is too weak**, so the real work happens outside Kurnace. *(Causes 1.1 and most of 1.6.)*
2. **The UI is a read-only mirror driven by request/response.** Nothing is optimistic or local-first. *(Causes the lag and snap-back in 1.2.)*
3. **Extensibility is nominal.** Manifests exist, but every interesting surface is hard-coded: renderer, components, settings, UI, agent launchers, runtimes. *(1.3)*
4. **The person and the agent share a timeline, not a workspace.** Context, questions, proposals and progress have no channel except a terminal. *(1.4)*
5. **Files are an implementation detail** (a hidden content store) instead of something you can see and organise. *(1.5)*
6. **Quality is measured on pipelines, not on use.** Tests prove frame-accuracy and crash safety, while nothing exercises a person editing a real project.

---

## 3. Target design

### 3.1 Live scenes and a Kurnace compositor (replacing Revideo)
Make the thing agents already build the native unit of composition.

**Scene** = a folder in the project, `scenes/<name>/`, containing an entry file plus a manifest:
```toml
# scenes/title-card/scene.toml
type = "html"            # html | canvas | python | (plugin-provided types)
entry = "index.html"
duration = 90            # frames; may be a param
[params]
title = { type = "string", default = "Hello" }
accent = { type = "color", default = "#ffd866" }
```
**Contract** (what the duckfight agent invented, formalised): the page exports `kurnace.meta` and `kurnace.renderFrame(frame, params) → Promise<void>`, which must be a pure function of `(frame, params)`. Python scenes implement the same contract over stdin/stdout, writing raw frames.

**Compositor:** a small renderer page that lays out the timeline's layers (media, scenes, text, shapes) with CSS transforms and keyframes. The same page runs:
- **in the app** as the live preview. Scenes hot-reload when their files change.
- **in headless Chromium** for final renders: per frame, call every layer's `renderFrame`, then capture.

Media layers:
- **Video** uses `<video>` during playback. When paused or rendering, it uses exact frames decoded by ffmpeg and served by the asset server. That gives frame accuracy without WebKit's `seeked` bugs.
- **Audio** mixes with ffmpeg at render time and plays through Web Audio in the preview.

Caching: rendered scene frames are cached by `hash(scene files, params, frame)`. Editing one scene re-renders only that scene.

**Components = reusable scenes with params.** Titles, lower thirds and solids become scene templates that plugins can ship. This replaces the hard-coded `switch`.

**Why replace Revideo:**
- It's the source of most bugs in both rendering and preview.
- Agents don't write it.
- The spec kept the renderer behind `timeline.json` for exactly this case (§10.4).

The agents' own `render.cjs` shows the capture core is about 100 lines. The work is in layout, keyframes and media timing, which we already have from the TimelineRenderer.

### 3.2 Real-time, local-first editing
- **Optimistic edits.** The UI applies an op to a pending overlay immediately, sends it, and reconciles when the core's update arrives. If the core rejects it, the overlay rolls back and shows the reason inline.
- **Direct manipulation is continuous.** Drags and trims render every frame locally. Only the drop commits an op.
- **Timeline v2:**
  - Drag from the library or Finder onto a track. Context menus.
  - An inspector for clip properties, scene params and keyframes.
  - Track headers with mute, solo, lock and height.
  - Ripple, roll and slip trims. J/K/L and I/O shortcuts.
  - Rendered transitions, audio gain and fades, regions.
  - Split `Timeline.tsx` into model, view and input layers.

### 3.3 Files you can see
- **Each project owns a `media/` folder,** which is the source of truth for that project's assets:
  - Imports copy into `media/`, with real names and folders.
  - Generated outputs land in `media/generated/<module>/` with a `.recipe.json` sidecar.
  - Scenes live in `scenes/`.
- **A core file watcher** (Rust `notify` crate) keeps the index in sync. Files that appear are ingested, renames keep the same asset ID (path plus hash tracking), and deletes deprecate the asset.
- **The global library becomes a shared library** you publish to on purpose ("Save to shared library") for cross-project reuse. It's no longer the default dumping ground.
- **The Library panel** gets Project and Shared tabs, a folder tree, working thumbnails, rename, move, reveal in Finder, delete, and "used in…" references.

### 3.4 Plugins (everything is a plugin)
A **plugin** is a folder with `kurnace-plugin.toml` declaring **contributions**:

| Contribution | Replaces today's hard-coding |
|---|---|
| `generators` | current modules (tts, audio.clean, gen.python, …) |
| `scene-types`, `scene-templates` | the renderer's `text` and `solid` switch |
| `effects`, `transitions` | unrendered transitions |
| `panels` (sandboxed iframe + postMessage API) | the bespoke Fish settings UI |
| `settings` (schema → auto-built UI; secrets in Keychain) | `fish_save` / `fish_status_cmd` commands |
| `agent-tools` (auto-exposed over MCP and in AGENTS.md) | hand-maintained docs |
| `agent-launchers` | the `CANDIDATES` constant |
| `importers`, `exporters` | export presets |

- **Built-ins move into `plugins/`** and use the same API, so dogfooding keeps the API honest.
- **Isolated runtimes:** each Python plugin gets its own venv managed by `uv` from its declared dependencies. Node plugins install their own dependencies.
- **Management:** `kurn plugin add <path|git>`, `list`, `enable`, `disable`, with per-project enablement in `project.toml`.

### 3.5 Agents and the person in one workspace
- **MCP server (`kurn mcp`)** as the primary agent interface. Claude Code and OpenCode both speak MCP. Tools are generated from the op catalog and plugin contributions, so they're typed with no hand-written JSON. The CLI stays for scripts.
- **Shared context:**
  - `context.get` returns your selection, playhead, visible range, focused panel and recent actions.
  - Right-click → **Ask agent…** sends the selection, as a reference, into the agent's session.
- **Agent → UI tools:**
  - `ui.ask` shows a question card with choices, and the agent waits for your answer.
  - `ui.notify` shows a toast or activity entry.
  - `ui.show` moves your playhead or view, or highlights a clip or asset.
  - `ui.propose` shows a change set as ghost clips that you **accept or reject**.
- **Visible work:**
  - A jobs view lists generation and render jobs, with progress and cancel.
  - Scene edits hot-reload in the preview.
  - Agent status (working, waiting on you, idle) comes from Claude Code hooks or Herdr.

### 3.6 Quality loop
- **A bug list from real use,** triaged and kept in the repo.
- **End-to-end UI tests on real projects.** The headless harness drives realistic edit sessions: drag, trim, undo, agent edits arriving mid-drag.
- **Performance budgets:** timeline at 60 fps with 500 clips; edit-to-pixel under 16 ms locally; scene hot reload under 300 ms.
- **Errors surface in the UI** with the context needed to fix them; nothing fails silently into a log.

---

## 4. Phases

Ordered by leverage. Each phase ends usable and has an exit test based on a real workflow.

| # | Phase | Scope | Exit test |
|---|---|---|---|
| **A** | Stabilize | Triage your bug list; fix crashers and data-loss bugs; working library thumbnails; error toasts; e2e harness on real projects | Your top-10 bugs fixed; harness edit session on `wideo-one` passes |
| **B** | Compositor + live scenes | Scene contract (html, canvas), compositor page (preview + headless render), frame-exact media, scene cache, hot reload; port `text`/`solid` to scene templates; migrate `duckfight` as the reference scene | `duckfight` and `wideo-one` rebuilt as native scenes; edit a scene file → preview updates in < 300 ms; final render matches |
| **C** | Real-time timeline | Optimistic overlay, timeline split into model/view/input, drag-and-drop, inspector, context menus, track headers, trims, transitions | Scripted 2-minute edit session with an agent editing concurrently: no snap-back, nothing lost |
| **D** | Files | Project `media/`, watcher, rename/move/reveal/delete, shared library, migration of existing projects | Drop a folder into `media/` → appears organized; rename in Finder keeps clips linked |
| **E** | Agent co-presence | `kurn mcp`, context, ask/notify/show/propose, jobs view, agent status | Agent asks a question in the UI, you answer, agent proceeds; a proposal is accepted by click |
| **F** | Plugins | Manifest + contributions, registry, isolated runtimes, built-ins migrated, settings schema UI, `kurn plugin` | Add a third-party plugin from a git URL that contributes a generator, a scene template and a panel, with no core changes |
| **G** | Shell polish | Resizable panels, command palette, design pass, release build | Signed `.app` installs and runs a full project without a dev server |

B comes before C because the timeline's clip model changes once scenes become first-class.

---

## 5. Decisions (2026-11-19)

1. **Replace Revideo with the Kurnace compositor** (§3.1). Accepted.
2. **Project `media/` is the source of truth for files** (§3.3), with the global store as the content cache and opt-in sharing. Accepted.
3. **MCP plus UI tools is the primary agent interface** (§3.5); the CLI stays. Accepted.
4. **Start with Phase B** (compositor and live scenes). Phase A bug triage runs alongside, as you hit bugs.

---

## 6. Progress

### Phase B: compositor and live scenes (2026-11-19). Core done.

**Built:**
- **Scene runtime** (`packages/compositor/scene-runtime.js`). Virtual clock, CSS and Web Animations paused at frame time, seeded randomness, `kurnace.scene()`, the legacy `window.renderFrame`, and a postMessage protocol.
- **Compositor** (`packages/compositor/src/compositor.ts`). One DOM renderer for both the app preview and final renders. Layers: image, exact video frame (ffmpeg), frame sequence, HTML scene (iframe), text, solid, audio. Uses v1's transform and keyframe semantics.
- **`render.compositor`** module, which replaces `render.revideo`. Headless Chromium, frames streamed to ffmpeg, parallel browsers, ffmpeg audio mix, BT.709 tagging.
- **Core:**
  - Scene clips (`scene` + `params`) in the Rust and TypeScript models, ops and validation.
  - `scenes.list` and `scene.new`; `kurn scene list|new`.
  - A scenes/ watcher that pushes `scene` events.
  - `AGENTS.md` documents the scene contract.
- **App:**
  - The preview runs on the compositor, with scrubbing, exact settle and a play/pause transport.
  - Hot reload, and live scene errors in the preview.
  - Asset server routes for exact frames, scenes and the runtime.
  - Revideo is removed from the app build, along with its React 19 / WebKit workarounds.
- **Revideo is deleted:** `modules/render.revideo` and `scripts/parity.sh`.

**Verified:**
- **Video frames are exact** in stills rendered out of order and in full renders (`modules/render.compositor/test`).
- **The scene contract works:** CSS animation at the exact pixel, `kurnace.scene` with params, the legacy contract, determinism, and errors failing the render.
- **duckfight** (bs-with-mogger) renders natively and is **pixel-identical** to the frames its agent baked with its own renderer, with the score mixed in.
- **wideo-one's** HTML pages work as scenes unchanged.
- **The real app (WKWebView)** draws an image, an exact video frame and a scene through the Tauri asset server, and shows a scene error on hot reload (`scripts/phase-b-webkit.sh`).
- **Every earlier suite passes on the new renderer:** phases 1, 3, 4, 5 and 6, and the Phase 2 live test.

**Still open in Phase B:**
- **Render speed.** About 11–13 fps for a heavy 1080p canvas scene on 8 cores, about 25 fps for simple scenes. Next steps: canvas read-back instead of screenshots for canvas-only scenes, and reusing browsers across renders.
- **No scene-frame cache yet.** Edits re-render whole ranges.
- **Python scenes** (frame generators speaking the same contract).
- **Supersampled typography** (render at 2× and downscale), as the wideo-one agent did by hand.
- **`text` and `solid` are built into the compositor** rather than shipped as scene templates (moves to Phase F with plugins).
- **Hot-reload latency hasn't been measured precisely** (target < 300 ms; observed "about a second" end to end in the WebKit test, including the core round trip).
- **`kurnace-spec.md` §10 still describes Revideo.**

### Phase C: real-time timeline (2026-11-19). Core done.

**Built:**
- **Optimistic edits** (`app/src/optimistic.ts`, plus `packages/timeline/src/apply.ts`, a TypeScript mirror of the op semantics):
  - Every edit shows on the next paint in the timeline, the inspector and the preview.
  - An edit is dropped from the overlay once the core's update includes it. A rejected edit rolls back and shows the reason as a toast.
  - Destructive agent ops that wait for approval aren't shown as done.
- **Fixed: a fast drag-and-release lost the last movement**, so the clip landed short of the cursor. The drag ref now updates synchronously.
- **Drag and drop onto the timeline:**
  - Library assets and project scenes land on the track and frame under the cursor.
  - An incompatible, locked or occupied spot gets a new track of the right kind.
  - Several items are laid end to end.
  - Files dropped from Finder onto the timeline are imported, then placed.
  - A drop indicator shows the timecode and "new track".
- **The Library lists project scenes** (draggable, refreshed on hot reload).
- **Inspector:**
  - Timing (start, length, in).
  - Transform at the playhead, with ◆ keyframe toggles; keyframed props are edited as keys.
  - Fit and volume.
  - Scene params built from the `scene.toml` schema (string, number, color, bool, with reset to default).
  - Text and solid properties, and "Show scene files".
- **Context menus:**
  - Clip: split at playhead, duplicate, transitions into the next clip, show scene files, copy ID, delete, ripple delete.
  - Track header: mute, lock, add track above or below, delete.
  - Background: add a marker here, add a track.
- **Transitions are rendered** in the compositor (preview and renders): crossfade (a proper dissolve with no dip in brightness), dip to black, wipe and slide, centred on the cut. They're drawn on the timeline as a bow-tie.
- **Test hooks:** `window.__kurnView` and `window.__kurnTimelineUI.point()`, plus harness `--script` and `--click`.

**Verified:**
- **`scripts/phase-c-session.sh`**, a real-mouse session in the UI against the real core with an agent adding 15 markers concurrently:
  - A dragged clip shows at its new position on the first frame after release and never snaps back.
  - An inspector edit applies immediately.
  - An overlapping edit rolls back with an explanation.
  - A scene dropped from the library lands at the dropped track and frame.
  - At the end the UI and the core agree exactly, and all agent markers are present.
- **Transition pixel tests** in `modules/render.compositor/test`.
- **Every earlier suite still passes.**

**Still open in Phase C:**
- **`Timeline.tsx` hasn't been split** into model, view and input layers (about 1,200 lines now).
- **Only basic trims exist.** Roll and slip trim modes aren't implemented; ripple exists only as ripple delete.
- **No J/K/L shuttle, I/O range, solo, track rename/reorder by drag, or track heights.**
- **Transitions don't fade audio.** Overlapping media audio isn't crossfaded in renders.
- **Scene params can't be keyframed** (only transform props can).
- **Selection after an optimistic add is lost.** A new clip carries a temporary local ID until the core's ID arrives, so a selection made in that window drops.
- **Finder drops onto the timeline are only tested in the headless harness,** not in WKWebView.

