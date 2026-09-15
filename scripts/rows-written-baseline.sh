#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
out="${1:?Usage: rows-written-baseline.sh /absolute/output-directory}"
case "${2:-text}" in text|tools) ;; *) echo 'Workload must be text or tools' >&2; exit 2;; esac
mkdir -p "$out"
out="$(cd "$out" && pwd)"
rm -f "$out/report.json" # a failed rerun must not leave a stale success receipt
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0
cargo run --locked -q -p cypher-doc --example rows_written_fixture -- "${2:-text}" > "$out/fixture.json"
export CYPHER_ROWS_FIXTURE="$out/fixture.json" NO_COLOR=1
(cd edge && npx tsc --noEmit -p tsconfig.rows.json && npx vitest run -c vitest.rows.config.ts) > "$out/workerd.log" 2>&1
python3 - "$out" <<'PY'
import json, pathlib, sys, subprocess, hashlib
out=pathlib.Path(sys.argv[1]); report={}
for line in (out/'workerd.log').read_text().splitlines():
    if line.startswith('ROWS_BASELINE_'):
        name, value=line.split('=',1); report[name]=json.loads(value)
assert len(report)>=4, 'Expected all workload results'
report['source']=subprocess.check_output(['git','rev-parse','HEAD'],text=True).strip()
report['worktreeDirty']=bool(subprocess.check_output(['git','status','--porcelain'],text=True).strip())
report['fixtureSHA256']=hashlib.sha256((out/'fixture.json').read_bytes()).hexdigest()
report['lockfileSHA256']={name:hashlib.sha256(pathlib.Path(name).read_bytes()).hexdigest()
                        for name in ['Cargo.lock','edge/package-lock.json']}
report['scope']='local real SQLite; simulated sockets and time; no cloud billing/latency measurement'
(out/'report.json').write_text(json.dumps(report,indent=2)+'\n')
print(out/'report.json')
PY
