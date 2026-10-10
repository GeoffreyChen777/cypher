#!/usr/bin/env python3
"""Check that documentation links and docs/ citations resolve.

    python3 scripts/tests/check-doc-links.py

Two checks over git-tracked files:

1. Every relative Markdown link `[text](target)` in a tracked `.md` file names
   an existing file or directory; a `#fragment` pointing into a Markdown file
   must match one of its headings (GitHub's anchor rules).
2. Every repo-relative citation `docs/<path>.md` in any tracked text file
   (code comments, workflows, scripts) names an existing file, so moving a doc
   cannot leave a stale pointer behind.

Exits non-zero and lists each broken reference as `file:line: message`.
"""
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

LINK = re.compile(r"(?<!\!)\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
CITATION = re.compile(r"(?<![\w/.-])docs/[\w./-]+?\.md\b")
HEADING = re.compile(r"^#{1,6}\s+(.*?)\s*#*\s*$")
FENCE = re.compile(r"^\s*(```|~~~)")

# Citations that name a docs/ file in another project, a test fixture or demo
# content rather than a file in this repository.
FOREIGN_CITATIONS = {
    ("crates/harness/src/pi/fork.rs", "docs/rpc.md"),  # pi's own repository
    ("docs/research/pi-rpc.md", "docs/rpc.md"),  # pi's own repository
    ("crates/engine/src/repos/tests.rs", "docs/readme.md"),  # test fixture
    ("apps/ios/CypherTests/MentionsTests.swift", "docs/中文.md"),  # test fixture
    ("apps/ios/Cypher/App/DemoDataset.swift", "docs/chat2-sync.md"),  # demo content
}


def tracked_files():
    out = subprocess.run(
        ["git", "ls-files", "-z"], cwd=ROOT, check=True, stdout=subprocess.PIPE
    ).stdout
    return [ROOT / p for p in out.decode().split("\0") if p]


def read_text(path):
    try:
        data = path.read_bytes()
    except OSError:
        return None
    if b"\0" in data:
        return None
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError:
        return None


def slug(heading):
    text = re.sub(r"`|\*\*|__", "", heading)
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    return re.sub(r"[^\w\- ]", "", text.strip().lower()).replace(" ", "-")


_ANCHORS = {}


def anchors(path):
    cache = _ANCHORS
    if path not in cache:
        found, counts, fenced = set(), {}, False
        for line in (read_text(path) or "").splitlines():
            if FENCE.match(line):
                fenced = not fenced
                continue
            match = None if fenced else HEADING.match(line)
            if match:
                base = slug(match.group(1))
                n = counts.get(base, 0)
                counts[base] = n + 1
                found.add(base if n == 0 else f"{base}-{n}")
        cache[path] = found
    return cache[path]


def check_links(path, text, errors):
    fenced = False
    for number, line in enumerate(text.splitlines(), 1):
        if FENCE.match(line):
            fenced = not fenced
            continue
        if fenced:
            continue
        for target in LINK.findall(line):
            if re.match(r"^[a-z][a-z0-9+.-]*:", target, re.I) or target.startswith("//"):
                continue
            file_part, _, fragment = target.partition("#")
            dest = (path.parent / file_part).resolve() if file_part else path
            where = f"{path.relative_to(ROOT)}:{number}"
            if not dest.exists():
                errors.append(f"{where}: link target not found: {target}")
            elif fragment and dest.suffix == ".md" and fragment not in anchors(dest):
                errors.append(f"{where}: no heading for anchor: {target}")


def check_citations(path, text, errors):
    rel = path.relative_to(ROOT).as_posix()
    if path == Path(__file__).resolve():
        return
    for number, line in enumerate(text.splitlines(), 1):
        for cited in CITATION.findall(line):
            if (rel, cited) in FOREIGN_CITATIONS:
                continue
            if not (ROOT / cited).is_file():
                errors.append(f"{rel}:{number}: cited doc not found: {cited}")


def main():
    errors = []
    for path in tracked_files():
        text = read_text(path)
        if text is None:
            continue
        if path.suffix == ".md":
            check_links(path, text, errors)
        check_citations(path, text, errors)
    for error in errors:
        print(error)
    if errors:
        print(f"{len(errors)} broken documentation reference(s)", file=sys.stderr)
        return 1
    print("documentation links verified")
    return 0


if __name__ == "__main__":
    sys.exit(main())
