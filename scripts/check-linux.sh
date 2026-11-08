#!/usr/bin/env bash
# Run CI's Linux checks from a Mac (or any Docker host), before pushing.
#
#   bash scripts/check-linux.sh [stage...]
#
# Stages:
#   scripts runtime rust   scripts/check.sh stages, in an ubuntu:24.04 container
#                          like ci.yml's ubuntu-24.04 jobs: the same Node
#                          (24.19.0), Python 3.12 and Rust (rust-toolchain.toml).
#                          These are the stages whose verdict depends on the OS;
#                          fmt, lint, edge and workflows answer the same anywhere.
#   installer              ci.yml's Python 3.8 installer regressions in
#                          ubuntu:20.04 (scripts/ci/installer-py38.sh)
#   all                    all of the above (the default)
#
# The working tree (tracked and untracked files, not ignored ones, plus .git) is
# copied into the container, so uncommitted changes are checked and the checkout
# is never written. Node, Rust and the Cargo target directory persist in Docker
# volumes named cypher-linux-*; `docker volume rm` them to start clean. The
# container matches the host's CPU: arm64 on Apple silicon, like the
# ubuntu-24.04-arm runner (ci's linux-backend job runs x86_64). Cargo runs one
# job per ~3 GiB of the container's memory; CHECK_LINUX_JOBS overrides it.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

NODE_VERSION=24.19.0
IMAGE=cypher-check-linux:ubuntu-24.04

stages=("$@")
[[ ${#stages[@]} -gt 0 ]] || stages=(all)
inner=()
installer=false
for stage in "${stages[@]}"; do
  case "$stage" in
    scripts | runtime | rust) inner+=("$stage") ;;
    installer) installer=true ;;
    all) inner+=(scripts runtime rust); installer=true ;;
    *) echo "unknown stage: $stage (scripts, runtime, rust, installer, all)" >&2; exit 2 ;;
  esac
done

command -v docker >/dev/null || { echo "check-linux.sh needs Docker" >&2; exit 1; }
docker info >/dev/null 2>&1 \
  || { echo "no Docker engine is running (start one, for example \`colima start\`)" >&2; exit 1; }

if ((${#inner[@]})); then
  # The packages ci.yml installs on the ubuntu-24.04 runner, plus the parts of
  # the runner image the checks rely on: lsof and ssh are named by the
  # permission tests, which resolve programs on PATH as the extension does.
  docker build -q -t "$IMAGE" - >/dev/null <<'DOCKERFILE'
FROM ubuntu:24.04
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update -qq \
 && apt-get install -y -qq ca-certificates curl git xz-utils build-essential pkg-config \
      cmake binutils python3 systemd shellcheck lsof openssh-client >/dev/null \
 && rm -rf /var/lib/apt/lists/*
DOCKERFILE
  echo "+ ubuntu:24.04: scripts/check.sh ${inner[*]}"
  { git ls-files -co --exclude-standard -z; printf '.git\0'; } \
    | COPYFILE_DISABLE=1 tar --no-xattrs --null -T - -cf - \
    | docker run --rm -i \
        -v cypher-linux-node:/opt/node \
        -v cypher-linux-rustup:/opt/rustup \
        -v cypher-linux-cargo:/opt/cargo \
        -v cypher-linux-target:/work/target \
        -e RUSTUP_HOME=/opt/rustup -e CARGO_HOME=/opt/cargo \
        -e NODE_VERSION="$NODE_VERSION" -e CHECK_LINUX_JOBS="${CHECK_LINUX_JOBS:-}" \
        -w /work "$IMAGE" bash -ec '
          tar -xf - -C /work
          git config --global --add safe.directory /work
          case "$(uname -m)" in aarch64) arch=arm64 ;; x86_64) arch=x64 ;; *) arch="$(uname -m)" ;; esac
          node_dir="/opt/node/node-v$NODE_VERSION-linux-$arch"
          if [[ ! -x "$node_dir/bin/node" ]]; then
            curl -fsSL "https://nodejs.org/dist/v$NODE_VERSION/node-v$NODE_VERSION-linux-$arch.tar.xz" \
              | tar -xJf - -C /opt/node
          fi
          if [[ ! -x /opt/cargo/bin/rustup ]]; then
            curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs \
              | sh -s -- -y -q --profile minimal --default-toolchain none --no-modify-path
          fi
          export PATH="$node_dir/bin:/opt/cargo/bin:$PATH"
          # GitHub'"'"'s runner has 16 GiB; a Docker VM often far less, and parallel
          # test links then die with signal 9. About 3 GiB per job.
          mem_gib=$(( $(awk "/MemTotal/ {print \$2}" /proc/meminfo) / 1048576 ))
          export CARGO_BUILD_JOBS="${CHECK_LINUX_JOBS:-$(( mem_gib / 3 > 0 ? mem_gib / 3 : 1 ))}"
          echo "memory ${mem_gib} GiB: $CARGO_BUILD_JOBS cargo jobs"
          # A Docker VM'"'"'s disk is small and shared with its images: no incremental
          # caches (as in CI) and line tables only, which keeps backtraces.
          export CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=line-tables-only
          echo "node $(node --version), $(python3 --version), $(rustup show active-toolchain | head -1)"
          bash scripts/check.sh "$@"
        ' check-linux "${inner[@]}"
fi

if $installer; then
  echo "+ ubuntu:20.04: Python 3.8 installer regressions"
  bash scripts/ci/installer-py38.sh
fi
