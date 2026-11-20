#!/bin/sh
# Phase 3 exit check (spec §17): the canvas timeline renders a project with
# waveform/thumbnail assets, shows an agent's live presence, and surfaces a
# destructive agent op in the approval dialog. Uses the headless Chrome harness
# so it does not need a window.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
KURN="$ROOT/target/debug/kurn"
WORK=${WORK:-$(mktemp -d -t kurn-p3)}
export KURNACE_HOME="$WORK/home" KURN_SOCKET="/tmp/kurn-p3-$$.sock"
OUT="$WORK/ui.png"
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

echo "== build + project"
cargo build -q -p kurn
ffmpeg -v error -y -f lavfi -i "color=c=0x26344f:s=320x180" -frames:v 1 "$WORK/bg.png"
ffmpeg -v error -y -f lavfi -i "testsrc=size=320x180:rate=30:duration=2" -c:v libx264 -pix_fmt yuv420p "$WORK/bars.mp4"
ffmpeg -v error -y -f lavfi -i "sine=frequency=330:duration=2" "$WORK/tone.wav"
"$KURN" project new "$WORK/p" --no-git --width 320 --height 180 >/dev/null
cd "$WORK/p"
BG=$("$KURN" lib add "$WORK/bg.png" --json | j "d[0]['id']")
VID=$("$KURN" lib add "$WORK/bars.mp4" --json | j "d[0]['id']")
AUD=$("$KURN" lib add "$WORK/tone.wav" --json | j "d[0]['id']")
"$KURN" op batch --args "[
 {\"type\":\"track.add\",\"args\":{\"id\":\"v1\",\"kind\":\"video\",\"name\":\"Background\"}},
 {\"type\":\"track.add\",\"args\":{\"id\":\"a1\",\"kind\":\"audio\",\"name\":\"Tone\"}},
 {\"type\":\"clip.add\",\"args\":{\"id\":\"bg\",\"track\":\"v1\",\"asset\":\"$BG\",\"start\":0,\"dur\":60,\"keyframes\":{\"opacity\":[{\"t\":0,\"v\":0},{\"t\":15,\"v\":1}]}}},
 {\"type\":\"clip.add\",\"args\":{\"id\":\"vid\",\"track\":\"v1\",\"asset\":\"$VID\",\"start\":60,\"in\":0,\"out\":30}},
 {\"type\":\"clip.add\",\"args\":{\"id\":\"tone\",\"track\":\"a1\",\"asset\":\"$AUD\",\"start\":0,\"dur\":60}},
 {\"type\":\"marker.add\",\"args\":{\"t\":30,\"label\":\"beat\"}}]" >/dev/null

echo "== agent presence + pending destructive op"
KURN_ACTOR=agent:claude "$KURN" presence set --playhead 45 --select bg,vid --json >/dev/null
KURN_ACTOR=agent:claude "$KURN" op ripple.delete --args '{"clip":"vid"}' --json >/dev/null 2>&1 || true

echo "== headless canvas + presence + approval dialog"
if ! curl -sf http://localhost:1420/kurnace.html >/dev/null 2>&1; then
  (cd "$ROOT/app" && pnpm dev >"$WORK/vite.log" 2>&1 &)
  STARTED_VITE=1
fi
i=0; until curl -sf http://localhost:1420/kurnace.html >/dev/null 2>&1; do
  i=$((i+1)); [ $i -gt 40 ] && { echo "timeout starting Vite"; exit 1; }; sleep 0.5
done
(node "$ROOT/app/scripts/ui-shot.mjs" "$WORK/p" "$OUT" --width 1500 --height 900 --wait 4500 \
  --eval "(async()=>{document.querySelector('.transport button')?.click();await new Promise(r=>setTimeout(r,1200));window.__played=Number(document.querySelector('.transport')?.dataset.frame??0);[...document.querySelectorAll('.render-bar button')].find(b=>b.textContent?.includes('Still'))?.click();await new Promise(r=>setTimeout(r,4500));})()" \
  --probe "({canvas:!!document.querySelector('canvas.tl-canvas'),who:[...document.querySelectorAll('.tl-who')].map(e=>e.textContent),pending:!!document.querySelector('.pending-dialog'),played:window.__played,still:document.querySelector('.render-out code')?.textContent,clips:document.querySelector('.tl-tools-2')?.textContent?.match(/\\d+ clips/)?.[0]})" \
  >"$LOG" 2>&1 & echo $! > "$WORK/ui.pid")
i=0; until grep -q '^saved ' "$LOG"; do
  i=$((i+1)); [ $i -gt 80 ] && { echo "timeout waiting for UI screenshot"; tail -20 "$LOG"; exit 1; }; sleep 0.5
done
kill "$(cat "$WORK/ui.pid")" 2>/dev/null || true

P=$(sed -n 's/^probe: //p' "$LOG")
echo "   $P"
for want in '"canvas":true' '"who":["claude"]' '"pending":true' '"clips":"3 clips"' 'renders/f'; do
  echo "$P" | grep -qF "$want" || { echo "FAIL: missing $want"; exit 1; }
done
PLAYED=$(echo "$P" | python3 -c 'import sys,json; print(json.load(sys.stdin).get("played",0))')
[ "$PLAYED" -gt 0 ] || { echo "FAIL: preview play button did not advance (frame $PLAYED)"; exit 1; }
echo "   player advanced to frame $PLAYED"
echo "PASS  (screenshot $OUT)"
