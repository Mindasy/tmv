#!/bin/bash
set -euo pipefail
arch=$(uname -m)
sed -i 's|http://repo.openeuler.org|https://repo.openeuler.org|g' /etc/yum.repos.d/*.repo
packages=()
for name in rust cargo rust-std-static; do
    packages+=("https://repo.openeuler.org/openEuler-24.03-LTS/update/$arch/Packages/$name-1.82.0-1.oe2403.$arch.rpm")
done
packages+=("https://repo.openeuler.org/openEuler-24.03-LTS/update/$arch/Packages/llvm-toolset-18-llvm-libs-18.1.8-10.oe2403.$arch.rpm")
dnf -y --disablerepo='*' --enablerepo=OS --setopt=install_weak_deps=False \
    --setopt=localpkg_gpgcheck=True --setopt=max_parallel_downloads=8 \
    install "${packages[@]}" gcc glibc-devel make rpm-build cpio python3 binutils git
rustc --version
cargo --version
