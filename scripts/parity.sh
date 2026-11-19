#!/bin/sh
# Preview/render parity check (spec §10.4): capture frames from the Revideo
# Player inside the real Tauri webview and compare with headless `kurn render still`.
# Gate: PSNR >= 30 dB. Exact frame accuracy is covered by modules/render.revideo/test/sync.test.ts;
# the residual here is font rasterization (WebKit vs Chromium), see README.
# Usage: scripts/parity.sh [project-dir] [frames]   e.g. scripts/parity.sh projects/demo 60,200
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
PROJ=$(cd "${1:-$ROOT/projects/demo}" && pwd)
FRAMES=${2:-60,200}
LOG=$(mktemp -t kurn-parity)
export PATH="$HOME/.cargo/bin:$PATH"

cleanup() { pkill -f "target/debug/kurnace-app" 2>/dev/null || true; pkill -f "tauri dev" 2>/dev/null || true; lsof -ti tcp:1420 | xargs kill 2>/dev/null || true; }
trap cleanup EXIT

rm -rf "$PROJ/renders/parity"
for f in $(echo "$FRAMES" | tr , ' '); do (cd "$PROJ" && "$ROOT/bin/kurn" render still --frame "$f" --out "$PROJ/renders/parity/headless-f$f.png" >/dev/null); done

(cd "$ROOT/app" && KURN_PROJECT="$PROJ" pnpm tauri dev --config "{\"build\":{\"devUrl\":\"http://localhost:1420/kurnace.html?parity=$FRAMES&capwait=${CAPWAIT:-1000}\"}}" >"$LOG" 2>&1 &)
i=0; until grep -q PARITY_DONE "$LOG"; do i=$((i+1)); [ $i -gt 120 ] && { echo "timeout; log: $LOG"; exit 1; }; sleep 1; done

status=0
for f in $(echo "$FRAMES" | tr , ' '); do
  psnr=$(ffmpeg -hide_banner -i "$PROJ/renders/parity/headless-f$f.png" -i "$PROJ/renders/parity/webview-f$f.png" \
    -lavfi "[0]format=rgb24[a];[1]format=rgb24[b];[a][b]psnr" -f null - 2>&1 | sed -n 's/.*average:\([^ ]*\).*/\1/p')
  ok=$(awk -v p="$psnr" 'BEGIN{print (p=="inf" || p+0 >= 30) ? "ok" : "MISMATCH"}')
  [ "$ok" = ok ] || status=1
  echo "frame $f: PSNR ${psnr} dB  $ok"
done
exit $status
