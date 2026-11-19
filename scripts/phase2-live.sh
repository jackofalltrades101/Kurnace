#!/bin/sh
# Phase 2 exit test (spec §17): "Human + agent see the same preview update live".
# An agent edits the timeline through the CLI while the app is open; the app's
# preview must update without reload, attribute the change to the agent, and
# match a headless render of the same frame. Opens a Kurnace window (~30 s).
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
KURN="$ROOT/target/debug/kurn"
WORK=${WORK:-$(mktemp -d -t kurn-p2)}
export KURNACE_HOME="$WORK/home" KURN_SOCKET="/tmp/kurn-p2-$$.sock" PATH="$HOME/.cargo/bin:$PATH"
LOG="$WORK/app.log"
cleanup() {
  pkill -f "target/debug/kurnace-app" 2>/dev/null || true; pkill -f "tauri dev" 2>/dev/null || true
  lsof -ti tcp:1420 | xargs kill 2>/dev/null || true; "$KURN" stop >/dev/null 2>&1 || true
}
trap cleanup EXIT
j() { python3 -c "import sys,json; d=json.load(sys.stdin); print($1)"; }
wait_for() { i=0; until grep -q "$1" "$LOG"; do i=$((i+1)); [ $i -gt 120 ] && { echo "timeout waiting for: $1"; tail -20 "$LOG"; exit 1; }; sleep 1; done; }

cargo build -q -p kurn
"$KURN" project new "$WORK/p" --no-git >/dev/null
cd "$WORK/p"
ffmpeg -v error -y -f lavfi -i "gradients=size=1920x1080:c0=0x0f2027:c1=0x2c5364:duration=1:speed=0" -frames:v 1 "$WORK/bg.png"
BG=$("$KURN" lib add "$WORK/bg.png" --json | j "d[0]['id']")
"$KURN" op batch --args "[
 {\"type\":\"track.add\",\"args\":{\"id\":\"v1\",\"kind\":\"video\"}},
 {\"type\":\"track.add\",\"args\":{\"id\":\"t1\",\"kind\":\"text\"}},
 {\"type\":\"clip.add\",\"args\":{\"id\":\"bg\",\"track\":\"v1\",\"asset\":\"$BG\",\"start\":0,\"dur\":90}},
 {\"type\":\"clip.add\",\"args\":{\"id\":\"title\",\"track\":\"t1\",\"component\":\"text\",\"start\":0,\"dur\":90,\"props\":{\"text\":\"Before\"}}}]" >/dev/null

echo "== launch app"
(cd "$ROOT/app" && KURN_PROJECT="$WORK/p" pnpm tauri dev --config '{"build":{"devUrl":"http://localhost:1420/kurnace.html?selftest=1&capture=45"}}' >"$LOG" 2>&1 &)
wait_for "TIMELINE rev"
echo "   app is live: $(grep -m1 'TIMELINE rev' "$LOG" | sed 's/.*\] //')"

echo "== agent edits through the CLI"
KURN_ACTOR=agent:claude "$KURN" op clip.set --args '{"clip":"title","props":{"text":"After: edited by an agent","fill":"#ffd866"}}'
wait_for "origin=agent:claude"
grep "origin=agent:claude" "$LOG" | head -1 | sed 's/.*\] /   app saw: /'
grep -q "origin=agent:claude changed=title" "$LOG" || { echo "FAIL: change not attributed to the title clip"; exit 1; }
wait_for "CAPTURED"

echo "== compare live preview with headless render"
"$KURN" render still --frame 45 --out "$WORK/p/renders/parity/headless-f45.png" >/dev/null
PSNR=$(ffmpeg -hide_banner -i "$WORK/p/renders/parity/headless-f45.png" -i "$WORK/p/renders/parity/live-f45.png" \
  -lavfi "[0]format=rgb24[a];[1]format=rgb24[b];[a][b]psnr" -f null - 2>&1 | sed -n 's/.*average:\([^ ]*\).*/\1/p')
echo "   PSNR live vs headless: $PSNR dB"
awk -v p="$PSNR" 'BEGIN{exit !(p=="inf" || p+0 >= 30)}' || { echo "FAIL: preview doesn't match render"; exit 1; }
echo "PASS  (frames in $WORK/p/renders/parity)"
