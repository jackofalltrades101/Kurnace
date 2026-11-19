#!/bin/sh
# Phase 4 exit check (spec §17, §13): a storyboard on the board becomes a
# rough cut in one command, with a PNG snapshot agents can inspect.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
KURN="$ROOT/target/debug/kurn"
WORK=${WORK:-$(mktemp -d -t kurn-p4)}
export KURNACE_HOME="$WORK/home" KURN_SOCKET="/tmp/kurn-p4-$$.sock"
cleanup() { "$KURN" stop >/dev/null 2>&1 || true; }
trap cleanup EXIT
j() { python3 -c "import sys,json; d=json.load(sys.stdin); print($1)"; }

echo "== build + media"
cargo build -q -p kurn
ffmpeg -v error -y -f lavfi -i "color=c=0x2a3f5f:s=320x180" -frames:v 1 "$WORK/scene1.png"
ffmpeg -v error -y -f lavfi -i "color=c=0x5f2a3f:s=320x180" -frames:v 1 "$WORK/scene2.png"
ffmpeg -v error -y -f lavfi -i "sine=frequency=280:duration=1" "$WORK/vo.wav"
"$KURN" project new "$WORK/p" --no-git --width 640 --height 360 >/dev/null
cd "$WORK/p"
A=$("$KURN" lib add "$WORK/scene1.png" --json | j "d[0]['id']")
B=$("$KURN" lib add "$WORK/scene2.png" --json | j "d[0]['id']")
V=$("$KURN" lib add "$WORK/vo.wav" --json | j "d[0]['id']")

echo "== board → rough cut"
"$KURN" board add --json "[
 {\"type\":\"frame\",\"x\":0,\"y\":0,\"width\":400,\"height\":260,\"name\":\"Hook\",\"customData\":{\"asset\":\"$A\",\"voice\":\"$V\"}},
 {\"type\":\"frame\",\"x\":500,\"y\":0,\"width\":400,\"height\":260,\"name\":\"Payoff\",\"customData\":{\"asset\":\"$B\"}}]" --json >/dev/null
"$KURN" board to-timeline --json >"$WORK/rough.json"
python3 - "$WORK/rough.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
assert d["result"]["status"]=="applied", d
det=d["details"]
assert det["scenes"]==2 and det["clips"]>=4 and det["duration"]>0, det
print(f"  scenes={det['scenes']} clips={det['clips']} duration={det['duration']}")
PY
"$KURN" timeline show --json >"$WORK/tl.json"
python3 - "$WORK/tl.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1])); tl=d["timeline"]
assert not [i for i in d["issues"] if i["level"]=="error"], d["issues"]
ids={t["id"] for t in tl["tracks"]}
assert {"v_board","a_board","t_board"} <= ids, ids
clips=list(tl["clips"].values())
assert any(c["track"]=="v_board" for c in clips)
assert any(c["track"]=="a_board" for c in clips)
assert any(c["track"]=="t_board" for c in clips)
assert tl["duration"]>0
print(f"  tracks={sorted(ids)} clips={len(clips)} duration={tl['duration']}")
PY

echo "== re-run replaces the rough cut"
"$KURN" board to-timeline --json >/dev/null
"$KURN" timeline show --json >"$WORK/tl2.json"
python3 - "$WORK/tl.json" "$WORK/tl2.json" <<'PY'
import json,sys
a=json.load(open(sys.argv[1]))["timeline"]; b=json.load(open(sys.argv[2]))["timeline"]
assert len(a["clips"]) == len(b["clips"]), (len(a["clips"]), len(b["clips"]))
print(f"  still {len(b['clips'])} clips")
PY

echo "== board snapshot"
"$KURN" board snapshot --png renders/board.png >/dev/null
python3 - "$WORK/p/renders/board.png" <<'PY'
import sys
b=open(sys.argv[1],'rb').read(8)
assert b == b"\x89PNG\r\n\x1a\n", b
print("  PNG ok")
PY
echo "PASS"
