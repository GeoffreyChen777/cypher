#!/usr/bin/env bash
# Select the Xcode pinned in .xcode-version, by its build number.
#
#   bash scripts/ci/select-xcode.sh
#
# Exports DEVELOPER_DIR for later workflow steps through $GITHUB_ENV, and
# always prints it as the last line (`DEVELOPER_DIR=...`) for local callers.
set -euo pipefail
# Local builds and CI use the same Xcode. CI used to take the newest install
# at or above a major-version floor, which drifted twice: Xcode 26.3 rejected
# code Xcode 27 accepted (iOS 0.2.0 (21)), and later builds silently moved to
# a release candidate. The runner image renames installs (`Xcode_27.1.app`,
# `Xcode_27.1_Release_Candidate.app`, `Xcode_27.2_beta.app` behind plain
# aliases), so installs are matched by the build `xcodebuild -version` reports,
# never by name. Beta installs are refused even when pinned: App Store
# Connect rejects their builds.
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
read -r want_version want_build < <(grep -v '^[[:space:]]*#' "$ROOT/.xcode-version" | grep -m1 .)
[[ -n "${want_build:-}" ]] || { echo ".xcode-version needs '<version> <build>'" >&2; exit 1; }

found=""
installed=()
for app in /Applications/Xcode*.app; do
  resolved="$(cd -P "$app" 2>/dev/null && pwd)" || continue
  dir="$resolved/Contents/Developer"
  [[ -d "$dir" ]] || continue
  info="$(DEVELOPER_DIR="$dir" xcodebuild -version 2>/dev/null)" || continue
  version="$(awk 'NR==1 {print $2}' <<<"$info")"
  build="$(awk '/Build version/ {print $3}' <<<"$info")"
  installed+=("  $resolved: Xcode $version ($build)")
  if [[ "$build" == "$want_build" ]]; then
    case "$resolved" in *[Bb]eta*) continue ;; esac
    found="$dir"
    break
  fi
done
if [[ -z "$found" ]]; then
  echo "need Xcode $want_version ($want_build), pinned in .xcode-version; installed:" >&2
  if ((${#installed[@]})); then printf '%s\n' "${installed[@]}" | sort -u >&2; else echo "  (none)" >&2; fi
  exit 1
fi
DEVELOPER_DIR="$found" xcodebuild -version
if [[ -n "${GITHUB_ENV:-}" ]]; then echo "DEVELOPER_DIR=$found" >>"$GITHUB_ENV"; fi
echo "DEVELOPER_DIR=$found"
