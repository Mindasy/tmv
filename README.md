# tmv

以 treemap 方式查看 Linux 进程虚拟地址空间的 TUI 工具，主要面向 arm64（4K/16K/64K 页、39/48/52 位 VA）。可以直接回答：

- 哪个 VMA / 文件 / 类别占了最多的内存
- 某个地址属于哪个模块、哪个函数、哪一行源码
- 这段映射里有多少页真正驻留、多少被换出、多少共享、有没有命中 THP
- 两次采样之间谁在增长
- 某个线程当前的调用栈

## 功能特性

### 视图与导航

- Treemap 主视图，矩形面积与所选度量成正比，按类别 / 文件 / 映射聚合
- 三级结构：进程 → 组 → VMA，可逐层下钻
- 度量切换：PSS / RSS / 虚拟大小 / 独占 / 共享 / Swap
- 排序切换：按大小降序 / 按地址升序
- 方向键按几何位置选择相邻块，支持 Home/End
- 顶部地址条按地址顺序展示所有 VMA 比例，选中范围有白色标记
- 详情面板：地址区间、权限、文件、偏移、Size/RSS/PSS/独占/共享/Swap/AnonHuge、子项 Top 列表

### 页级信息

- 读取 `/proc/<pid>/pagemap`，逐页展示匿名驻留、文件、共享、Swap、未映射、THP
- 大映射自动抽样（最多 4096 个样本），标题标注“采样 N”
- root 下补充 `/proc/kpagecount`（共享页）与 `/proc/kpageflags`（THP）
- 非 root 时 PFN 被内核抹零，标题提示“PFN 需 root”
- 详情面板同步显示驻留率、Swap 率、文件 / 共享 / THP 占比
- 同一区间再次扫描时，标题显示页级变化：`驻留+n 换出+n 取消+n 共享+n 独占+n`

### 内存查看

- `x` 以当前选中映射的起始地址打开十六进制视图：地址 + 16 字节 hex + ASCII，类似 gdb 的 `x/32x`
- 一次读取 512 字节（32 行），`↑↓` 逐行、`PgUp/PgDn` 翻页、`Home/End` 跳首尾
- 读取走 `/proc/<pid>/mem`，需要 ptrace 权限；未映射或权限不足时面板内直接显示原因
- 回放与进程退出后不可用，面板会说明原因；`--demo` 用合成字节预览界面

### 内存账务

- `smaps_rollup` 对账：把 VMA 汇总的 RSS / PSS / Swap 与内核账本逐项比对，差异直接列出
- KSM：全局 `run` / `pages_sharing` / `pages_unshared` / `full_scans`，以及 `/proc/<pid>/ksm_stat`
- cgroup：自动识别 v2（`0::/path` + `memory.current`）与 v1（`N:memory:...` + `memory.usage_in_bytes`），读取 current / peak / max / swap 与 memory.stat
- NUMA：解析 `/proc/<pid>/numa_maps`，按节点汇总页数，并统计 `anon_huge`
- THP：全局策略（always / madvise / never）+ AnonHuge / FilePmd / ShmemPmd 合计 + 可合并的 VMA 数
- Swap 明细：`SwapPss`、cgroup swap 与各类别 Swap 汇总
- page_owner：探测 `/sys/kernel/debug/page_owner` 是否可读
- TUI 内 `a` 打开账务面板，`r` 重新采集；`--report` 输出结果末尾附「内存账务」段

### 符号与地址解析

- 解析 ELF symtab + dynsym，自动处理 `ET_DYN` / `ET_EXEC` 的加载偏差
- 通过 DWARF 显示 `文件:行`
- 主文件被 strip 时，用 build-id 或 `.gnu_debuglink` 在 `/usr/lib/debug` 下查找独立调试文件
- `[vdso]` / `[vsyscall]` 从 `/proc/<pid>/mem` 读出 ELF 后解析符号（需要 ptrace 权限）
- 支持 Rust 符号 demangle、`(deleted)` 路径后缀
- 读取 `/tmp/perf-<pid>.map` 解析 JIT 符号（`[jit]` 模块）
- `/` 输入任意十六进制地址：解析模块 / 符号 / 源码行，并自动跳到包含它的 VMA

### 视图与过滤

- `v` 在 treemap 与「映射列表」之间切换，列表默认按当前度量降序，`o` 可切换为地址升序，展示地址区间 / 虚拟 / RSS / PSS / Swap / 权限 / 路径
- 列表选中项与 treemap 焦点、右侧详情、页图联动，`Enter` 回到 treemap
- `f` 输入过滤关键字，匹配路径、标签（如 `[heap]`）或权限；`Enter` 应用，空串清除
- 过滤同时作用于 treemap、列表与 `/` 地址跳转，状态行显示「过滤:关键字 命中/总数」
- 被过滤掉的地址用 `/` 查询时会提示「不在当前过滤结果中」
- `?` 打开帮助面板，集中列出全部按键，任意键关闭

### 时间维度

- 与上一份快照对比，按 `(起始地址, 路径)` 匹配 VMA
- 块标签显示 `▲4.0M` / `▼1.2M`，状态行显示总量变化
- 详情显示 `变化 ▲4.0M (392.0M → 396.0M)`
- `--interval N` 启动时开启定时采样，`r` 立即采样，`rr` 开关定时刷新，`s` 打开设置界面
- 设置界面可调整刷新间隔（1/2/5/10/15/30/60/120 秒）、定时刷新、页图与变化标记
- 快照与页图采集在后台线程执行，采集期间界面保持可交互，状态行显示「采集中…」
- 主循环等待时长自适应：采集期间 60ms 取结果，空闲 250ms，进程退出后 1s
- 目标进程退出后自动停止采样：状态行显示「进程已退出 / 已停止」，保留最后一次快照继续查看
- `d` 开关变化显示

### 导出与自动化

- `--json`：把快照输出为 JSON（`tmv.snapshot` v1），可直接作为基线
- `--replay FILE`：把快照 JSON 加载回 TUI 浏览，不采集、不 attach 进程，用于离机复盘
- `--report`：输出人读文本报告，含总量、按类别、按文件 Top N、VMA Top N
- `--save-baseline FILE`：保存当前快照；`--baseline FILE`：与基线对比
- `--top N`：控制报告与差值列表的条目数
- `--metric <virtual|rss|pss|private|shared|swap>`：指定面积与差值使用的度量
- `--lookup <ADDR>`：解析十六进制地址并打印模块 / 符号 / 源码行，适合脚本与现场排查
- `--stack`：回溯主线程调用栈并逐帧打印模块 / 符号 / 源码行，标题标出所用算法
- 退出码区分失败原因，便于脚本判断（见下表）

### 线程与调用栈

- 枚举 `/proc/<pid>/task/*`，显示 tid、名称、状态
- `PTRACE_ATTACH` 读取寄存器：aarch64 取 `x0–x30 / sp / pc / pstate` 与 TLS（`TPIDR_EL0`），x86_64 取 `rax…rip / rsp / rbp` 与 TLS（`fs_base`）
- 优先用 `.eh_frame` 的 DWARF CFI 规则逐帧展开；主文件无 CFI 时自动改用 build-id / `.gnu_debuglink` 的调试文件
- CFI 走不动时自动回退到帧指针链（aarch64 的 x29 / x86_64 的 rbp），栈标题标注 `DWARF` 或 `帧指针`
- 返回地址自动剥离 PAC / TBI 高位，并按 VA 位宽截断到用户态地址
- 逐帧符号解析，显示 `#0 0x... symbol+0x24 file:line`
- attach 后必定 detach；权限失败时给出 yama 提示

### 平台与工程

- arm64 几何抽象：页大小 4K/16K/64K、VA 位宽 39/48/52、PMD 大页尺寸、TBI/MTE tag 剥离
- 双架构：aarch64 与 x86_64 均支持寄存器读取、CFI / 帧指针回溯与 TLS
- `--demo` 合成数据，可在 macOS 上预览全部界面（非 Linux 不读取 `/proc`）
- Release 提供 aarch64 / x86_64 的 musl 静态二进制，解包即可在 openEuler 等发行版运行
- `cargo test --workspace` 覆盖几何、解析、treemap 布局、diff、符号与 UI 渲染

## 界面布局

```
┌ 状态行：pid / 度量+总量 / 分组 / 排序 / VA 几何 / root / Δ总量 / 自动间隔 ┐
├──────────────────────────────────────┬───────────────────────────────┤
│ Treemap（面积 ∝ 度量）                │ 详情：地址、符号、源码行、     │
│ 块内：名称 + 大小 + ▲▼变化            │ 变化、权限、统计、页汇总       │
├──────────────────────────────────────┴───────────────────────────────┤
│ 页图（彩色页状态 + 图例）  或  线程栈（#[帧] PC 符号 源码行）           │
├──────────────────────────────────────────────────────────────────────┤
│ 地址条：按地址顺序的比例带 + 选中范围标记                              │
├──────────────────────────────────────────────────────────────────────┤
│ 键位提示 / 地址输入框                                                 │
└──────────────────────────────────────────────────────────────────────┘
```

## 构建

### macOS 交叉编译 aarch64 静态二进制

无需 Docker 或 Zig，使用 Rust 自带的 `rust-lld` 与 musl 自包含 CRT：

```bash
export PATH="$HOME/.cargo/bin:$PATH"
rustup target add aarch64-unknown-linux-musl

export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=\
"$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | awk '/host:/{print $2}')/bin/rust-lld"

cargo build --release --target aarch64-unknown-linux-musl
```

产物：

```
target/aarch64-unknown-linux-musl/release/tmv
```

`file` 应显示 `ELF 64-bit LSB executable, ARM aarch64, statically linked`。

### Linux 原生构建

```bash
cargo build --release
```

### 测试

```bash
cargo test --workspace
```

## 部署与运行

```bash
scp target/aarch64-unknown-linux-musl/release/tmv <用户>@<主机>:/tmp/
ssh <用户>@<主机>
chmod +x /tmp/tmv
```

| 命令 | 说明 |
|---|---|
| `./tmv <pid>` | 查看指定进程 |
| `sudo ./tmv <pid>` | 启用 PFN、共享页、THP、ptrace 等全部能力 |
| `./tmv <pid> --interval 5` | 每 5 秒自动采样一次 |
| `./tmv <pid> --json > snap.json` | 输出快照 JSON 后退出 |
| `./tmv <pid> --report` | 输出文本报告后退出 |
| `./tmv <pid> --save-baseline base.json` | 保存基线快照 |
| `./tmv <pid> --baseline base.json --report` | 与基线对比后输出报告 |
| `./tmv <pid> --report --metric rss --top 20` | 指定度量与条目数 |
| `./tmv <pid> --lookup 0xffffa5d1a748` | 解析单个地址后退出 |
| `./tmv <pid> --stack` | 回溯主线程调用栈后退出 |
| `./tmv --demo` | 合成数据预览，可在 macOS 直接运行 |
| `./tmv --help` | 参数说明 |

PID 不存在时输出：

```
Error: 进程 2000000000 不存在
```

退出码：

| 退出码 | 含义 |
|---|---|
| `0` | 正常结束 |
| `1` | 参数错误（缺少 PID 等） |
| `2` | 进程不存在，或 `--lookup` 的地址没有对应符号 |
| `3` | 权限不足（无法读取 `/proc/<pid>`） |
| `4` | 其他采集 / IO / 基线文件错误 |

## 键位

| 按键 | 功能 |
|---|---|
| `←↑↓→` / `hjkl` | 按几何方向选择相邻块；列表视图下逐行移动 |
| `Enter` | 下钻到选中的组；列表视图下切回 treemap |
| `Esc` / `Backspace` | 返回上一层；根层 `Esc` 退出 |
| `Home` / `End` | 跳到同级第一个 / 最后一个 |
| `v` | 切换主视图：treemap ↔ 映射列表（按当前度量降序） |
| `f` | 输入过滤关键字（匹配路径 / 标签 / 权限），`Enter` 应用，`Esc` 取消 |
| `m` | 切换度量（PSS → RSS → 虚拟 → 独占 → 共享 → Swap） |
| `g` | 切换分组（类别 → 文件 → 映射） |
| `o` | 切换排序（大小 ↔ 地址），切换后回到根层 |
| `d` | 开关变化量显示 |
| `p` | 开关页图面板 |
| `t` | 开关线程栈面板 |
| `a` | 开关账务面板（rollup 对账 / KSM / cgroup / NUMA / THP） |
| `x` | 打开内存查看器：以选中映射起始地址做十六进制 + ASCII 查看（类似 gdb `x`），`↑↓` 滚动、`PgUp/PgDn` 翻页、`Home/End` 跳首尾、`Esc` 关闭 |
| `[` / `]` | 在线程栈面板中切换上一个 / 下一个线程 |
| `u` | 重新回溯当前线程 |
| `/` | 输入十六进制地址，`Enter` 解析并跳转，`Esc` 取消 |
| `r` | 立即重新采样（并刷新线程列表） |
| `rr` | 快速连按两次 `r`：开关定时刷新 |
| `s` | 打开设置界面（间隔、定时刷新、页图、变化标记） |
| `?` | 打开帮助面板（任意键关闭） |
| `q` / `Ctrl-C` | 退出 |

设置界面内：`↑↓` 选择，`←→` / `+` / `-` 调整，`Enter` / `Space` 切换，`Esc` / `s` 关闭。

## 典型工作流

### 定位内存大户

1. 默认 PSS + 类别分组，先看 `heap`、`匿名私有`、`文件私有` 谁最大
2. 按 `g` 切到文件分组，看具体是哪个 `.so` 或数据文件
3. `Enter` 下钻，方向键选中具体 VMA，右侧看 PSS / 共享 / 权限
4. 页图中“未映射”比例高说明该区间有空洞，下钻到单个 VMA 更准确

### 查看共享与 THP

1. 进入文件分组，选中 libc 等共享库
2. 页图标题显示「共享 x% / THP y%」
3. root 下才会判定共享页与 THP；非 root 显示「PFN 需 root」

### 从地址反查代码位置

1. 按 `/`，输入如 `0x5500498d5080`，回车
2. 自动跳到包含该地址的 VMA
3. 详情顶部显示模块、符号、偏移、`文件:行`
4. 系统库被 strip 时只有 dynsym 名字，没有源码行

### 找内存增长

1. 启动加 `--interval 5`，或手动按 `r` 建立基线
2. 块标签出现 `▲` / `▼`，状态行显示总量变化
3. 下钻到增长的组，详情子项列表会标出每个子项的变化
4. `d` 可临时关闭变化标记

### 对账与排查资源上限

1. 按 `a` 打开账务面板
2. 「差异」接近 0 说明 VMA 汇总与 `smaps_rollup` 一致；明显不为 0 通常是采集期间进程在变化，或部分 VMA 不可读
3. 看 cgroup 的「当前 / 上限 / 峰值」判断是否逼近容器限额
4. KSM 合并页多说明共享度高；结合全局 THP 策略与 AnonHuge 判断大页收益
5. 根层 `r` 重新采集快照后，账务面板会同步刷新

### 采集基线与对比（脚本可用）

```bash
# 1. 建立基线
./tmv 1234 --save-baseline base.json

# 2. 运行一段时间后对比
./tmv 1234 --baseline base.json --report --top 20

# 3. 需要结构化数据时
./tmv 1234 --baseline base.json --json > now.json
```

`base.json` 与 `--json` 输出同一格式（`format` / `version` / 快照字段 / 可选 `diff`），
版本高于当前支持的版本会直接报错退出。

### 查看线程调用栈

1. 按 `t` 打开线程栈面板（会短暂 attach，目标线程暂停）
2. `[` / `]` 切换线程，`u` 重新回溯
3. 需要 root，或 `sysctl kernel.yama.ptrace_scope=0`
4. 目标程序需保留帧指针（`-fno-omit-frame-pointer`），否则中间帧可能缺失

## 权限与依赖

| 信息 | 依赖 |
|---|---|
| VMA / smaps 统计 | 同 uid 或 root；`smaps` 不可读时降级到 `maps`，仅虚拟大小可用 |
| 页状态 | `pagemap` 可读（同 uid） |
| PFN / 共享页 / THP | root（`CAP_SYS_ADMIN`） |
| 符号 / 源码行 | 目标文件可读；行号需 DWARF 未被 strip，strip 时尝试 build-id / `.gnu_debuglink` |
| vDSO 符号 | 读取 `/proc/<pid>/mem`，需要 ptrace 权限 |
| JIT 符号 | 进程生成 `/tmp/perf-<pid>.map` |
| 线程栈 | ptrace 权限 + DWARF CFI 或帧指针；容器内需 `--pid=host` 或可读目标 `/proc` |
| smaps_rollup / numa_maps / ksm_stat | 同 uid 或 root |
| KSM 全局状态、THP 策略 | 读 `/sys/kernel/mm/...`，通常所有用户可读 |
| cgroup 内存账本 | cgroup v2 + memory 控制器；容器内读到的是容器视角 |
| page_owner | root 且挂载 debugfs（`/sys/kernel/debug/page_owner`） |

## arm64 相关处理

- 页大小从 `sysconf(_SC_PAGESIZE)` 获取，支持 4K / 16K / 64K
- 页表索引位数与 PMD 大页尺寸随页大小推导：4K→2M、16K→32M、64K→512M
- VA 位宽按进程最高用户地址推断（39 位地址是 48 位子集，默认按 48 位处理）
- 地址解析前统一剥离 TBI / MTE 顶层 tag，并按 VA 位宽截断（不做符号扩展）
- 栈回溯的返回地址经过同样的规范化，等价于 `xpaci`

## 已知边界

- 页图是点采样（最多 4096 样本），标题会标“采样 N”
- 聚合组的页图按连续地址区间扫描，中间空洞计入“未映射”
- 差值只对比上一份快照；VMA 起始地址复用但路径变化时按“新增 + 移除”处理
- 读取进程信息依赖 Linux `/proc`；macOS 等平台只能用 `--demo`，直接传 PID 会以退出码 4 提示平台不支持
- `--replay` 回放时不能重新采样、看页图、回溯线程栈或采集账务，界面会提示原因；`--report` 会跳过账务段
- `x` 内存查看器一次读取 512 字节，依赖 ptrace 权限；跨过未映射边界时会短读
- 栈回溯的 CFI 只覆盖「CFA = SP/FP + 偏移」且 RA/FP 保存在栈上的规则；其它规则回退到帧指针链
- 页级变化只比较同一区间的连续两次扫描，切换选中项或区间变化时不显示
- 进程退出检测用 `kill(pid, 0)`，只判断 pid 是否存在；pid 被复用后不会自动切换到新进程
- 过滤是大小写不敏感的子串匹配，不支持正则或通配符
- treemap 面积使用当前值，未取前后快照的较大者
- 栈回溯优先 DWARF CFI，规则不支持（表达式 / 值偏移）时整条链回退帧指针
- MTE tag 只在最高字节上剥离，未做内存标签校验
- page_owner 只做可用性探测，未按 PFN 反查分配栈（单次全表扫描代价过高）
- cgroup 只读 memory 控制器；v1 的 swap 依赖 `memory.memsw` 是否挂载
- smaps 与 smaps_rollup 是两次读取，运行中的进程会有 KB 级差异（含内核 kB 舍入）
- `[vdso]` 没有 DWARF，只有符号名，没有源码行

## 项目结构

```
tmv/
├── Cargo.toml                  # workspace
├── tmv.spec                    # 可选的自建 RPM 描述
├── .github/workflows/          # CI 与 Release 自动化
└── crates/
    ├── tmv-core/           # 数据模型与 /proc 采集
    │   ├── geometry.rs         # 页大小、VA 位宽、PMD 尺寸、地址规范化
    │   ├── vma.rs              # Vma / Perms / SmapsStats / Category / Metric
    │   ├── proc.rs             # maps、smaps 解析与降级
    │   ├── pagemap.rs          # 页状态采集与 kpage 补充
    │   ├── diff.rs             # 快照差值
    │   ├── accounting.rs       # rollup 对账、KSM、cgroup、NUMA、THP
    │   ├── export.rs           # JSON 快照、基线读写、文本报告
    │   ├── tree.rs             # treemap 节点树与聚合
    │   ├── watch.rs            # 目标进程存活检查
    │   └── demo.rs             # 合成数据
    ├── tmv-symbols/        # ELF 符号、DWARF 行号 / CFI、perf JIT map
    ├── tmv-ptrace/         # 线程枚举、寄存器、DWARF / FP 回溯、TLS
    ├── tmv-tui/            # 状态机、treemap 布局、界面渲染
    └── tmv-cli/            # 参数解析、终端初始化、事件循环
```

## 常用开发命令

```bash
cargo test --workspace                 # 全部测试
cargo run -- --demo                    # macOS 上预览界面
cargo fmt --all                        # 格式化
```

## 发布与分发（GitHub Releases）

发布走 GitHub：推送 `v*` tag 后，[release.yml](file:///Users/mindas/code/mviewer/.github/workflows/release.yml) 自动构建静态二进制并附到 Release。

| 产物 | 用途 |
| --- | --- |
| `tmv-<tag>-aarch64-unknown-linux-musl.tar.gz` | aarch64 静态二进制，openEuler / 麒麟等直接可用 |
| `tmv-<tag>-x86_64-unknown-linux-musl.tar.gz` | x86_64 静态二进制 |
| `tmv-<version>.tar.gz` / `tmv-<version>-vendor.tar.gz` | RPM 用的源码包与 vendor 依赖包 |
| `*.sha256` / `SHA256SUMS` | 校验文件 |

二进制包内是 `tmv` 可执行文件 + `README.md` + `LICENSE`，解包即可运行，不依赖目标机 glibc 版本。

### 发版步骤

```bash
git tag -a v0.1.0 -m "tmv 0.1.0"
git push origin v0.1.0
```

workflow 会用 `cross` 构建两个 musl 目标，打包、计算 sha256，然后创建 Release 并自动生成 changelog。也可以在 Actions 页面手动 `workflow_dispatch`，产物名为 `tmv-dev-<target>.tar.gz`。

### 持续集成

[ci.yml](file:///Users/mindas/code/mviewer/.github/workflows/ci.yml) 在 push / PR 时执行：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --locked
```

### 可选：自建 RPM

仓库根目录的 `tmv.spec` 用于自建 RPM（不进发行版仓库），`ExclusiveArch` 覆盖 `aarch64` 与 `x86_64`。目标发行版没有 rust crate 的 `-devel` 包，因此依赖随 `vendor` 包提供，构建过程不联网。

```bash
cargo vendor vendor

tar --transform 's,^,tmv-0.1.0/,' \
    --exclude target --exclude .git --exclude '._*' \
    -czf tmv-0.1.0.tar.gz \
    Cargo.toml Cargo.lock README.md LICENSE clippy.toml tmv.spec crates
tar -czf tmv-0.1.0-vendor.tar.gz vendor

cp tmv-0.1.0*.tar.gz ~/rpmbuild/SOURCES/
rpmbuild -ba tmv.spec
sudo dnf install -y ~/rpmbuild/RPMS/$(uname -m)/tmv-0.1.0-1.*.rpm
```

- spec 里 `Source0` / `Source1` 指向 GitHub Release 附件；本地构建时把上面两个包放进 `SOURCES/` 即可，不会联网
- `%check` 会执行 `cargo test --release --offline --locked`，不想跑测试可加 `--nocheck`
- 不想用 RPM 时，直接用 Release 里的静态二进制即可

### 与 openEuler 24.03 的 Rust 版本兼容

openEuler 24.03 LTS 自带 `rust 1.82`，无法解析 `edition2024` 的依赖清单。为此 `Cargo.toml` 与 `Cargo.lock` 已把依赖固定在 MSRV ≤ 1.82 的版本：

| 依赖 | 锁定版本 | 原因 |
| --- | --- | --- |
| `clap` | `=4.5.23` | 更高版本会拉起 `clap_lex 1.x`（edition2024） |
| `addr2line` | `0.25` | 与 `object 0.37` 组合，避开 `ruzstd 0.9` |
| `gimli` | `0.32` | `0.34` 为 edition2024 |
| `object` | `0.37`（关闭默认 feature） | 避免压缩相关依赖 |
| `instability` | `0.3.1` | `0.3.14` 要求 rustc 1.88 |
| `unicode-segmentation` | `1.12.0` | `1.13` 要求 rustc 1.85 |

`Cargo.lock` 已按此解析并提交。workspace 与各 crate 都声明了 `rust-version = "1.82"`，仓库根目录的 `clippy.toml` 也设了 `msrv = "1.82"`，因此 clippy 与 rust-analyzer 不会再建议 1.83 以后才稳定的标准库 API；构建环境低于该版本时 cargo 会直接报错。升级这些依赖前，请先在 openEuler 24.03 上跑一次 `cargo build --offline --locked`，确认没有 MSRV 冲突。

