#!/bin/sh
# Phase 4 UI check: Excalidraw mounts on the live board doc and the "To
# timeline" button turns frames into a rough cut. Headless Chrome; no window.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
KURN="$ROOT/target/debug/kurn"
WORK=${WORK:-$(mktemp -d -t kurn-p4ui)}
export KURNACE_HOME="$WORK/home" KURN_SOCKET="/tmp/kurn-p4ui-$$.sock"
OUT="$WORK/board.png"
LOG="$WORK/ui.log"
STARTED_VITE=0
cleanup() {
  [ -f "$WORK/ui.pid" ] && kill "$(cat "$WORK/ui.pid")" 2>/dev/null || true
  pkill -f 'ui-shot.mjs' 2>/dev/null || true
  [ "$STARTED_VITE" = 1 ] && lsof -ti tcp:1420 | xargs kill 2>/dev/null || true
  "$KURN" stop >/dev/null 2>&1 || true
}
trap cleanup EXIT
j() { python3 -c "import sys,json; d=json.load(sys.stdin); print($1)"; }

echo "== build + board"
cargo build -q -p kurn
ffmpeg -v error -y -f lavfi -i "color=c=0x2a3f5f:s=320x180" -frames:v 1 "$WORK/scene.png"
"$KURN" project new "$WORK/p" --no-git --width 640 --height 360 >/dev/null
cd "$WORK/p"
A=$("$KURN" lib add "$WORK/scene.png" --json | j "d[0]['id']")
"$KURN" board add --json "[
 {\"type\":\"frame\",\"x\":0,\"y\":0,\"width\":400,\"height\":260,\"name\":\"Hook\",\"customData\":{\"asset\":\"$A\"}},
 {\"type\":\"text\",\"x\":40,\"y\":300,\"text\":\"storyboard\"}]" --json >/dev/null

echo "== headless board → rough cut"
if ! curl -sf http://localhost:1420/kurnace.html >/dev/null 2>&1; then
  (cd "$ROOT/app" && pnpm dev >"$WORK/vite.log" 2>&1 &)
  STARTED_VITE=1
fi
i=0; until curl -sf http://localhost:1420/kurnace.html >/dev/null 2>&1; do
  i=$((i+1)); [ $i -gt 40 ] && { echo "timeout starting Vite"; exit 1; }; sleep 0.5
done
(node "$ROOT/app/scripts/ui-shot.mjs" "$WORK/p" "$OUT" --width 1500 --height 900 --wait 4500 \
  --eval "(async()=>{[...document.querySelectorAll('.center-tabs button')].find(b=>b.textContent==='Board')?.click();await new Promise(r=>setTimeout(r,2200));[...document.querySelectorAll('.board-bar button')].find(b=>b.textContent==='To timeline')?.click();await new Promise(r=>setTimeout(r,1800));})()" \
  --probe "({board:!!document.querySelector('.board-host .excalidraw'),bar:document.querySelector('.board-bar')?.textContent,note:document.querySelector('.board .note')?.textContent})" \
  >"$LOG" 2>&1 & echo $! > "$WORK/ui.pid")
i=0; until grep -q '^saved ' "$LOG"; do
  i=$((i+1)); [ $i -gt 80 ] && { echo "timeout waiting for UI screenshot"; tail -20 "$LOG"; exit 1; }; sleep 0.5
done
kill "$(cat "$WORK/ui.pid")" 2>/dev/null || true

P=$(sed -n 's/^probe: //p' "$LOG")
echo "   $P"
echo "$P" | grep -qF '"board":true' || { echo "FAIL: Excalidraw did not mount"; exit 1; }
echo "$P" | grep -qF 'rough cut: 1 scenes' || { echo "FAIL: To timeline did not run"; exit 1; }
"$KURN" timeline show --json >"$WORK/tl.json"
python3 - "$WORK/tl.json" <<'PY'
import json,sys
tl=json.load(open(sys.argv[1]))["timeline"]
ids={t["id"] for t in tl["tracks"]}
assert {"v_board","t_board"} <= ids, ids
print(f"   board tracks present ({len(tl['clips'])} clips)")
PY
echo "PASS  (screenshot $OUT)"
