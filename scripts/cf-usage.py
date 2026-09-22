#!/usr/bin/env python3
"""Cloudflare usage against the Workers Paid allowances, for this billing cycle.

Answers one question: is any meter heading for an overage before the cycle
closes? Every figure is read from the account, not estimated, and the billing
cycle is read from the subscription rather than assumed to be a calendar month.

    CLOUDFLARE_API_TOKEN=... python3 scripts/cf-usage.py [--json] [--fail-at 80]

The token needs Account Analytics Read; reading the cycle boundaries also needs
Billing Read, and without it the script falls back to a calendar month and says
so. Exits non-zero when a meter is projected at or above `--fail-at` percent, so
it can run as a scheduled guard.

Durable Object requests are the subtle one. The analytics API counts every
inbound WebSocket message as an invocation, while billing folds them 20:1, so
the dashboard figure overstates the bill. This reports the BILLED figure:

    billable = http + alarm + hibernation / 20
"""
import argparse
import datetime
import json
import os
import sys
import time
import urllib.error
import urllib.request

ACCOUNT = "1489e726fc5baa8ecc7a6e1a8c8ed3f8"
API = "https://api.cloudflare.com/client/v4"
WS_MESSAGES_PER_REQUEST = 20

# Workers Paid inclusions. R2 storage is the standalone R2 free tier.
ALLOWANCES = [
    # (label, key, included, unit)
    ("DO requests (billed)", "do_requests", 1_000_000, ""),
    ("DO rows written", "do_rows_written", 50_000_000, ""),
    ("DO rows read", "do_rows_read", 25_000_000_000, ""),
    ("DO duration", "do_gbs", 400_000, " GB-s"),
    ("Workers requests", "worker_requests", 10_000_000, ""),
    ("R2 storage", "r2_storage_gb", 10, " GB"),
    ("R2 class A ops", "r2_class_a", 1_000_000, ""),
    ("R2 class B ops", "r2_class_b", 10_000_000, ""),
]
R2_CLASS_A = {"PutObject", "ListObjects", "PutBucket", "CopyObject", "DeleteObject", "CreateBucket"}


def call(token, url, body=None):
    request = urllib.request.Request(
        url,
        data=json.dumps(body).encode() if body else None,
        headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"},
    )
    for attempt in range(4):
        try:
            return json.load(urllib.request.urlopen(request, timeout=60))
        except urllib.error.HTTPError as err:
            if err.code == 429:
                time.sleep(10 * (attempt + 1))
                continue
            return {"errors": [{"code": err.code, "message": err.read().decode()[:200]}]}
        except (urllib.error.URLError, TimeoutError, OSError) as err:
            return {"errors": [{"message": str(err)[:200]}]}
    return {"errors": [{"message": "rate limited"}]}


def graphql(token, query, variables):
    body = call(token, API + "/graphql", {"query": query, "variables": variables})
    if body.get("errors"):
        return None
    accounts = (body.get("data") or {}).get("viewer", {}).get("accounts") or []
    return accounts[0] if accounts else None


def cycle(token):
    """The Workers Paid period, or a calendar month when Billing Read is absent."""
    body = call(token, f"{API}/accounts/{ACCOUNT}/subscriptions")
    for sub in body.get("result") or []:
        if str(sub.get("rate_plan", {}).get("id", "")).startswith("workers"):
            start, end = sub.get("current_period_start"), sub.get("current_period_end")
            if start and end:
                return datetime.date.fromisoformat(start[:10]), datetime.date.fromisoformat(end[:10]), True
    today = datetime.date.today()
    start = today.replace(day=1)
    end = (start + datetime.timedelta(days=32)).replace(day=1)
    return start, end, False


def collect(token, start, today):
    variables = {"a": ACCOUNT, "s": str(start), "e": str(today)}
    window = "filter:{date_geq:$s,date_leq:$e}"

    def query(key, fields, dims):
        return (
            "query($a:String!,$s:Date!,$e:Date!){viewer{accounts(filter:{accountTag:$a}){"
            f"{key}(limit:2000,{window}){{{fields}dimensions{{{dims}}}}}}}}}}}"
        )

    usage = dict.fromkeys((k for _, k, _, _ in ALLOWANCES), 0.0)
    detail = {}

    groups = graphql(token, query("durableObjectsInvocationsAdaptiveGroups", "sum{requests}", "namespaceId type"), variables)
    if groups is None:
        return None, None
    by_type = {"http": 0, "alarm": 0, "hibernation": 0}
    for group in groups["durableObjectsInvocationsAdaptiveGroups"]:
        by_type[group["dimensions"]["type"]] = by_type.get(group["dimensions"]["type"], 0) + group["sum"]["requests"]
    usage["do_requests"] = by_type["http"] + by_type["alarm"] + by_type["hibernation"] / WS_MESSAGES_PER_REQUEST
    detail["do_invocations_raw"] = by_type
    time.sleep(3)

    groups = graphql(token, query("durableObjectsPeriodicGroups", "sum{activeTime rowsWritten rowsRead}", "namespaceId"), variables)
    if groups:
        rows = groups["durableObjectsPeriodicGroups"]
        usage["do_rows_written"] = sum(g["sum"]["rowsWritten"] for g in rows)
        usage["do_rows_read"] = sum(g["sum"]["rowsRead"] for g in rows)
        usage["do_gbs"] = sum(g["sum"]["activeTime"] for g in rows) / 1e6 * 0.125
    time.sleep(3)

    groups = graphql(token, query("workersInvocationsAdaptive", "sum{requests}", "scriptName"), variables)
    if groups:
        usage["worker_requests"] = sum(g["sum"]["requests"] for g in groups["workersInvocationsAdaptive"])
    time.sleep(3)

    groups = graphql(token, query("r2OperationsAdaptiveGroups", "sum{requests}", "bucketName actionType"), variables)
    if groups:
        rows = groups["r2OperationsAdaptiveGroups"]
        usage["r2_class_a"] = sum(g["sum"]["requests"] for g in rows if g["dimensions"]["actionType"] in R2_CLASS_A)
        usage["r2_class_b"] = sum(g["sum"]["requests"] for g in rows if g["dimensions"]["actionType"] not in R2_CLASS_A)
    time.sleep(3)

    # Storage is a level, not an accumulation. Taking `max` across the whole
    # cycle would report a peak that has since been deleted, so read the most
    # recent day present and use that as the current level.
    recent = {"a": ACCOUNT, "s": str(max(start, today - datetime.timedelta(days=4))), "e": str(today)}
    groups = graphql(token, query("r2StorageAdaptiveGroups", "max{payloadSize metadataSize}", "bucketName date"), recent)
    if groups:
        rows = groups["r2StorageAdaptiveGroups"]
        # The newest date is often still aggregating and reports nothing, so
        # fall back to the newest day that actually carries bytes. Reporting a
        # partial day as "0 GB" would hide a full bucket.
        totals = {}
        for group in rows:
            date = group["dimensions"]["date"]
            totals[date] = totals.get(date, 0) + group["max"]["payloadSize"] + group["max"]["metadataSize"]
        latest = max((d for d, size in totals.items() if size > 0), default=None)
        current = [g for g in rows if g["dimensions"]["date"] == latest]
        usage["r2_storage_gb"] = totals.get(latest, 0) / 1e9
        detail["r2_as_of"] = latest
        detail["r2_by_bucket"] = {
            g["dimensions"]["bucketName"]: round((g["max"]["payloadSize"] + g["max"]["metadataSize"]) / 1e9, 4)
            for g in current
            if g["max"]["payloadSize"]
        }
    return usage, detail


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--fail-at", type=float, default=80.0,
                        help="exit non-zero when a meter is projected at or above this percent")
    args = parser.parse_args()
    token = os.environ.get("CLOUDFLARE_API_TOKEN", "").strip()
    if not token:
        print("Set CLOUDFLARE_API_TOKEN (Account Analytics Read; Billing Read for exact cycle dates)",
              file=sys.stderr)
        return 2

    start, end, exact = cycle(token)
    today = datetime.date.today()
    elapsed = max((today - start).days, 1)
    total = max((end - start).days, 1)
    usage, detail = collect(token, start, today)
    if usage is None:
        print("Analytics query failed; is the token missing Account Analytics Read?", file=sys.stderr)
        return 2

    report, worst = [], 0.0
    for label, key, included, unit in ALLOWANCES:
        used = usage[key]
        # Storage is a level, not an accumulation: it does not scale with time.
        projected = used if "storage" in key else used * total / elapsed
        percent = 100 * projected / included
        worst = max(worst, percent)
        report.append({"meter": label, "key": key, "used": used, "included": included,
                       "projected": projected, "percent_projected": percent, "unit": unit})

    if args.json:
        print(json.dumps({"cycle_start": str(start), "cycle_end": str(end),
                          "cycle_from_subscription": exact, "day": elapsed, "days": total,
                          "meters": report, "detail": detail,
                          "worst_percent": worst}, indent=2))
    else:
        source = "subscription" if exact else "calendar month (no Billing Read)"
        print(f"\nCycle {start} -> {end} ({source}), day {elapsed} of {total}\n")
        print(f"{'meter':24}{'used':>16}{'included':>16}{'projected':>16}{'% of cap':>10}  risk")
        for row in report:
            risk = "OVER" if row["percent_projected"] >= 100 else (
                "HIGH" if row["percent_projected"] >= args.fail_at else (
                    "watch" if row["percent_projected"] >= 50 else "ok"))
            print(f"{row['meter']:24}{row['used']:>16,.0f}{row['included']:>16,}"
                  f"{row['projected']:>16,.0f}{row['percent_projected']:>9.1f}%  {risk}")
        raw = detail.get("do_invocations_raw", {})
        if raw:
            print(f"\nDO invocations as the dashboard shows them: "
                  f"http={raw.get('http', 0):,} alarm={raw.get('alarm', 0):,} "
                  f"ws_messages={raw.get('hibernation', 0):,}")
            print(f"  billed = http + alarm + ws/{WS_MESSAGES_PER_REQUEST} = {usage['do_requests']:,.0f}")
        if detail.get("r2_by_bucket"):
            print(f"\nR2 storage by bucket (GB), as of {detail.get('r2_as_of', 'latest')}:")
            for name, size in sorted(detail["r2_by_bucket"].items(), key=lambda kv: -kv[1]):
                print(f"  {name:32}{size:>9.3f}")
        print()
    return 1 if worst >= args.fail_at else 0


if __name__ == "__main__":
    sys.exit(main())
