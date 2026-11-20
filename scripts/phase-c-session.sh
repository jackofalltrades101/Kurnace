#!/bin/sh
# Phase C exit test: scripted edit session in the app UI (headless Chrome against
# the real core) while an agent edits concurrently. See app/scripts/sessions/phase-c.mjs.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
export KURN_BIN="$ROOT/target/debug/kurn"
WORK=${WORK:-$(mktemp -d -t kurn-pc)}
export KURNACE_HOME="$WORK/home" KURN_SOCKET="/tmp/kurn-pc-$$.sock" PATH="$HOME/.cargo/bin:$PATH"
cleanup() { lsof -ti tcp:1420 | xargs kill 2>/dev/null || true; "$KURN_BIN" stop >/dev/null 2>&1 || true; }
trap cleanup EXIT
cargo build -q -p kurn
"$KURN_BIN" project new "$WORK/p" --no-git --width 1280 --height 720 >/dev/null
cd "$WORK/p"
"$KURN_BIN" scene new intro >/dev/null
"$KURN_BIN" op batch --args '[
 {"type":"track.add","args":{"id":"v1","kind":"component","order":0}},
 {"type":"track.add","args":{"id":"v2","kind":"component","order":1}},
 {"type":"clip.add","args":{"id":"c1","track":"v1","component":"solid","start":0,"dur":60,"props":{"fill":"#3d5a8a"}}},
 {"type":"clip.add","args":{"id":"c2","track":"v1","component":"solid","start":200,"dur":60,"props":{"fill":"#7a4e8a"}}},
 {"type":"clip.add","args":{"id":"c3","track":"v2","component":"solid","start":0,"dur":150,"props":{"fill":"#3c7a5c"}}}]' >/dev/null
(cd "$ROOT/app" && pnpm dev >"$WORK/vite.log" 2>&1 &)
i=0; until curl -s -o /dev/null http://localhost:1420/kurnace.html; do i=$((i+1)); [ $i -gt 60 ] && { echo "vite didn't start"; exit 1; }; sleep 0.5; done
rc=0
node "$ROOT/app/scripts/ui-shot.mjs" "$WORK/p" "$WORK/session.png" --wait 4000 --script "$ROOT/app/scripts/sessions/phase-c.mjs" >"$WORK/run.log" 2>&1 || rc=$?
grep -E "^   |^-- |SCRIPT FAIL|Error:" "$WORK/run.log" | grep -v "404" || true
[ $rc -eq 0 ] && echo "PASS  (screenshot $WORK/session.png)" || { echo "FAIL (log: $WORK/run.log)"; exit 1; }
