%global debug_package %{nil}

# Build with tools/release/build-openeuler.sh on openEuler 24.03.

%global project_url https://github.com/Mindasy/tmv
%global release_tag v%{version}

Name:           tmv
Version:        0.2.0
Release:        1%{?dist}
Summary:        Treemap viewer for a process virtual address space
Group:          Applications/System

License:        MIT
URL:            %{project_url}
Source0:        %{project_url}/releases/download/%{release_tag}/%{name}-%{version}.tar.gz
# cargo vendor 生成的第三方依赖，随 release 一同发布。
Source1:        %{project_url}/releases/download/%{release_tag}/%{name}-%{version}-vendor.tar.gz

ExclusiveArch:  aarch64 x86_64

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gcc
BuildRequires:  make
BuildRequires:  glibc-devel

%description
tmv 以 WizTree 风格的 treemap 展示 Linux 进程的虚拟地址空间：
面积正比于所选的度量（PSS / RSS / 虚拟 / 独占 / 共享 / Swap），
可下钻到单个 VMA，查看页级状态、符号与源码行、线程调用栈，
并对账 smaps_rollup、KSM、cgroup、NUMA 与 THP。

主要面向 aarch64（4K / 16K / 64K 页、39 / 48 / 52 位 VA），
同样支持 x86_64。

%prep
%autosetup -n %{name}-%{version}

# 解出 vendored 依赖，并让 cargo 只使用本地目录，构建过程不联网。
tar -xzf %{SOURCE1}
mkdir -p .cargo
cat > .cargo/config.toml <<'EOF'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
EOF

%build
export CARGO_HOME="%{_builddir}/%{name}-%{version}/.cargo-home"
# Refresh workspace inputs restored from the source archive when reusing target.
if [ -n "${CARGO_TARGET_DIR:-}" ]; then find crates -type f -exec touch {} +; fi
cargo build --release --offline --locked --frozen
make -C tools/heaptrace CFLAGS="%{optflags} -fno-omit-frame-pointer"

%install
install -Dpm0755 "${CARGO_TARGET_DIR:-target}"/release/%{name} %{buildroot}%{_bindir}/%{name}
install -Dpm0755 tools/heaptrace/libtmvtrace.so %{buildroot}%{_libdir}/tmv/libtmvtrace.so
install -Dpm0644 tools/heaptrace/demo/memory_demo.c %{buildroot}%{_docdir}/tmv/examples/memory_demo.c
install -Dpm0644 tmv.1 %{buildroot}%{_mandir}/man1/tmv.1
# 不生成 debuginfo 包（见文件头 debug_package），这里显式剥离调试符号。
%{__strip} --strip-unneeded %{buildroot}%{_bindir}/%{name}

%check
export CARGO_HOME="%{_builddir}/%{name}-%{version}/.cargo-home"
cargo test --release --offline --locked --frozen

%files
%{_bindir}/%{name}
%{_libdir}/tmv/
%{_docdir}/tmv/examples/
%{_mandir}/man1/tmv.1*
%doc README.md RELEASE-NOTES.md
%license LICENSE

%changelog
* Wed Nov 25 2026 tmv maintainers - 0.2.0-1
- Bundle allocation collector, offline replay and mouse interaction

* Wed Nov 04 2026 tmv maintainers - 0.1.0-1
- Initial package for openEuler
