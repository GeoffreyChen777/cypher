#!/usr/bin/env bash
# apt-get install as root in the ubuntu:20.04 (focal) release container,
# without depending on Ubuntu's own servers alone: GitHub's runners could not
# resolve archive.ubuntu.com for hours on 2026-11-11. Ubuntu's servers come
# first, with retries; when they still fail, each fallback mirror for this
# architecture in turn. apt checks every package against Ubuntu's archive
# keys, so a mirror can be slow or stale, never trusted with altered
# packages. Plain http: ca-certificates is often among what is installed.
#
# .github/workflows/linux.yml inlines the same steps (its container has no
# git, so no checkout, before this runs); change both together.
#
#   bash scripts/ci/apt-install.sh <package>...
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

# apt retries nothing by default.
printf 'Acquire::Retries "3";\nAcquire::http::Timeout "20";\n' >/etc/apt/apt.conf.d/80-ci-retries

# `apt-get update` exits 0 when its downloads fail; the install then cannot
# find the packages, so the pair fails together.
install() { apt-get update -qq && apt-get install -y -qq "$@"; }

install "$@" && exit 0

# Mirrors that carry focal, focal-updates and focal-security (arm64 lives
# in the separate ubuntu-ports archive, which fewer mirrors keep).
case "$(dpkg --print-architecture)" in
  amd64) mirrors=(http://mirrors.edge.kernel.org/ubuntu http://mirror.us.leaseweb.net/ubuntu http://mirrors.mit.edu/ubuntu) ;;
  arm64) mirrors=(http://ftp.tu-chemnitz.de/pub/linux/ubuntu-ports http://mirrors.ustc.edu.cn/ubuntu-ports) ;;
  *) mirrors=() ;;
esac
cp /etc/apt/sources.list /etc/apt/sources.list.ubuntu
for mirror in "${mirrors[@]}"; do
  echo "apt-install: Ubuntu's servers failed; trying $mirror" >&2
  sed -E "s#http://(archive|security|ports)\.ubuntu\.com/ubuntu(-ports)?/?#$mirror/#" \
    /etc/apt/sources.list.ubuntu >/etc/apt/sources.list
  install "$@" && exit 0
done
echo "apt-install: no server could install $*" >&2
exit 1
