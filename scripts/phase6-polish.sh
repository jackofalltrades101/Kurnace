#!/bin/sh
# Phase 6 checks: local semantic search, module API budget, export presets.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
KURN="$ROOT/target/debug/kurn"
WORK=${WORK:-$(mktemp -d -t kurn-p6)}
export KURNACE_HOME="$WORK/home" KURN_SOCKET="/tmp/kurn-p6-$$.sock"
cleanup() { "$KURN" stop >/dev/null 2>&1 || true; }
trap cleanup EXIT
j() { python3 -c "import sys,json; d=json.load(sys.stdin); print($1)"; }

echo "== build"
cargo build -q -p kurn
"$KURN" project new "$WORK/p" --no-git --width 320 --height 180 >/dev/null
cd "$WORK/p"
mkdir -p modules/test.api
cat > modules/test.api/module.toml <<'EOF'
id = "test.api"
version = "0.1.0"
kind = "generator"
name = "API stub"
runtime = "python"
entry = "run.py"
license = "MIT"
deterministic = false
[inputs]
text = { type = "string", default = "hello" }
[outputs]
file = { type = "file" }
[agent]
expose = true
cost_hint = "api"
cost_estimate_usd = 1.0
EOF
cat > modules/test.api/run.py <<'EOF'
import json, sys
json.load(sys.stdin)
print(json.dumps({"type":"done"}), flush=True)
EOF
python3 - <<'PY'
from pathlib import Path
p=Path('project.toml'); lines=p.read_text().splitlines()
lines=[l for l in lines if not l.startswith(('api_budget_usd','api_budget_warn_pct'))]
i=lines.index('[agents]')
lines[i+1:i+1]=['api_budget_usd = 0.01','api_budget_warn_pct = 80.0']
p.write_text('\n'.join(lines)+'\n')
PY
# project.new already opened the project; reload the edited config.
"$KURN" stop >/dev/null 2>&1 || true
"$KURN" timeline show --json >/dev/null

echo "== semantic search"
ffmpeg -v error -y -f lavfi -i color=c=0x223344:s=64x64 -frames:v 1 "$WORK/dusk-gradient.png"
ffmpeg -v error -y -f lavfi -i color=c=0x992211:s=64x64 -frames:v 1 "$WORK/red-sunset.png"
ffmpeg -v error -y -f lavfi -i sine=frequency=300:duration=1 "$WORK/bass-loop.wav"
"$KURN" lib add "$WORK/dusk-gradient.png" --tag bg,dusk --name "Dusk Gradient" >/dev/null
"$KURN" lib add "$WORK/red-sunset.png" --tag bg,sunset --name "Red Sunset" >/dev/null
"$KURN" lib add "$WORK/bass-loop.wav" --name "Bass Loop" >/dev/null
"$KURN" lib search "dusky sunset" --semantic --json >"$WORK/semantic.json"
python3 - "$WORK/semantic.json" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
names=[a["name"] for a in d]
assert "Dusk Gradient" in names or "Red Sunset" in names, names
print("  semantic hits:", names[:3])
PY

echo "== budget blocks API module"
set +e
"$KURN" run test.api --json >"$WORK/budget-block.json"
CODE=$?
set -e
[ "$CODE" = 3 ] || { echo "FAIL: expected exit 3, got $CODE"; cat "$WORK/budget-block.json"; exit 1; }
grep -q 'API budget' "$WORK/budget-block.json" || { cat "$WORK/budget-block.json"; exit 1; }
echo "  blocked with exit 3"

echo "== approve the pending run"
OP=$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["op"])' "$WORK/budget-block.json")
python3 - "$KURN_SOCKET" "$OP" "$WORK/p" <<'PY'
import json,socket,sys
sock,op,proj=sys.argv[1:]
s=socket.socket(socket.AF_UNIX,socket.SOCK_STREAM); s.connect(sock)
s.sendall((json.dumps({"id":1,"method":"run.decide","actor":"human:test",
  "params":{"project":proj,"op":op,"approve":True}})+"\n").encode())
buf=b""
while True:
    while b"\n" not in buf:
        buf += s.recv(65536)
    line, buf = buf.split(b"\n", 1)
    r = json.loads(line)
    if "event" not in r:
        break
assert r.get("ok"), r
print("  approved run:", "applied" if r["result"].get("status") is None or r["result"].get("status")=="applied" else r["result"].get("status"))
PY

echo "== budget allows after raise + records spend"
python3 - <<'PY'
from pathlib import Path
p=Path('project.toml'); s=p.read_text().replace('api_budget_usd = 0.01','api_budget_usd = 10.0'); p.write_text(s)
PY
"$KURN" stop >/dev/null 2>&1 || true
"$KURN" timeline show --json >/dev/null
"$KURN" run test.api --json >"$WORK/budget-ok.json"
"$KURN" budget --json >"$WORK/budget.json"
python3 - "$WORK/budget.json" <<'PY'
import json,sys
b=json.load(open(sys.argv[1]))
assert b["spent_usd"] >= 0.99, b
assert b["limit_usd"] == 10.0, b
print(f"  spent ${b['spent_usd']:.2f} / ${b['limit_usd']:.2f}")
PY

echo "== export preset"
ffmpeg -v error -y -f lavfi -i testsrc=size=320x180:rate=30:duration=2 -f lavfi -i sine=frequency=440:duration=2 -c:v libx264 -pix_fmt yuv420p -c:a aac renders/final.mp4
"$KURN" run export.mp4 --input renders/final.mp4 --preset vertical --out renders/vertical.mp4 --json >/dev/null
DIM=$(ffprobe -v error -select_streams v -show_entries stream=width,height -of csv=p=0 renders/vertical.mp4)
[ "$DIM" = "1080,1920" ] || { echo "FAIL: expected 1080,1920 got $DIM"; exit 1; }
echo "  vertical $DIM"
echo "PASS"
