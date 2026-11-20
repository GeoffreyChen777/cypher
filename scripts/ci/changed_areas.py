#!/usr/bin/env python3
"""Decide which of ci.yml's expensive jobs a change needs.

    python3 scripts/ci/changed_areas.py                 # in ci.yml
    python3 scripts/ci/changed_areas.py --base origin/main
    python3 scripts/ci/changed_areas.py --files docs/README.md apps/ios/Cypher/App.swift

Two areas are optional; every other job always runs:

  rust  Test (Linux), Build (Linux), Lint (macOS) and Test (macOS). Skipped
        only when every changed file is one of NOT_RUST below, files no Rust
        build or test reads. A path this script has never heard of runs them.
  ios   Test (iOS). Runs when a changed file is one of IOS below, what the app
        and its tests are built from.

In ci.yml the changed files are a pull request's whole diff against its base,
or what a push to main added. When they cannot be determined, and on manual
runs, both areas run. Prints the verdict and writes it to $GITHUB_OUTPUT.
"""
import argparse
import fnmatch
import os
import subprocess
import sys

# Changing how CI decides or runs is a reason to run all of it.
EVERYTHING = (
    ".github/actions/*",
    ".github/workflows/ci.yml",
    "scripts/ci/changed_areas.py",
)

# What `check.sh macos-test` compiles from the iOS app and the binary embeds
# from the Edge; test_changed_areas.py checks these against their users.
RUST_READS = (
    "apps/edge/src/install.sh",
    "apps/ios/Cypher/Sync/ChatFrames.swift",
    "apps/ios/Cypher/Sync/CommandSignature.swift",
    "apps/ios/Cypher/Sync/DeviceTrust.swift",
    "apps/ios/Cypher/Sync/PreviewProjection.swift",
    "apps/ios/Cypher/Sync/StreamPreview.swift",
)

NOT_RUST = (
    ".agents/*",
    "AGENTS.md",
    "LICENSE",
    "README.md",
    "THIRD_PARTY_NOTICES.md",
    "apps/edge/*",
    "apps/ios/*",
    "apps/landing/*",
    "apps/www-redirect/*",
    "docs/*",
)

IOS = (
    ".xcode-version",
    "apps/ios/*",
    "protocol/*",
    "scripts/check.sh",
    "scripts/ci/ios-unit-tests.sh",
    "scripts/ci/select-xcode.sh",
)


def matches(path, patterns):
    # fnmatch's `*` crosses `/`, so `docs/*` is everything under docs/.
    return any(fnmatch.fnmatchcase(path, pattern) for pattern in patterns)


def areas(files):
    """Which optional areas a list of changed paths needs; None means unknown."""
    if files is None or any(matches(path, EVERYTHING) for path in files):
        return {"rust": True, "ios": True}
    return {
        "rust": any(path in RUST_READS or not matches(path, NOT_RUST) for path in files),
        "ios": any(matches(path, IOS) for path in files),
    }


def git(*arguments):
    return subprocess.run(("git",) + arguments, check=True, capture_output=True, text=True).stdout


def diff(old, new):
    # Without rename detection a moved file counts at both of its paths.
    return [path for path in git("diff", "--name-only", "--no-renames", "-z", old, new).split("\0")
            if path]


def changed_in_ci(event, before):
    """The files a workflow run is about, or None when that cannot be told."""
    try:
        if event == "pull_request":
            # The checkout is the merge of the pull request into its base;
            # against the base (its first parent) it is the whole pull request.
            if len(git("rev-list", "--parents", "-n", "1", "HEAD").split()) != 3:
                return None
            return diff("HEAD^1", "HEAD")
        if event == "push" and before.strip("0"):
            if subprocess.run(("git", "cat-file", "-e", before + "^{commit}"),
                              capture_output=True).returncode != 0:
                git("fetch", "--quiet", "--no-tags", "--depth=1", "origin", before)
            return diff(before, "HEAD")
    except subprocess.CalledProcessError as error:
        print("could not list the changed files: " + (error.stderr or str(error)).strip(),
              file=sys.stderr)
    return None


def main():
    parser = argparse.ArgumentParser(description="Which optional CI jobs a change needs.")
    source = parser.add_mutually_exclusive_group()
    source.add_argument("--base", help="compare HEAD with its merge base with this ref")
    source.add_argument("--files", nargs="*", help="the changed paths themselves")
    options = parser.parse_args()
    if options.files is not None:
        files = options.files
    elif options.base:
        files = diff(git("merge-base", options.base, "HEAD").strip(), "HEAD")
    else:
        files = changed_in_ci(os.environ.get("GITHUB_EVENT_NAME", ""), os.environ.get("BEFORE", ""))
    needed = areas(files)
    if files is None:
        print("changed files unknown: every job runs")
    else:
        print("{} changed files".format(len(files)))
    lines = ["{}={}".format(area, "true" if run else "false") for area, run in sorted(needed.items())]
    print("\n".join(lines))
    output = os.environ.get("GITHUB_OUTPUT")
    if output:
        with open(output, "a") as handle:
            handle.write("\n".join(lines) + "\n")


if __name__ == "__main__":
    main()
