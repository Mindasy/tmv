#!/bin/bash
# Requires a disposable native openEuler host and root to install/remove RPM.
set -euo pipefail
cd "$(dirname "$0")/../.."
source /etc/os-release
[[ "${ID,,}" == openeuler && "$VERSION_ID" == 24.03 ]]
arch=$(uname -m)
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
dist="$PWD/dist/$arch"
[[ $EUID == 0 ]]
rm -f "$dist/ACCEPTANCE.txt"
# Refuse to replace an existing installation; do not remove user software.
if rpm -q tmv >/dev/null 2>&1; then echo 'Use a clean host without tmv installed' >&2; exit 1; fi
(cd "$dist"; sha256sum -c SHA256SUMS)
work=$(mktemp -d /var/tmp/tmv-accept.XXXXXX)
installed=0
cleanup() { if [[ "$installed" == 1 ]]; then rpm -e tmv; fi; }
trap cleanup EXIT
check_auto() {
    # Run away from the source checkout without --library or TMV_TRACE_LIB.
    (cd "$work"; env -u TMV_TRACE_LIB "$1" run --no-ui --output "$2" -- /usr/bin/sleep 2)
    python3 - "$2" <<'PY'
import json,pathlib,sys,time
p=pathlib.Path(sys.argv[1])
for _ in range(100):
    m=json.loads((p/'manifest.json').read_text())
    if m['state']!='recording': break
    time.sleep(.1)
assert m['state']=='complete',m
assert any(f.stat().st_size>0 for f in p.rglob('*.bin')),list(p.rglob('*'))
PY
}
run_tests() {
    python3 tools/release/live-layout-test.py "$1"
    TMV_BIN="$1" TMV_LIBRARY="$2" bash tools/heaptrace/test.sh
    TMV_BIN="$1" TMV_LIBRARY="$2" bash tools/heaptrace/session-test.sh
}
rpm -ivh "$dist/tmv-$version-1.oe2403.$arch.rpm"
installed=1
rpm -V tmv
check_auto /usr/bin/tmv "$work/rpm.tmvr"
run_tests /usr/bin/tmv /usr/lib64/tmv/libtmvtrace.so
rpm -e tmv
installed=0
[[ ! -e /usr/bin/tmv && ! -e /usr/lib64/tmv/libtmvtrace.so ]]
tar -xzf "$dist/tmv-$version-openEuler-24.03-$arch.tar.gz" -C "$work"
pkg="$work/tmv-$version-openEuler-24.03-$arch"
check_auto "$pkg/tmv" "$work/archive.tmvr"
run_tests "$pkg/tmv" "$pkg/libtmvtrace.so"
"$pkg/tmv" report "$pkg/examples/demo.tmvr" --at 9 --stacks > "$work/example-report.txt"
grep -q Cache "$work/example-report.txt"
(cd "$dist"; sha256sum *.rpm *.tar.gz) > "$dist/ACCEPTED-SHA256SUMS"
printf 'PASS openEuler 24.03 %s: RPM install/remove, archive, preload discovery, attribution, replay, stacks, PTY mouse\n' "$arch" | tee "$dist/ACCEPTANCE.txt"
