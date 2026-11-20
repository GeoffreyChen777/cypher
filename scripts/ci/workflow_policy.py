"""Policy checks for the GitHub workflows that actionlint does not make.

The production lock: GitHub.com supports `concurrency.queue` (actions-nga,
fpt/ghec), which actionlint 1.7.12 does not know:
https://github.com/github/docs/blob/main/data/reusables/actions/actions-group-concurrency.md
Only this new field's diagnostic is exempted in actionlint.sh; no other
syntax-check diagnostics are suppressed.

The conventions in docs/operations/ci-cd.md: how workflow files are named,
that every step is named, and that actions are pinned by commit.
"""
from pathlib import Path
import re


def check_queue(text):
    queues = 0
    lines = text.splitlines()
    for index, line in enumerate(lines):
        if not re.fullmatch(r"\s*concurrency:\s*", line):
            continue
        indent = len(line) - len(line.lstrip())
        fields = {}
        for child in lines[index + 1:]:
            if not child.strip() or child.lstrip().startswith("#"):
                continue
            if len(child) - len(child.lstrip()) <= indent:
                break
            key, separator, value = child.strip().partition(":")
            if separator:
                if key in fields:
                    raise ValueError("Duplicate concurrency field")
                fields[key] = value.strip()
        if "queue" in fields:
            if fields != {"group": "cypher-production", "cancel-in-progress": "false", "queue": "max"}:
                raise ValueError("Production queue must be max, non-cancelling and use cypher-production")
            queues += 1
    declarations = sum(bool(re.search(r"(?:^|[\s,{])['\"]?queue['\"]?\s*:", line))
                       for line in lines if not line.lstrip().startswith("#"))
    if declarations != queues:
        raise ValueError("queue must use the checked block-style concurrency mapping")
    return queues


FILE_NAME = re.compile(r"(ci(-[a-z0-9]+)*|deploy|release-[a-z0-9]+(-[a-z0-9]+)*)\.yml")
# A step is a list item that opens with one of a step's keys; `name` comes first.
UNNAMED_STEP = re.compile(r"\s*- (uses|run|id|if|env|with|shell|working-directory"
                          r"|continue-on-error|timeout-minutes):")
USES = re.compile(r"\s*(?:- )?uses:\s*(\S+)(.*)")
PINNED = re.compile(r"[\w.-]+/[\w./-]+@[0-9a-f]{40}")


def check_conventions(name, text):
    """Raise ValueError for a step without a leading name or an unpinned action."""
    for number, line in enumerate(text.splitlines(), 1):
        where = "{}:{}: ".format(name, number)
        if UNNAMED_STEP.match(line):
            raise ValueError(where + "a step starts with its name")
        uses = USES.match(line)
        if uses and not uses.group(1).startswith("./"):
            if not PINNED.fullmatch(uses.group(1)) or not uses.group(2).startswith(" # v"):
                raise ValueError(where + "pin the action by commit, with its release in a comment")


def main():
    github = Path(__file__).resolve().parents[2] / ".github"
    for path in sorted((github / "workflows").iterdir()):
        if not FILE_NAME.fullmatch(path.name):
            raise ValueError(path.name + " is not named ci*.yml, deploy.yml or release-<target>.yml")
        text = path.read_text()
        check_conventions(path.name, text)
        # Every workflow that writes the production channel holds the lock.
        # release-ios.yml is absent on purpose: it publishes to TestFlight and
        # touches no R2 object or release pointer.
        expected = 1 if path.name in ("deploy.yml", "release-linux.yml", "release-macos.yml",
                                      "release-pi-runtime.yml") else 0
        if check_queue(text) != expected:
            raise ValueError(path.name + " has an unexpected number of production locks")
    for path in sorted((github / "actions").glob("*/action.yml")):
        check_conventions(str(path.relative_to(github)), path.read_text())
    print("Workflow policy verified")


if __name__ == "__main__":
    main()
