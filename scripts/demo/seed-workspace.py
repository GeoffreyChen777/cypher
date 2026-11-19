#!/usr/bin/env python3
"""Seed the workspace demo into a running mock engine: enough plans, routines,
sessions and pending decisions to judge the whole workspace layout.

  "Atlas"   the full picture: a chat that reads like a week of work (a saved
            routine, plans approved, rejected and still waiting, result
            notices with the agent's short answer), three plans running with
            their steps and sessions, one approved and waiting for a slot,
            finished, failed, stopped and rejected ones, six routines (one
            running, one waiting on a permission prompt, one failed, a
            reminder, a switched-off one), sessions it runs in three
            projects, and a Needs you page with permission prompts,
            questions and plans to approve
  "Scout"   a second workspace with one plan to approve, for the switcher
  Sessions  a few sessions of the owner's own in the same projects

    scripts/demo/seed-workspace.py <engine-data-dir> <projects-dir>

The engine must run the mock harness and have been started with
CYPHER_MOCK_RULES=<engine-data-dir>/mock-rules.json: the replies come from
rules this script writes there (crates/harness/src/mock/script.rs), so no
marker shows in a transcript. Needs target/debug/examples/rpc_probe
(cargo build -p cypher-rpc --example rpc_probe). Re-runs do nothing (marker:
<engine-data-dir>/.workspace-demo-seeded). Takes about two minutes: plans and
routines move at the pace of the engine's own loops.
"""

import json
import os
import subprocess
import sys
import time
import uuid

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
PROBE = os.path.join(ROOT, "target", "debug", "examples", "rpc_probe")

if len(sys.argv) != 3:
    sys.exit("Usage: seed-workspace.py <engine-data-dir> <projects-dir>")
ENGINE_DIR = os.path.abspath(os.path.expanduser(sys.argv[1]))
PROJECTS = os.path.abspath(os.path.expanduser(sys.argv[2]))
RULES_PATH = os.path.join(ENGINE_DIR, "mock-rules.json")
MARKER = os.path.join(ENGINE_DIR, ".workspace-demo-seeded")
NOTICE = "A notice from your workspace"

# ── the engine ────────────────────────────────────────────────────────────────


def probe(args):
    return subprocess.run([PROBE, ENGINE_DIR, *args], capture_output=True, text=True)


def call(method, params):
    out = probe([method, json.dumps(params)])
    if out.returncode != 0:
        sys.exit(f"{method} failed: {out.stderr.strip()}\n{json.dumps(params)[:400]}")
    text = out.stdout.strip()
    try:
        return json.loads(text)
    except ValueError:
        return text


def frame(method, params=None):
    out = probe([method, json.dumps(params or {}), "--stream", "1"])
    lines = out.stdout.strip().splitlines()
    if out.returncode != 0 or not lines:
        sys.exit(f"{method} gave no frame: {out.stderr.strip()}")
    return json.loads(lines[0])


def mutate(op, **fields):
    return call("Mutate", {"op": op, **fields})


def tool(chat_id, name, args, call_id=None):
    params = {"chatId": chat_id, "tool": name, "args": args}
    if call_id:
        params["toolCallId"] = call_id
    reply = call("BotTool", params)
    return reply["text"] if isinstance(reply, dict) else str(reply)


def wait(what, check, seconds=60):
    deadline = time.time() + seconds
    while time.time() < deadline:
        found = check()
        if found:
            return found
        time.sleep(0.4)
    sys.exit(f"timed out waiting for {what}")


def records():
    return frame("WatchBotRecords")


def work_row(work_id):
    return next((w for w in records()["work"] if w["id"] == work_id), None)


def routine_row(routine_id):
    return next((r for r in records()["routines"] if r["id"] == routine_id), None)


def status_of(chat_id):
    row = next((s for s in frame("WatchSessions") if s["chatId"] == chat_id), None)
    return row["status"] if row else None


def preview_of(chat_id):
    row = next((c for c in frame("WatchChats") if c["id"] == chat_id), None)
    return (row or {}).get("lastMessagePreview") or ""


def wait_said(chat_id, text):
    """Until the chat's newest message is `text` and its turn has ended."""
    lead = " ".join(text.split())[:40]
    wait(
        f"“{lead}”",
        lambda: " ".join(preview_of(chat_id).split()).startswith(lead)
        and status_of(chat_id) == "idle",
    )


def new_id():
    return str(uuid.uuid4())


MOCK = {
    "harness": "mock",
    "model": None,
    "reasoning": None,
    "modelOptions": {},
    "sandbox": "workspace-write",
}


def run_in(chat_id, cwd, prompt):
    call(
        "QueueCommand",
        {
            "chatId": chat_id,
            "command": {
                "kind": "run",
                "request": {
                    "prompt": prompt,
                    "harness": "mock",
                    "model": None,
                    "reasoning": None,
                    "cwd": cwd,
                    "sandbox": "workspace-write",
                    "resume": None,
                },
                "messageId": new_id(),
            },
        },
    )


def interrupt(chat_id):
    call("QueueCommand", {"chatId": chat_id, "command": {"kind": "interrupt"}})


# ── replies: the mock's rules, most specific kind first ───────────────────────

KINDS = ("notice", "any notice", "brief", "routine", "session", "owner")
RULES = {kind: [] for kind in KINDS}


def rule(kind, match, script):
    RULES[kind].append({"match": match, "script": script})
    ordered = [r for k in KINDS for r in RULES[k]]
    scratch = RULES_PATH + ".tmp"
    with open(scratch, "w") as out:
        json.dump(ordered, out, indent=1)
    os.replace(scratch, RULES_PATH)


def text(words):
    return {"text": words}


def ran(command, output=""):
    return {"tool": {"id": new_id()[:8], "call": {"kind": "exec", "command": command}, "output": output}}


def used(name, output=""):
    """One of the workspace's own tools, as its chat's chips name them."""
    return {"tool": {"id": new_id()[:8], "call": {"kind": "unknown", "name": name}, "output": output}}


def hold(command):
    return {"hold": command}


def ask(header, question, options, permission=None):
    asked = {"id": new_id()[:8], "header": header, "question": question, "options": options}
    if permission:
        asked["permission"] = {"kind": permission}
    return {"ask": [asked]}


RUN = ["Run once", "Allow for this chat", "Always allow", "Deny", "Deny and stop"]
ALLOW = ["Allow once", "Allow for this chat", "Always allow", "Deny", "Deny and stop"]

# ── a workspace ───────────────────────────────────────────────────────────────


class Workspace:
    def __init__(self, name, scope):
        bot = call("CreateBot", {"targetDeviceId": DEVICE, "name": name, "scope": scope})
        self.id, self.chat, self.name = bot["id"], bot["chatId"], name
        self.started = {}  # a plan's background session → the sessions it started
        mutate("setChatConfig", chatId=self.chat, config=MOCK)
        self.cwd = next(c for c in frame("WatchChats") if c["id"] == self.chat)["cwd"]

    def say(self, words, reply):
        """The owner writes `words`; the agent answers with `reply` (steps
        ending in a text)."""
        rule("owner", [words], reply)
        run_in(self.chat, self.cwd, words)
        wait_said(self.chat, reply[-1]["text"])

    def routine(self, name, prompt, schedule, words=None, answer=None, **more):
        """Save a routine; with `words`, as the agent's answer to them in the
        chat (its card)."""
        if "project" in more:
            more["project"] = SPACES[more["project"]]
        saved = tool(self.chat, "save_routine", {"name": name, "prompt": prompt, "schedule": schedule, **more})
        if not saved.startswith("Saved routine"):
            sys.exit(f"save_routine refused: {saved}")
        if words:
            self.say(words, [{"tool": {"id": new_id()[:8], "call": {"kind": "unknown", "name": "save_routine"}, "output": saved}}, text(answer)])
        return next(r["id"] for r in records()["routines"] if r["botId"] == self.id and r["name"] == name)

    def run_routine(self, routine_id, name, script, told=None):
        """Run a routine now with `script` as its run. With `told`, wait for
        its result notice and the agent's answer to it."""
        # A workspace routine's run opens with its header, a project
        # routine's session with a line naming it.
        rule("routine", [f'Routine "{name}" ·'], script)
        rule("routine", [f'run of the routine "{name}"'], script)
        if told:
            rule("notice", [f'[Routine "{name}" ', NOTICE], [text(told)])
        mutate("runBotRoutine", routineId=routine_id)
        wait(f"{name} to start", lambda: (routine_row(routine_id).get("lastRun") or {}).get("chatId"))
        if told:
            wait_said(self.chat, told)

    def plan(self, words, title, summary, steps, looked=("search_sessions", "read_session"), answer=None):
        """The owner asks; the agent looks around and proposes a plan. The
        plan's work id."""
        call_id = "call-" + new_id()[:8]
        answer = answer or "The plan is ready for your approval."
        reply = [used(name) for name in looked]
        reply.append({"tool": {"id": call_id, "call": {"kind": "unknown", "name": "propose_plan", "input": {"title": title}}, "output": "Proposed."}})
        reply.append(text(answer))
        self.say(words, reply)
        steps = [{**step, "project": SPACES[step["project"]]} if "project" in step else step for step in steps]
        tool(self.chat, "propose_plan", {"title": title, "summary": summary, "steps": steps}, call_id)
        return wait(f"the plan {title}", lambda: next((w["id"] for w in records()["work"] if w["botId"] == self.id and w["title"] == title), None))

    def approve(self, work_id, title):
        """Approve a plan and wait for its background session, which then
        waits on its sessions until the plan ends."""
        mutate("resolveBotPlan", workId=work_id, approve=True)
        thread = wait(f"{title} to start", lambda: (lambda w: w.get("threadChatId") if w["state"] == "running" else None)(work_row(work_id)))
        wait(f"{title}'s session", lambda: status_of(thread) == "working")
        return thread

    def step(self, thread, number, state, note=None):
        args = {"step": number, "state": state}
        if note:
            args["note"] = note
        tool(thread, "update_step", args)

    def session(self, thread, step, project, title, prompt, script, until="idle"):
        """Start a session for a step; its run is `script`. Its chat id."""
        rule("session", [prompt], script)
        said = tool(thread, "start_session", {"step": step, "project": SPACES[project], "title": title, "prompt": prompt})
        if "cypher://session/" not in said:
            sys.exit(f"start_session refused: {said}")
        chat_id = said.split("cypher://session/")[1].split(")")[0]
        wait(f"session {title}", lambda: status_of(chat_id) == until)
        self.started.setdefault(thread, []).append(chat_id)
        return chat_id

    def finish(self, thread, title, outcome, result, told):
        """End a plan: its result, then the notice and the agent's answer."""
        word = "finished" if outcome == "done" else "failed"
        rule("notice", [f'[Plan "{title}" {word}]', NOTICE], [text(told)])
        tool(thread, "finish_work", {"outcome": outcome, "result": result})
        interrupt(thread)
        # Old news: the owner has looked at these.
        for chat_id in self.started.get(thread, []):
            mutate("markChatSeen", chatId=chat_id)
        wait_said(self.chat, told)


def link(title, chat_id):
    return f"[{title}](cypher://session/{chat_id})"


# ── the data ──────────────────────────────────────────────────────────────────

if os.path.exists(MARKER):
    print(f"▸ workspace demo already seeded in {ENGINE_DIR}")
    sys.exit(0)
if not os.path.exists(PROBE):
    sys.exit("build rpc_probe first: cargo build -p cypher-rpc --example rpc_probe")

DEVICE = call("EngineInfo", {})["deviceId"]
# What the script does not stage itself (a plan approved later by hand, a
# job that ends later) still reads like a workspace.
rule("brief", ["Carry out this approved plan"], [text("Starting with step 1."), hold("wait_for_session")])
rule("any notice", [NOTICE], [text("It has ended; the card above has what it left.")])

print("▸ projects and the owner's own sessions")
SPACES = {}
for name in ("billing", "web-api", "mobile-app"):
    path = os.path.join(PROJECTS, name)
    os.makedirs(path, exist_ok=True)
    SPACES[name] = new_id()
    mutate("createSpace", spaceId=SPACES[name], deviceId=DEVICE, path=path, name=name, gitDetected=False)

# (project, title, prompt, the run, how it stands, read by the owner)
OWN = [
    ("billing", "Flaky invoice export test", "The invoice export test fails about one run in ten. Find out why.",
     [ran("cargo test -p billing export -- --test-threads=1", "test export::totals_match ... ok\ntest export::large_account ... FAILED"),
      text("It is an ordering problem: `large_account` reads rows without an `ORDER BY`, so the totals line moves when Postgres returns them in another order. I added the ordering and the test passed 50 runs in a row.")],
     "idle", True),
    ("billing", "Currency column for the ledger", "Add a currency column to the ledger table, defaulting to USD.",
     [ran("sqlx migrate add ledger_currency", "Creating migrations/20261118_ledger_currency.sql"),
      text("Added the migration and the model field. Existing rows default to `USD`; the API returns the new field.")],
     "idle", True),
    ("web-api", "Rate-limit the login endpoint", "Rate-limit POST /login to five attempts a minute per address.",
     [ran("rg -n \"fn login\" src", "src/routes/auth.rs:41:pub async fn login("),
      ask("Storage", "Where should the attempt counters live?", ["In Redis, shared by every instance", "In memory, per instance"])],
     "awaitingInput", False),
    ("web-api", "Upgrade to Node 22", "Move the build and CI to Node 22.",
     [ran("npm test", "412 passing"), text("The build, the tests and CI run on Node 22 now. One dependency needed a bump: `sharp` to 0.34.")],
     "idle", False),
    ("mobile-app", "Crash on cold start (Android 14)", "The app crashes on cold start on Android 14. Stack trace attached in the issue.",
     [text("Looking at the trace."), hold("./gradlew connectedAndroidTest")],
     "working", False),
]
for project, title, prompt, script, stands, read in OWN:
    chat_id = new_id()
    rule("session", [prompt], script)
    mutate("createChat", chatId=chat_id, spaceId=SPACES[project], config=MOCK)
    mutate("renameChat", chatId=chat_id, title=title)
    run_in(chat_id, os.path.join(PROJECTS, project), prompt)
    wait(title, lambda: status_of(chat_id) == stands)
    if read:
        mutate("markChatSeen", chatId=chat_id)

print("▸ Atlas")
atlas = Workspace("Atlas", {"projects": {"mode": "all"}})

brief = atlas.routine(
    "Morning brief", "What moved overnight, what is stuck, and what needs me.",
    {"kind": "daily", "time": "09:00", "days": [1, 2, 3, 4, 5]},
    words="Every weekday at 9, tell me what moved overnight and what's stuck.",
    answer="Saved. You'll get the brief at 9:00 on weekdays.")
atlas.run_routine(
    brief, "Morning brief",
    [used("session_activity"), used("list_sessions"),
     text("**Overnight**\n\n- *Currency column for the ledger* finished in billing: the migration and the model field are in.\n- *Upgrade to Node 22* finished in web-api; `sharp` went to 0.34.\n\n**Stuck**\n\n- *Rate-limit the login endpoint* has been waiting on your answer since yesterday: where the attempt counters should live.\n- *Crash on cold start (Android 14)* is still running its device tests.\n\n**Needs you**\n\n- The login question above. Nothing else.")],
    told="Two sessions finished overnight and one is waiting on you: the login rate limit needs to know where its counters live.")

print("▸ plans that ended")
title = "Fix invoice rounding"
work = atlas.plan(
    "The CSV export and the web app disagree on invoice totals. Find out why and fix it.", title,
    "Round invoice totals once, half-even, in billing; then check the web API against the fix.",
    [{"title": "Reproduce the mismatch", "project": "billing", "detail": "A failing test for a total of 0.125."},
     {"title": "Round totals half-even", "project": "billing", "detail": "In a worktree; one rounding at the total, not per line."},
     {"title": "Run the API contract tests", "project": "web-api", "detail": "Against billing's branch."},
     {"title": "Report"}],
    looked=("search_sessions", "read_session", "project_git"),
    answer="The export rounds each line and the web app rounds the total. I proposed a plan that rounds once, at the total.")
thread = atlas.approve(work, title)
atlas.step(thread, 1, "running")
s1 = atlas.session(thread, 1, "billing", "Reproduce the rounding mismatch",
                   "Write a failing test that shows the CSV export and the invoice page disagree for a total of 0.125.",
                   [ran("cargo test -p billing rounding", "test rounding::export_matches_page ... FAILED\n  left: 0.12\n right: 0.13"),
                    text("Reproduced: the export rounds each line down, the page rounds the sum half-up. The failing test is `rounding::export_matches_page`.")])
atlas.step(thread, 1, "done", "Fails for 0.125: 0.12 against 0.13")
atlas.step(thread, 2, "running")
s2 = atlas.session(thread, 2, "billing", "Round invoice totals half-even",
                   "Make the CSV export and the invoice page round the invoice total once, half-even. Keep the new test green.",
                   [ran("cargo test -p billing", "214 passed; 0 failed"),
                    text("Both paths call `Money::round_total` now. All 214 billing tests pass, the new one included.")])
atlas.step(thread, 2, "done", "One rounding in Money::round_total")
atlas.step(thread, 3, "running")
s3 = atlas.session(thread, 3, "web-api", "Run the API contract tests",
                   "Run the contract tests against billing's rounding branch and report any total that changed.",
                   [ran("npm run test:contract", "86 passing\n2 snapshots updated"),
                    text("All 86 contract tests pass. Two snapshots changed by one cent, both for the better.")])
atlas.step(thread, 3, "done", "86 passing, two snapshots moved a cent")
atlas.step(thread, 4, "done")
atlas.finish(
    thread, title, "done",
    f"Invoice totals now round once, half-even, in the export and on the page.\n\n- **Cause:** the export rounded every line, the page rounded the sum. Shown in {link('Reproduce the rounding mismatch', s1)}.\n- **Fix:** both call `Money::round_total`, in {link('Round invoice totals half-even', s2)}. 214 billing tests pass.\n- **Checked:** {link('Run the API contract tests', s3)}: 86 passing; two snapshots moved by one cent.\n\n**Needs you:** review the branch `atlas/round-totals` and merge it.",
    "The totals agree now: both paths round once, half-even. The branch `atlas/round-totals` is ready for your review.")

title = "Upgrade the payments SDK"
work = atlas.plan(
    "Upgrade the payments SDK to v5.", title,
    "Move billing to payments-sdk 5 and run the refund and capture suites.",
    [{"title": "Bump the SDK", "project": "billing"},
     {"title": "Adapt the refund flow", "project": "billing"},
     {"title": "Run the payment suites", "project": "billing"}],
    looked=("project_git",),
    answer="Billing is on 4.9.1. I proposed the upgrade in three steps.")
thread = atlas.approve(work, title)
atlas.step(thread, 1, "running")
s1 = atlas.session(thread, 1, "billing", "Bump payments-sdk to 5.2",
                   "Bump payments-sdk to 5.2 and fix what no longer compiles.",
                   [ran("cargo update -p payments-sdk --precise 5.2.0", "Updating payments-sdk v4.9.1 -> v5.2.0"),
                    ran("cargo check -p billing", "error[E0599]: no method named `capture_later` found for struct `Charge`"),
                    text("It compiles except for refunds: v5 removed `Charge::capture_later`, which the delayed-refund flow is built on. There is no drop-in replacement.")])
atlas.step(thread, 1, "done", "5.2.0; one API is gone")
atlas.step(thread, 2, "failed", "v5 has no capture_later")
atlas.finish(
    thread, title, "failed",
    f"Stopped at step 2: payments-sdk 5 removed `Charge::capture_later`, and delayed refunds are built on it ({link('Bump payments-sdk to 5.2', s1)}).\n\nNothing was merged; billing still runs 4.9.1.\n\n**Needs you:** decide whether refunds move to the new two-step `authorize` / `settle` API (about a day of work) or the upgrade waits.",
    "The upgrade failed: v5 drops the call our delayed refunds rely on. Nothing changed in billing; I need your decision on reworking refunds.")

title = "Remove stale feature flags"
work = atlas.plan(
    "Clean up the feature flags we shipped last quarter.", title,
    "Delete the flags that have been on for everyone since September, and the code behind their off branches.",
    [{"title": "List flags at 100%", "project": "web-api"},
     {"title": "Remove them and their dead branches", "project": "web-api"}],
    answer="Seven flags have been on for everyone since September. The plan removes them and the code they guard.")
thread = atlas.approve(work, title)
atlas.step(thread, 1, "done", "Seven flags at 100% since September")
atlas.step(thread, 2, "running")
s1 = atlas.session(thread, 2, "web-api", "Remove seven stale flags",
                   "Remove these seven feature flags and the code behind their off branches: new-checkout, fast-search, csv-v2, sso-okta, dark-emails, bulk-invite, usage-meter.",
                   [ran("rg -l \"flags\\.(newCheckout|fastSearch|csvV2)\" src | wc -l", "23"), ran("npm test", "409 passing"),
                    text("Removed all seven flags: 23 files touched, 640 lines deleted, 409 tests pass.")])
atlas.step(thread, 2, "done", "23 files, 640 lines gone")
atlas.finish(
    thread, title, "done",
    f"Seven flags that had been on for everyone are gone, with their off branches: 23 files, 640 lines deleted ({link('Remove seven stale flags', s1)}). 409 tests pass.\n\n**Needs you:** nothing; the PR is open as `atlas/stale-flags`.",
    "Seven stale flags are removed, 640 lines lighter, tests green. The PR is open; nothing needs you.")

title = "Audit npm dependencies"
work = atlas.plan(
    "Audit our npm dependencies for known vulnerabilities.", title,
    "Run the audit in web-api and the mobile app, and upgrade what has a fix.",
    [{"title": "Audit web-api", "project": "web-api"},
     {"title": "Audit the mobile app", "project": "mobile-app"},
     {"title": "Upgrade what has a fix"}],
    looked=("list_projects",),
    answer="Proposed for web-api and the mobile app; billing has no npm dependencies.")
thread = atlas.approve(work, title)
atlas.step(thread, 1, "running")
atlas.session(thread, 1, "web-api", "npm audit in web-api",
              "Run npm audit and list every advisory with a fixed version available.",
              [text("Running the audit."), hold("npm audit --json")], until="working")
mutate("stopBotWork", workId=work)
wait("the audit to stop", lambda: work_row(work)["state"] == "stopped")
wait("the stop's card", lambda: "Audit npm dependencies" in preview_of(atlas.chat), 30)

title = "Rewrite the importer in Rust"
work = atlas.plan(
    "Could we rewrite the importer in Rust?", title,
    "Port the CSV importer from Node to a Rust service behind the same API.",
    [{"title": "Port the parser", "project": "web-api"},
     {"title": "Port the validation rules", "project": "web-api"},
     {"title": "Shadow-run both against a week of uploads", "project": "web-api"},
     {"title": "Switch over"}],
    looked=("search_sessions", "project_git"),
    answer="We could. It is about two weeks of work for a 3x faster import; the plan is there if you want it.")
mutate("resolveBotPlan", workId=work, approve=False)
wait("the rejection", lambda: work_row(work)["state"] == "cancelled")

print("▸ routines")
audit = atlas.routine("Dependency audit", "Check every project for dependencies with known advisories.",
                      {"kind": "daily", "time": "08:00", "days": [1]})
atlas.run_routine(audit, "Dependency audit", [text("Checking the lockfiles."), hold("npm audit --json")])
run_chat = (routine_row(audit)["lastRun"] or {})["chatId"]
wait("the audit run", lambda: status_of(run_chat) == "working")
rule("notice", ['[Routine "Dependency audit" ', NOTICE],
     [text("The dependency audit did not finish: its run was stopped. Run it again from its page when you want the list.")])
interrupt(run_chat)
wait_said(atlas.chat, "The dependency audit did not finish")

nightly = atlas.routine(
    "Nightly tests", "Run the full test suite and fix anything flaky.",
    {"kind": "daily", "time": "02:00"}, project="billing",
    words="Run the billing tests every night and fix what's flaky.",
    answer="Saved. It runs in billing at 2:00 every night, as a session of its own.")
atlas.run_routine(nightly, "Nightly tests",
                  [ran("cargo test -p billing", "212 passed; 2 failed"),
                   ask("Network", "Allow the agent to reach crates.io?\n\nContext:\nDomain: crates.io\nReason: Fetch `proptest` 1.5 to pin the flaky generators.\nNote: Commands reach the network only through the filter; other domains still ask.",
                       ["Allow for this command", "Allow for this chat", "Always allow", "Deny", "Deny and stop"], "network"),
                   text("Pinned.")])
watch = atlas.routine("CI watch", "Look at main's CI and say what broke and who should look.",
                      {"kind": "interval", "minutes": 30})
atlas.run_routine(watch, "CI watch", [used("project_git"), text("Main is red; reading the failing job."), hold("gh run view --log-failed")])
tomorrow = time.strftime("%Y-%m-%dT09:00", time.localtime(time.time() + 86400))
atlas.routine("Review the 0.6 release notes", "Remind me to review the 0.6 release notes, and say what changed since the draft.",
              {"kind": "once", "at": tomorrow})
atlas.routine("Stale PR sweep", "List pull requests with no activity for a week and ping their authors.",
              {"kind": "daily", "time": "17:30"}, enabled=False)

print("▸ plans under way")
title = "Speed up the CSV export"
work = atlas.plan(
    "The CSV export takes 40 seconds on large accounts. Speed it up.", title,
    "Find where the export spends its time, batch the queries, and prove it with a benchmark.",
    [{"title": "Profile the export", "project": "billing"},
     {"title": "Batch the invoice queries", "project": "billing", "detail": "One query per page of invoices instead of one per invoice."},
     {"title": "Add a benchmark", "project": "billing"},
     {"title": "Report"}],
    looked=("search_sessions", "read_session"),
    answer="It runs one query per invoice. I proposed batching them, with a benchmark to prove the gain.")
thread = atlas.approve(work, title)
atlas.step(thread, 1, "running")
atlas.session(thread, 1, "billing", "Profile the export query",
              "Profile the CSV export on the largest test account and say where the time goes.",
              [ran("cargo flamegraph --bench export", "wrote flamegraph.svg"),
               text("92% of the time is in `invoice_lines_for`: 18,000 single-row queries for the largest account.")])
atlas.step(thread, 1, "done", "92% in 18,000 single-row queries")
atlas.step(thread, 2, "running")
atlas.session(thread, 2, "billing", "Batch the invoice queries",
              "Replace the per-invoice line queries in the CSV export with one query per page of 500 invoices.",
              [ran("cargo test -p billing export", "31 passed"),
               ask("Approval", "Run this command outside the sandbox?\n\nContext:\nPurpose: Time the export against a copy of production data\nCommand: pg_restore --dbname=billing_bench ~/dumps/billing-2026-11-17.dump\nPattern: pg_restore …\nReason: The sandbox cannot read ~/dumps or reach the local Postgres socket.\nNote: It runs as you, with your files, keys, keychain and network.", RUN, "escalate"),
               text("Restored.")], until="awaitingInput")
atlas.session(thread, 2, "billing", "Add an export benchmark",
              "Add a criterion benchmark for the CSV export at 1k, 10k and 100k invoices.",
              [text("Writing the benchmark."), hold("cargo bench --bench export")], until="working")

title = "Migrate sessions to the new auth tokens"
work = atlas.plan(
    "Move sessions to the new auth tokens before the old ones expire on the 30th.", title,
    "Issue new-format tokens alongside the old ones, move every service over, then stop accepting the old format.",
    [{"title": "Inventory token usage", "project": "web-api"},
     {"title": "Add the token exchange endpoint", "project": "web-api"},
     {"title": "Move the mobile app over", "project": "mobile-app"}],
    looked=("list_projects", "search_sessions"),
    answer="Eleven days is enough if the gateway goes first. The plan is in three steps and needs your approval.")
thread = atlas.approve(work, title)
atlas.step(thread, 1, "running")
atlas.session(thread, 1, "web-api", "Inventory token usage",
              "List every place the web API reads or issues a session token, and which services call each.",
              [ran("rg -n \"SessionToken\" src | wc -l", "47"),
               ask("Order", "Which services should move to the new tokens first?\n\nContext:\nThe gateway and the mobile app issue 90% of the tokens. The admin console and the webhook worker are small but have no staging environment.",
                   ["Gateway and mobile first, the rest a week later", "Everything at once, behind a flag", "Admin console first, as a trial"])],
              until="awaitingInput")
atlas.session(thread, 1, "web-api", "Add the token exchange endpoint",
              "Add POST /auth/exchange that trades an old session token for a new-format one.",
              [ran("npm test -- auth", "58 passing"),
               ask("Approval", "Allow the agent to write outside the project folders?\n\nContext:\nPath: ~/Developer/infra/gateway/routes.yaml\nTool: edit\nReason: The gateway needs a route for /auth/exchange before the endpoint can be reached.\nChange:\n@@ routes.yaml @@\n   - path: /auth/refresh\n     service: web-api\n+  - path: /auth/exchange\n+    service: web-api\n+    rate_limit: 30/min",
                   ALLOW, "access"),
               text("Route added.")], until="awaitingInput")

title = "Localize the checkout flow"
work = atlas.plan(
    "Localize the checkout flow for German and French.", title,
    "Extract the checkout strings, translate them, and check the layouts with the longer text.",
    [{"title": "Extract the web strings", "project": "web-api"},
     {"title": "Translate to German and French", "project": "web-api"},
     {"title": "Extract the mobile strings", "project": "mobile-app"},
     {"title": "Check the layouts", "project": "mobile-app"},
     {"title": "Report"}],
    looked=("list_projects",),
    answer="Proposed: web first, then the mobile app, with a layout check for the longer German strings.")
thread = atlas.approve(work, title)
atlas.step(thread, 1, "done", "118 strings")
atlas.step(thread, 2, "running")
atlas.session(thread, 2, "web-api", "Translate checkout to German and French",
              "Translate the 118 extracted checkout strings to German and French. Keep the placeholders.",
              [ran("npm run i18n:check", "de: 118/118\nfr: 118/118"), text("Both locales are complete; placeholders verified.")])
atlas.step(thread, 2, "done", "de and fr complete")
atlas.step(thread, 3, "running")
atlas.session(thread, 3, "mobile-app", "Extract checkout strings",
              "Move every hard-coded string in the mobile checkout screens into the string catalog.",
              [text("Going through the checkout screens."), hold("xcodebuild -exportLocalizations")], until="working")

print("▸ plans waiting")
title = "Release 0.6"
work = atlas.plan(
    "Cut the 0.6 release once the export fix is in.", title,
    "Bump the versions, run the release checks, tag and publish.",
    [{"title": "Bump versions", "project": "web-api"},
     {"title": "Run the release checks", "project": "web-api"},
     {"title": "Tag and publish", "project": "web-api"}],
    looked=("project_git",),
    answer="Proposed. Three plans are running already, so it starts when one of them ends.")
mutate("resolveBotPlan", workId=work, approve=True)

atlas.plan(
    "Our public API has no reference docs. Can you write them?", "Document the public API",
    "Generate an OpenAPI description from the routes, fill in the gaps by hand, and publish a reference page.",
    [{"title": "Generate the OpenAPI description", "project": "web-api"},
     {"title": "Describe the 14 undocumented endpoints", "project": "web-api"},
     {"title": "Publish the reference page", "project": "web-api"}],
    looked=("project_git", "search_sessions"),
    answer="Yes. Most of it can be generated; 14 endpoints need describing by hand. The plan awaits your approval.")
atlas.plan(
    "Add dark mode to the mobile app.", "Add dark mode to the mobile app",
    "Move the colours to semantic tokens, add the dark palette, and check every screen in both.",
    [{"title": "Move colours to semantic tokens", "project": "mobile-app", "detail": "No visual change in light mode."},
     {"title": "Add the dark palette", "project": "mobile-app"},
     {"title": "Screenshot every screen in both modes", "project": "mobile-app"},
     {"title": "Report"}],
    looked=("list_projects", "search_sessions"),
    answer="Proposed in four steps; the first one changes nothing you can see. It waits for your approval.")

print("▸ Scout")
scout = Workspace("Scout", {"projects": {"mode": "selected", "spaceIds": [SPACES["mobile-app"]]}})
scout.routine("Crash report digest", "Summarise yesterday's crash reports: new crashes, the top five, and what regressed.",
              {"kind": "daily", "time": "08:30"})
scout.plan(
    "Triage the top five crashes from this week.", "Triage this week's top crashes",
    "Reproduce each of the five most frequent crashes and open a fix or an issue for it.",
    [{"title": "Reproduce the five crashes", "project": "mobile-app"},
     {"title": "Fix what is a one-liner", "project": "mobile-app"},
     {"title": "File issues for the rest", "project": "mobile-app"}],
    looked=("search_sessions",),
    answer="Five crashes make up 80% of this week's reports. I proposed reproducing each, fixing the simple ones and filing the rest.")

open(MARKER, "w").close()
print("✓ seeded. Atlas has the full picture; Scout and the Sessions view have a little.")
