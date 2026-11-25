#!/bin/bash
# Native build. Source/vendor preparation can use the configured Cargo registry;
# rpmbuild itself is offline. Run on either supported architecture.
set -euo pipefail
cd "$(dirname "$0")/../.."
source /etc/os-release
[[ "${ID,,}" == openeuler && "$VERSION_ID" == 24.03 ]]
arch=$(uname -m)
[[ "$arch" == aarch64 || "$arch" == x86_64 ]]
version=$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)
root=$PWD
dist="$root/dist/$arch"
mkdir -p "$dist"
rm -f "$dist/ACCEPTANCE.txt" "$dist/UPGRADE.txt"
work=$(mktemp -d "${TMV_BUILD_ROOT:-/var/tmp}/tmv-package.XXXXXX")
mkdir -p "$work"/{SOURCES,SPECS,BUILD,BUILDROOT,RPMS,SRPMS}
# Explicit clean source tree: never package old binaries, recordings or caches.
python3 - "$root" "$work/SOURCES/tmv-$version.tar.gz" "$version" <<'PY'
import pathlib,sys,tarfile
root=pathlib.Path(sys.argv[1])
with tarfile.open(sys.argv[2],'w:gz') as t:
    files=['Cargo.toml','Cargo.lock','README.md','RELEASE-NOTES.md','LICENSE','clippy.toml','tmv.spec','tmv.1']
    for folder in ['crates','tools']:
        files += [str(p.relative_to(root)) for p in (root/folder).rglob('*') if p.is_file() and p.suffix in ('.rs','.toml','.c','.h','.sh','.py','.md')]
    files += ['tools/heaptrace/Makefile']
    for f in sorted(set(files)):
        t.add(root/f,arcname='tmv-'+sys.argv[3]+'/'+f,recursive=False)
PY
if [[ -n "${TMV_VENDOR_ARCHIVE:-}" ]]; then
    cp "$TMV_VENDOR_ARCHIVE" "$work/SOURCES/tmv-$version-vendor.tar.gz"
else
    cargo vendor --respect-source-config --locked "$work/vendor" > "$work/vendor-config.toml"
    tar -czf "$work/SOURCES/tmv-$version-vendor.tar.gz" -C "$work" vendor
fi
rpmbuild -ba --define "_topdir $work" --define 'dist .oe2403' tmv.spec 2>&1 | tee "$dist/build.log"
cp "$work/RPMS/$arch/"*.rpm "$dist/"
cp "$work/SRPMS/"*.rpm "$dist/"
cp "$work/SOURCES/"*.tar.gz "$dist/"
pkg="tmv-$version-openEuler-24.03-$arch"
mkdir -p "$work/extract" "$work/$pkg/examples"
(cd "$work/extract"; rpm2cpio "$dist/tmv-$version-1.oe2403.$arch.rpm" | cpio -idmu)
cp "$work/extract/usr/bin/tmv" "$work/$pkg/"
cp "$work/extract/usr/lib64/tmv/libtmvtrace.so" "$work/$pkg/"
cp README.md RELEASE-NOTES.md LICENSE "$work/$pkg/"
cp tools/heaptrace/demo/memory_demo.c "$work/$pkg/examples/"
TMV_BIN="$work/$pkg/tmv" TMV_LIBRARY="$work/$pkg/libtmvtrace.so" TMV_DEMO_DEST="$work/$pkg/examples" \
    bash tools/heaptrace/session-test.sh 2>&1 | tee "$dist/archive-functional.log"
tar -czf "$dist/$pkg.tar.gz" -C "$work" "$pkg"
{
    sha256sum "$work/SOURCES/"*.tar.gz
    cat /etc/os-release
    uname -a
    rustc --version; cargo --version; gcc --version | head -1
    rpm -q glibc rpm-build
    git rev-parse HEAD 2>/dev/null || true
    git diff --stat 2>/dev/null || true
    readelf -h "$work/$pkg/tmv"
    readelf --version-info "$work/$pkg/libtmvtrace.so"
    ldd "$work/$pkg/tmv"; ldd "$work/$pkg/libtmvtrace.so"
} > "$dist/BUILD-INFO.txt"
cp RELEASE-NOTES.md "$dist/"
(cd "$dist"; sha256sum *.rpm *.tar.gz > SHA256SUMS)
printf 'Built candidate packages: %s\nBuild workspace retained: %s\n' "$dist" "$work"
