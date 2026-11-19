# Kurnace

A local desktop tool where a person and AI agents build videos programmatically, on the same timeline at the same time. The design is in [`kurnace-spec.md`](kurnace-spec.md) (v0.3, local-only).

## Status

| Phase | State |
|---|---|
| 0 — Spike | Done. Revideo TimelineRenderer, frame-accurate stills/drafts/finals, Player runs in Tauri (macOS checked) |
| 1 — Core | Done. Rust core: Yjs (`yrs`) doc host with crash-safe persistence, op layer with `batch`/per-actor undo/pending destructive ops, SQLite + blob library, module runner, `kurn` CLI + daemon, `tts.fish`, `tts.say`, `audio.clean`, `gen.python` |
| 2 — App | Done (dev build). Tauri app as a core client: live Yjs replica over IPC, Revideo preview that updates as agents edit, library panel with drag-and-drop import, terminal panes for agents (`KURN_ACTOR` set), activity feed, pending-op approvals |
| 3 — Timeline UI | Done (dev build). Canvas timeline with scroll/zoom, drag/trim, snapping, multi-select, split, ripple delete, keyframe lanes + easing, markers, waveform/thumbnail strips, live agent presence, pending-approval dialog |
| 4 — Board | Done (dev build). Excalidraw bound to the core board doc, derived ops, `kurn board add/frame/snapshot/to-timeline`, asset cards, board → rough cut |
| 5 — 3D/Godot | Done (dev build). `godot.render` (Godot 4.7.2 **.NET/C#**), `three.render` (headless Chromium), tree-hashed seq assets with native fps, sequence playback in renders, variants |
| 6 — Polish | Implemented (dev): local semantic search, API budgets + status, export presets, activity feed, opt-in module sandbox. Production signing/notarization remains. |

## Layout

```
crates/kurn-core   core library: ops, timeline/board docs, persistence, library, modules, socket server
crates/kurn        `kurn` CLI and `kurn daemon`
packages/timeline  timeline model + validator (TypeScript, shared with the renderer)
modules/           render.revideo, godot.render (C#), three.render, tts.fish, tts.say, audio.clean, gen.python
app/               Tauri app: src-tauri (core client, asset scheme, PTYs) + src (React UI)
projects/demo      demo project
scripts/           parity, phase-1 exit and stress tests, test-all
```

## Use

Needs Node 24+, pnpm, Rust, and ffmpeg on PATH.

```sh
pnpm install
cargo build -p kurn            # or just use bin/kurn, which builds on first run
cd projects/demo
../../bin/kurn timeline show
../../bin/kurn render still --frame 60      # → renders/f60.png
```

- The first `kurn` command starts the core in the background (`~/.kurnace/core.sock`, log in `~/.kurnace/core.log`).
- The library lives in `~/Kurnace/library`.
- `kurn stop` stops the core.
- Set `KURNACE_HOME` and `KURN_SOCKET` to isolate a test environment. The socket path must stay under 104 bytes on macOS.
- Agents identify themselves with `KURN_ACTOR=agent:<name>`. On project open/create, Kurnace writes/refreshes `AGENTS.md`, `CLAUDE.md` (which imports `@AGENTS.md` for Claude Code), and Claude Code deny rules (`.claude/settings.json`). Launching an agent refreshes them first, so project context is injected without a user prompt.
- Agents publish their cursor for the timeline with `kurn presence set --playhead 240 --select c_01,c_02`; the app draws their playhead and selection in the timeline. Presence expires after 60 s without an update.
- Board work is the same core behind `kurn board`: `board add --json '[…]'`, `board frame "Scene 1" --at 0,0`, `board elements`, `board snapshot --png renders/board.png`, and `board to-timeline` (one batch op). The live board is `.ws/docs/board.*`; `board.excalidraw` is its export.
- 3D: `kurn lib add ./godot-project` then `kurn run godot.render --source ast_… --frames 120` (requires **Godot 4.7.2 .NET/Mono**, C# projects only), or `kurn run three.render --scene '<descriptor>' --frames 120`. Both produce a tree-hashed `seq` asset with native fps, playable on the timeline and in the live preview (frames are served by the loopback asset server).
- Library search: `kurn lib search "dusk gradient" --semantic` uses a local Ollama embedding model when available, with an offline hashed n-gram fallback.
- Budgets: set `[agents].api_budget_usd` / `api_budget_warn_pct` in `project.toml`; `kurn budget` shows spend. Over-limit API runs become pending requests (exit code 3); approve them from the app's approval dialog and the module runs. `KURN_BUDGET_OVERRIDE=1` is an explicit CLI override.
- Export: `kurn run export.mp4 --input renders/final.mp4 --preset youtube|vertical|square|web --out renders/out.mp4`.
- API keys (`*_API_KEY`) are forwarded from the shell that runs `kurn` to modules, e.g. `FISH_API_KEY=… kurn run tts.fish --text "Hi"`.
- **Settings (⌘,)** stores the Fish API key in the OS keychain and default voice/model/speed in `~/.kurnace/settings.json`. The app injects them into module runs and agent panes, and includes a Fish test/generate form. Agents can then run `kurn run tts.fish --text "…"` without being given the key.

## App

```sh
cd app
KURN_PROJECT=$PWD/../projects/demo pnpm tauri dev    # or omit KURN_PROJECT to pick a project
```

- The app opens on a **project dashboard**: recent projects, open any folder with `project.toml`, create a project (name, folder, fps, resolution, git), and Reveal in Finder.
- Opening a project attaches to the running core; `KURN_PROJECT` still bypasses the dashboard for development/automation.
- New/opened projects get `AGENTS.md`, `CLAUDE.md` (imports `@AGENTS.md`) and `.claude/settings.json`; launching an agent refreshes them first, so Claude/OpenCode receive project conventions automatically without a prompt.
- The preview toolbar has **Still**, **Draft**, **Final** and **Export** controls. Outputs go to `renders/`; **Reveal** opens the last output in Finder. Rendering still uses headless Chromium.
- `scripts/build-mac-app.sh` builds a self-contained local app at `target/release/bundle/macos/Kurnace.app` and a DMG under `target/release/bundle/dmg/` (core sidecar, modules, Node, Chromium cache; ad-hoc signed). The current build is installed at `/Applications/Kurnace.app`.
- `scripts/build-windows.sh` cross-builds a portable Windows x64 app (`dist/windows/Kurnace-0.0.1-windows-x64.zip`) and NSIS installer (`dist/windows/Kurnace-Setup-0.0.1.exe`). It needs `cargo-xwin`, LLVM and LLD; Windows needs the WebView2 runtime. The bundle carries `kurn.exe`, Node, Python, ffmpeg/ffprobe and headless Chromium. Godot projects still need Godot 4.7.2 .NET.
- Run agents from the Agents panel; their `kurn` edits appear in the preview, timeline and activity feed as they happen.
- The timeline is canvas-rendered: drag/trim clips (hold Alt to bypass snapping), Shift/⌘-click for multi-select, `S` splits at the playhead, Delete ripple-deletes, `0` fits, and ⌘/Ctrl+wheel zooms. Selecting one clip exposes its keyframe lanes; double-click a lane to add a keyframe, drag keyframes to retime, and use the easing picker above the canvas.
- Waveform peaks and thumbnail strips are generated on ingest under `~/Kurnace/library/peaks` and `thumbs` (video needs `ffmpeg`) and drawn on clips.
- Switch the center pane to **Board**. Excalidraw edits sync through the core as derived ops; drag a library asset onto the board to make an asset card, add frames, then use **To timeline** (or `kurn board to-timeline`) for a rough cut.
- `node app/scripts/ui-shot.mjs <project> out.png` screenshots the UI in headless Chrome against the real core, for layout checks. Start `pnpm -C app dev` first.

## Known issues

- **Preview can stall after seeking (macOS/WKWebView, intermittent).** Revideo's preview waits on `<video>` `seeked` events that WebKit sometimes never fires. The app detects it and offers **Reload preview**. Final output is unaffected: renders run in headless Chromium, the source of truth (spec §10.4). Details: `modules/render.revideo/README.md`.
- **macOS distribution:** `scripts/build-mac-app.sh` stages the core sidecar, modules, Node and the Chromium cache, builds `Kurnace.app` (~974 MB) plus a DMG (~426 MB), and ad-hoc signs the app. Apple developer signing/notarization still needs credentials. ffmpeg/ffprobe and Python 3 remain external prerequisites for the media/TTS/generator modules.
- **Over-budget module runs need the app open to approve.** They return a pending request and exit code 3; a human approves in the same dialog used for destructive ops. `KURN_BUDGET_OVERRIDE=1` remains an explicit CLI escape hatch.
- **Module sandbox is opt-in and not a full security boundary.** `KURN_SANDBOX=1` wraps modules in a macOS `sandbox-exec` profile that limits writes to the job workdir, module dir, temp and language caches. Reads stay open, and network access is not restricted; untrusted/agent-authored modules still need the Phase 6+ review gate (Q5).
- **Godot movie capture needs a display driver.** Godot 4.7.2's `--headless` dummy renderer crashes inside MovieWriter, so `godot.render` opens a Godot window briefly on macOS. C# projects must target `net8.0` and are built with `dotnet` before capture.
- **`y-excalidraw` peer range is behind Excalidraw.** We pin `@excalidraw/excalidraw` 0.18.1 (React 19) with `y-excalidraw` 2.0.12, whose declared peer is 0.17.6. Board edits are covered by `scripts/phase4-board.sh` and the headless UI checks; re-test after any Excalidraw upgrade.
- **Herdr** is on PATH here and appears in the Agents panel. Its working/blocked/idle status integration is not built yet; the panel only shows pane running/exited. Source is Apache-2.0, but the 0.1.0 crate on crates.io is AGPL, so install from source, not `cargo install herdr`.

## Test

```sh
scripts/test-all.sh                 # unit, render accuracy, phase-1 exit + stress, board, 3D
scripts/phase5-3d.sh                # Godot C# + three.js renders, variant, seq on timeline
scripts/phase6-polish.sh            # semantic search, API budget, export preset
scripts/build-mac-app.sh            # app + DMG with core/modules/Node/Chromium
scripts/test-all.sh --with-app      # also opens the app: live-edit, preview parity, canvas timeline
scripts/phase4-board.sh             # board ops + snapshot + board → timeline, no window
```
