# 动态内存占用示例

在仓库根目录完成 `cargo build --release`、`make -C tools/heaptrace` 后：

```sh
sh tools/heaptrace/demo/run.sh
```

约 18 秒完成，脚本输出 PID 和本次报告目录。每阶段保留文本、JSON、SVG 及独立 RSS 报告。

| 阶段 | Cache 存活 | Session.buffer 存活 | Temporary 存活 |
|---|---:|---:|---:|
| 1 | 32 MiB | 8 MiB | 0（64 MiB 已释放） |
| 2 | 32 MiB | 24 MiB | 0 |
| 3 | 0 | 24 MiB | 0 |
| 退出 | 0 | 0 | 0 |

所有申请都写入内存，便于对照 RSS；RSS 仍包含运行库等开销，不与申请字节完全一致。
符号标定规则根据示例二进制 build-id 自动生成。realloc 扩容仍归入原来的 Session.buffer。
临时分配在当前占用中为零，但 JSON 可看到累计申请/释放 64 MiB 及该调用点峰值。

C++ 类、结构体、模板与重载示例见 `cache_service.cpp`；堆申请和映射 RSS 对照见
`mapping_lifecycle.c`。在本目录用文件首行的命令编译后，用 `tmv run --no-ui --output demo.tmvr -- ./程序名`
录制，再用 `tmv open demo.tmvr --at 9` 回放。二进制 RPM 用户可从对应版本源码取得这些文件。
