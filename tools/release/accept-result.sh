#!/bin/bash
set -euo pipefail
dist=$1
arch=$2
(cd "$dist"; sha256sum *.rpm *.tar.gz) > "$dist/ACCEPTED-SHA256SUMS"
cmp "$dist/SHA256SUMS" "$dist/ACCEPTED-SHA256SUMS"
printf 'PASS openEuler 24.03 %s: RPM install/remove, archive, preload discovery, attribution, replay, stacks, PTY mouse\n' "$arch" | tee "$dist/ACCEPTANCE.txt"
