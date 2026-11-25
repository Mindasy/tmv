#!/bin/bash
# Run inside the disposable openEuler build container; keep network work visible.
set -euo pipefail
phase=${1:?expected os, update, or install}
opts=(--setopt=timeout=30 --setopt=minrate=10240 --setopt=retries=2
      --setopt=max_parallel_downloads=8 --setopt=keepcache=True
      --setopt=install_weak_deps=False --setopt=skip_if_unavailable=False)
case "$phase" in
  os|update)
    repo=OS
    [[ "$phase" != update ]] || repo=update
    echo "Fetching openEuler 24.03 $repo repository metadata"
    dnf -v "${opts[@]}" --disablerepo='*' --enablerepo="$repo" makecache
    ;;
  install)
    echo 'Installing build/test dependencies from OS and update'
    dnf -v "${opts[@]}" --disablerepo='*' --enablerepo=OS,update install -y \
      rust cargo gcc glibc-devel make rpm-build cpio python3 binutils git
    rustc --version
    cargo --version
    ;;
  *) echo "Unknown dependency phase: $phase" >&2; exit 2 ;;
esac
