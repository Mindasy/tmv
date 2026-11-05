use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use crossterm::event::{self, Event, KeyEventKind};
use ratatui::DefaultTerminal;
use tmv_core::{
    accounting, build_tree, demo, export, Accounting, DiffSummary, GroupBy, Metric,
    ProcessSnapshot, SnapshotDocument, SortOrder, TreeOptions,
};
use tmv_symbols::SymbolEngine;
use tmv_tui::{ui, App, Config};

/// `--read` 默认读取的字节数。
const DEFAULT_READ_LEN: usize = 256;

/// `--read` 单次读取上限，避免误输入一次读走大量内存。
const MAX_READ_LEN: usize = 64 * 1024;

/// `--read` 每行展示的字节数，与 `xxd` 一致。
const READ_LINE: usize = 16;

/// 以 treemap 方式查看进程虚拟地址空间（主要面向 Linux arm64）。
#[derive(Debug, Parser)]
#[command(name = "tmv", version, about)]
struct Args {
    /// 目标进程 PID
    pid: Option<i32>,

    /// 使用合成数据，便于在没有 Linux /proc 的环境下预览界面
    #[arg(long)]
    demo: bool,

    /// 回放已有的快照 JSON：不采集、不 attach 进程
    #[arg(long, value_name = "FILE", conflicts_with_all = ["demo", "pid"])]
    replay: Option<PathBuf>,

    /// 自动采样间隔（秒），0 表示只在按 r 时刷新
    #[arg(long, default_value_t = 0, value_name = "SECONDS")]
    interval: u64,

    /// 输出快照 JSON 后退出，不启动 TUI
    #[arg(long)]
    json: bool,

    /// 输出文本报告后退出，不启动 TUI
    #[arg(long, conflicts_with = "json")]
    report: bool,

    /// 报告与差值列表的条目数
    #[arg(long, default_value_t = 10, value_name = "N")]
    top: usize,

    /// 读取基线快照（JSON），用于计算差值
    #[arg(long, value_name = "FILE")]
    baseline: Option<PathBuf>,

    /// 把当前快照保存为 JSON
    #[arg(long, value_name = "FILE")]
    save_baseline: Option<PathBuf>,

    /// 面积与差值使用的度量
    #[arg(long, value_enum, value_name = "METRIC")]
    metric: Option<MetricArg>,

    /// 解析一个十六进制地址后退出（用于脚本与验证）
    #[arg(long, value_name = "ADDR", conflicts_with_all = ["json", "report"])]
    lookup: Option<String>,

    /// 读取内存并以十六进制输出后退出（ADDR[:LEN]，默认 256 字节）
    #[arg(
        long,
        value_name = "ADDR[:LEN]",
        conflicts_with_all = ["json", "report", "lookup", "stack"]
    )]
    read: Option<String>,

    /// 把当前 treemap 导出为 SVG 后退出
    #[arg(
        long,
        value_name = "FILE",
        conflicts_with_all = ["json", "report", "lookup", "read", "stack"]
    )]
    svg: Option<PathBuf>,

    /// 回溯主线程调用栈后退出（需要 ptrace 权限）
    #[arg(long, conflicts_with_all = ["json", "report", "lookup"])]
    stack: bool,

    /// 不读写 ~/.config/tmv/config.toml
    #[arg(long)]
    no_config: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum MetricArg {
    Virtual,
    Rss,
    Pss,
    Private,
    Shared,
    Swap,
}

impl From<MetricArg> for Metric {
    fn from(value: MetricArg) -> Self {
        match value {
            MetricArg::Virtual => Metric::Virtual,
            MetricArg::Rss => Metric::Rss,
            MetricArg::Pss => Metric::Pss,
            MetricArg::Private => Metric::Private,
            MetricArg::Shared => Metric::Shared,
            MetricArg::Swap => Metric::Swap,
        }
    }
}

fn main() -> Result<()> {
    restore_sigpipe();
    let args = Args::parse();
    let privileged = is_privileged();

    let snapshot = match collect_snapshot(&args) {
        Ok(snapshot) => snapshot,
        Err(err) => exit_with(&err, args.pid),
    };

    let baseline = load_baseline(args.baseline.as_deref());
    if let Some(path) = &args.save_baseline {
        write_baseline(path, &snapshot);
    }

    if let Some(text) = &args.lookup {
        lookup_address(&args, &snapshot, text);
    }

    if let Some(spec) = &args.read {
        print_memory(&args, &snapshot, spec);
    }

    if let Some(path) = &args.svg {
        write_svg(&args, &snapshot, path);
    }

    if args.stack {
        if args.replay.is_some() {
            eprintln!("Error: 回放模式不能回溯调用栈（没有可 attach 的进程）");
            std::process::exit(4);
        }
        print_stack(&args, &snapshot);
    }

    // 非交互模式：输出 JSON 或文本报告后直接退出。
    if args.json || args.report {
        let metric = resolve_metric(&args, &snapshot);
        if args.report {
            print!(
                "{}",
                export::report(&snapshot, metric, args.top, baseline.as_ref())
            );
            // 回放没有实时账务，跳过该段。
            if args.replay.is_none() {
                let accounting = if args.demo {
                    Accounting::demo(&snapshot)
                } else {
                    Accounting::collect(snapshot.pid)
                };
                print!("{}", accounting::report(&accounting, &snapshot));
            }
        } else {
            let diff = baseline
                .as_ref()
                .map(|before| DiffSummary::new(before, &snapshot, metric, args.top));
            let mut document = SnapshotDocument::new(snapshot);
            document.diff = diff;
            println!("{}", document.to_json());
        }
        return Ok(());
    }

    // 只影响 TUI 的偏好；--json / --report 等一次性输出不会读写配置。
    let config = if args.no_config {
        Config::default()
    } else {
        Config::load()
    };

    let mut terminal = match ratatui::try_init() {
        Ok(terminal) => terminal,
        Err(err) => {
            eprintln!("Error: 无法初始化终端（需要 TTY）: {err}");
            std::process::exit(4);
        }
    };
    let mut app = App::new(snapshot, args.pid, args.demo, privileged);
    app.apply_config(&config);
    if args.replay.is_some() {
        // 回放不 spawn 采集线程，pid 只用于界面显示。
        app.pid = Some(app.snapshot.pid);
        app.set_replay(true);
    }
    if let Some(metric) = args.metric {
        app.set_metric(metric.into());
    }
    if let Some(baseline) = baseline {
        app.set_baseline(baseline);
    }
    if args.interval > 0 {
        app.set_interval(Some(Duration::from_secs(args.interval)));
    }
    let result = run(&mut terminal, &mut app);
    ratatui::restore();
    if !args.no_config {
        // 退出时把界面偏好写回，下次启动沿用；写失败不影响退出码。
        if let Err(err) = app.to_config().save() {
            eprintln!("Warning: 无法保存配置: {err}");
        }
    }
    result
}

fn collect_snapshot(args: &Args) -> std::io::Result<ProcessSnapshot> {
    if args.demo {
        return Ok(demo::snapshot());
    }
    if let Some(path) = &args.replay {
        return load_snapshot(path);
    }
    let Some(pid) = args.pid else {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "请提供目标进程 PID，或使用 --demo 预览界面",
        ));
    };
    // 非 Linux 没有 /proc，提前给出平台提示，避免误报「进程不存在」。
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        Err(std::io::Error::new(
            ErrorKind::Unsupported,
            "tmv 读取进程信息依赖 Linux /proc；当前平台只能用 --demo 预览界面",
        ))
    }
    #[cfg(target_os = "linux")]
    ProcessSnapshot::collect(pid)
}

/// 读取 `--json` / `--save-baseline` 写出的快照文件。
fn load_snapshot(path: &Path) -> std::io::Result<ProcessSnapshot> {
    let text = std::fs::read_to_string(path)?;
    SnapshotDocument::from_json(&text)
        .map(|document| document.snapshot)
        .map_err(|err| std::io::Error::new(ErrorKind::InvalidData, err))
}

/// 采集失败的退出码：2 进程不存在、3 权限不足、4 其他采集错误。
fn exit_with(err: &std::io::Error, pid: Option<i32>) -> ! {
    match err.kind() {
        ErrorKind::InvalidInput => {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
        ErrorKind::NotFound => {
            eprintln!("Error: {err}");
            std::process::exit(2);
        }
        ErrorKind::PermissionDenied => {
            eprintln!(
                "Error: 无法读取 /proc/{}（需要同 uid 或 root 权限）: {err}",
                pid.unwrap_or_default()
            );
            std::process::exit(3);
        }
        // 平台不支持时不再叠加「采集失败」前缀，直接给出原始说明。
        ErrorKind::Unsupported => {
            eprintln!("Error: {err}");
            std::process::exit(4);
        }
        // 快照文件损坏或版本不兼容。
        ErrorKind::InvalidData => {
            eprintln!("Error: 快照文件无法使用: {err}");
            std::process::exit(4);
        }
        _ => {
            eprintln!("Error: 采集失败: {err}");
            std::process::exit(4);
        }
    }
}

/// 解析地址并打印模块 / 符号 / 源码行；不进入 TUI。
fn lookup_address(args: &Args, snapshot: &ProcessSnapshot, text: &str) {
    let trimmed = text
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    let address = match u64::from_str_radix(trimmed, 16) {
        Ok(address) => address,
        Err(_) => {
            eprintln!("Error: 地址无效: {text}");
            std::process::exit(1);
        }
    };

    let mut engine = if args.demo {
        SymbolEngine::for_demo(snapshot)
    } else {
        SymbolEngine::for_process(snapshot)
    };
    let Some(resolved) = engine.resolve(address) else {
        eprintln!("Error: 0x{address:x} 没有对应的模块或 JIT 符号");
        std::process::exit(2);
    };

    let symbol = resolved
        .symbol
        .clone()
        .unwrap_or_else(|| resolved.module.clone());
    println!(
        "0x{address:x}  {}  {symbol}+0x{:x}",
        resolved.module, resolved.offset
    );
    if let Some(location) = resolved.location() {
        println!("  {location}");
    }
    std::process::exit(0);
}

/// 读取一段内存并按 `xxd` 风格输出；不进入 TUI。
fn print_memory(args: &Args, snapshot: &ProcessSnapshot, spec: &str) {
    let (address, len) = match parse_read_spec(spec) {
        Ok(value) => value,
        Err(err) => {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
    };
    if args.replay.is_some() {
        eprintln!("Error: 回放模式没有内存内容（快照不含页数据）");
        std::process::exit(4);
    }

    let bytes = if args.demo {
        demo::memory(address, len)
    } else {
        match tmv_ptrace::read_memory(snapshot.pid, address, len) {
            Ok(bytes) => bytes,
            Err(err) => {
                let hint = tmv_ptrace::permission_hint(&err)
                    .map(|hint| format!("（{hint}）"))
                    .unwrap_or_default();
                eprintln!("Error: 读取 0x{address:x} 失败: {err}{hint}");
                let code = if err.kind() == ErrorKind::PermissionDenied {
                    3
                } else {
                    4
                };
                std::process::exit(code);
            }
        }
    };

    println!(
        "pid {} · 0x{address:x} · {} 字节",
        snapshot.pid,
        bytes.len()
    );
    for (index, chunk) in bytes.chunks(READ_LINE).enumerate() {
        let line = address + (index * READ_LINE) as u64;
        let mut hex = String::with_capacity(READ_LINE * 3);
        for offset in 0..READ_LINE {
            hex.push(' ');
            match chunk.get(offset) {
                Some(byte) => hex.push_str(&format!("{byte:02x}")),
                None => hex.push_str("  "),
            }
        }
        let ascii: String = (0..READ_LINE)
            .map(|offset| match chunk.get(offset) {
                Some(byte) if byte.is_ascii_graphic() || *byte == b' ' => *byte as char,
                Some(_) => '.',
                None => ' ',
            })
            .collect();
        println!("{line:016x}{hex}  |{ascii}|");
    }
    std::process::exit(0);
}

/// 解析 `ADDR[:LEN]`：地址按十六进制，长度支持十进制与 `0x` 前缀。
fn parse_read_spec(spec: &str) -> Result<(u64, usize), String> {
    let (address_text, len_text) = match spec.split_once(':') {
        Some((address, len)) => (address, Some(len)),
        None => (spec, None),
    };

    let digits = address_text
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    let address =
        u64::from_str_radix(digits, 16).map_err(|_| format!("地址无效: {address_text}"))?;

    let len = match len_text.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => parse_len(text)?,
        None => DEFAULT_READ_LEN,
    };
    if len == 0 {
        return Err("长度必须大于 0".to_string());
    }
    if len > MAX_READ_LEN {
        return Err(format!("长度过大，单次最多读取 {MAX_READ_LEN} 字节"));
    }
    Ok((address, len))
}

fn parse_len(text: &str) -> Result<usize, String> {
    let value = match text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse::<u64>(),
    }
    .map_err(|_| format!("长度无效: {text}"))?;
    usize::try_from(value).map_err(|_| format!("长度过大: {text}"))
}

/// 导出两层 treemap 为 SVG；分组与排序沿用界面默认值。
fn write_svg(args: &Args, snapshot: &ProcessSnapshot, path: &Path) {
    let metric = resolve_metric(args, snapshot);
    let tree = build_tree(
        snapshot,
        TreeOptions {
            metric,
            group_by: GroupBy::Category,
            sort: SortOrder::SizeDesc,
        },
    );
    let svg = tmv_tui::svg::render(
        &tree,
        &tmv_tui::svg::SvgOptions {
            title: format!("tmv · pid {} · 面积 = {}", snapshot.pid, metric.label()),
            ..Default::default()
        },
    );
    if let Err(err) = std::fs::write(path, svg) {
        eprintln!("Error: 无法写入 {}: {err}", path.display());
        std::process::exit(4);
    }
    println!(
        "已导出 {}（{} 个分组，度量 {}）",
        path.display(),
        tree.children.len(),
        metric.label()
    );
    std::process::exit(0);
}

/// 回溯主线程调用栈并逐帧解析符号；不进入 TUI。
fn print_stack(args: &Args, snapshot: &ProcessSnapshot) {
    let mut engine = if args.demo {
        SymbolEngine::for_demo(snapshot)
    } else {
        SymbolEngine::for_process(snapshot)
    };

    let stack = if args.demo {
        tmv_ptrace::demo::stack(snapshot, 0)
    } else {
        match tmv_ptrace::unwind_thread(
            snapshot.pid,
            snapshot.pid,
            &snapshot.geometry,
            Some(&mut engine),
            64,
        ) {
            Ok(stack) => stack,
            Err(err) => {
                let hint = tmv_ptrace::permission_hint(&err)
                    .map(|hint| format!("（{hint}）"))
                    .unwrap_or_default();
                eprintln!("Error: 栈回溯失败: {err}{hint}");
                let code = if err.kind() == ErrorKind::PermissionDenied {
                    3
                } else {
                    4
                };
                std::process::exit(code);
            }
        }
    };

    println!(
        "线程 {} ({}) · {} 帧 · {}",
        stack.name,
        stack.tid,
        stack.frames.len(),
        stack.method.label()
    );
    if let Some(tls) = stack.tls {
        println!("  TLS 0x{tls:x}");
    }
    for (index, frame) in stack.frames.iter().enumerate() {
        match engine.resolve(frame.pc) {
            Some(resolved) => {
                let symbol = resolved
                    .symbol
                    .clone()
                    .unwrap_or_else(|| resolved.module.clone());
                print!(
                    "#{index} 0x{:012x}  {}  {symbol}+0x{:x}",
                    frame.pc, resolved.module, resolved.offset
                );
                if let Some(location) = resolved.location() {
                    print!("  {location}");
                }
                println!();
            }
            None => println!("#{index} 0x{:012x}  ?", frame.pc),
        }
    }
    std::process::exit(0);
}

fn resolve_metric(args: &Args, snapshot: &ProcessSnapshot) -> Metric {
    match args.metric {
        Some(metric) => {
            let metric: Metric = metric.into();
            if !snapshot.has_smaps && metric != Metric::Virtual {
                Metric::Virtual
            } else {
                metric
            }
        }
        None if snapshot.has_smaps => Metric::Pss,
        None => Metric::Virtual,
    }
}

fn load_baseline(path: Option<&Path>) -> Option<ProcessSnapshot> {
    let path = path?;
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) => {
            eprintln!("Error: 无法读取基线文件 {}: {err}", path.display());
            std::process::exit(4);
        }
    };
    match SnapshotDocument::from_json(&text) {
        Ok(document) => Some(document.snapshot),
        Err(err) => {
            eprintln!("Error: 基线文件 {} 无效: {err}", path.display());
            std::process::exit(4);
        }
    }
}

fn write_baseline(path: &Path, snapshot: &ProcessSnapshot) {
    let document = SnapshotDocument::new(snapshot.clone());
    if let Err(err) = std::fs::write(path, document.to_json()) {
        eprintln!("Error: 无法写入基线文件 {}: {err}", path.display());
        std::process::exit(4);
    }
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        if event::poll(app.poll_timeout())? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    app.handle_key(key);
                }
            }
        }

        app.tick();
        if app.should_quit {
            break;
        }
    }
    Ok(())
}

/// 下游关闭管道（如 `tmv ... | head`）时按 Unix 惯例静默退出，避免 `println!` panic。
fn restore_sigpipe() {
    #[cfg(unix)]
    // SAFETY: 只把 SIGPIPE 恢复成默认处置，不涉及内存。
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

fn is_privileged() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid 只读取进程属性。
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_spec_defaults_to_window() {
        assert_eq!(
            parse_read_spec("0x7f0000").unwrap(),
            (0x7f0000, DEFAULT_READ_LEN)
        );
        // 冒号后为空时也用默认长度。
        assert_eq!(
            parse_read_spec("0x7f0000:").unwrap(),
            (0x7f0000, DEFAULT_READ_LEN)
        );
    }

    #[test]
    fn read_spec_accepts_decimal_and_hex_length() {
        assert_eq!(parse_read_spec("7fff0000:64").unwrap(), (0x7fff_0000, 64));
        assert_eq!(parse_read_spec("0x1000:0x20").unwrap(), (0x1000, 32));
    }

    #[test]
    fn read_spec_rejects_invalid_input() {
        assert!(parse_read_spec("zz").is_err());
        assert!(parse_read_spec("1000:0").is_err());
        assert!(parse_read_spec("1000:abc").is_err());
        assert!(
            parse_read_spec(&format!("1000:{}", MAX_READ_LEN + 1)).is_err(),
            "超过上限应报错"
        );
        assert_eq!(
            parse_read_spec(&format!("1000:{}", MAX_READ_LEN))
                .unwrap()
                .1,
            MAX_READ_LEN
        );
    }
}
