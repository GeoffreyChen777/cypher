#!/usr/bin/env bash
set -euo pipefail
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64)
    target=linux_amd64
    sha=8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8 ;;
  Darwin-arm64)
    target=darwin_arm64
    sha=aba9ced2dee8d27fecca3dc7feb1a7f9a52caefa1eb46f3271ea66b6e0e6953f ;;
  *) echo "unsupported actionlint host" >&2; exit 1 ;;
esac
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl --fail --silent --show-error --location --proto '=https' --proto-redir '=https' \
  --connect-timeout 15 --max-time 90 \
  "https://github.com/rhysd/actionlint/releases/download/v1.7.12/actionlint_1.7.12_$target.tar.gz" \
  -o "$tmp/actionlint.tar.gz"
python3 - "$tmp/actionlint.tar.gz" "$sha" <<'PY'
import hashlib, sys
assert hashlib.sha256(open(sys.argv[1], "rb").read()).hexdigest() == sys.argv[2], "actionlint checksum mismatch"
PY
tar -xzf "$tmp/actionlint.tar.gz" -C "$tmp" actionlint
# GitHub's documented queue:max predates actionlint support. Validate this
# field ourselves and exempt ONLY its unknown-key diagnostic, not general YAML
# or expression errors. Remove this exception when upgrading actionlint.
python3 scripts/ci/workflow_policy.py
"$tmp/actionlint" -shellcheck= -pyflakes= \
  -ignore 'unexpected key "queue" for "concurrency" section\. expected one of "cancel-in-progress", "group"' \
  .github/workflows/*.yml
