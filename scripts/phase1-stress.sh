#!/bin/sh
# Phase 1 exit criteria (spec §17):
#  1. two agents editing the same timeline concurrently leave it valid;
#  2. docs survive a core crash (kill -9) with every acknowledged op intact.
set -eu
ROOT=$(cd "$(dirname "$0")/.." && pwd)
KURN="$ROOT/target/debug/kurn"
WORK=${WORK:-$(mktemp -d -t kurn-stress)}
export KURNACE_HOME="$WORK/home" KURN_SOCKET="/tmp/kurn-stress-$$.sock"
trap '"$KURN" stop >/dev/null 2>&1 || true' EXIT
j() { python3 -c "import sys,json; d=json.load(sys.stdin); print($1)"; }

"$KURN" project new "$WORK/p" --no-git >/dev/null
cd "$WORK/p"
ffmpeg -v error -y -f lavfi -i color=red:size=64x64 -frames:v 1 "$WORK/red.png"
A=$("$KURN" lib add "$WORK/red.png" --json | j "d[0]['id']")
"$KURN" op batch --args "[
 {\"type\":\"track.add\",\"args\":{\"id\":\"ta\",\"kind\":\"video\"}},
 {\"type\":\"track.add\",\"args\":{\"id\":\"tb\",\"kind\":\"video\"}},
 {\"type\":\"track.add\",\"args\":{\"id\":\"shared\",\"kind\":\"video\"}},
 {\"type\":\"clip.add\",\"args\":{\"id\":\"x\",\"track\":\"shared\",\"asset\":\"$A\",\"start\":0,\"dur\":10}}]" >/dev/null

echo "== 1. concurrency: 2 agents × 40 ops, plus contested slots on a shared track"
agent() {
  name=$1 track=$2
  n=0
  while [ $n -lt 20 ]; do
    KURN_ACTOR=agent:$name "$KURN" op clip.add --args "{\"track\":\"$track\",\"asset\":\"$A\",\"start\":$((n*10)),\"dur\":10}" >/dev/null
    # Both agents race for the same slot on the shared track; at most one may win.
    KURN_ACTOR=agent:$name "$KURN" op clip.add --args "{\"id\":\"slot$n-$name\",\"track\":\"shared\",\"asset\":\"$A\",\"start\":$((100+n*10)),\"dur\":10}" >/dev/null 2>&1 || true
    KURN_ACTOR=agent:$name "$KURN" op clip.move --args "{\"clip\":\"x\",\"start\":$(( (n % 5) * 2 ))}" >/dev/null 2>&1 || true
    n=$((n+1))
  done
}
agent alpha ta & P1=$!
agent beta tb & P2=$!
wait $P1 $P2
"$KURN" timeline show --json > "$WORK/tl.json"
python3 - "$WORK/tl.json" <<'PY'
import json, sys
d = json.load(open(sys.argv[1])); tl = d["timeline"]
errs = [i for i in d["issues"] if i["level"] == "error"]
by = lambda t: [c for c in tl["clips"].values() if c["track"] == t]
slots = [k for k in tl["clips"] if k.startswith("slot")]
won = {k.split("-")[0] for k in slots}
print(f"  ta={len(by('ta'))} tb={len(by('tb'))} shared slots won={len(slots)} (unique slots {len(won)}) errors={len(errs)}")
assert not errs, errs
assert len(by("ta")) == 20 and len(by("tb")) == 20, "lost ops"
assert len(slots) == len(won) == 20, "a slot was double-booked or lost"
print("  PASS")
PY

echo "== 2. crash safety: kill -9 the core mid-stream"
"$KURN" op track.add --args '{"id":"tc","kind":"video"}' >/dev/null
: > "$WORK/acked"
( n=0; while [ $n -lt 200 ]; do
    if KURN_NO_AUTOSTART=1 KURN_ACTOR=agent:gamma "$KURN" op clip.add --args "{\"id\":\"k$n\",\"track\":\"tc\",\"asset\":\"$A\",\"start\":$((n*5)),\"dur\":5}" >/dev/null 2>&1; then
      echo "k$n" >> "$WORK/acked"
    fi
    n=$((n+1)); done ) &
LOOP=$!
sleep 1.5
PID=$("$KURN" status --json | j "d['pid']")
kill -9 "$PID"
wait $LOOP || true
ACKED=$(wc -l < "$WORK/acked" | tr -d ' ')
"$KURN" timeline show --json > "$WORK/tl2.json"   # auto-restarts the core, reloads from disk
python3 - "$WORK/tl2.json" "$WORK/acked" <<'PY'
import json, sys
tl = json.load(open(sys.argv[1]))["timeline"]
acked = open(sys.argv[2]).read().split()
missing = [k for k in acked if k not in tl["clips"]]
print(f"  acknowledged before crash: {len(acked)} · present after restart: {len(acked) - len(missing)}")
assert acked, "core died before any op was acknowledged; increase the sleep"
assert not missing, f"lost acknowledged ops: {missing[:5]}"
print("  PASS")
PY
