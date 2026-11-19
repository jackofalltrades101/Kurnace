#!/bin/sh
# Every automated check, fastest first. Usage: scripts/test-all.sh [--with-app]
# --with-app also runs the checks that open a Kurnace window (~1 min).
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd); cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"
echo "== rust unit tests";            cargo test -q --workspace 2>&1 | grep -E "test result|FAILED|panicked"
echo "== build kurn";                 cargo build -q -p kurn
echo "== timeline model (TS)";        node --test 'packages/*/test/*.test.ts' 2>&1 | grep -E "^ℹ (pass|fail)"
echo "== timeline canvas maths (TS)";  node --test 'app/test/*.test.ts' 2>&1 | grep -E "^ℹ (pass|fail)"
echo "== render frame accuracy";      node --test 'modules/*/test/*.test.ts' 2>&1 | grep -E "^ℹ (pass|fail)"
echo "== phase 1: concurrency+crash"; scripts/phase1-stress.sh | grep -E "PASS|FAIL|ack"
echo "== phase 1: voiced slideshow";  scripts/phase1-exit.sh | tail -2
echo "== phase 4: board → rough cut";  scripts/phase4-board.sh | tail -4
echo "== phase 5: 3D (Godot C# + three.js)"; scripts/phase5-3d.sh | tail -8
echo "== phase 6: search/budget/export"; scripts/phase6-polish.sh | tail -10
echo "== app UI typecheck";           (cd app && npx tsc -p . && echo ok)
if [ "${1:-}" = "--with-app" ]; then
  echo "== phase 2: live preview (opens a window)"; scripts/phase2-live.sh | tail -4
  echo "== preview parity (opens a window)"; scripts/parity.sh
  echo "== phase 3: canvas timeline (headless Chrome)"; scripts/phase3-ui.sh | tail -4
  echo "== phase 4: board UI (headless Chrome)"; scripts/phase4-ui.sh | tail -4
fi
