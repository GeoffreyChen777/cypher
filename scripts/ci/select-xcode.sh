#!/usr/bin/env bash
# Select the newest installed release Xcode at or above a major-version floor.
#
#   bash scripts/ci/select-xcode.sh 27
#
# Exports DEVELOPER_DIR for later workflow steps through $GITHUB_ENV, or prints
# it when run outside GitHub Actions.
set -euo pipefail
# The runner image retires and RENAMES Xcodes without notice: an
# exact `/Applications/Xcode_26.app` pin failed this job outright
# ("missing DEVELOPER_DIR path") the day the image moved on. Pin a
# floor instead and take the newest install that clears it, so a
# point release is picked up automatically while an image carrying
# only an older SDK fails loudly rather than building against it.
# Betas are skipped: App Store Connect rejects their builds, and the
# xcode-27 image installs them beside the release under plain-named
# aliases too (Xcode_27.2.app -> Xcode_27.2_beta.app), so judge the
# resolved install, not the name.
floor="${1:?usage: select-xcode.sh <minimum major version>}"
best_version=""
best_dir=""
for app in /Applications/Xcode*.app; do
  resolved="$(cd -P "$app" 2>/dev/null && pwd)" || continue
  case "$resolved" in *[Bb]eta*) continue ;; esac
  dir="$resolved/Contents/Developer"
  [ -d "$dir" ] || continue
  version="$(DEVELOPER_DIR="$dir" xcodebuild -version 2>/dev/null | awk 'NR==1 {print $2}')"
  [ -n "$version" ] || continue
  newest="$(printf '%s\n%s\n' "${best_version:-0}" "$version" | sort -V | tail -1)"
  if [ "$newest" = "$version" ]; then
    best_version="$version"
    best_dir="$dir"
  fi
done
if [ -z "$best_dir" ] || [ "${best_version%%.*}" -lt "$floor" ]; then
  echo "need a release Xcode $floor or newer; installed:" >&2
  ls -d /Applications/Xcode*.app >&2 2>/dev/null || echo "  (none)" >&2
  exit 1
fi
echo "Xcode $best_version ($best_dir)"
DEVELOPER_DIR="$best_dir" xcodebuild -version
if [ -n "${GITHUB_ENV:-}" ]; then
  echo "DEVELOPER_DIR=$best_dir" >>"$GITHUB_ENV"
else
  echo "DEVELOPER_DIR=$best_dir"
fi
