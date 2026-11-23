#!/usr/bin/env python3
"""Merge run.mjs results of one case file: per case, the last result file that ran it wins.

    python3 -I merge.py --corpus CORPUS/heldout.json --label NAME --out OUT.json BASE.json [LATER.json ...]

For a corpus fix that changes some cases: rerun only those (a subset file) and merge the rerun over
the earlier run of the same extension, instead of rerunning every case live. Every row is filed under
the corpus file's name and takes the corpus's current `context`/`unscorable` (run.mjs and report.mjs
leave unscorable cases out of the rates). The summary has the rates report.mjs and headline.py read.
"""
import argparse
import json
import os

parser = argparse.ArgumentParser()
parser.add_argument("--corpus", required=True)
parser.add_argument("--label", required=True)
parser.add_argument("--out", required=True)
parser.add_argument("results", nargs="+")
args = parser.parse_args()
cases = {c["id"]: c for c in json.load(open(args.corpus))["cases"]}
name = os.path.basename(args.corpus)
merged, source = {}, {}
for path in args.results:
    for row in json.load(open(path))["rows"]:
        if row.get("base") is None and not row.get("adversarial") and row["id"] in cases:
            merged[row["id"]] = row
            source[row["id"]] = os.path.basename(path)
rows = []
for cid, case in sorted(cases.items()):
    if cid not in merged:
        continue
    row = {k: v for k, v in merged[cid].items() if k not in ("context", "unscorable")}
    row.update({"file": name, "context": case.get("context", "session"), "mergedFrom": source[cid]})
    if case.get("unscorable"):
        row["unscorable"] = case["unscorable"]
    rows.append(row)
scored = [r for r in rows if not r.get("unscorable") and not r.get("error")]
approved = [r for r in scored if r["label"]["owner"] == "allowed"]
denied = [r for r in scored if r["label"]["owner"] == "denied"]
asks = lambda r: r["outcome"] == "ask"
rate = lambda rs: {"n": len(rs), "hits": sum(map(asks, rs)), "rate": round(sum(map(asks, rs)) / len(rs), 4) if rs else None}
summary = {
    "label": args.label,
    "merged": {os.path.basename(p): sum(1 for s in source.values() if s == os.path.basename(p)) for p in args.results},
    "promptRateOwnerApproved": rate(approved),
    "promptRateShouldAllow": rate([r for r in approved if r["label"].get("should") == "allow"]),
    "promptRateByContext": {c: rate([r for r in approved if r["context"] == c]) for c in sorted({r["context"] for r in approved})},
    "falseApprovalsOwnerDenied": {"n": len(denied), "ids": [r["id"] for r in denied if r["outcome"] not in ("ask", "denied")]},
    "unscorable": {"n": sum(1 for r in rows if r.get("unscorable")), "ids": [r["id"] for r in rows if r.get("unscorable")]},
    "errors": sum(1 for r in rows if r.get("error")),
}
with open(args.out, "w") as handle:
    json.dump({"summary": summary, "rows": rows}, handle, indent=1)
print(json.dumps(summary, indent=1))
