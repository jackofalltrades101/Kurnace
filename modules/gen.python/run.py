import json, os, pathlib, subprocess, sys
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "_lib"))
from kurnmod import log, main, output, progress  # noqa: E402


def run(req):
    ctx = req["_ctx"]
    work = pathlib.Path(ctx["work_dir"])
    out = work / "out"
    out.mkdir()
    if req.get("code"):
        script = work / "script.py"
        script.write_text(req["code"])
    elif req.get("file"):
        root = ctx.get("project_root") or "."
        script = (pathlib.Path(root) / req["file"]).resolve()
        if not script.exists():
            raise FileNotFoundError(f"{req['file']} (relative to {root})")
    else:
        raise ValueError("pass --code '<python>' or --file path/to/script.py")
    env = {**os.environ, "KURN_OUT": str(out), "KURN_ARGS": json.dumps(req.get("args", {})),
           "KURN_SEED": str(ctx.get("seed") or 0)}
    progress(10, f"running {script.name}")
    r = subprocess.run([sys.executable, str(script)], cwd=work, env=env, capture_output=True, text=True)
    for line in r.stdout.splitlines()[-20:]:
        log(line)
    if r.returncode != 0:
        raise RuntimeError(f"script exited {r.returncode}: {r.stderr.strip()[-800:]}")
    files = sorted(p for p in out.rglob("*") if p.is_file())
    if not files:
        raise RuntimeError("script wrote nothing to $KURN_OUT")
    for p in files:
        output(p.stem, p)
    progress(100, f"{len(files)} file(s)")


main(run)
