#!/usr/bin/env bash
# The installer regressions on Python 3.8: scripts/tests/test-linux-cli.py in
# ubuntu:20.04, the release container (oldest glibc and Python the Linux build
# supports). Needs Docker. ci.yml and scripts/check-linux.sh both run this.
#
#   bash scripts/ci/installer-py38.sh
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
docker run --rm -v "$ROOT:/repo:ro" -w /repo ubuntu:20.04 bash -ec '
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq
  apt-get install -y -qq python3 curl ca-certificates >/dev/null
  python3 --version
  python3 scripts/tests/test-linux-cli.py
'
