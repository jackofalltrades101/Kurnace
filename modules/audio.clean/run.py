import json, pathlib, re, shutil, subprocess, sys, tomllib
HERE = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parent / "_lib"))
from kurnmod import log, main, output, progress  # noqa: E402


def ff(args):
    r = subprocess.run(["ffmpeg", "-hide_banner", "-nostats", "-y", *args], capture_output=True, text=True)
    if r.returncode != 0:
        raise RuntimeError(r.stderr.strip().splitlines()[-1] if r.stderr.strip() else "ffmpeg failed")
    return r.stderr


def chain(p):
    f = [f"highpass=f={p.get('highpass_hz', 80)}"]
    for b in p.get("eq", []):
        f.append(f"equalizer=f={b['f']}:t=q:w={b.get('width_q', 1)}:g={b['gain_db']}")
    c = p.get("compressor")
    if c:
        f.append(f"acompressor=threshold={c['threshold_db']}dB:ratio={c['ratio']}:attack={c['attack_ms']}"
                 f":release={c['release_ms']}:makeup={c.get('makeup_db', 0)}dB")
    d = p.get("deesser")
    if d:
        f.append(f"deesser=i={d.get('intensity', 0.4)}:f={d.get('frequency', 0.55)}")
    return f


def run(req):
    src = req["input"]["path"]
    work = pathlib.Path(req["_ctx"]["work_dir"])
    name = req.get("preset") or "voice-narrator"
    preset_file = HERE / "presets" / f"{name}.toml"
    if not preset_file.exists():
        raise ValueError(f"unknown preset {name!r} (have: {', '.join(p.stem for p in (HERE / 'presets').glob('*.toml'))})")
    p = tomllib.loads(preset_file.read_text())
    loud = p.get("loudness", {})
    target = req.get("target_lufs", loud.get("target_lufs", -16))
    tp, lra = loud.get("true_peak_db", -1.5), loud.get("lra", 11)

    stage = work / "in.wav"
    ff(["-i", src, "-ac", "1", "-ar", "48000", str(stage)])

    if req.get("denoise", True) and shutil.which("deep-filter"):
        progress(15, "denoising (DeepFilterNet)")
        dn = work / "dn"
        dn.mkdir()
        subprocess.run(["deep-filter", "-o", str(dn), str(stage)], check=True, capture_output=True)
        stage = next(dn.glob("*.wav"))
    elif req.get("denoise", True):
        log("deep-filter not on PATH; skipping denoise", "warn")

    pre = ",".join(chain(p))
    progress(40, "measuring loudness")
    meas = ff(["-i", str(stage), "-af", f"{pre},loudnorm=I={target}:TP={tp}:LRA={lra}:print_format=json", "-f", "null", "-"])
    m = json.loads(re.search(r"\{[^{}]*\"input_i\"[^{}]*\}", meas, re.S).group(0))

    progress(70, "processing")
    out = work / "clean.wav"
    ln = (f"loudnorm=I={target}:TP={tp}:LRA={lra}:measured_I={m['input_i']}:measured_TP={m['input_tp']}"
          f":measured_LRA={m['input_lra']}:measured_thresh={m['input_thresh']}:offset={m['target_offset']}:linear=true")
    ff(["-i", str(stage), "-af", f"{pre},{ln}", "-ar", "48000", "-ac", "1", str(out)])

    check = ff(["-i", str(out), "-af", "ebur128=peak=true", "-f", "null", "-"])
    got = re.findall(r"I:\s+(-?[\d.]+) LUFS", check)
    log(f"preset {name}: target {target} LUFS, result {got[-1] if got else '?'} LUFS")
    progress(100, "done")
    output("audio", out)


main(run)
