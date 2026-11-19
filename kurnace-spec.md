# Kurnace — Technical Specification

**Status:** Draft v0.3 · **Date:** 2026-11-07 · **Owner:** Gubran Dormoush
**Name:** Kurnace (app) · `kurn` (CLI)

### Changelog
- **v0.3 (2026-11-07):** **Fully local, single machine.** There is no home server for now.
  - The local core is the single writer of everything. It hosts the Yjs docs, the library index and blob store, the op log, snapshots and git commits.
  - The UI syncs with the core over Tauri IPC. `kurn` talks to the core over a local socket.
  - The library lives on local disk.
  - Multi-machine collaboration moves to §16 as future work. The data model keeps actors, Yjs and ULIDs so adding it later is additive.
- **v0.2 (2026-11-07):**
  - Ops became an intent API compiled to Yjs transactions, with derived ops for embedded-editor edits.
  - Clips became an ID-keyed CRDT layout.
  - Added rational fps and sample-offset rules.
  - Separated assets from content blobs, with tree hashes for folder assets.
  - Added the `batch` op, pending-op confirmation, the `yrs` core, agent file-rule enforcement, the budget's scope, the `--set` syntax, and a preview/render parity check.
  - (Its home-server design is superseded by v0.3.)
- **v0.1 (2026-11-07):** Initial draft.

---

## 1. Overview

A desktop application where a user and one or more AI agents (Claude Opus 5.5, DeepSeek) co-create videos **in real time, on the same project, at the same time**, all on one machine.

Videos are built **programmatically**: assets, audio, and timelines are produced by code and parameters, not by end-to-end generative AI. Every asset has a visible, re-runnable recipe.

### 1.1 Goals
- One centralized workspace: whiteboard → assets → timeline → render.
- Humans and agents use the **same operations** through the **same interfaces** (UI, CLI, agent tools).
- Modular: new tools plug in through a manifest without touching the core.
- An ever-growing, searchable, reusable asset library shared across projects.
- A timeline that is powerful but lightweight.
- **Fully local:** no server, no account, and it works offline. External APIs (Fish TTS, LLMs) are optional modules.
- Permissive licenses only (MIT / Apache 2.0 / BSD / public domain).

### 1.2 Non-goals (v1)
- Pure generative video (text-to-video models).
- Public distribution / SaaS. Internal tool.
- **Multi-machine collaboration** (shared server, remote users, sync between computers). Deferred; see §16.
- Full NLE parity (effects stacks, nested sequences, color grading suites).
- Mobile clients.

---

## 2. Design Principles

1. **Everything is code or data.** Briefs, storyboards, timelines, boards, and asset recipes are text files under version control.
2. **One operation layer.** Every mutation (move a clip, add an asset, draw a box) is a typed operation. UI, CLI, and agents all emit the same ops. Edits made inside embedded editors are recorded as derived ops (6.1).
3. **Recipes over outputs.** A generated asset stores the module + params that made it, so it can be regenerated or varied.
4. **The core stays small.** Rendering, TTS, audio, 3D, and Godot are all modules.
5. **Agents see what they make.** Agents can render stills/clips and inspect them; that feedback loop is first-class.
6. **Pin everything.** Pre-1.0 dependencies (Revideo, y-excalidraw) are version-pinned and wrapped behind interfaces.
7. **One writer.** The local core is the only process that writes project docs, the library index, the op log, snapshots, and git commits. Everything else (the UI, `kurn`, agents) goes through it.
8. **Collaboration-ready, not collaboration-dependent.** Actors, Yjs docs, and globally unique IDs are kept even though v1 has one machine, so adding multi-machine sync later doesn't require migrating data.

---

## 3. Technology Stack & Licensing

| Layer | Choice | License | Notes |
|---|---|---|---|
| Desktop shell | Tauri 2 | MIT / Apache 2.0 | Rust backend, system webview UI (WKWebView / WebView2 / WebKitGTK) |
| UI framework | React + TypeScript | MIT | |
| Agent sessions | Herdr (sidecar binary) | Apache 2.0 | Controlled via socket API/CLI; keep LICENSE/NOTICE |
| Terminal panes | xterm.js | MIT | Attached to Herdr panes |
| Render engine | Revideo (`@revideo/*`) | MIT | Pre-1.0 → pin version; set `DISABLE_TELEMETRY=true` |
| Preview | Revideo `<Player/>` | MIT | Embedded in preview panel |
| CRDT (UI side) | Yjs + y-protocols | MIT | Webview docs, synced to the core over Tauri IPC (no websocket server) |
| CRDT (core) | `yrs` (y-crdt) | MIT | Rust port of Yjs, wire-compatible; the core hosts the authoritative docs; fallback: Node sidecar (see Q6) |
| Whiteboard | Excalidraw + y-excalidraw | MIT | Pin/vendor y-excalidraw (small community pkg) |
| Asset index | SQLite | Public domain | Local disk; opened only by the core |
| 3D | three.js | MIT | Rendered to frame sequences or live canvas |
| Game-engine shots | Godot 4 (`--write-movie`) | MIT | Headless frame-sequence export |
| Scripting | Python 3 | PSF | Generator modules |
| Audio cleanup | ffmpeg filters + DeepFilterNet | LGPL* / MIT-Apache | See 3.1 |
| TTS | Fish Audio API | Commercial API | Swappable module; local TTS later |
| LLM agents | Claude Code, OpenCode (DeepSeek) | Tools, not bundled | Run inside Herdr panes |

### 3.1 License caveats
- **ffmpeg:** use an LGPL build. `libx264` is GPL. That is acceptable for internal use with no distribution, but prefer hardware encoders (VideoToolbox/NVENC/QSV/VAAPI) or OpenH264 if the app is ever distributed.
- **Excluded:** Remotion (team-size license; contractors count from 5.0), tldraw (production license key), Spotify pedalboard (GPLv3).
- **Policy:** any new dependency must be MIT/Apache/BSD/ISC/public domain, or an external API. Record it in `LICENSES.md`.

---

## 4. Architecture

```
┌──────────────────────── Tauri Desktop App (UI / webview) ──────────────┐
│  Board  │  Asset Library  │  Preview (Revideo Player)  │  Agent Panes  │
│                         Timeline (canvas)                              │
└───────────────────────────────▲───────────────────────────▲────────────┘
              Tauri IPC: Yjs sync│msgs + JSON-RPC            │ xterm.js
                    ┌────────────┴─────────────┐        ┌────┴──────────┐
                    │  Core (Rust, in Tauri)   │        │  Herdr        │
                    │  - Doc host (yrs)        │        │  sidecar      │
                    │  - Op layer + oplog      │        │  - Claude Code│
                    │  - Library (SQLite+blobs)│        │  - OpenCode   │
                    │  - Module registry       │        │  - watchers   │
                    │  - Job runner, event bus │        └────┬──────────┘
                    │  - Snapshots + git       │◄────────────┘
                    └────────────┬─────────────┘  `kurn` CLI over local socket
                                 │ spawns
                    ┌────────────┴─────────────────────────────┐
                    │  Modules (any language, JSON stdin/out)  │
                    │  tts.fish · audio.clean · render.revideo │
                    │  gen.python.* · three.render · godot.*   │
                    └──────────────────────────────────────────┘
```

### 4.1 Components
| Component | Responsibility |
|---|---|
| **Core (Rust, in Tauri)** | The single writer. It hosts the authoritative Yjs docs via `yrs` and persists them. It validates and applies ops, writes the oplog, and owns the library (SQLite index and blob store). It also runs the module registry, job queue, and event bus, exports snapshots, and makes git commits |
| **UI (webview)** | React app. Keeps a Yjs replica of each open doc and syncs it with the core over Tauri IPC using y-protocols sync messages. Explicit ops go to the core as JSON-RPC |
| **`kurn` CLI** | Thin client over the core's local socket API. Used by humans and agents. Supports `--json` output for agents |
| **Headless core (`kurn daemon`)** | The same core with no window, so agents can work while the app is closed. Only one core can hold a project at a time (see 4.2) |
| **Herdr sidecar** | Hosts local agent sessions + long-running watchers; persists across app restarts |
| **Modules** | All real work: TTS, cleanup, generation, rendering, export |

### 4.2 Local transport and locking
- **Socket:** the core listens on `~/.kurnace/core.sock` (a Unix domain socket with mode `0600`) on macOS and Linux, and on a per-user named pipe on Windows. There is no TCP port and no auth token; access is limited to the OS user.
- **Project lock:** a core opening a project takes `projects/<slug>/.ws/lock`, which holds its PID and socket path. If the app is launched while a headless core holds the project, it attaches to that core instead of opening a second writer.
- **Library lock:** the library has its own lock, held by whichever core process is running. Only one core runs per user at a time; it can have several projects open.

---

## 5. Project Structure

```
~/Kurnace/                     # default root (configurable in ~/.kurnace/config.toml)
  library/                     # shared across projects, local disk
    blobs/ab/ab3f…             # content-addressed blobs (files or tree manifests)
    recipes/                   # recipe JSON per generated asset
    index.sqlite               # opened only by the core
    thumbs/  peaks/            # previews, waveform peaks

  projects/<project-slug>/
    project.toml               # name, fps, resolution, render defaults
    brief.md                   # idea, tone, references
    storyboard.yaml            # scenes, shots, VO lines, framing notes
    board.excalidraw           # whiteboard snapshot (written by core; Yjs is live source)
    timeline.json              # timeline snapshot (written by core; Yjs is live source)
    scenes/                    # custom Revideo scene/components
    modules/                   # project-local modules (optional)
    renders/                   # stills, drafts, finals
    .ws/
      docs/                    # persisted Yjs updates per doc (+ periodic compaction)
      oplog.jsonl              # op log
      lock                     # project lock (4.2)
      cache/
    AGENTS.md                  # project conventions for agents

  modules/                     # global modules
    tts.fish/ module.toml  run.py
    audio.clean/ module.toml  run.py
    render.revideo/ module.toml  index.ts
    ...
```

- Projects are git repos. They reference assets by ID and never copy them.
- **Library location:** it must be on a local disk. Don't put it in a network share or a cloud-synced folder (iCloud Drive, Dropbox, OneDrive, Google Drive): file sync and network filesystems break SQLite's locking and can corrupt the index. On startup the core warns if the library path appears to be inside one of these.
- `.ws/docs/` is the live source of truth for the docs. `timeline.json` and `board.excalidraw` are readable exports. `.ws/` is git-ignored except for `oplog.jsonl` (optional; see `project.toml`).

---

## 6. Operation Layer

Ops are the **intent API**. The core validates an op and compiles it into a single Yjs transaction on the authoritative doc. That transaction's `origin` carries the actor and op ID. The core then appends the op to `.ws/oplog.jsonl` and broadcasts it on the event bus. The UI receives the resulting Yjs update over IPC.

```json
{
  "id": "op_01J…",
  "actor": { "type": "agent", "name": "claude", "session": "herdr:pane-3" },
  "ts": "2026-11-07T23:30:00Z",
  "doc": "timeline",
  "type": "clip.move",
  "args": { "clip": "c_12", "track": "v2", "start": 184 },
  "derived": false
}
```

### 6.1 Explicit vs. derived ops
- **Explicit ops** come from the CLI, from agents, and from UI actions that have an op form (timeline edits, library actions). The core validates them before applying them.
- **Derived ops** are recorded for edits that happen inside embedded editors and write the webview's Yjs replica directly, such as Excalidraw through y-excalidraw. The webview sends the Yjs update together with a derived-op description (e.g. `el.update`, the human actor). The core applies the update and logs the op with `derived: true`. Derived ops are not validated before they apply; they exist for attribution, the activity feed, and per-actor undo.

### 6.2 Op catalog (v1)
| Doc | Ops |
|---|---|
| timeline | `track.add/remove/reorder`, `clip.add/remove/move/trim/split/set`, `keyframe.set/remove`, `marker.add/remove`, `transition.set`, `ripple.delete` |
| board | `el.add/update/remove`, `frame.add`, `link.asset`, `link.scene` |
| storyboard | `scene.add/update/remove/reorder`, `shot.add/update/remove` |
| library | `asset.add/tag/untag/deprecate`, `asset.variant` (runs a recipe) |
| any | `batch` — an ordered list of ops applied as **one Yjs transaction and one undo step**; it is rejected as a whole if any child op fails validation |

Composite actions such as ripple delete, `board to-timeline`, multi-select moves, and splits across tracks are always emitted as a `batch` (or as a dedicated op that compiles to one transaction).

### 6.3 Rules
- **Validation is authoritative.** Every explicit op passes through the single core, so it is validated against the current doc before it is applied. Ops are applied one at a time, so ops from the UI and from several agents never race inside the core. Derived ops (6.1) are checked against schema invariants after they apply; violations are flagged in the UI and are not silently fixed.
- **Undo/redo** uses Yjs `UndoManager` with `trackedOrigins` scoped per actor, so your undo never reverts an agent's edit. The UI's undo manager tracks the human origin. The core keeps a separate undo manager for each agent actor, exposed as `kurn undo` / `kurn redo`.
- **Attribution:** every op has an actor; the UI can color/filter changes by actor.
- **Agent guardrails:** if `project.toml` sets `agent_confirm_destructive = true` (the default), destructive ops need human confirmation. Destructive means `*.remove` on more than N items, `track.remove`, and `ripple.delete` across locked boundaries. Behaviour while waiting:
  1. `kurn` returns right away with `{"status":"pending","op":"op_…"}` and exit code `3`. Nothing is applied.
  2. A confirmation prompt appears in the app. If the app isn't open, the prompt waits for the next launch and the op is listed by `kurn op pending`.
  3. The agent polls `kurn op status <id>` or blocks with `kurn op wait <id> --timeout 300`. The result is `applied`, `rejected`, or `expired` (the default expiry is 10 min).
  4. If the doc has changed since the op was proposed, it is re-validated when confirmed. If it no longer validates, it fails with `stale`.
- **Persistence:** every applied Yjs update is appended to `.ws/docs/<doc>.updates` before the op is acknowledged. The file is compacted into a single state snapshot on idle and on shutdown.
- **Git:** the core exports snapshots (`timeline.json`, `board.excalidraw`) and auto-commits when idle (e.g. 30 s without ops), with a message summarising the actors involved. Hand-edited files (`scenes/`, `storyboard.yaml`) are committed as usual by the user or an agent.

---

## 7. Timeline

### 7.1 Data model
```json
{
  "version": 2,
  "fps": { "num": 30, "den": 1 },
  "width": 1920, "height": 1080,
  "duration": 1800,
  "tracks": [
    { "id": "v1", "kind": "video", "name": "Background", "order": 0, "muted": false, "locked": false },
    { "id": "a1", "kind": "audio", "name": "VO", "order": 1, "muted": false, "locked": false }
  ],
  "clips": {
    "c_01": {
      "track": "v1",
      "asset": "ast_01J…",
      "start": 0, "in": 0, "out": 240,
      "props": { "x": 0, "y": 0, "scale": 1, "opacity": 1 },
      "keyframes": {
        "opacity": [{ "t": 0, "v": 0, "ease": "easeOutCubic" }, { "t": 15, "v": 1 }]
      },
      "component": null
    },
    "c_02": {
      "track": "a1",
      "asset": "ast_01K…",
      "start": 30, "in": 0, "out": 450,
      "offset_samples": 0
    }
  },
  "markers": { "m_01": { "t": 120, "label": "beat drop" } },
  "transitions": { "tr_01": { "between": ["c_01", "c_02"], "type": "crossfade", "dur": 12 } }
}
```

### 7.2 Time units
- All timeline values (`start`, `in`, `out`, `duration`, keyframe `t`, marker `t`, transition `dur`) are **integer frames at the project fps**. There is no floating-point time.
- `in` and `out` are offsets **into the source**, still counted in project frames. Clip length is `out - in`. For example, `in = 30` at 30 fps means "start 1 s into the source", whatever the source's native frame rate.
- Frame rates are stored as rationals (`num/den`, e.g. 30000/1001). Source time in seconds is `in × den / num`. The renderer resolves that time to the nearest source frame and never resamples timing.
- Audio is placed at frame boundaries. For sub-frame sync, an audio clip may set `offset_samples`, a signed number of samples at the source sample rate added after frame placement. Mixing is sample-accurate and never rounded to frames.
- Changing the project fps is a migration: the core rescales every frame value and rounds it, then writes the result as one `batch` op.

### 7.3 CRDT layout
Even on one machine, the webview replica and the core doc can both change before they sync (a human dragging a clip while an agent's op lands). The layout keeps those concurrent edits from conflicting. It is also ready for multi-machine sync later.
- `clips`, `markers`, and `transitions` are `Y.Map`s keyed by ID. A clip's track is the `track` field, and its order within the track comes from `start`. Moving a clip between tracks is a field update, not a delete plus insert, so two concurrent moves cannot duplicate the clip.
- `tracks` is a `Y.Map` keyed by ID. Order comes from a fractional `order` field, so reordering is a single field write.
- `props` and each keyframe lane are nested `Y.Map`s / `Y.Array`s, so concurrent edits to different props merge cleanly.
- Overlap between clips on a track is prevented when the core validates an op. Any overlap left by a concurrent UI edit is flagged (6.3).
- `timeline.json` is the snapshot form of the same structure.

### 7.4 UI requirements (v1)
- Canvas-rendered (no DOM node per clip). Target: 60 fps scroll/zoom with 500+ clips.
- Snapping (playhead, clip edges, markers), ripple delete, razor/split, multi-select.
- Keyframe lanes per animatable prop with an easing picker.
- Precomputed waveform peaks and thumbnail strips (generated on asset ingest).
- Presence: show each agent's playhead and selection alongside the user's. Agents publish them through the core (`kurn presence set …`), and the core relays them to the UI as Yjs awareness.

### 7.5 Deferred
Effects stacks, nested sequences, color grading, multicam.

---

## 8. Asset Library

### 8.1 Storage
- **Blobs** are content-addressed: `sha256` → `blobs/<2-char>/<hash>`, so identical bytes are stored once.
- **Directory assets** (`seq` frame sequences, `godot` projects, three.js scenes with textures) are stored as a **tree manifest**: a canonical JSON list of `{relative_path, sha256, size}`, sorted by path. Each file is stored as its own blob, and the manifest is hashed and stored as a blob too. The manifest's hash is the asset's content hash.
- **Assets** are records that point at a blob. Asset ID: `ast_<ULID>`. IDs are assigned at creation and **stable forever**. They are *not* derived from the content hash, so two imports of the same bytes with different names or licenses are two assets sharing one blob. ULIDs also stay unique if libraries from different machines are merged later (§16).
- **Ownership:** only the core opens `index.sqlite`, in WAL mode. Other processes (modules, agents, the UI) reach the library through the core's API, never by opening the database file.

### 8.2 Index schema (SQLite)
```sql
CREATE TABLE blobs (
  hash TEXT PRIMARY KEY,       -- sha256 of file bytes, or of tree manifest
  is_tree INT NOT NULL DEFAULT 0,
  size INT NOT NULL,
  created_at TEXT
);
CREATE TABLE assets (
  id TEXT PRIMARY KEY,         -- ast_<ULID>
  blob_hash TEXT NOT NULL REFERENCES blobs(hash),
  kind TEXT NOT NULL,          -- image|video|audio|font|model3d|godot|component|seq
  name TEXT,
  mime TEXT,
  width INT, height INT,
  duration_frames INT,         -- at the asset's native rate
  fps_num INT, fps_den INT,    -- native rate (rational)
  sample_rate INT,             -- audio
  source TEXT NOT NULL,        -- generated|imported
  recipe_id TEXT,              -- NULL if imported
  parent_id TEXT,              -- set when this is a variant
  license TEXT,                -- for imported assets
  created_by TEXT, created_at TEXT,
  deprecated INT DEFAULT 0
);
CREATE INDEX assets_blob ON assets(blob_hash);
CREATE TABLE tags (asset_id TEXT, tag TEXT, PRIMARY KEY (asset_id, tag));
CREATE TABLE usage (asset_id TEXT, project TEXT, ref TEXT);
CREATE TABLE recipes (
  id TEXT PRIMARY KEY,
  recipe_hash TEXT NOT NULL,   -- hash of canonical {module, version, params, inputs, seed}
  module TEXT, module_version TEXT,
  params_json TEXT, inputs_json TEXT,   -- inputs = other asset IDs
  seed INT
);
CREATE INDEX recipes_hash ON recipes(recipe_hash);
-- v2: embeddings table for semantic search (local model)
```

### 8.3 Recipes and variants
- Generated assets store `{module, version, params, input asset IDs, seed}`.
- **Recipe cache:** if a module declares `deterministic = true` in its manifest, re-running an identical recipe (same `recipe_hash`) returns the existing asset and does not run the module again. Non-deterministic modules, such as API TTS, always run. Pass `--cached` to reuse the last output anyway.
- **Variants:** `kurn lib variant <id> --set …` re-runs the recipe with overrides and creates a new asset whose `parent_id` points at the original.
- **`--set` syntax:**
  - `key=value` always sets an absolute value of the input's type, so `target_lufs=-6` means −6.
  - For `number` inputs only, relative forms are allowed: `key+=n`, `key-=n`, `key*=n`. They apply to the parent recipe's value (or the input's default).
  - Results are clamped to the manifest's `min`/`max`, and the clamp is reported in the JSON output.
  - Unknown keys are an error.

### 8.4 Ingest pipeline
On add: hash (or tree-hash) → copy into `blobs/` (skipped when the blob already exists) → probe (ffprobe: rational fps, sample rate) → thumbnail/peaks → create asset record → event `asset.added`.

### 8.5 Agent access
```
kurn lib search "dusk gradient" --kind image --json
kurn lib get ast_01J… --json
kurn lib add ./file.png --tag bg,dusk
kurn lib variant ast_01J… --set hue+=20
```

---

## 9. Module System

### 9.1 Manifest (`module.toml`)
```toml
id = "tts.fish"
version = "0.1.0"
kind = "generator"            # generator | panel | renderer | exporter | agent-tool | watcher
name = "Fish TTS"
description = "Generate voiceover from text via Fish Audio API"
runtime = "python"            # python | node | bin | godot
entry = "run.py"
license = "MIT"
deterministic = false         # true → identical recipes hit the cache (8.3)

[requires]
env = ["FISH_API_KEY"]
bins = ["ffmpeg"]

[inputs]
text  = { type = "string", required = true }
voice = { type = "string", default = "narrator-1" }
speed = { type = "number", default = 1.0, min = 0.5, max = 2.0 }

[outputs]
audio = { type = "asset", kind = "audio" }

[ui]
icon = "mic"
category = "Audio"

[agent]
expose = true
cost_hint = "api"             # local | api | gpu-heavy
cost_estimate_usd = 0.002     # optional, per run; used for budget tracking (14.5)
```

### 9.2 Protocol
- The core spawns the module, writes a JSON request to stdin, and reads newline-delimited JSON events from stdout:
  - `{"type":"progress","pct":40,"msg":"synthesizing"}`
  - `{"type":"log","level":"info","msg":"…"}`
  - `{"type":"output","name":"audio","path":"/tmp/…/vo.wav"}`
  - `{"type":"cost","usd":0.0021}` (optional, the actual spend)
  - `{"type":"done"}` or `{"type":"error","msg":"…"}`
- Output files are ingested into the library automatically, with the recipe attached.
- Modules get a temp working dir and the project root (read-only by default). Input assets are passed as paths to blobs, which modules must treat as read-only.
- **No real sandbox in v1.** Modules run as the user with that user's permissions; "read-only" is a convention, not an enforced boundary. Only install modules you trust. Real sandboxing (macOS `sandbox-exec`, Linux bubblewrap/landlock) is a prerequisite for agent-authored modules (Q5).

### 9.3 One manifest → three surfaces
| Surface | Generated from manifest |
|---|---|
| UI | Auto-built form from `[inputs]`, listed under `[ui].category` |
| CLI | `kurn run tts.fish --text "…" --voice narrator-1` |
| Agents | Listed in `kurn modules --json` and in the generated `AGENTS.md` tool section |

### 9.4 Module kinds
| Kind | Purpose | Example |
|---|---|---|
| generator | Produces assets | `tts.fish`, `gen.gradient`, `three.render` |
| renderer | Timeline → video | `render.revideo` |
| exporter | Post-render packaging | `export.youtube-preset`, `export.vertical` |
| panel | Custom UI panel (web component) | `panel.color-palette` |
| agent-tool | Helpers only agents call | `inspect.frame-diff` |
| watcher | Long-running, reacts to events | `watch.storyboard-tts` |

### 9.5 Initial module set (v1)
`tts.fish`, `audio.clean`, `audio.mix`, `render.revideo`, `render.still`, `gen.python` (generic script runner), `three.render`, `godot.render`, `import.media`, `export.mp4`.

---

## 10. Rendering (Revideo)

### 10.1 TimelineRenderer
A single generic Revideo project that reads `timeline.json` (passed as a project variable) and builds the scene graph:
- Media clips → `<Video>`, `<Img>`, `<Audio>` nodes, placed by `start/in/out` (converted to seconds using the rational fps, 7.2).
- Component clips → imported from `scenes/` or the library, with props bound.
- Keyframes → tweens with the specified easing.
- Transitions → built-in helpers (crossfade, slide, wipe) implemented once.

Custom per-video code lives in `scenes/` as components, never as one-off timeline JSX.

Before a render, the core exports a fresh `timeline.json` from the live doc, so the renderer never reads a stale snapshot.

### 10.2 Commands
```
kurn render still --frame 240 --out renders/f240.png     # agent feedback loop
kurn render draft --range 0-300 --scale 0.5              # fast preview
kurn render final --preset 1080p30
```

### 10.3 Configuration
- `DISABLE_TELEMETRY=true` set by the core for all Revideo processes.
- Revideo version pinned in `render.revideo/package.json`; upgrades are a deliberate PR.

### 10.4 Preview / render parity
- Final renders and `kurn render still` run in **headless Chromium**. The in-app `<Player/>` runs in the **system webview**: WKWebView on macOS, WebView2 (Chromium) on Windows, WebKitGTK on Linux.
- Known divergence risks: codec support (WebKitGTK depends on GStreamer plugins), WebCodecs availability, font rendering, and canvas/filter differences.
- Rule: **headless Chromium output is the source of truth**. Agents always judge visuals from `kurn render still`, never from the preview.
- Phase 0 must check that the Player plays a multi-track timeline in Tauri on every target OS. If a platform falls short, its fallback is to show draft renders streamed from the core (frames or a short MP4) instead of the live Player.

### 10.5 Risk mitigation
The renderer is a module behind the timeline schema. If Revideo stalls, a replacement renderer only has to consume `timeline.json`.

---

## 11. Audio Pipeline

```
storyboard.yaml VO line
   → tts.fish            (raw.wav)
   → audio.clean         DeepFilterNet denoise → ffmpeg: highpass, EQ, compressor,
                         de-ess, loudnorm (-16 LUFS VO / -14 LUFS final)
   → library asset       (recipe recorded)
   → timeline audio clip (frame-placed, optional offset_samples)
final: audio.mix         per-track gain + ducking (sidechaincompress) → mux
```
- Cleanup chains are named presets in YAML (`presets/voice-narrator.yaml`), so the agent can tweak values instead of rewriting code.
- Waveform peaks are generated on ingest for the timeline.
- `audio.mix` works at sample accuracy: clip positions are converted from frames to samples exactly, using the rational fps, and `offset_samples` is then added.
- `watch.storyboard-tts` (optional): re-runs TTS when a VO line changes.

---

## 12. 3D and Godot

| Need | Approach |
|---|---|
| Faux 3D / 2.5D | Layered parallax + perspective transforms in Revideo components |
| Real 3D | `three.render` module: three.js scene file + params → PNG sequence asset (`kind = seq`, tree-hashed) |
| Game-style shots / physics | `godot.render` module: `godot --headless --write-movie out.png --fixed-fps 30 scene.tscn` → PNG sequence |

Frame sequences are first-class assets and play as clips on the timeline. Godot projects and three.js scenes are stored as tree-hashed assets with recipes, so tweaking a camera angle produces a variant. When a sequence is exported at a rate other than the project fps, it records that rate as `fps_num/fps_den`.

---

## 13. Whiteboard

- **Engine:** Excalidraw, embedded in the webview and bound to the board doc's Yjs replica through y-excalidraw. The replica syncs to the core over IPC.
- **Human edits** in Excalidraw write the replica directly and are recorded as **derived ops** (6.1) for attribution and the activity feed.
- **Programmatic API:** agents write simplified element skeletons. These are converted with `convertToExcalidrawElements` (shapes, text, labeled/bound arrows, frames, images) and applied as explicit `el.add` ops. That converter is a JS function, so the core runs it in a small Node helper or in the webview (decide in Phase 4).

```
kurn board add --json '[{"type":"rectangle","x":0,"y":0,"label":{"text":"Scene 1: Hook"}}]'
kurn board frame "Scene 2" --at 800,0
kurn board snapshot --png renders/board.png      # agent looks at the board
```

### 13.1 Project links
- **Frames = scenes.** A frame can be linked to a `storyboard.yaml` scene ID.
- **Asset cards:** drag from the library → image element with `customData.asset = "ast_…"`.
- **Board → timeline:** `kurn board to-timeline` builds a rough cut and emits it as a single `batch` op. It creates one sequence per linked frame, ordered left to right, and places the linked assets and VO.

---

## 14. Agent Integration

### 14.1 Runtime
- Agents run locally in Herdr panes inside the app (Claude Code, OpenCode for DeepSeek). Herdr native integrations give working/blocked/idle status in the UI.
- Sessions persist across app restarts. With the app closed, agents can keep working against a headless core (`kurn daemon`, 4.1).

### 14.2 How agents act
- **Only through `kurn`** (CLI → core ops) and editing files in `scenes/`, `modules/`, `storyboard.yaml`, `brief.md`.
- Agents must not edit `timeline.json`, `board.excalidraw`, `.ws/`, or `library/` directly. These rules are enforced in two layers:
  1. **Agent permission config, generated per project.** For Claude Code, `.claude/settings.json` gets `permissions.deny` rules for `Edit`/`Write` on those paths. OpenCode gets the equivalent permission config. The rule is also stated in `AGENTS.md`.
  2. **Core safety net.** Snapshots are export-only and are written by the core from the live docs (6.3). If a snapshot file is edited on disk, the core overwrites it at the next export and logs a warning naming the file. That edit never reaches the live docs.

### 14.3 `AGENTS.md` (generated per project)
Includes:
- project conventions;
- the module list, auto-generated from manifests;
- the op catalog, including `batch` and pending-op handling (exit code `3`, `kurn op wait`);
- rendering commands;
- a "look before you claim" rule: render a still with `kurn render still` after any visual change, and never judge from the preview.

A Revideo conventions skill file is maintained in-repo since there are no official Revideo agent skills.

### 14.4 Agent roles (default)
| Agent | Role |
|---|---|
| Claude Opus 5.5 | Lead: concept, storyboard, scene components, timeline structure, review |
| DeepSeek (Flash) | Worker: batch TTS, generator scripts, bulk tagging, repetitive edits |

Handoff happens via Herdr (lead creates/messages worker panes and waits on output). Shared state is always the project files and Yjs docs, never agent memory.

### 14.5 Cost and safety
- **Budget scope:** `api_budget_usd` in `project.toml` covers **module API spend only**, such as TTS or a future image API. That spend is tracked from `{"type":"cost"}` events, or from `cost_estimate_usd` when a module doesn't report it. At the warning threshold the status bar warns. At the limit, `kurn run` of `cost_hint = "api"` modules needs human confirmation, which works like a pending op (6.3).
- **Not covered:** LLM spend by the agents themselves (Claude Code, OpenCode). The core can't see it. Track it with each provider's own billing and limits.
- Destructive-op confirmation (6.3). Agents cannot delete library assets (deprecate only).

---

## 15. UI Layout

```
┌──────────────┬────────────────────────────────┬───────────────────┐
│ Library      │  Preview (Revideo Player)      │ Agents (Herdr)    │
│  search/tags │   ⇄ toggle: Board (Excalidraw) │  claude ● working │
│  thumbnails  │                                │  deepseek ○ idle  │
│              │                                │  [terminal pane]  │
├──────────────┴────────────────────────────────┴───────────────────┤
│ Timeline (canvas): tracks · clips · keyframe lanes · markers      │
├───────────────────────────────────────────────────────────────────┤
│ Status: jobs · agent presence · budget · pending confirmations    │
└───────────────────────────────────────────────────────────────────┘
```
- Panels are dockable; module `panel` kinds register here.
- Command palette (`Ctrl+K`) lists every op and module.
- Activity feed: the op log, filterable by actor ("what did the agent just do?"), including derived ops.
- Pending confirmations: destructive ops and over-budget runs waiting for a human, with Approve and Reject buttons.

---

## 16. Future: Multi-Machine Collaboration (out of scope for v1)

v1 runs on one machine with no network services. The design keeps the following so that sharing between machines can be added later without migrating data:

| Kept in v1 | Why it matters later |
|---|---|
| Yjs docs (`yrs` in core, y-protocols over IPC) | A sync server (e.g. y-websocket) becomes one more provider attached to the core's docs |
| Actor on every op; per-actor undo | Remote users are just more actors |
| ID-keyed CRDT layout (7.3) | Concurrent edits from multiple machines merge without duplicated clips |
| `ast_<ULID>` asset IDs, content-addressed blobs | Libraries from several machines can be merged or synced without ID collisions; blobs dedupe across machines |
| Single-writer library behind the core API | Can later move behind an HTTP service on a shared host without changing callers |

When collaboration is added, the open decisions are:
- where the shared library and index live (a server-hosted library service, never SQLite on a network share);
- who exports snapshots and makes git commits (exactly one writer);
- whether to validate ops on the server;
- auth and transport (a VPN or mesh, with per-user tokens).

---

## 17. Roadmap

| Phase | Scope | Exit criteria |
|---|---|---|
| **0 — Spike** | Revideo upstream health check; TimelineRenderer from a hand-written v2 `timeline.json`; `kurn render still`; Player-in-Tauri parity check | Renders a 10 s multi-track clip; agent can render + view a still; Player plays it in Tauri on each target OS (or fallback chosen, 10.4); go/no-go on Revideo recorded |
| **1 — Core** | `yrs` doc host + persistence, op layer + `batch`, oplog, local library (SQLite + blobs), `kurn` CLI + `kurn daemon`, module protocol, `tts.fish`, `audio.clean` (no UI) | Agent produces a VO'd slideshow entirely via CLI; two agents editing the same timeline concurrently leave it valid; docs survive a core crash/restart; Q6 confirmed |
| **2 — App** | Tauri shell, IPC Yjs sync, preview panel, library panel, Herdr panes, agent permission config generation | User and agent see the same preview update live; app attaches to a running headless core |
| **3 — Timeline UI** | Canvas timeline, snapping, keyframe lanes, agent presence, pending-confirmation UI | Edit a 2-min video comfortably with an agent editing simultaneously |
| **4 — Board** | Excalidraw + Yjs, derived ops, programmatic API, board → timeline | Storyboard on board becomes a rough cut in one command |
| **5 — 3D/Godot** | `three.render`, `godot.render`, tree-hashed frame-sequence assets | 3D shot generated, varied, and cut into a timeline |
| **6 — Polish** | Semantic search, export presets, budgets, activity feed, module sandboxing | Used for a real production |

---

## 18. Risks & Open Questions

| # | Item | Mitigation / decision needed |
|---|---|---|
| R1 | Revideo is pre-1.0 and company-backed (Midrender) | Check upstream activity (commits, releases, issue response) before Phase 0 and record it; pin, wrap behind renderer module, contribute fixes upstream |
| R2 | y-excalidraw is a small community package | Pin or vendor; it's a thin binding |
| R3 | Herdr is young and YC-backed (license could change on future versions) | Pin version; Apache versions remain usable/forkable |
| R4 | Timeline UI perf | Canvas rendering from day one; benchmark in Phase 3 |
| R5 | Agent edits conflicting with human edits | Serialized op application in the core, per-actor undo, presence, locking per track (`locked`), ID-keyed CRDT layout (7.3) |
| R6 | SQLite corruption if the library sits in a network or cloud-synced folder | Core is the only opener; startup warning for synced/network paths (5) |
| R7 | Preview differs from final render (system webview vs. headless Chromium) | Chromium output is the source of truth; parity check in Phase 0; draft-render fallback (10.4) |
| R8 | Derived ops from Excalidraw aren't validated before they apply | Post-apply invariant checks + UI flag (6.3); board invariants are loose, so impact is low |
| Q1 | ~~App name~~ | Decided: Kurnace (`kurn`) |
| Q2 | ~~Library hosting~~ | Decided for v1: local disk, owned by the core. Shared hosting revisited with collaboration (16) |
| Q3 | Local TTS fallback (which model?) | Research before Phase 6; fits the local-first goal |
| Q4 | Music: programmatic composition module (e.g. MIDI → synth) or library only? | — |
| Q5 | Should agents be able to author new modules themselves? | Leaning yes. Prerequisites: real OS-level sandboxing (9.2) plus a human review gate before a module is registered |
| Q6 | CRDT in the Rust core: `yrs` vs. Node sidecar running Yjs | Default `yrs` (wire-compatible with Yjs); confirm in Phase 1 that y-excalidraw docs and `UndoManager` origins interoperate, else fall back to a Node sidecar |
| Q7 | Where `convertToExcalidrawElements` runs for agent board ops | Node helper vs. webview; decide in Phase 4 (13) |

---

## Appendix A — CLI Reference (v1)

```
kurn daemon [--project <slug>]     # headless core
kurn project new <slug>            kurn project open <slug>
kurn lib search|get|add|tag|variant|deprecate
kurn run <module-id> [--param value ...] [--cached] [--json]
kurn modules [--json]
kurn op <doc> <type> --args '<json>'
kurn op batch --file ops.json      # or --args '[{...},{...}]'
kurn op status <op-id>             kurn op wait <op-id> [--timeout 300]
kurn op pending
kurn undo | redo                   # per-actor (6.3)
kurn presence set --playhead 240 [--select c_01,c_02]
kurn timeline show|clip add|clip move|clip trim|split|keyframe set
kurn board add|frame|snapshot|to-timeline
kurn render still|draft|final
kurn jobs [--watch]                 kurn log [--actor claude] [--derived]
```

Exit codes: `0` success · `1` error · `2` validation failed · `3` pending human confirmation · `4` core not running.

## Appendix B — `project.toml`

```toml
name = "Example Explainer"
fps = "30/1"                # rational; e.g. "30000/1001"
width = 1920
height = 1080

[render]
preset = "1080p30"
encoder = "auto"            # videotoolbox | nvenc | qsv | vaapi | openh264 | libx264

[agents]
lead = "claude"
worker = "deepseek"
agent_confirm_destructive = true
destructive_threshold = 5   # N in 6.3
pending_expiry_s = 600
api_budget_usd = 10.0       # module API spend only (14.5)
api_budget_warn_pct = 80

[git]
auto_commit = true
idle_commit_s = 30
commit_oplog = false        # include .ws/oplog.jsonl in git

[audio]
vo_preset = "voice-narrator"
target_lufs = -14
```
