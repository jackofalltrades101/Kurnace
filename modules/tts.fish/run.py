import json, os, pathlib, sys, urllib.error, urllib.request
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "_lib"))
from kurnmod import log, main, output, progress  # noqa: E402

API = "https://api.fish.audio/v1/tts"


def run(req):
    voice = req.get("voice") or os.environ.get("KURN_FISH_VOICE", "")
    model = req.get("model") or os.environ.get("KURN_FISH_MODEL") or "s2.1-pro"
    speed = req.get("speed") or os.environ.get("KURN_FISH_SPEED") or 1.0
    body = {"text": req["text"], "format": "wav", "sample_rate": 44100,
            "prosody": {"speed": float(speed)}}
    if voice:
        body["reference_id"] = voice
    http = urllib.request.Request(API, data=json.dumps(body).encode(), method="POST", headers={
        "Authorization": f"Bearer {os.environ['FISH_API_KEY']}",
        "Content-Type": "application/json",
        "model": model,
    })
    progress(10, "synthesizing")
    out = pathlib.Path(req["_ctx"]["work_dir"]) / "vo.wav"
    try:
        with urllib.request.urlopen(http, timeout=120) as r, open(out, "wb") as f:
            while chunk := r.read(1 << 16):
                f.write(chunk)
    except urllib.error.HTTPError as e:
        detail = e.read().decode(errors="replace")[:300]
        raise RuntimeError(f"Fish API {e.code}: {detail}") from None
    log(f"{len(req['text'])} chars → {out.stat().st_size} bytes")
    progress(100, "done")
    output("audio", out)


main(run)
