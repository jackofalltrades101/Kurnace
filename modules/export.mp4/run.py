#!/usr/bin/env python3
"""Export presets (spec §9.4): package a rendered file with ffmpeg."""
import json
import os
import subprocess
import sys
from pathlib import Path

PRESETS = {
    "youtube": {
        "size": (1920, 1080),
        "mode": "pad",
        "crf": 20,
        "audio": "192k",
    },
    "vertical": {
        "size": (1080, 1920),
        "mode": "crop",
        "crf": 20,
        "audio": "160k",
    },
    "square": {
        "size": (1080, 1080),
        "mode": "crop",
        "crf": 20,
        "audio": "160k",
    },
    "web": {
        "size": (1280, 720),
        "mode": "pad",
        "crf": 23,
        "audio": "128k",
    },
}


def emit(**kw):
    print(json.dumps(kw), flush=True)


def fail(msg):
    emit(type="error", msg=str(msg))
    sys.exit(1)


def resolve(root, p):
    path = Path(p)
    return path if path.is_absolute() else (Path(root) / path)


def main():
    req = json.load(sys.stdin)
    root = req.get("_ctx", {}).get("project_root") or os.getcwd()
    src = resolve(root, req["input"])
    if not src.exists():
        fail(f"input not found: {src}")
    preset_name = req.get("preset", "youtube")
    preset = PRESETS.get(preset_name)
    if not preset:
        fail(f"unknown preset {preset_name!r}; choose {', '.join(PRESETS)}")
    out = resolve(root, req["out"])
    out.parent.mkdir(parents=True, exist_ok=True)
    w, h = preset["size"]
    crf = int(req.get("crf", preset["crf"]))
    if preset["mode"] == "crop":
        vf = f"scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h}"
    else:
        vf = (
            f"scale={w}:{h}:force_original_aspect_ratio=decrease,"
            f"pad={w}:{h}:(ow-iw)/2:(oh-ih)/2:color=black"
        )

    cmd = [
        "ffmpeg", "-y", "-v", "error", "-i", str(src),
        "-vf", vf,
        "-c:v", "libx264", "-crf", str(crf), "-preset", "medium",
        "-pix_fmt", "yuv420p",
        "-c:a", "aac", "-b:a", preset["audio"],
        "-movflags", "+faststart",
        str(out),
    ]
    emit(type="progress", pct=5, msg=f"exporting {preset_name} {w}x{h}")
    emit(type="log", level="info", msg=" ".join(cmd))
    p = subprocess.run(cmd, capture_output=True, text=True)
    if p.returncode != 0:
        fail((p.stderr or p.stdout).strip()[-2000:])
    emit(type="progress", pct=95, msg="done")
    emit(type="output", name="file", path=str(out))
    emit(type="done")


if __name__ == "__main__":
    try:
        main()
    except Exception as e:
        fail(f"{type(e).__name__}: {e}")
