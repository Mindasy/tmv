%global debug_package %{nil}

# 本 spec 用于自建 RPM（发行版仓库不收录，产物随 GitHub Release 发布）。
#
# 构建源码包（在仓库根目录执行）：
#   cargo vendor vendor
#   tar --transform 's,^,tmv-0.1.0/,' --exclude target --exclude .git \
#       --exclude '._*' -czf tmv-0.1.0.tar.gz \
#       Cargo.toml Cargo.lock README.md LICENSE clippy.toml tmv.spec tmv.1 crates
#   tar -czf tmv-0.1.0-vendor.tar.gz vendor
# 然后 rpmbuild -ba tmv.spec（两个 tar 包需在 ~/rpmbuild/SOURCES）。
#
# 目标发行版不提供 rust crate 的 -devel 包，因此依赖用 vendor 方式随包提供，
# 由 Source1 解出，构建过程不联网。

%global project_url https://github.com/Mindasy/tmv

Name:           tmv
Version:        0.1.0
Release:        1%{?dist}
Summary:        Treemap viewer for a process virtual address space
Group:          Applications/System

License:        MIT
URL:            %{project_url}
Source0:        %{project_url}/releases/download/v%{version}/%{name}-%{version}.tar.gz
# cargo vendor 生成的第三方依赖，随 release 一同发布。
Source1:        %{project_url}/releases/download/v%{version}/%{name}-%{version}-vendor.tar.gz

ExclusiveArch:  aarch64 x86_64

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gcc

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
cargo build --release --offline --locked --frozen

%install
install -Dpm0755 target/release/%{name} %{buildroot}%{_bindir}/%{name}
install -Dpm0644 tmv.1 %{buildroot}%{_mandir}/man1/tmv.1
# 不生成 debuginfo 包（见文件头 debug_package），这里显式剥离调试符号。
%{__strip} --strip-unneeded %{buildroot}%{_bindir}/%{name}

%check
export CARGO_HOME="%{_builddir}/%{name}-%{version}/.cargo-home"
cargo test --release --offline --locked --frozen

%files
%{_bindir}/%{name}
%{_mandir}/man1/tmv.1*
%doc README.md
%license LICENSE

%changelog
* Wed Nov 04 2026 tmv maintainers - 0.1.0-1
- Initial package for openEuler
