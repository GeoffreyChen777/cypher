#!/usr/bin/env python3
"""The one-table headline of RESULTS.md across rounds, from run.mjs results.

    python3 -I headline.py RESULTS_DIR before:after:round2:round3f:round4 [NAMES]

Per side it reads <side>-live.json (+ <side>-live-fresh.json), <side>-stub-{recorded,approve,regression}.json
and the consent-claim runs <side>c-… (before ignores the consent field, so its plain runs stand for it;
a side with no consent-claim live run uses its consent-claim stub, which equals the forced verdict).
"""
import json
import os
import sys

results, sides = sys.argv[1], sys.argv[2].split(":")
names = sys.argv[3].split(":") if len(sys.argv) > 3 else sides


def rows(name):
    path = os.path.join(results, f"{name}.json")
    return json.load(open(path))["rows"] if os.path.exists(path) else None


def merged(*names):
    out, seen = [], set()
    found = False
    for name in names:
        got = rows(name)
        if got is None:
            continue
        found = True
        for r in got:
            key = (r.get("file"), r.get("id"))
            if key not in seen:
                seen.add(key)
                out.append(r)
    return out if found else None


approves = lambda r: r.get("outcome") not in (None, "ask", "denied")
forced = lambda r: r.get("adversarialStub", {}).get("outcome") not in (None, "ask", "denied")
fresh = lambda r: r.get("file") == "adversarial-fresh.json"
plain = lambda r: r.get("base") is None and not r.get("adversarial") and not r.get("error")


def pct(rs, pred):
    return f"{100 * sum(map(pred, rs)) / len(rs):.1f}%" if rs else "–"


def count(rs, pred, sel):
    picked = [r for r in rs if sel(r)] if rs else []
    return f"{sum(map(pred, picked))}" if picked else "–"


def side_values(side):
    consent = "before" if side == "before" else f"{side}c"
    live = merged(f"{side}-live", f"{side}-live-fresh") or []
    held = [r for r in live if plain(r) and r.get("file") == "heldout.json"]
    approved = [r for r in held if r.get("label", {}).get("owner") == "allowed"]
    v = {}
    v["live prompt rate, owner-approved held-out"] = pct(approved, lambda r: r["outcome"] == "ask") if approved else "(not run)"
    v["live prompt rate, should=allow"] = pct([r for r in approved if r.get("label", {}).get("should") == "allow"], lambda r: r["outcome"] == "ask") if approved else "(not run)"
    denied = [r for r in held if r.get("label", {}).get("owner") == "denied"]
    v["live false approvals, owner-denied"] = f"{sum(map(approves, denied))}/{len(denied)}" if denied else "(not run)"
    adv = [r for r in live if r.get("adversarial") and not r.get("error")]
    v["original adversarial (59), live"] = count(adv, approves, lambda r: not fresh(r))
    v["fresh adversarial (124), live"] = count(adv, approves, fresh)
    v["original adversarial, forced, plain stub"] = count(adv, forced, lambda r: not fresh(r) and "adversarialStub" in r)
    v["fresh adversarial, forced, plain stub"] = count(adv, forced, lambda r: fresh(r) and "adversarialStub" in r)
    if not adv:
        # No live run: the recorded stub is the forced verdict.
        ps = [r for r in (merged(f"{side}-stub-recorded", f"{side}-fresh-stub-recorded") or []) if r.get("adversarial")]
        v["original adversarial, forced, plain stub"] = count(ps, approves, lambda r: not fresh(r)) + " (stub)"
        v["fresh adversarial, forced, plain stub"] = count(ps, approves, fresh) + " (stub)"
        v["original adversarial (59), live"] = v["fresh adversarial (124), live"] = "(not run)"
    clive = merged(f"{consent}-live", f"{consent}-live-fresh") if side != "before" else None
    cadv = [r for r in (clive or []) if r.get("adversarial") and "adversarialStub" in r]
    if cadv:
        v["original adversarial, forced, consent-claim stub"] = count(cadv, forced, lambda r: not fresh(r))
        v["fresh adversarial, forced, consent-claim stub"] = count(cadv, forced, fresh)
    else:
        cstub = merged(f"{consent}-stub-recorded", f"{consent}-fresh-stub-recorded") or []
        cs = [r for r in cstub if r.get("adversarial")]
        v["original adversarial, forced, consent-claim stub"] = count(cs, approves, lambda r: not fresh(r)) if side != "before" else v["original adversarial, forced, plain stub"]
        v["fresh adversarial, forced, consent-claim stub"] = count(cs, approves, fresh) if side != "before" else v["fresh adversarial, forced, plain stub"]
    ca = [r for r in (merged(f"{consent}-stub-approve") or []) if plain(r)]
    cd = [r for r in ca if r.get("label", {}).get("owner") == "denied"]
    v["owner-denied approved, approve-all, consent-claim stub"] = f"{sum(map(approves, cd))}/{len(cd)}" if cd else "–"
    pa = [r for r in (merged(f"{side}-stub-approve") or []) if plain(r) and r.get("label", {}).get("owner") == "allowed"]
    pc = [r for r in ca if r.get("label", {}).get("owner") == "allowed"]
    v["prompt rate, approve-all stub (plain / consent-claim)"] = f"{pct(pa, lambda r: r['outcome'] == 'ask')} / {pct(pc, lambda r: r['outcome'] == 'ask')}"
    rec = merged(f"{side}-stub-recorded") or []
    by = {(r.get("file"), r.get("id")): r.get("outcome") for r in rec if not r.get("error")}
    mut = [(r, by.get((r.get("file"), r.get("base")))) for r in rec if r.get("base") is not None and not r.get("error")]
    mut = [(r, b) for r, b in mut if b is not None and b != "ask"]
    v["mutation stability, bases that didn't ask (stub recorded, plain)"] = pct(mut, lambda m: (m[0]["outcome"] == "ask") == (m[1] == "ask")) if mut else "–"
    base_reg = {r["id"]: r for r in (rows("before-stub-regression") or [])}
    reg = rows(f"{side}-stub-regression")
    if side == "before" or not reg:
        v["regression sample: newly ask / newly pass"] = "–"
    else:
        ch = [r for r in reg if r["id"] in base_reg and (base_reg[r["id"]]["outcome"] == "ask") != (r["outcome"] == "ask")]
        v["regression sample: newly ask / newly pass"] = f"{sum(r['outcome'] == 'ask' for r in ch)} / {sum(r['outcome'] != 'ask' for r in ch)}"
    return v


values = [side_values(s) for s in sides]
print("| metric | " + " | ".join(names) + " |")
print("|---|" + "---|" * len(sides))
for key in values[0]:
    print(f"| {key} | " + " | ".join(v.get(key, "–") for v in values) + " |")
