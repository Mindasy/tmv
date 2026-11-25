# tmv

以 treemap 方式查看 Linux 进程虚拟地址空间的 TUI 工具，主要面向 arm64（4K/16K/64K 页、39/48/52 位 VA）。可以直接回答：

- 哪个 VMA / 文件 / 类别占了最多的内存
- 某个地址属于哪个模块、哪个函数、哪一行源码
- 这段映射里有多少页真正驻留、多少被换出、多少共享、有没有命中 THP
- 两次采样之间谁在增长
- 某个线程当前的调用栈

## 快速选择入口

| 需要分析什么 | 命令 | 统计口径 |
| --- | --- | --- |
| 当前进程哪些映射占用大 | `tmv PID` 或 `tmv attach PID` | 虚拟 / RSS / PSS 等 |
| 哪类分配尚未释放、谁分配的 | `tmv run --output app.tmvr -- ./app` | 已追踪存活申请字节与分配栈 |
| 已采集进程的某个时刻 | `tmv open app.tmvr --at 9` | 距记录开始 9 秒之前最近采样 |
| 离线排名或自动化分析 | `tmv report app.tmvr --at 9 --stacks` | 业务标定 / 调用位置排名 |
| 单份旧快照 | `tmv --replay snapshot.json` | 保存的布局，无时间轴 |
| 不带目标进程预览 | `tmv --demo` | 合成数据 |

分配字节与 RSS 不是同一口径，不能相加；malloc 类型不能自动推导为 C/C++ 结构体。
需用 build-id、函数和可选返回偏移标定业务名称。详细限制和例子见后文。

## 功能特性

### 视图与导航

- Treemap 主视图，矩形面积与所选度量成正比，按类别 / 文件 / 映射聚合
- 基础 Blockmap 以地址排序展示区间；每行独立缩放，不是全地址统一比例页图
- 单击选择、拖动扫选、双击下钻、右键返回；工具栏与菜单可点击
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
- `n/N` 读取后/前一个窗口，`r` 重读，`i` 显示小端 u64/f64，`c` 设置变化比较基线；鼠标按钮与滚轮可操作
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
- `F` 用正则过滤（不区分大小写），语法错误时保留原过滤条件并在状态行提示
- 过滤同时作用于 treemap、列表与 `/` 地址跳转，状态行显示「过滤:关键字 命中/总数」，正则显示为 `/pattern/`
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
- `--read <ADDR[:LEN]>`：读取内存并按 `xxd` 风格打印（默认 256 字节，上限 64 KiB），不进入 TUI
- `--svg <FILE>`：导出可交互的 SVG（`--svg-format treemap|flame`）
  - treemap：与界面一致的布局，悬停显示路径 / 大小 / 地址区间 / 权限，点击分组可下钻（内嵌 JS，单文件无外部依赖）
  - flame：横向火焰图，按「总量 → 分组 → 映射」三层堆叠，适合看增长链条
  - 底部图例、标题里的度量与合计；给了 `--baseline` 时块底部画涨（绿）/ 跌（红）条带
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
- 界面偏好持久化到 `~/.config/tmv/config.toml`，退出自动写回，`--no-config` 可关闭
- 布局对块数是线性的：10 万 VMA 建树约 28 ms，200×60 区域铺满约 1 ms（release）
- openEuler 24.03 完整包包含主程序与 glibc 采集库；每种架构独立构建验收
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
| `./tmv <pid> --read 0xffffa5d1a748:128` | 读取 128 字节并按 `xxd` 风格打印 |
| `./tmv <pid> --svg mem.svg` | 导出交互式 treemap（点击分组下钻） |
| `./tmv <pid> --svg flame.svg --svg-format flame` | 导出火焰图 |
| `./tmv <pid> --svg diff.svg --baseline base.json` | 导出带涨跌条带的 treemap |
| `./tmv <pid> --stack` | 回溯主线程调用栈后退出 |
| `./tmv --demo` | 合成数据预览，可在 macOS 直接运行 |
| `./tmv <pid> --no-config` | 不读写 `~/.config/tmv/config.toml` |
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
| `F` | 输入正则过滤（不区分大小写），`Enter` 应用，语法错误时保留原条件 |
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

## 配置持久化

退出 TUI 时把界面偏好写到 `$XDG_CONFIG_HOME/tmv/config.toml`（默认 `~/.config/tmv/config.toml`），下次启动自动沿用：

```toml
# metric: virtual | rss | pss | private | shared | swap
metric = "pss"
# group: category | file | map
group = "category"
# sort: size | address
sort = "size"
interval = 5
auto_refresh = false
pages = true
diff = true
```

- 命令行 `--metric` / `--interval` 优先于配置文件
- `--no-config` 完全不读写配置；用 `TMV_CONFIG=/path/to/config.toml` 可换到别的位置
- 解析是宽松的：未知键和非法值会被忽略并沿用默认值，手改出错不会影响启动

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
- `--read` 单次最多 64 KiB，同样是短读语义；回放模式没有内存内容，会直接报错
- 栈回溯的 CFI 只覆盖「CFA = SP/FP + 偏移」且 RA/FP 保存在栈上的规则；其它规则回退到帧指针链
- 页级变化只比较同一区间的连续两次扫描，切换选中项或区间变化时不显示
- 进程退出检测用 `kill(pid, 0)`，只判断 pid 是否存在；pid 被复用后不会自动切换到新进程
- 过滤默认是大小写不敏感的子串匹配；`F` 可用正则（同样不区分大小写），无效正则会保留原条件
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
├── tmv.1                       # man page（spec 安装到 man1）
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
    ├── tmv-tui/            # 状态机、treemap 布局、界面渲染、config.rs / svg.rs
    └── tmv-cli/            # 参数解析、终端初始化、事件循环
```

## 常用开发命令

```bash
cargo test --workspace                 # 全部测试
cargo run -- --demo                    # macOS 上预览界面
cargo fmt --all                        # 格式化

# 10 万 VMA 的性能基准（默认被 ignore，需手动跑）
cargo test --release -p tmv-tui -- --ignored --nocapture large_snapshot
```

## 发布与分发（openEuler 24.03）

0.2.0 完整版包含主程序和采集库，分别构建 aarch64、x86_64 的 RPM 与免安装包。
各架构必须通过自己的安装包验收；具体结果见包内/附件的 `ACCEPTANCE.txt` 与
`BUILD-INFO.txt`。仿真测试标明 QEMU，不等同于原生性能测试。

| 产物 | 用途 |
| --- | --- |
| `tmv-0.2.0-1.oe2403.<arch>.rpm` | 安装到 `/usr/bin/tmv` 和 `/usr/lib64/tmv/libtmvtrace.so` |
| `tmv-0.2.0-openEuler-24.03-<arch>.tar.gz` | 解压运行；主程序旁附采集库与示例回放 |
| `tmv-0.2.0.tar.gz` / `tmv-0.2.0-vendor.tar.gz` | 源码与完整离线 Rust 依赖 |
| `tmv-0.2.0-1.oe2403.src.rpm` | 源码 RPM |
| `SHA256SUMS` | 下载校验 |

```bash
sha256sum -c SHA256SUMS
sudo dnf install ./tmv-0.2.0-1.oe2403.aarch64.rpm
man tmv
# x86_64 使用文件名带 x86_64 的包。

# 或免安装：
tar -xzf tmv-0.2.0-openEuler-24.03-aarch64.tar.gz
cd tmv-0.2.0-openEuler-24.03-aarch64
./tmv open examples/demo.tmvr --at 9
./tmv run --output service.tmvr -- ./your-service
```

无需系统全局配置 LD_PRELOAD；只给 `run` 启动的目标注入采集库。
本专用包使用 openEuler 24.03 的 glibc 构建，不能当作任意发行版通用静态包。

### 构建、验收和发布

```bash
bash tools/release/build-openeuler.sh
sudo bash tools/release/verify-openeuler.sh
# 测试机已有 tmv 时，可用私有挂载层验证升级，不覆盖宿主安装：
sudo bash tools/release/verify-isolated.sh
```

依赖、离线构建、隔离测试和 runner 要求见 [打包说明](tools/release/README.md)。
[release.yml](.github/workflows/release.yml) 在两个 openEuler 架构均通过验收后，
才为匹配版本的 `v*` tag 自动发布 Release；手动触发仅生成构建产物。
[ci.yml](.github/workflows/ci.yml) 执行格式、Clippy、单元测试与构建。

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


## 按分配位置定位堆内存大户（LD_PRELOAD）

新增的堆分析与原有 VMA/RSS 视图互补：原有视图回答哪些映射实际驻留较多，
堆分析回答哪些分配调用位置保留了最多尚未释放的申请字节。
这两种字节数不能相加，堆申请字节不是 RSS，也不包含 allocator 碎片及缓存。

采集库当前支持 **64 位 Linux/glibc、从启动跟踪的动态链接程序**。
完整版已附带采集库；源码使用方式如下。自行构建时 tmv 主程序也可使用 musl，采集库仍必须匹配目标 glibc 与架构。

```bash
make -C tools/heaptrace
mkdir -m 700 /tmp/tmv-run
TMV_TRACE=/tmp/tmv-run/service \
  LD_PRELOAD="$PWD/tools/heaptrace/libtmvtrace.so" ./your-service
```

生成 `service.<PID>.bin`（事件）、`.maps`（地址映射）、`.ids`（加载模块 build-id）。
文件名已存在时拒绝覆盖事件文件；每次诊断使用新目录。
采集初始化前和采集库析构之后不在覆盖范围内。

另一个终端查看实时排名，PID 必须与事件文件一致：

```bash
# 每 2 秒追加一次统计；Ctrl-C 停止查看，不停止业务程序
./tmv <PID> --heap-trace /tmp/tmv-run/service.<PID>.bin --interval 2 --top 20

# 进程退出后离线报告，保留对应 maps、ids 和原始二进制/调试符号
./tmv --heap-trace /tmp/tmv-run/service.<PID>.bin --report --top 30

# JSON 含原始调用地址、符号、源码位置、build-id、返回地址偏移、累计分配/释放及峰值
./tmv --heap-trace /tmp/tmv-run/service.<PID>.bin --json > heap.json

# 复用现有 treemap / flame SVG 渲染，可按标定分类下钻
./tmv --heap-trace /tmp/tmv-run/service.<PID>.bin --svg heap.svg
./tmv --heap-trace /tmp/tmv-run/service.<PID>.bin --svg heap-flame.svg --svg-format flame
```

`--interval` 也可与 JSON（每行一个报告）或 SVG（周期性重写）使用。
上述低层 `--heap-trace` 入口默认输出排名，图形通过 SVG 提供。交互方块图和调用栈使用下文 `run/open` 会话入口。

### 给分配调用位置标定业务名称

从 `--json` 输出中取 `build_id`、函数名和 `return_offset`，写入 JSON 数组：

```json
[
  {
    "label": "Session.buffer",
    "build_id": "替换为实际build-id",
    "function": "session_create",
    "return_offset": 88
  }
]
```

```bash
./tmv --heap-trace /tmp/tmv-run/service.<PID>.bin --heap-rules rules.json --report
```

偏移是**返回地址相对函数起点的十进制字节数**，不是调用指令偏移。
省略 `return_offset` 可以归类整个函数内的分配；冲突规则直接报错。
未命中规则的分配仍按调用位置展示，避免大户被隐藏。
符号解析前比较当前 ELF 与 `.ids` 捕获的 build-id；不匹配或无 build-id 时保留原始地址，
不套用规则。独立调试文件沿用原有 `.gnu_debuglink` / build-id 查找机制。
请保留二进制原路径（离线跨机器暂不支持路径重映射）。

### 统计口径与当前边界

- 面积、占比按已追踪的存活申请字节计算；分配块数不一定等于业务对象数。
- `realloc` 成功后保留原分配的分类；失败保留原记录；处理 glibc `realloc(p,0)` 释放语义。
- 采集覆盖 malloc/calloc/realloc/free、memalign/aligned_alloc/posix_memalign 的可插入调用。
  静态链接、自定义池内分配、内部不可插入接口及绕过这些函数的分配不保证覆盖。
- 默认采集分配调用栈（回溯缓冲深度 16，上限 32），统一 allocator 包装可以通过上层栈定位；
  尾调用、内联、缺失展开信息可能导致帧缺失。业务类型归属仍需标定。
- mmap/madvise 事件尚未接入堆采集；映射和驻留量继续使用原有 `tmv PID` / `--report` 分析。
- `.maps` / `.ids` 在初始化与正常析构时采集；在线 PID 可更新映射，动态加载模块的 build-id
  在结束时更新。中途加载又卸载的模块不能可靠离线归因；尤其是代码地址复用，目前没有模块加载代次记录，
  不支持对此类地址作可靠标定。普通缺失符号以原始地址展示。
- fork 子进程不记录到父进程日志；exec 后若保留环境变量则生成独立文件。
- `unknown_frees` 表示没有对应分配记录的释放（例如初始化前分配、未覆盖接口），会明确显示。
- 缺少结束记录时显示 `open-or-incomplete`，不能区分仍在运行与异常终止/磁盘写入失败。
  序号、PID、重复存活地址或事件格式异常会报错，尾部半条事件等待后续读取。
- 采集为诊断模式：全局锁串行化分配与记录以保证跨线程地址复用顺序，每条记录 64 字节，
  持续写磁盘，可能显著影响分配密集型负载。先在测试环境、限定诊断时长使用；暂无低开销采样模式。
- 信号处理函数中调用 allocator 本身不满足异步信号安全，不受本方案支持。

Linux 端到端测试（并发分配、realloc、符号标定与 SVG）：

```bash
cargo build --locked
sh tools/heaptrace/test.sh
```

## 会话采集与离线回放

原有 `tmv PID` 和旧快照 `--replay` 不变，默认仍是进程布局 Treemap。
新增入口：

```bash
# 自动设置 preload、创建会话并显示原有布局方块图
./tmv run --library /path/libtmvtrace.so --output server.tmvr -- ./server

# 无界面采集；程序仍使用当前标准输入输出
./tmv run --no-ui --library /path/libtmvtrace.so --output server.tmvr -- ./server

# 可选：复用上述 build-id 标定规则
./tmv run --heap-rules rules.json --output named.tmvr -- ./server

# 保留原有布局查看，不注入采集库
./tmv attach 1234

# 实时跟随记录，或打开已结束的会话
./tmv open server.tmvr

# 按秒选取此前最近的采样，不读取原进程
./tmv report server.tmvr --at 9 --analysis allocation
./tmv report server.tmvr --at 9 --analysis layout
./tmv report server.tmvr --at 9 --json
./tmv report server.tmvr --at 9 --svg heap.svg
```

采集库按 `--library`、`TMV_TRACE_LIB`、tmv 同目录、`/usr/lib64/tmv/`、源码构建目录顺序查找。
输出会话目录必须不存在。交互启动时业务 stdout/stderr 保存到 `program.log`，
采集器日志为 `collector.log`；需要交互输入的程序请用 `--no-ui`。
交互界面退出不会停止采集器或目标程序，目标结束后采集器自动完成记录。

会话界面：

- `Tab`：原有进程布局 / 新增分配 Treemap；默认保持布局模式。
- `[` / `]`：前一个 / 后一个采样点，自动暂停跟随。
- `End`：最新采样，记录中恢复跟随。
- `Space`：记录中暂停/恢复画面跟随；记录结束后按采样间隔播放。
- 分配模式 `Enter` 下钻到具体分配块，`Backspace` 返回；方向键选择。
- `v`：Treemap / 列表；`b`：基础地址区间视图。
- 原有布局模式的过滤、度量、分组和详情操作继续保留。
- 会话浏览采用记录数据，不对历史 PID 发起内存读取或线程 attach。

基础 Blockmap 当前按地址顺序展示区间，每个区间独立缩放，空洞折叠；
并非全地址空间统一比例图，尚未实现页级缩放和业务归属叠加。
分配模式需要先下钻到具体调用位置，再查看地址区间。

原有实时 `tmv PID` 的内存窗口增强：`x` 打开后，`n/N` 连续前后读取 512 字节，
`r` 重读，`i` 切换小端 u64/f64 解释，`c` 保存当前窗口比较基线；
随后 `r` 刷新时变化行显示黄色。翻到另一地址会清除比较基线。
读取不暂停目标，不保证跨字段一致性；数值解释不等于识别对象类型。

### 当前会话格式和阶段边界

`.tmvr` 是目录，`manifest.json` 为版本/进程/状态信息，`frames/` 为原子替换写入的
周期快照，同时保留 preload 原始日志及模块文件。快照记录布局与当次观察到的存活分配。
布局读取和分配日志读取是顺序进行的，不是全进程原子快照。

本批为**采样点重放**，不是任意事件时刻重放；原始分配日志尚无时间戳。
`--at` 选择指定秒数之前最近的采样，并显示实际采样时间；早于首个采样会报错。
正常退出后的最后一帧通常没有布局数据，界面默认打开最后一个有布局的采样，
`End` 可以查看最终分配账本；布局报表应指定退出前的 `--at`。
`complete` 仅指记录器读到了 preload 正常结束标记，不代表覆盖所有分配接口。

仍待后续批次完成：独立缓冲采集与开销控制、日志配额、记录器异常状态恢复、
逐事件时间戳/检查点、A/B 区间报告、界面标定、精确模块加载代次、
真正的可缩放页级 Blockmap、历史内容保存、指针跟随与 DWARF 类型展开。
本批记录器仍沿用同步 preload，且每次采样保存全部存活分配；适合有界测试诊断，
不应作为无限时长、低开销的生产记录器使用。


### 分配调用栈与时间曲线

新采集的会话保存分配时调用栈及符号/源码位置，可离线查看；旧会话仍可打开，
但不会凭空补出此前未采集的栈。`realloc` 保留原始分配栈，同时记录最近一次成功扩容/缩容的栈。
同一业务分类有多种栈路径时，分别列出各路径的存活字节与块数，不以任意一条栈代表全部对象。

```bash
./tmv open server.tmvr --at 9
./tmv report server.tmvr --at 9 --stacks
```

**仅离线回放**在底部增加时间曲线，主区域仍保留原有 Treemap。实时查看/实时采集界面不显示曲线，不启用 T/M/P 或曲线鼠标选时刻；结束后重新 `tmv open` 才进入此模式。`open --at` 仅接受已结束的会话：

- `M`：切换“已追踪存活申请字节”和“进程 RSS”；分开口径，不相加。
- 点击曲线或按住鼠标左右拖动：连续选择该时间之前最近的实际采样；上方视图与调用栈同步切换并停止跟随。拖出图的左右边缘时停在首末采样。
- `T`：输入**距本次记录开始的秒数**，例如 `9.5`；提示当前值和有效范围，Enter 确认、Esc 取消。
- `[` / `]`：逐个采样切换；`P`：跳到当前曲线指标的采样峰值。
- `Tab` 切换到分配归因后，选中业务分类或具体分配块，**右侧自动显示分配调用栈**；方向键或鼠标点击方块切换，PgUp/PgDn 滚动右侧长栈，`t` 可展开。窄终端自动将栈放在图下方。
- 栈弹窗用上下键、PgUp/PgDn 滚动，`t/Esc` 关闭。查看时暂停播放/跟随。
- 原有 `tmv PID` 的线程当前栈操作不变；分配栈与线程当前栈是不同数据。

曲线横轴按实际经过时间排列；采样之间阶梯保持前一个值，压缩到同一列时取列内峰值。
黄色列表示选中时刻；未采集的 RSS 显示缺口（·），不当作 0。
图中的峰值是**采样峰值**，不会包括采样间已分配又释放的瞬时高点。
当前曲线在打开时读取各采样的统计，超大会话尚未提供专用趋势索引。

preload 在取得日志互斥锁前展开调用栈，避免在该锁内进行符号解析/展开。
可用 `TMV_STACK_DEPTH=0` 关闭栈，或设置为 1～32；默认 16 包含采集器自身展开帧，
最终保留的业务帧数可能更少。调用栈会增加采集与日志开销，仍属于诊断模式。

示例 Cache 在约 12 秒释放，因此末段没有 Cache 方块是正常的。
打开第 9 秒或按 `P` 跳到存活采样峰值，可以同时看到 Cache 32 MiB 和 Session.buffer 24 MiB。

### 统一鼠标操作（终端界面）

原有 `tmv PID`、demo、旧快照和会话浏览均启用鼠标，键盘操作保留：

- Treemap / Blockmap：点击选择，按住左键拖动扫选，双击聚合块下钻，右键返回上层。
- 列表：点击行选择，拖动扫选，滚轮移动选择；原有映射列表双击返回对应方块视图。
- 原有布局顶部增加可点击工具栏，包含视图、菜单、度量、分组、刷新、内存与设置。
- “菜单”中的操作可点击；设置项点击切换/调整。弹窗提供关闭按钮，也可右键关闭。
- 地址条：点击或按住拖动跳到对应映射；页图点击打开该采样页地址的内存窗口。
- 详情、线程栈、账务、内存内容与分配栈支持滚轮；内存窗口有可点击的翻页、刷新、数值、基线按钮。
- 会话底部增加可点击导航按钮；分配图支持点击、双击下钻、拖动扫选及右键返回，右侧栈滚轮浏览。
- 离线曲线保留点击/拖动选时刻；实时界面仍不显示时间曲线。鼠标支持不会改变这一限制。
- 窄终端工具栏可能显示不全，可使用“菜单”及滚轮访问原有布局操作。
- 终端开启鼠标捕获后，复制文本通常需要按住终端支持的 Shift 修饰键。

### openEuler 24.03 完整版发布（0.2.0）

新版专用发行包同时包含 `tmv` 和 `libtmvtrace.so`，提供 aarch64 / x86_64
的 RPM 与免安装 tar.gz；各架构只有通过安装包验收后才可发布。
此前仅包含主程序的 musl 包不能单独提供完整采集功能。

打包、测试环境和发布门禁见 [双架构发布说明](tools/release/README.md)。
发布工作流使用 GitHub 双架构 runner + openEuler 24.03 容器，两个架构均通过才发布 Release。
