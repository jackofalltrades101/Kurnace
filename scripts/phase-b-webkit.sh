#!/bin/sh
# Phase B check in the real app (WKWebView): the compositor preview draws an
# exact video frame, an image and an HTML scene through the Tauri asset server,
# and hot-reloads the scene when its file changes. Opens a window (~40 s).
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
KURN="$ROOT/target/debug/kurn"
WORK=${WORK:-$(mktemp -d -t kurn-pb)}
export KURNACE_HOME="$WORK/home" KURN_SOCKET="/tmp/kurn-pb-$$.sock" PATH="$HOME/.cargo/bin:$PATH"
LOG="$WORK/app.log"
cleanup() {
  pkill -f "target/debug/kurnace-app" 2>/dev/null || true; pkill -f "tauri dev" 2>/dev/null || true
  lsof -ti tcp:1420 | xargs kill 2>/dev/null || true; "$KURN" stop >/dev/null 2>&1 || true
}
trap cleanup EXIT
j() { python3 -c "import sys,json; d=json.load(sys.stdin); print($1)"; }
wait_for() { i=0; until grep -q "$1" "$LOG"; do i=$((i+1)); [ $i -gt 120 ] && { echo "timeout waiting for: $1"; tail -20 "$LOG"; exit 1; }; sleep 1; done; }

cargo build -q -p kurn
"$KURN" project new "$WORK/p" --no-git --width 1280 --height 720 >/dev/null
cd "$WORK/p"
ffmpeg -v error -y -f lavfi -i testsrc2=size=640x360:rate=30 -t 4 -pix_fmt yuv420p "$WORK/bars.mp4"
ffmpeg -v error -y -f lavfi -i "gradients=size=1280x720:c0=0x0f2027:c1=0x2c5364:duration=1:speed=0" -frames:v 1 "$WORK/bg.png"
V=$("$KURN" lib add "$WORK/bars.mp4" --json | j "d[0]['id']")
I=$("$KURN" lib add "$WORK/bg.png" --json | j "d[0]['id']")
"$KURN" scene new title >/dev/null
"$KURN" op batch --args "[
 {\"type\":\"track.add\",\"args\":{\"id\":\"bg\",\"kind\":\"video\"}},
 {\"type\":\"track.add\",\"args\":{\"id\":\"v\",\"kind\":\"video\"}},
 {\"type\":\"track.add\",\"args\":{\"id\":\"s\",\"kind\":\"component\"}},
 {\"type\":\"clip.add\",\"args\":{\"id\":\"img\",\"track\":\"bg\",\"asset\":\"$I\",\"start\":0,\"dur\":90}},
 {\"type\":\"clip.add\",\"args\":{\"id\":\"vid\",\"track\":\"v\",\"asset\":\"$V\",\"start\":0,\"in\":15,\"dur\":90,\"props\":{\"scale\":0.5,\"x\":300,\"y\":150}}},
 {\"type\":\"clip.add\",\"args\":{\"id\":\"ttl\",\"track\":\"s\",\"scene\":\"title\",\"start\":0,\"dur\":90,\"params\":{\"title\":\"WebKit\"}}}]" >/dev/null

echo "== launch app (probe frame 30)"
(cd "$ROOT/app" && KURN_PROJECT="$WORK/p" pnpm tauri dev --config '{"build":{"devUrl":"http://localhost:1420/kurnace.html?selftest=1&probe=30"}}' >"$LOG" 2>&1 &)
wait_for "PREVIEW frame=30"
LINE=$(grep -m1 "PREVIEW frame=30" "$LOG" | sed 's/.*\] //')
echo "   $LINE"
echo "$LINE" | grep -q "visible=3 images=2/2 scenes=1 errors=none" || { echo "FAIL: preview didn't draw image + exact video frame + scene"; exit 1; }

echo "== hot reload: break the scene, expect its error in the preview"
N=$(grep -c "PREVIEW frame=30" "$LOG")
sed -i '' 's/render(frame, ctx) {/render(frame, ctx) { throw new Error("broken on purpose");/' scenes/title/index.html
sleep 2
KURN_ACTOR=agent:claude "$KURN" op clip.set --args '{"clip":"ttl","params":{"title":"again"}}' >/dev/null
i=0; until [ "$(grep -c "PREVIEW frame=30" "$LOG")" -gt "$N" ]; do i=$((i+1)); [ $i -gt 60 ] && { echo "timeout"; exit 1; }; sleep 1; done
LINE=$(grep "PREVIEW frame=30" "$LOG" | tail -1 | sed 's/.*\] //')
echo "   $LINE"
echo "$LINE" | grep -q "broken on purpose" || { echo "FAIL: scene error not shown after hot reload"; exit 1; }
echo "PASS"
