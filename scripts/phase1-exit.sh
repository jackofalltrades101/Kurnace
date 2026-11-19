#!/bin/sh
# Phase 1 exit test (spec §17): an agent produces a voiced slideshow entirely
# through the `kurn` CLI, then we verify the rendered file.
# Runs against an isolated KURNACE_HOME so it never touches ~/Kurnace.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
KURN="$ROOT/target/debug/kurn"
WORK=${WORK:-$(mktemp -d -t kurn-p1exit)}
export KURNACE_HOME="$WORK/home" KURN_SOCKET="/tmp/kurn-exit-$$.sock" KURN_ACTOR=agent:claude
trap '"$KURN" stop >/dev/null 2>&1 || true' EXIT
j() { python3 -c "import sys,json; d=json.load(sys.stdin); print($1)"; }
say() { printf '\n== %s\n' "$*"; }

say "project"
"$KURN" project new "$WORK/slideshow" --name "Voiced slideshow" >/dev/null
cd "$WORK/slideshow"

say "slides (gen.python → 3 gradient PNGs)"
SLIDES=$("$KURN" run gen.python --json --args '{"colors":[["0x1d2b64","0xf8cdda"],["0x0f2027","0x2c5364"],["0x42275a","0x734b6d"]]}' --code '
import json, os, subprocess
args = json.loads(os.environ["KURN_ARGS"])
for i, (a, b) in enumerate(args["colors"], 1):
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i",
        f"gradients=size=1920x1080:c0={a}:c1={b}:duration=1:speed=0", "-frames:v", "1",
        os.path.join(os.environ["KURN_OUT"], f"slide{i}.png")], check=True)
')
echo "$SLIDES" | j "'\n'.join(f'{k}: {v[\"id\"]}' for k, v in sorted(d['assets'].items()))"

say "voice-over (tts.say → audio.clean, per slide)"
LINES="Kurnace turns code into video.|Every asset keeps the recipe that made it.|Humans and agents edit the same timeline."
i=0
VOS=""
IFS='|'
for line in $LINES; do
  i=$((i+1))
  raw=$("$KURN" run tts.say --text "$line" --json | j "d['assets']['audio']['id']")
  clean=$("$KURN" run audio.clean --input "$raw" --json | j "d['assets']['audio']['id']")
  VOS="$VOS $clean"
  echo "slide$i: $raw → $clean"
done
unset IFS

say "timeline (one batch op)"
BATCH=$(SLIDES="$SLIDES" VOS="$VOS" KURN="$KURN" python3 - <<'PY'
import json, math, os, subprocess
slides = json.loads(os.environ["SLIDES"])["assets"]
vos = os.environ["VOS"].split()
titles = ["Code → video", "Recipes, not outputs", "One timeline"]
fps, pad = 30, 15
ops = [{"type": "track.add", "args": {"id": i, "kind": k, "name": n}} for i, k, n in
       [("v1", "video", "Slides"), ("t1", "text", "Titles"), ("a1", "audio", "VO")]]
t = 0
for n, vo in enumerate(vos, 1):
    a = json.loads(subprocess.check_output([os.environ["KURN"], "lib", "get", vo, "--json"]))
    vo_frames = math.ceil(a["duration_s"] * fps)
    length = vo_frames + 2 * pad
    ops += [
        {"type": "clip.add", "args": {"id": f"s{n}", "track": "v1", "asset": slides[f"slide{n}"]["id"], "start": t, "dur": length,
            "props": {"fit": "cover"}, "keyframes": {"scale": [{"t": 0, "v": 1.0}, {"t": length, "v": 1.06}]}}},
        {"type": "clip.add", "args": {"id": f"h{n}", "track": "t1", "component": "text", "start": t, "dur": length,
            "props": {"text": titles[n - 1], "fontSize": 120},
            "keyframes": {"opacity": [{"t": 0, "v": 0, "ease": "easeOutCubic"}, {"t": 12, "v": 1}, {"t": length - 10, "v": 1}, {"t": length, "v": 0}]}}},
        {"type": "clip.add", "args": {"id": f"vo{n}", "track": "a1", "asset": vo, "start": t + pad, "dur": vo_frames}},
    ]
    t += length
print(json.dumps(ops))
PY
)
"$KURN" op batch --args "$BATCH"
"$KURN" timeline show

say "look before you claim: still at frame 40"
"$KURN" render still --frame 40

say "final render"
"$KURN" render final

say "verify"
OUT="$WORK/slideshow/renders/final.mp4"
DUR=$("$KURN" timeline show --json | j "d['timeline']['duration']")
FRAMES=$(ffprobe -v error -count_frames -select_streams v -show_entries stream=nb_read_frames -of csv=p=0 "$OUT")
LUFS=$(ffmpeg -hide_banner -nostats -i "$OUT" -af ebur128 -f null - 2>&1 | sed -n 's/^ *I: *\(-[0-9.]*\) LUFS/\1/p' | tail -1)
echo "timeline: $DUR frames · video: $FRAMES frames · integrated loudness: $LUFS LUFS"
[ "$FRAMES" = "$DUR" ] || { echo "FAIL: frame count"; exit 1; }
python3 -c "import sys; l=float('$LUFS'); sys.exit(0 if -30 < l < -10 else 1)" || { echo "FAIL: audio level"; exit 1; }
echo "ops by agent: $("$KURN" log --json --limit 100 | j "len([r for r in d if r['actor']['name']=='claude'])")"
echo "PASS  ($OUT)"
