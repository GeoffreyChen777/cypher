#!/usr/bin/env python3
"""What production would have read at each case's time, from evidence only: the case folder's checked-out
branch, the pull requests gh would list for that branch, and which earlier creators surely made something.

    python3 -I facts.py --audit AUDIT --org ORG --cases heldout.json[,dev.json] --out facts.json

The fake machine (machine.mjs) keeps every repository on main and has no gh, so it can't know what
production reads from git (core.ts `workspaceBranches`, `gitAt`) and gh (`pullRequestsWithHead`).
This reconstructs those facts per case; run.mjs `--facts FILE` hands them to the machine. Never from the
label, the owner's decision or the agent's prose. Sources, latest before the record's time wins:

- branch (`checkout`):
  1. git's own output of a command that ran in the case folder, in any of the chat's sessions before
     the record: `On branch X`, `## X...` (status -sb), `[X 1a2b3c4] …` (commit), `git branch
     --show-current` / `rev-parse --abbrev-ref HEAD` printing X, `Switched to (a new) branch 'X'`;
  2. the engine's record of making that folder: "<folder> on the new branch X" (a workspace's
     create_worktree / start_session result), or `git worktree add -b X <folder>` that succeeded,
     in any session before the record;
  3. the folder's own git reflog, if it still exists: its HEAD now, walked back over checkouts and
     branch renames after the record's time.
  A default branch (main/master) or a detached HEAD is no workspace branch.
- prs: `gh pr create` results naming one PR URL (head from `-H/--head`, else the folder's branch at
  that point by the rules above), and `gh pr view/list --json` output holding number + headRefName.
- branches: local branches git's output showed in the chat's folder (a commit on it, a switch, `On branch`, a
  push of it), except the ones the case's earlier creators make (so the ledger still sees them new).
- ranExist: the corpus's `ran` creators all have a non-error result (build_corpus.py keeps only those):
  what they make existed afterwards, so the machine makes it between plan and settle (machine.mjs).
"""
import argparse
import bisect
import collections
import glob
import importlib.util
import json
import os
import re
import subprocess

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("build_corpus", os.path.join(HERE, "build_corpus.py"))
bc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bc)

HOME = os.path.expanduser("~")
DEFAULTS = {"main", "master"}
ON_BRANCH = re.compile(r"^On branch (\S+)$", re.M)
SHORT_STATUS = re.compile(r"^## ([^\s.][^\s]*?)(?:\.\.\.\S+)?(?: \[.*\])?$", re.M)
COMMIT_LINE = re.compile(r"^\[([\w./-]+)(?: \(root-commit\))? [0-9a-f]{7,40}\] ", re.M)
SWITCHED = re.compile(r"Switched to (?:a new )?branch '([^']+)'")
SHOW_CURRENT = re.compile(r"\bgit\b[^|;&]*\b(branch --show-current|rev-parse --abbrev-ref HEAD|symbolic-ref --short HEAD)\b")
NEW_BRANCH = re.compile(r"(/\S+?)[,]? on the new branch ([\w./-]+)")
WT_ADD = re.compile(r"\bgit\b(?:\s+-C\s+(\S+))?\s+worktree\s+add\s+(?:-b|-B)\s+(\S+)\s+(\S+)")
PR_URL = re.compile(r"github\.com/([\w.-]+/[\w.-]+)/pull/(\d+)")
# git's output naming a local branch that exists: a commit on it, a switch to it, a push of it.
LOCAL_BRANCH = [COMMIT_LINE, SWITCHED, ON_BRANCH, re.compile(r"^\s*\* \[new branch\]\s+([\w./-]+) -> ", re.M), re.compile(r"branch '([^']+)' set up to track", re.M)]
CREATES_BRANCH = re.compile(r"(?:checkout|switch)\s+(?:-[bcBC])\s+([\w./-]+)|worktree\s+add\s+(?:-[bB])\s+([\w./-]+)|\bbranch\s+([\w][\w./-]*)\s*(?:$|[;&|])")


def real(path):
    return path.replace("@HOME@", HOME)


def in_folder(command, cwd, session_cwd):
    """The command runs in `cwd`: the session's folder is it and no `cd`/`-C` goes elsewhere, or it cds there."""
    here = session_cwd.rstrip("/") == cwd.rstrip("/")
    for match in re.finditer(r"(?:^|[;&|(]\s*)cd\s+(\"[^\"]+\"|'[^']+'|\S+)", command):
        target = os.path.expanduser(match.group(1).strip("\"'").replace("$HOME", HOME)).rstrip("/")
        here = target == cwd.rstrip("/")
    for match in re.finditer(r"\bgit\s+-C\s+(\"[^\"]+\"|'[^']+'|\S+)", command):
        target = os.path.expanduser(match.group(1).strip("\"'").replace("$HOME", HOME)).rstrip("/")
        if target not in (".", cwd.rstrip("/")):
            return False
    return here


def branch_from_output(command, text):
    if not re.search(r"\bgit\b", command):
        return None
    name = None
    for pattern in (ON_BRANCH, SWITCHED):
        found = pattern.findall(text)
        if found:
            name = found[-1]
    if re.search(r"\bgit\b[^|;&]*\bstatus\b[^|;&]*(-sb|-bs|--branch)", command):
        found = SHORT_STATUS.findall(text)
        if found:
            name = found[-1]
    if re.search(r"\bgit\b[^|;&]*\bcommit\b", command):
        found = COMMIT_LINE.findall(text)
        if found:
            name = found[-1]
    if not name and SHOW_CURRENT.search(command):
        lines = [l.strip() for l in text.split("\n") if l.strip()]
        if lines and re.fullmatch(r"[\w./-]+", lines[0]) and lines[0] != "HEAD":
            name = lines[0]
    return name


class Evidence:
    """Every session entry with tool calls/results, by time, for the sessions it was asked about."""

    def __init__(self, org):
        self.org = org
        self.made = []  # (time ms, folder, branch): the engine's / git's record of making a worktree
        hits = subprocess.run(["grep", "-rl", "--include=*.jsonl", "-e", "on the new branch", "-e", "worktree add -b", "-e", "worktree add -B", os.path.join(org, "agent-sessions")], capture_output=True, text=True).stdout.split()
        for path in hits:
            entries = bc.load_session(path, cache=False) or []
            calls = {}
            for entry in entries:
                if entry.get("type") != "message":
                    continue
                message = entry["message"]
                stamp = bc.ms(entry.get("timestamp", "")) or 0
                if message.get("role") == "assistant":
                    for part in message.get("content") or []:
                        if isinstance(part, dict) and part.get("type") == "toolCall":
                            calls[part.get("id")] = (part.get("arguments") or {}).get("command")
                elif message.get("role") == "toolResult" and not message.get("isError"):
                    text = bc.text_of(message.get("content"))
                    for folder, branch in NEW_BRANCH.findall(text):
                        self.made.append((stamp, os.path.expanduser(folder).rstrip("/"), branch))
                    command = calls.get(message.get("toolCallId"))
                    if isinstance(command, str):
                        for match in WT_ADD.finditer(command):
                            folder = os.path.expanduser(match.group(3).replace("$HOME", HOME))
                            if os.path.isabs(folder):
                                self.made.append((stamp, folder.rstrip("/"), match.group(2)))
        self.made.sort()

    def made_branch(self, cwd, when):
        found = None
        for stamp, folder, branch in self.made:
            if stamp <= when and folder == cwd.rstrip("/"):
                found = branch
        return found


def reflog_branch(cwd, when):
    """The branch checked out in `cwd` at `when` (ms), from its HEAD reflog walked back from now."""
    if not os.path.isdir(cwd):
        return None
    run = lambda *args: subprocess.run(["git", "-C", cwd, *args], capture_output=True, text=True, timeout=10)
    head = run("symbolic-ref", "--short", "-q", "HEAD")
    if head.returncode != 0:
        return None
    current = head.stdout.strip()
    log = run("log", "-g", "--format=%ct%x09%gs", "HEAD")
    if log.returncode != 0:
        return None
    entries = []
    for line in log.stdout.splitlines():
        stamp, _, subject = line.partition("\t")
        if stamp.isdigit():
            entries.append((int(stamp) * 1000, subject))
    if not entries or min(s for s, _ in entries) > when:
        return None  # the folder's history starts after the record: not the same checkout
    for stamp, subject in entries:  # newest first
        if stamp <= when:
            break
        renamed = re.match(r"Branch: renamed refs/heads/(\S+) to refs/heads/(\S+)", subject)
        moved = re.match(r"checkout: moving from (\S+) to (\S+)", subject)
        if renamed and renamed.group(2) == current:
            current = renamed.group(1)
        elif moved and moved.group(2) == current:
            current = moved.group(1)
    return current


def chat_entries(index, chat, when):
    """The chat's own session entries before `when`, oldest session first, with each session's cwd."""
    out = []
    for path in index.chat_sessions.get(chat, []):
        entries = bc.load_session(path) or []
        header = next((e for e in entries if e.get("type") == "session"), {})
        for entry in entries:
            stamp = bc.ms(entry.get("timestamp", "")) or 0
            if stamp and stamp <= when:
                out.append((stamp, header.get("cwd") or "", entry))
    out.sort(key=lambda item: item[0])
    return out


def facts_for(index, evidence, record, case):
    when = bc.ms(record["time"]) or 0
    cwd = real(case["cwd"]).rstrip("/")
    branch, source = None, None
    prs = {}
    calls = {}
    running = None  # the folder's branch as the transcript shows it so far
    local = set()  # local branches git's output showed (in the chat's repositories)
    for stamp, session_cwd, entry in chat_entries(index, record.get("chatId") or "", when):
        if entry.get("type") != "message":
            continue
        message = entry["message"]
        if message.get("role") == "assistant":
            for part in message.get("content") or []:
                if isinstance(part, dict) and part.get("type") == "toolCall" and isinstance((part.get("arguments") or {}).get("command"), str):
                    calls[part.get("id")] = (part["arguments"]["command"], session_cwd)
        elif message.get("role") == "toolResult" and message.get("toolCallId") in calls and not message.get("isError"):
            command, session_cwd = calls[message["toolCallId"]]
            text = bc.text_of(message.get("content"))
            if re.search(r"\bgit\b", command) and (in_folder(command, cwd, session_cwd) or session_cwd.rstrip("/") == cwd):
                for pattern in LOCAL_BRANCH:
                    local.update(pattern.findall(text))
            if in_folder(command, cwd, session_cwd):
                name = branch_from_output(command, text)
                if name:
                    running = name
                    branch, source = name, "transcript"
                if re.search(r"\bgh pr create\b", command):
                    urls = set(PR_URL.findall(text))
                    if len(urls) == 1:
                        repo, number = urls.pop()
                        head = re.search(r"(?:^|\s)(?:-H|--head)(?:=|\s+)([\w./-]+)", command)
                        base = re.search(r"(?:^|\s)(?:-B|--base)(?:=|\s+)([\w./-]+)", command)
                        head = head.group(1) if head else running
                        if head:
                            prs[number] = {"number": number, "head": head, **({"base": base.group(1)} if base else {}), "repo": repo.lower(), "by": "gh pr create"}
            if re.search(r"\bgh pr (view|list)\b[^|;&]*--json", command):
                for match in re.finditer(r'\{[^{}]*"headRefName"\s*:\s*"([^"]+)"[^{}]*\}', text):
                    number = re.search(r'"number"\s*:\s*(\d+)', match.group(0))
                    base = re.search(r'"baseRefName"\s*:\s*"([^"]+)"', match.group(0))
                    if number:
                        prs[number.group(1)] = {"number": number.group(1), "head": match.group(1), **({"base": base.group(1)} if base else {}), "by": "gh pr view/list"}
    if branch is None:
        made = evidence.made_branch(cwd, when)
        if made:
            branch, source = made, "creation record"
    if branch is None:
        logged = reflog_branch(cwd, when)
        if logged:
            branch, source = logged, "reflog"
    out = {}
    if branch and branch not in DEFAULTS and branch != "HEAD":
        out["checkout"] = branch
        out["checkoutSource"] = source
    if prs:
        out["prs"] = list(prs.values())
    # Local branches that existed, except those the chat's own earlier creators make (their ledger entry needs them new).
    made = {name for line in case.get("ran") or [] for m in CREATES_BRANCH.finditer(line) for name in m.groups() if name}
    branches = sorted(b for b in local if b not in DEFAULTS and b != "HEAD" and b not in made and b != out.get("checkout"))
    if branches:
        out["branches"] = branches
    if case.get("ran"):
        out["ranExist"] = True
    return out


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--audit", required=True)
    parser.add_argument("--org", required=True)
    parser.add_argument("--cases", required=True)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()
    records = {}
    with open(args.audit, encoding="utf-8") as handle:
        for number, line in enumerate(handle):
            try:
                records[number] = json.loads(line)
            except ValueError:
                pass
    index = bc.Index(args.org)
    evidence = Evidence(args.org)
    out, stats = {}, collections.Counter()
    for file in args.cases.split(","):
        for case in json.load(open(file))["cases"]:
            record = records.get(case["id"])
            if not record:
                continue
            facts = facts_for(index, evidence, record, case)
            out[str(case["id"])] = facts
            split = case.get("split", "?")
            stats[f"{split}: cases"] += 1
            for key in ("checkout", "prs", "branches", "ranExist"):
                if key in facts:
                    stats[f"{split}: {key}"] += 1
            if "checkout" in facts:
                stats[f"{split}: checkout from {facts['checkoutSource']}"] += 1
    json.dump({"about": "facts.py: per-case facts production would read (branch, gh PRs, creators that made things), from evidence only", "stats": dict(stats), "cases": out}, open(args.out, "w"), indent=1)
    print(json.dumps(dict(sorted(stats.items())), indent=1))


if __name__ == "__main__":
    main()
