#!/usr/bin/env python3
"""The root-cause table of RESULTS.md: every live ask on owner-approved held-out, by class.

    python3 -I rootcause.py DATA_DIR CLASSES LIVE APPROVE RECORDED [CONSENT_APPROVE]

CLASSES is a hand-made map {"cases": {"<id>": {"class": "a".."g", "note": "..."}}} (kept in the data
dir: it quotes the owner). LIVE/APPROVE/RECORDED are run.mjs results (live, --stub approve,
--stub recorded) of the same extension on the same corpus. "Forced by" is the live decision's
mechanism (mechanism.mjs, mapped to the checks: A consent, B target, judge, floor; D lifecycle
counts with A when its reason is the reply check); "deterministic" counts the cases that also ask
with the approve-all stub, i.e. the code asks whatever the reviewing model says.
"""
import collections
import json
import os
import sys

CLASSES = {
    "a": "consent present, phrased differently than the check accepts",
    "b": "action given, target only implied by the task (the chat's own branch/PR/release commit)",
    "c": "target misparsed (names in prose or in the line read as other entities)",
    "d": "judge cautious without a concrete concern",
    "e": "should ask (a concrete risk the owner didn't take on)",
    "f": "the chat's own work, not recognised (extra class: provenance the code can't see)",
    "g": "risk overgraded: a read-only or private, reversible action graded as a danger (extra class)",
}

data, classes_file, live_file, approve_file, recorded_file = sys.argv[1:6]
consent_file = sys.argv[6] if len(sys.argv) > 6 else None
load = lambda path: json.load(open(path if os.path.isabs(path) else os.path.join(data, path)))
classes = load(classes_file)["cases"]
rows = lambda path: {r["id"]: r for r in load(path)["rows"] if r.get("base") is None and not r.get("adversarial")}
live, approve, recorded = rows(live_file), rows(approve_file), rows(recorded_file)
consent = rows(consent_file) if consent_file else {}
cases = {c["id"]: c for c in load("corpus/heldout.json")["cases"]}


def check(row):
    mech = row.get("mechanism", "")
    if mech == "D lifecycle" and "doesn't plainly agree" in (row.get("explicit") or ""):
        return "A consent"
    return {"judge said ask": "judge"}.get(mech, mech)


def clip(text, n=70):
    one = " ".join(str(text or "").split())
    return (one[:n] + "…" if len(one) > n else one).replace("|", "\\|")


asks = [r for r in live.values() if not r.get("unscorable") and r["label"]["owner"] == "allowed" and r["outcome"] == "ask"]
missing = [r["id"] for r in asks if str(r["id"]) not in classes]
if missing:
    sys.exit(f"unclassified asks: {missing}")
scored = [r for r in live.values() if not r.get("unscorable") and r["label"]["owner"] == "allowed"]
print(f"Live asks on owner-approved held-out: {len(asks)}/{len(scored)}.")
print()
print("| class | n | forced by (live) | also asks under approve-all | examples |")
print("|---|---|---|---|---|")
by = collections.defaultdict(list)
for r in asks:
    by[classes[str(r["id"])]["class"]].append(r)
for key in CLASSES:
    group = by.get(key, [])
    if not group:
        continue
    forced = collections.Counter(check(r) for r in group)
    determined = sum(1 for r in group if approve.get(r["id"], {}).get("outcome") == "ask")
    examples = "<br>".join(f"#{r['id']} `{clip(cases[r['id']].get('command') or ' '.join(cases[r['id']].get('paths', [])), 60)}`: {clip(classes[str(r['id'])]['note'], 90)}" for r in group[:3])
    print(f"| ({key}) {CLASSES[key]} | {len(group)} | {', '.join(f'{k} {v}' for k, v in forced.most_common())} | {determined} | {examples} |")
print()
denied = [r for r in live.values() if not r.get("unscorable") and r["label"]["owner"] == "denied"]
print("| owner-denied | how the owner answered | live | approve-all stub | consent-claim approve-all | recorded stub | what decides it |")
print("|---|---|---|---|---|---|---|")
for r in sorted(denied, key=lambda r: r["id"]):
    case = cases[r["id"]]
    choice = case["recorded"].get("choice") or "(not recorded)"
    a, s = approve.get(r["id"], {}), recorded.get(r["id"], {})
    k = consent.get(r["id"], {})
    why = r.get("explicit") or (case["recorded"].get("kind") or r.get("kind") or "")
    print(f"| #{r['id']} `{clip(case.get('command') or ' '.join(case.get('paths', [])), 50)}` | {choice} | {r['outcome']} ({check(r) if r['outcome'] == 'ask' else r.get('mechanism')}) | {a.get('outcome')} | {k.get('outcome', '–')} | {s.get('outcome')} | tier {r.get('tier')}: {clip(r.get('kind') or '', 60)}; {clip(r.get('explicit') or (r.get('reviewer') or ''), 90)} |")
