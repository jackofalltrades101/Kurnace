#!/bin/sh
# Phase 5 exit check (spec §12, §17): generate 3D shots with the C# Godot 4.7.2
# renderer and the three.js renderer, vary one, cut both into a timeline and
# render a still.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
KURN="$ROOT/target/debug/kurn"
WORK=${WORK:-$(mktemp -d -t kurn-p5)}
export KURNACE_HOME="$WORK/home" KURN_SOCKET="/tmp/kurn-p5-$$.sock"
cleanup() { "$KURN" stop >/dev/null 2>&1 || true; }
trap cleanup EXIT
j() { python3 -c "import sys,json; d=json.load(sys.stdin); print($1)"; }

echo "== build kurn"
cargo build -q -p kurn
"$KURN" project new "$WORK/p" --no-git --width 320 --height 180 >/dev/null
cd "$WORK/p"

echo "== Godot 4.7.2 .NET/C# → sequence"
GODOT=$("$KURN" lib add "$ROOT/modules/godot.render/test/fixture" --json | j "d[0]['id']")
"$KURN" run godot.render --source "$GODOT" --frames 6 --fps 30 --width 320 --height 180 --json >"$WORK/godot.json"
GSEQ=$(j "d['assets']['frames']['id']" <"$WORK/godot.json")
python3 - "$WORK/godot.json" <<'PY'
import json,sys
a=json.load(open(sys.argv[1]))["assets"]["frames"]
assert a["kind"]=="seq", a
assert (a["fps_num"],a["fps_den"])==(30,1), a
assert a["width"]==320 and a["height"]==180, a
print(f"  {a['id']} seq {a['fps_num']}/{a['fps_den']} {a['duration_s']:.3f}s")
PY

echo "== three.js → sequence"
"$KURN" run three.render --scene '{"background":"#101820","objects":[{"type":"box","color":"#4f8cff","rotationSpeed":[0.2,0.6,0.1]},{"type":"sphere","radius":0.5,"position":[-1.5,0.2,0],"color":"#ffb84f"}],"lights":[{"type":"directional","position":[3,5,2],"intensity":2.5}]}' --frames 5 --fps 24 --width 320 --height 180 --json >"$WORK/three.json"
TSEQ=$(j "d['assets']['frames']['id']" <"$WORK/three.json")
python3 - "$WORK/three.json" <<'PY'
import json,sys
a=json.load(open(sys.argv[1]))["assets"]["frames"]
assert a["kind"]=="seq", a
assert (a["fps_num"],a["fps_den"])==(24,1), a
print(f"  {a['id']} seq {a['fps_num']}/{a['fps_den']} {a['duration_s']:.3f}s")
PY

echo "== variant"
"$KURN" lib variant "$TSEQ" --set frames=6 --json >"$WORK/variant.json"
python3 - "$WORK/variant.json" "$TSEQ" <<'PY'
import json,sys
a=json.load(open(sys.argv[1]))["assets"]["frames"]
assert a["parent_id"]==sys.argv[2], a
assert a["kind"]=="seq"
print(f"  variant {a['id']} parent {a['parent_id']}")
PY

echo "== cut into timeline + still"
"$KURN" op batch --args "[
 {\"type\":\"track.add\",\"args\":{\"id\":\"v1\",\"kind\":\"video\",\"name\":\"3D A\"}},
 {\"type\":\"track.add\",\"args\":{\"id\":\"v2\",\"kind\":\"video\",\"name\":\"3D B\"}},
 {\"type\":\"clip.add\",\"args\":{\"id\":\"g\",\"track\":\"v1\",\"asset\":\"$GSEQ\",\"start\":0,\"out\":6}},
 {\"type\":\"clip.add\",\"args\":{\"id\":\"t\",\"track\":\"v2\",\"asset\":\"$TSEQ\",\"start\":6,\"out\":5}}]" >/dev/null
"$KURN" render still --frame 2 --out "$WORK/p/renders/godot.png" --json >/dev/null
"$KURN" render still --frame 7 --out "$WORK/p/renders/three.png" --json >/dev/null
python3 - "$WORK/p/renders/godot.png" "$WORK/p/renders/three.png" <<'PY'
import sys
for p in sys.argv[1:]:
    b=open(p,'rb').read(8)
    assert b==b"\x89PNG\r\n\x1a\n", (p,b)
    print(f"  PNG {p.rsplit('/',1)[-1]} ok")
PY
"$KURN" render final --out "$WORK/p/renders/3d.mp4" --json >/dev/null
FRAMES=$(ffprobe -v error -count_frames -select_streams v -show_entries stream=nb_read_frames -of csv=p=0 "$WORK/p/renders/3d.mp4" | cut -d, -f1)
python3 - "$FRAMES" <<'PY'
import sys
n=int(sys.argv[1].strip())
assert n==11, n
print(f"  final {n} frames")
PY
echo "PASS"
