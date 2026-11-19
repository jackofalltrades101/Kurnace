import pathlib, subprocess, sys
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "_lib"))
from kurnmod import main, output, progress  # noqa: E402


def run(req):
    work = pathlib.Path(req["_ctx"]["work_dir"])
    aiff, wav = work / "vo.aiff", work / "vo.wav"
    rate = round(175 * float(req.get("speed", 1.0)))  # say's default is ~175 wpm
    progress(10, "synthesizing")
    subprocess.run(["say", "-v", req.get("voice") or "Samantha", "-r", str(rate), "-o", str(aiff), req["text"]],
                   check=True, capture_output=True)
    subprocess.run(["ffmpeg", "-y", "-v", "error", "-i", str(aiff), "-ar", "48000", "-ac", "1", str(wav)],
                   check=True, capture_output=True)
    progress(100, "done")
    output("audio", wav)


main(run)
