#!/usr/bin/env python3
"""Godot 4.7.2 .NET (C#) → PNG frame sequence.

Protocol: one JSON request on stdin, NDJSON events on stdout (spec §9.2).
The module intentionally refuses GDScript-only projects and non-.NET Godot
builds: Phase 5 is specified against the C# model.
"""
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

MIN_VERSION = "4.7.2"


def emit(**kw):
    print(json.dumps(kw), flush=True)


def log(msg, level="info"):
    emit(type="log", level=level, msg=str(msg))


def fail(msg):
    emit(type="error", msg=str(msg))
    sys.exit(1)


def find_godot():
    """Prefer the installed 4.7.2 mono app, then PATH candidates."""
    candidates = []
    env = os.environ.get("KURN_GODOT")
    if env:
        candidates.append(env)
    home = Path.home()
    candidates += [
        str(home / "Applications/Godot_mono_4.7.2.app/Contents/MacOS/Godot"),
        "/Applications/Godot_mono_4.7.2.app/Contents/MacOS/Godot",
        str(home / "Applications/Godot_mono.app/Contents/MacOS/Godot"),
        "/Applications/Godot_mono.app/Contents/MacOS/Godot",
        str(home / "Desktop/Godot_mono.app/Contents/MacOS/Godot"),
    ]
    for name in ("godot-mono", "godot4-mono", "godot4", "godot"):
        p = shutil.which(name)
        if p:
            candidates.append(p)
    seen = set()
    for c in candidates:
        if not c or c in seen or not Path(c).exists():
            continue
        seen.add(c)
        try:
            out = subprocess.run([c, "--version"], capture_output=True, text=True, timeout=20)
            version = (out.stdout or out.stderr).strip().splitlines()[0]
        except Exception:
            continue
        if version.startswith(MIN_VERSION) and "mono" in version.lower():
            return c, version
    fail(
        f"godot.render needs the Godot {MIN_VERSION} .NET/Mono build (C#). "
        "Set KURN_GODOT to its binary. GDScript-only or non-.NET builds are not supported."
    )


def app_nupkgs(godot_bin):
    p = Path(godot_bin).resolve()
    # macOS: Godot.app/Contents/MacOS/Godot
    for parent in [p.parent.parent, p.parent, p.parent.parent.parent]:
        cand = parent / "Resources" / "GodotSharp" / "Tools" / "nupkgs"
        if cand.exists():
            return cand
        cand = parent / "GodotSharp" / "Tools" / "nupkgs"
        if cand.exists():
            return cand
    return None


def run(cmd, cwd, timeout, label):
    log(f"{label}: {' '.join(str(x) for x in cmd)}")
    try:
        p = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        fail(f"{label} timed out after {timeout}s")
    if p.stdout.strip():
        for line in p.stdout.strip().splitlines()[-20:]:
            log(line)
    if p.returncode != 0:
        tail = "\n".join((p.stderr or p.stdout).strip().splitlines()[-20:])
        fail(f"{label} failed (exit {p.returncode}):\n{tail}")
    return p


def main():
    req = json.load(sys.stdin)
    project = req.get("source") or {}
    if project.get("kind") != "godot":
        fail("source must be a `godot` library asset (a directory with project.godot)")
    src = Path(project.get("path", ""))
    if not (src / "project.godot").exists():
        fail(f"no project.godot in {src}")

    work = Path(req["_ctx"]["work_dir"])
    proj = work / "project"
    out = work / "frames"
    out.mkdir(parents=True, exist_ok=True)

    def ignore(_dir, names):
        return [n for n in names if n in {".godot", ".mono", "bin", "obj", ".vs", "__pycache__"}]

    shutil.copytree(src, proj, ignore=ignore)

    csprojs = list(proj.glob("*.csproj"))
    if not csprojs:
        fail(
            "godot.render requires a C# (.NET) Godot project: found no *.csproj. "
            "This build is GDScript-only; the Phase 5 module intentionally uses the C# model."
        )
    csproj = csprojs[0]

    frames = int(req.get("frames", 60))
    fps = int(req.get("fps", 30))
    width = int(req.get("width", 640))
    height = int(req.get("height", 360))
    scene = str(req.get("scene", "") or "").strip()

    godot, version = find_godot()
    log(f"using Godot {version}")

    nupkgs = app_nupkgs(godot)
    if nupkgs:
        (proj / "NuGet.Config").write_text(
            '<?xml version="1.0" encoding="utf-8"?>\n'
            "<configuration>\n  <packageSources>\n"
            f'    <add key="godot" value="{nupkgs}" />\n'
            '    <add key="nuget.org" value="https://api.nuget.org/v3/index.json" />\n'
            "  </packageSources>\n</configuration>\n"
        )

    emit(type="progress", pct=5, msg="building C# project")
    run(
        ["dotnet", "build", str(csproj.name), "-c", "Debug", "--nologo", "-v:q"],
        proj,
        timeout=300,
        label="dotnet build",
    )
    emit(type="progress", pct=25, msg=f"rendering {frames} frames at {fps} fps")

    movie = out / "frame.png"
    cmd = [
        godot,
        "--path",
        str(proj),
        "--resolution",
        f"{width}x{height}",
        # The dummy/headless renderer cannot feed MovieWriter; on macOS this
        # opens a hidden-ish window briefly. gl_compatibility is the most
        # portable renderer for movie capture.
        "--rendering-method",
        "gl_compatibility",
        "--rendering-driver",
        "opengl3",
        "--audio-driver",
        "Dummy",
        "--write-movie",
        str(movie),
        "--fixed-fps",
        str(fps),
        "--quit-after",
        str(frames),
    ]
    if scene:
        cmd.append(scene)
    # MovieWriter needs a rendering display driver; on macOS the default
    # `macos` driver works (the headless dummy driver crashes in Godot 4.7.2).
    if sys.platform == "darwin":
        cmd[1:1] = ["--display-driver", "macos"]

    emit(type="progress", pct=30, msg="Godot movie writer")
    run(cmd, proj, timeout=max(120, frames * 2), label="godot")

    pngs = sorted(out.glob("frame*.png"))
    if len(pngs) != frames:
        fail(f"Godot wrote {len(pngs)} frames, expected {frames}")
    for junk in out.glob("*.wav"):
        junk.unlink()

    meta = {
        "engine": "godot",
        "engine_version": "4.7.2",
        "language": "csharp",
        "fps_num": fps,
        "fps_den": 1,
        "frames": len(pngs),
        "width": width,
        "height": height,
    }
    # The library reads this when ingesting a tree asset (fps/duration, §12).
    (out / "kurn.json").write_text(json.dumps(meta, sort_keys=True, indent=2) + "\n")

    emit(type="progress", pct=95, msg="ingesting sequence")
    emit(type="output", name="frames", path=str(out))
    emit(type="done")


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except Exception as e:  # protocol error, not a traceback on stdout
        emit(type="error", msg=f"{type(e).__name__}: {e}")
        sys.exit(1)
