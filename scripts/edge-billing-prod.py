#!/usr/bin/env python3
"""Classify a production `wrangler tail` sample into billable Durable Object requests.

The production counterpart to `scripts/edge-billing-local.mjs`. Local wrangler
exposes spans and can be driven deterministically; production cannot, so the
only attribution available is the tail event's `entrypoint` field, which names
the Durable Object class. The `durableObjectId` alone tells you nothing.

    python3 scripts/edge-billing-prod.py --seconds 305        # sample and report
    python3 scripts/edge-billing-prod.py --from FILE          # re-read a capture
    python3 scripts/edge-billing-prod.py --seconds 305 --json

Billing model, from Cloudflare's published conversion:

    billable = http + alarm + inbound_websocket_messages / 20

Two counting rules that are easy to get wrong, and would each inflate the
result by roughly 2x if missed:

  * One client call produces TWO tail events -- a Worker-level one whose
    `entrypoint` is null, and a Durable Object one naming the class. Only the
    latter is a Durable Object request. The Worker-level count is reported
    separately because it bills on its own, much larger, meter.
  * Outbound websocket messages are free, and `ping` never wakes the object at
    all (all four rooms call `setWebSocketAutoResponse`), so keepalives are
    absent from tail by construction. Anything counted here really was an
    inbound application message.

Nothing from a request body or a header is read, and path ids are redacted, so
a sample carries no chat content, token or device identity.
"""
import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
import time
from collections import Counter
from urllib.parse import urlparse

WS_MESSAGES_PER_REQUEST = 20
# Room ids, chat ids and device ids are unbounded cardinality and identify a
# user's data; the shape of the route is the only part that carries meaning.
ID_PATH = re.compile(r"/(chat2|device|registry|workspace|session)/[^/]+")


def decode_stream(raw):
    """`wrangler tail --format json` emits concatenated pretty-printed objects,
    not JSONL, so a line-oriented reader silently yields nothing."""
    decoder = json.JSONDecoder()
    index, length, events = 0, len(raw), []
    while index < length:
        while index < length and raw[index] in " \t\r\n":
            index += 1
        if index >= length:
            break
        try:
            obj, index = decoder.raw_decode(raw, index)
        except ValueError:
            # A truncated final object, or a banner line; skip one char and
            # resynchronise rather than abandoning the whole capture.
            index += 1
            continue
        events.append(obj)
    return events


def classify(event):
    """(kind, label) for one tail event. `kind` decides the billing arithmetic."""
    entrypoint = event.get("entrypoint")
    body = event.get("event")
    if not isinstance(body, dict):
        return ("other", f"{entrypoint or 'Worker'}: non-dict event")
    if "request" in body:
        request = body["request"]
        path = ID_PATH.sub(r"/\1/<id>", urlparse(request.get("url", "")).path)
        label = f"{request.get('method', '?')} {path}"
        return ("worker_http", label) if entrypoint is None else ("do_http", f"{entrypoint} {label}")
    if "getWebSocketEvent" in body:
        kind = (body["getWebSocketEvent"] or {}).get("webSocketEventType", "?")
        return ("ws", f"{entrypoint or 'Worker'} ws:{kind}")
    if "scheduledTime" in body or "cron" in body:
        return ("alarm", f"{entrypoint or 'Worker'} alarm")
    # Never silently drop an unrecognised shape: an uncounted event is an
    # undercount, which is the failure mode that flatters the result.
    return ("unclassified", f"{entrypoint or 'Worker'} {sorted(body.keys())}")


def report(events, seconds, as_json):
    kinds = Counter()
    labels = Counter()
    outcomes = Counter()
    for event in events:
        kind, label = classify(event)
        kinds[kind] += 1
        labels[(kind, label)] += 1
        outcomes[event.get("outcome", "?")] += 1

    do_http, ws, alarm = kinds["do_http"], kinds["ws"], kinds["alarm"]
    billable = do_http + alarm + ws / WS_MESSAGES_PER_REQUEST
    per_hour = 3600.0 / seconds if seconds else 0.0

    rows = []
    for (kind, label), count in labels.most_common():
        if kind in ("do_http", "alarm"):
            billed = count
        elif kind == "ws":
            billed = count / WS_MESSAGES_PER_REQUEST
        else:
            billed = 0.0
        rows.append({"kind": kind, "source": label, "count": count,
                     "rate_per_h": count * per_hour, "billable_per_h": billed * per_hour})

    total_billable_h = billable * per_hour
    if as_json:
        print(json.dumps({"seconds": seconds, "events": len(events),
                          "do_http": do_http, "ws_messages": ws, "alarms": alarm,
                          "worker_requests": kinds["worker_http"],
                          "unclassified": kinds["unclassified"],
                          "billable_per_h": total_billable_h,
                          "outcomes": dict(outcomes), "sources": rows}, indent=2))
        return total_billable_h

    print(f"\nproduction sample: {seconds}s, {len(events)} tail events\n")
    print("BILLABLE DURABLE OBJECT REQUESTS")
    print(f"  DO HTTP (1:1)              {do_http:>7}")
    print(f"  WS messages (20:1)         {ws:>7} -> {ws / WS_MESSAGES_PER_REQUEST:.2f}")
    print(f"  alarms (1:1)               {alarm:>7}")
    print("  " + "-" * 40)
    print(f"  TOTAL BILLABLE             {billable:>7.2f}   = {total_billable_h:,.0f}/hour")
    print(f"\n  (Worker-level requests, separate meter: {kinds['worker_http']}"
          f" = {kinds['worker_http'] * per_hour:,.0f}/hour)")
    if kinds["unclassified"]:
        print(f"  WARNING: {kinds['unclassified']} unclassified events -- billing is an UNDERCOUNT")

    print("\nBY SOURCE (billable/hour, descending)")
    for row in sorted(rows, key=lambda r: -r["billable_per_h"]):
        if row["kind"] == "worker_http":
            continue
        share = 100 * row["billable_per_h"] / total_billable_h if total_billable_h else 0
        print(f"  {row['billable_per_h']:>8,.1f}/h  {share:>5.1f}%  "
              f"({row['count']} in sample)  {row['source']}")
    bad = {k: v for k, v in outcomes.items() if k not in ("ok", "?")}
    if bad:
        print(f"\n  non-ok outcomes: {bad}")
    print()
    return total_billable_h


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--seconds", type=int, default=305,
                        help="sample length; 305 matches the pre-optimization baseline")
    parser.add_argument("--worker", default="cypher-edge")
    parser.add_argument("--from", dest="source", help="classify an existing capture instead")
    parser.add_argument("--keep", help="write the raw capture here for re-reading")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args()

    if args.source:
        raw = open(args.source, errors="replace").read()
        seconds = args.seconds
    else:
        path = args.keep or tempfile.mkstemp(prefix="cypher-tail-", suffix=".json")[1]
        if not args.json:
            print(f"sampling {args.worker} for {args.seconds}s -> {path}", file=sys.stderr)
        # Raw tail carries caller IPs and request paths. Cloudflare redacts the
        # credential, but the capture is still private: owner-only, whether
        # it came from mkstemp or from --keep.
        with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600), "w") as sink:
            os.chmod(path, 0o600)
            proc = subprocess.Popen(
                ["npx", "wrangler", "tail", args.worker, "--format", "json"],
                cwd="edge", stdout=sink, stderr=subprocess.DEVNULL)
            started = time.time()
            try:
                time.sleep(args.seconds)
            finally:
                proc.terminate()
                try:
                    proc.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    proc.kill()
            seconds = max(time.time() - started, 1)
        raw = open(path, errors="replace").read()

    events = decode_stream(raw)
    if not events:
        print("No tail events decoded. Is the worker receiving traffic?", file=sys.stderr)
        return 1
    report(events, seconds, args.json)
    return 0


if __name__ == "__main__":
    sys.exit(main())
