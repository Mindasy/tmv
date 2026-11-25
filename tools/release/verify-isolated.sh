#!/bin/bash
# Run real RPM transactions in private overlays, preserving an existing install.
set -euo pipefail
cd "$(dirname "$0")/../.."
[[ $EUID == 0 ]]
root=$PWD
if [[ "${1:-}" != --inside ]]; then
    exec unshare --mount --propagation private bash "$0" --inside
fi
work=$(mktemp -d /var/tmp/tmv-rpm-overlay.XXXXXX)
for path in /usr /etc /var/lib/rpm; do
    name=${path//\//_}
    mkdir -p "$work/$name-upper" "$work/$name-work"
    mount -t overlay overlay -o "lowerdir=$path,upperdir=$work/$name-upper,workdir=$work/$name-work" "$path"
done
arch=$(uname -m)
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
if rpm -q tmv >/dev/null 2>&1; then
    previous=$(rpm -q tmv)
    rpm -Uvh "$root/dist/$arch/tmv-$version-1.oe2403.$arch.rpm"
    rpm -V tmv
    /usr/bin/tmv --version
    printf 'PASS upgrade from %s in private mount namespace\n' "$previous" > "$root/dist/$arch/UPGRADE.txt"
    rpm -e tmv
fi
bash tools/release/verify-openeuler.sh
