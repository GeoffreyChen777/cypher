#!/usr/bin/env bash
# One evaluation round of a committed extension: export <sha>, run every stubbed and live set.
#
#   eval/round.sh <sha> <label> [stub|live|all]
#
# Results: $D/results/<label>-{stub-recorded,stub-approve,stub-regression,live,live-mutations}.json,
# compared with the corrected before runs. Live needs the network and a Claude account (escalate).
set -euo pipefail
sha=$1
label=$2
what=${3:-all}
D=${CYPHER_EVAL_DIR:-$HOME/.cypher-development/permissions-eval}
EVAL=$(cd "$(dirname "$0")" && pwd)
repo=$(git -C "$EVAL" rev-parse --show-toplevel)
ext=$D/$label/pi-runtime/extensions
if [ ! -d "$ext" ]; then
  mkdir -p "$D/$label"
  git -C "$repo" archive "$sha" pi-runtime/extensions | tar -x -C "$D/$label"
  git -C "$repo" rev-parse "$sha" > "$D/$label/COMMIT"
fi
cd "$D"
run() { node "$EVAL/run.mjs" --extensions "$ext" --owner-words full "$@" > /dev/null; }
pids=()
if [ "$what" != live ]; then
  for stub in recorded approve; do
    run --cases corpus/mutations-heldout.json,corpus/adversarial.json,corpus/adversarial-fresh.json --stub $stub --concurrency 4 \
      --label "$label-stub-$stub" --out "results/$label-stub-$stub.json" --compare "results/before-stub-$stub.json" &
    pids+=($!)
  done
  run --cases corpus/regression-heldout.json --stub recorded --concurrency 3 \
    --label "$label-stub-regression" --out "results/$label-stub-regression.json" --compare results/before-stub-regression.json &
  pids+=($!)
fi
if [ "$what" != stub ]; then
  run --cases corpus/heldout.json,corpus/adversarial.json,corpus/adversarial-fresh.json --context corpus/context-heldout.json --live --concurrency 6 \
    --label "$label-live" --out "results/$label-live.json" --compare results/before-live.json &
  pids+=($!)
  run --cases corpus/mutations-heldout.json --sample 40 --seed 20261122 --context corpus/context-heldout.json --live --concurrency 4 \
    --label "$label-live-mutations" --out "results/$label-live-mutations.json" &
  pids+=($!)
fi
for pid in "${pids[@]}"; do wait "$pid"; done
echo "round $label ($sha) done: $D/results/$label-*.json"
