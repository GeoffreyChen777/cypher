"""Strict check for the GitHub concurrency.queue field missing in actionlint 1.7.12.

GitHub.com supports it (actions-nga, fpt/ghec):
https://github.com/github/docs/blob/main/data/reusables/actions/actions-group-concurrency.md
Only this new field's diagnostic is exempted in actionlint.sh; no other
syntax-check diagnostics are suppressed.
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


def main():
    root = Path(__file__).resolve().parents[2] / ".github/workflows"
    for path in root.glob("*.yml"):
        expected = 1 if path.name in ("deploy.yml", "release.yml") else 0
        if check_queue(path.read_text()) != expected:
            raise ValueError(path.name + " has an unexpected number of production locks")
    print("Production concurrency queue policy verified")


if __name__ == "__main__":
    main()
