# openEuler 24.03 双架构发布

版本：0.2.0。aarch64 与 x86_64 使用完全相同的脚本，必须分别通过验收。
允许在 QEMU 系统仿真运行 x86_64 测试，但必须标明仿真，不能用其结果推断原生性能。

在可销毁的 openEuler 24.03 测试机（root）准备依赖：

```sh
dnf install -y rust cargo gcc glibc-devel make rpm-build cpio python3 binutils git
bash tools/release/build-openeuler.sh
bash tools/release/verify-openeuler.sh 2>&1 | tee dist/$(uname -m)/acceptance.log
```

构建脚本先准备源码与 cargo vendor，再离线 rpmbuild。已有 vendor 包时可设置
`TMV_VENDOR_ARCHIVE=/path/tmv-0.2.0-vendor.tar.gz` 全程离线构建。构建目录默认在
/var/tmp（可以使用 TMV_BUILD_ROOT 指定磁盘目录），保留用于诊断。
输出在 dist/<arch>/，包含 RPM、SRPM、完整 tar 包、源码/vendor、校验与构建记录。
源码包使用文件白名单，排除本地二进制、录制文件、缓存。

验收脚本要求机器原先没有 tmv RPM，不覆盖已有安装。安装、测试、卸载 RPM，
再解压并测试 tar 包；临时录制和安装过程只作用于测试机。
覆盖分配归属、Cache 生命周期、调用栈、离线报表、SVG、原有布局与鼠标交互、
时间轴拖动、退出界面不终止目标，以及脱离源码目录的自动采集库定位。
只有全部通过才生成 ACCEPTANCE.txt。`verify-isolated.sh` 可在私有 overlay 挂载中额外测试已有旧版升级；原生性能另行测试。

发布工作流使用 GitHub 托管 `ubuntu-24.04`（x86_64）和 `ubuntu-24.04-arm`
（aarch64）runner，在官方 `openeuler/openeuler:24.03-lts` 容器内原生执行相同脚本。
不使用会滚动指向新 SP 的 `24.03` / `latest`；产物记录实际容器镜像 digest。
容器使用宿主 Linux 内核，因此另行保留 openEuler 完整 VM 的验证结果；容器测试
不应描述为 openEuler 内核覆盖。ptrace 测试仅为该临时容器添加 SYS_PTRACE 和
解除 seccomp 限制，不使用宿主 PID 命名空间。

push 匹配 Cargo.toml 版本的 `v*` tag 自动启动流水线。两个架构均完成构建、
真实进程测试、RPM 安装/卸载和 tar 包测试，且验收对应校验和一致，才自动发布
GitHub Release。带 `-` 的版本 tag 标为预发布。手动运行仅产生 Actions artifacts，
不创建 Release。不需要手工上传发布附件。

示例包演示的是采样状态，不是每次分配事件的精确重放。采集要求同架构
64 位 Linux/glibc 动态链接目标；不能由 musl 主程序的兼容性推导采集兼容性。

发布前核对：两架构 ACCEPTANCE.txt、构建日志、ELF/GLIBC 依赖记录和 SHA256SUMS。
分别标注验证过的系统/SP，不声明未经测试的架构或 SP 已受支持。

参考：
- [openEuler 官方容器镜像说明](https://hub.docker.com/r/openeuler/openeuler/)
- [GitHub 托管 runner](https://docs.github.com/en/actions/reference/runners/github-hosted-runners)
