use anyhow::{bail, Context, Result};
use clap::{Subcommand, ValueEnum};
use std::{
    collections::HashMap,
    fs::{self, File},
    io::{IsTerminal, Read, Seek},
    path::{Path, PathBuf},
    process::{Command as Process, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tmv_core::{
    heap::Ledger,
    session::{self, Frame, Manifest},
    ProcessSnapshot,
};
use tmv_symbols::SymbolEngine;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Analysis {
    Layout,
    Allocation,
}
#[derive(Debug, Subcommand)]
pub enum Command {
    /// 启动目标并采集布局/分配；退出查看界面不停止目标或采集器
    Run {
        /// 新建会话目录（必须不存在；默认 tmv-时间戳.tmvr）
        #[arg(long)]
        output: Option<PathBuf>,
        /// 采集库路径；完整安装包自动发现 libtmvtrace.so
        #[arg(long)]
        library: Option<PathBuf>,
        /// 业务名称标定 JSON：build_id、function、可选 return_offset
        #[arg(long)]
        heap_rules: Option<PathBuf>,
        /// 采样间隔秒数（1～3600）；采样状态并非逐事件重放
        #[arg(long,default_value_t=1,value_parser=clap::value_parser!(u64).range(1..=3600))]
        interval: u64,
        /// 不打开 TUI；目标保留当前标准输入输出
        #[arg(long)]
        no_ui: bool,
        #[arg(long, hide = true)]
        collector: bool,
        /// -- 后为目标程序及其参数（同架构动态链接 Linux/glibc 程序）
        #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
        program: Vec<String>,
    },
    /// 查看已有进程布局，不注入采集器，也不补录历史分配
    Attach {
        /// 目标进程 PID
        pid: i32,
    },
    /// 打开会话；Tab 切换布局/分配，? 查看按键，已结束时支持时间轴回放
    Open {
        /// .tmvr 会话目录
        path: PathBuf,
        /// 距记录开始的秒数，选取该时刻或此前最近的采样；仅适用于已结束的会话
        #[arg(long)]
        at: Option<f64>,
    },
    /// 按采样时刻输出离线统计、分配栈、JSON 或 Treemap SVG
    Report {
        /// .tmvr 会话目录
        path: PathBuf,
        /// 距记录开始的秒数，选取该时刻或此前最近的采样；省略时读取最新采样
        #[arg(long)]
        at: Option<f64>,
        /// allocation 为存活申请字节，layout 为保存的进程布局
        #[arg(long, value_enum, default_value = "allocation")]
        analysis: Analysis,
        /// 排名/栈报告最多显示的分类数量
        #[arg(long, default_value_t = 20)]
        top: usize,
        /// 输出采样帧 JSON（包含布局及分配记录）
        #[arg(long)]
        json: bool,
        /// 导出所选分析的 Treemap SVG
        #[arg(long)]
        svg: Option<PathBuf>,
        /// 文本报告追加分配栈；JSON 已含栈，不能补出旧记录缺失的栈
        #[arg(long)]
        stacks: bool,
    },
}
pub fn dispatch(command: &Command) -> Result<()> {
    match command {
        Command::Attach { pid } => {
            let status = Process::new(std::env::current_exe()?)
                .arg(pid.to_string())
                .status()?;
            if !status.success() {
                bail!("tmv attach exited with {status}");
            }
            Ok(())
        }
        Command::Open { path, at } => {
            let at = at.map(seconds_ms).transpose()?;
            tmv_tui::session::run_at(path, at).map_err(Into::into)
        }
        Command::Report {
            path,
            at,
            analysis,
            top,
            json,
            svg,
            stacks,
        } => {
            report(path, *at, *analysis, *top, *json, svg.as_deref())?;
            if *stacks && !json {
                let m = session::manifest(path)?;
                let index = at
                    .map(seconds_ms)
                    .transpose()?
                    .map(|t| session::at(path, &m, t))
                    .transpose()?
                    .unwrap_or(m.frames.saturating_sub(1));
                let f = tmv_symbols::replay::frame(path, index, m.pid)?;
                for node in session::allocation_tree(&f).children.iter().take(*top) {
                    println!("{}", session::stack_lines(&f, &node.label, None).join("\n"));
                }
            }
            Ok(())
        }
        Command::Run {
            output,
            library,
            heap_rules,
            interval,
            no_ui,
            collector,
            program,
        } => {
            if !cfg!(target_os = "linux") {
                bail!("tmv run requires Linux/glibc; tmv open/report are available offline");
            }
            let library = library
                .clone()
                .or_else(|| std::env::var_os("TMV_TRACE_LIB").map(PathBuf::from))
                .unwrap_or_else(|| {
                    let exe = std::env::current_exe().unwrap_or_default();
                    let sibling = exe.with_file_name("libtmvtrace.so");
                    if sibling.exists() {
                        sibling
                    } else if Path::new("/usr/lib64/tmv/libtmvtrace.so").is_file() {
                        PathBuf::from("/usr/lib64/tmv/libtmvtrace.so")
                    } else {
                        exe.parent()
                            .and_then(Path::parent)
                            .and_then(Path::parent)
                            .unwrap_or(Path::new("."))
                            .join("tools/heaptrace/libtmvtrace.so")
                    }
                });
            let library = fs::canonicalize(&library).with_context(|| {
                format!(
                    "collector not found: {}; build tools/heaptrace or use --library",
                    library.display()
                )
            })?;
            let dir = output
                .clone()
                .unwrap_or_else(|| PathBuf::from(format!("tmv-{}.tmvr", now_ms())));
            if !collector {
                if !no_ui && (!std::io::stdout().is_terminal() || !std::io::stdin().is_terminal()) {
                    bail!("interactive run requires a terminal; use --no-ui");
                }
                fs::create_dir(&dir).with_context(|| {
                    format!("create new session {} (must not exist)", dir.display())
                })?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
                }
                fs::create_dir(dir.join("frames"))?;
                if let Some(rules) = heap_rules {
                    let bytes = fs::read(rules)?;
                    let _: Vec<crate::heap::Rule> = serde_json::from_slice(&bytes)?;
                    fs::write(dir.join("rules.json"), bytes)?;
                }
            }
            if *no_ui || *collector {
                return record(&dir, &library, *interval, program, *collector);
            }
            let dir = fs::canonicalize(dir)?;
            let log = File::create(dir.join("collector.log"))?;
            let mut collector_cmd = Process::new(std::env::current_exe()?);
            collector_cmd
                .arg("run")
                .arg("--collector")
                .arg("--no-ui")
                .arg("--output")
                .arg(&dir)
                .arg("--library")
                .arg(library)
                .arg("--interval")
                .arg(interval.to_string())
                .arg("--")
                .args(program)
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log);
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                collector_cmd.process_group(0);
            }
            let mut child = collector_cmd.spawn()?;
            let start = Instant::now();
            loop {
                if session::manifest(&dir).is_ok_and(|m| m.frames > 0) {
                    break;
                }
                if let Some(status) = child.try_wait()? {
                    bail!(
                        "collector exited {status}; see {}/collector.log",
                        dir.display()
                    );
                }
                if start.elapsed() > Duration::from_secs(30) {
                    bail!(
                        "collector is still starting; inspect {}/collector.log",
                        dir.display()
                    );
                }
                thread::sleep(Duration::from_millis(100));
            }
            view(&dir)?;
            println!(
                "Session: {}. Viewer closed; recording/target continue until target exits.",
                dir.display()
            );
            Ok(())
        }
    }
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn record(
    dir: &Path,
    library: &Path,
    interval: u64,
    program: &[String],
    detached: bool,
) -> Result<()> {
    let dir = fs::canonicalize(dir)?;
    let rules: Vec<crate::heap::Rule> = if dir.join("rules.json").exists() {
        serde_json::from_slice(&fs::read(dir.join("rules.json"))?)?
    } else {
        Vec::new()
    };
    let mut module_ids = HashMap::new();
    let mut cmd = Process::new(&program[0]);
    cmd.args(&program[1..]);
    let mut preload = library.as_os_str().to_os_string();
    if let Some(existing) = std::env::var_os("LD_PRELOAD") {
        preload.push(":");
        preload.push(existing);
    }
    cmd.env("LD_PRELOAD", preload)
        .env("TMV_TRACE", dir.join("heap"));
    if detached {
        let log = File::create(dir.join("program.log"))?;
        cmd.stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log);
    }
    let mut child = cmd.spawn().context("start target")?;
    let pid = child.id();
    let mut m = Manifest {
        format: "tmv.session".into(),
        version: 1,
        pid,
        command: program.to_vec(),
        state: "recording".into(),
        frames: 0,
        interval_ms: interval * 1000,
        started_unix_ms: now_ms(),
        error: None,
    };
    session::write_json(&dir.join("manifest.json"), &m)?;
    eprintln!("tmv: PID {pid}; session {}", dir.display());
    let start = Instant::now();
    let mut ledger = Ledger::default();
    let mut source = None;
    let mut labels = HashMap::new();
    let mut stack_symbols = HashMap::new();
    let result = (|| -> Result<()> {
        loop {
            let exited = child.try_wait()?;
            if source.is_none() {
                source = File::open(dir.join(format!("heap.{pid}.bin"))).ok();
            }
            if let Some(file) = source.as_mut() {
                let available = file
                    .metadata()?
                    .len()
                    .saturating_sub(file.stream_position()?);
                ledger.read(&mut file.take(available))?;
            }
            let snapshot = ProcessSnapshot::collect(pid as i32);
            if let Ok(snapshot) = &snapshot {
                let mut symbols = SymbolEngine::for_process(snapshot);
                for (_, a) in ledger.allocations() {
                    for pc in a.stack.iter().chain(a.resize_stack.iter()) {
                        stack_symbols.entry(*pc).or_insert_with(|| {
                            let resolved = symbols.resolve(pc.saturating_sub(1));
                            let location = resolved.as_ref().and_then(|r| r.location());
                            session::StackFrame {
                                address: *pc,
                                symbol: resolved
                                    .map(|r| {
                                        format!(
                                            "{}!{}+0x{:x}",
                                            r.module,
                                            r.symbol.unwrap_or_else(|| "?".into()),
                                            r.offset
                                        )
                                    })
                                    .unwrap_or_else(|| format!("0x{pc:x}")),
                                location,
                            }
                        });
                    }
                    if labels.contains_key(&a.caller) {
                        continue;
                    }
                    let resolved = symbols.resolve(a.caller);
                    let path = snapshot
                        .vmas
                        .iter()
                        .find(|v| a.caller >= v.start && a.caller < v.end)
                        .map(|v| v.pathname.clone());
                    let id = path.and_then(|p| {
                        module_ids
                            .entry(p.clone())
                            .or_insert_with(|| crate::heap::build_id(&p))
                            .clone()
                    });
                    let matched = rules
                        .iter()
                        .filter(|r| {
                            resolved.as_ref().is_some_and(|symbol| {
                                symbol
                                    .symbol
                                    .as_deref()
                                    .is_some_and(|s| tmv_symbols::function_matches(s, &r.function))
                                    && r.return_offset.is_none_or(|offset| offset == symbol.offset)
                            }) && id.as_deref() == Some(r.build_id.as_str())
                        })
                        .collect::<Vec<_>>();
                    if matched.len() > 1 {
                        bail!("conflicting heap rules for caller 0x{:x}", a.caller);
                    }
                    let label = matched.first().map(|r| r.label.clone()).unwrap_or_else(|| {
                        resolved
                            .map(|r| {
                                format!(
                                    "{}!{}+0x{:x}",
                                    r.module,
                                    r.symbol.unwrap_or_else(|| "?".into()),
                                    r.offset
                                )
                            })
                            .unwrap_or_else(|| format!("0x{:x}", a.caller))
                    });
                    labels.insert(a.caller, label);
                }
            }
            let frame = Frame {
                elapsed_ms: start.elapsed().as_millis() as u64,
                layout_error: snapshot.as_ref().err().map(ToString::to_string),
                snapshot: snapshot.ok(),
                allocations: ledger
                    .allocations()
                    .map(|(address, a)| session::Allocation {
                        address: *address,
                        size: a.size,
                        caller: a.caller,
                        stack: a
                            .stack
                            .iter()
                            .map(|pc| {
                                stack_symbols.get(pc).cloned().unwrap_or_else(|| {
                                    session::StackFrame {
                                        address: *pc,
                                        symbol: format!("0x{pc:x}"),
                                        location: None,
                                    }
                                })
                            })
                            .collect(),
                        resize_stack: a
                            .resize_stack
                            .iter()
                            .map(|pc| {
                                stack_symbols.get(pc).cloned().unwrap_or_else(|| {
                                    session::StackFrame {
                                        address: *pc,
                                        symbol: format!("0x{pc:x}"),
                                        location: None,
                                    }
                                })
                            })
                            .collect(),
                        label: labels
                            .get(&a.caller)
                            .cloned()
                            .unwrap_or_else(|| format!("0x{:x}", a.caller)),
                    })
                    .collect(),
                heap_records: ledger.records,
                unknown_frees: ledger.unknown_frees,
                heap_ended: ledger.ended,
            };
            session::write_json(
                &dir.join("frames").join(format!("{:08}.json", m.frames)),
                &frame,
            )?;
            m.frames += 1;
            if let Some(status) = exited {
                m.state = if ledger.ended {
                    "complete"
                } else {
                    "incomplete"
                }
                .into();
                if !status.success() {
                    m.error = Some(format!("target exited {status}"));
                }
                if ledger.records == 0 {
                    m.error=Some("no allocator events: preload unsupported, initialization failed or target exited too early".into());
                }
            }
            session::write_json(&dir.join("manifest.json"), &m)?;
            if exited.is_some() {
                break;
            }
            thread::sleep(Duration::from_secs(interval));
        }
        Ok(())
    })();
    if let Err(e) = &result {
        m.state = "error".into();
        m.error = Some(e.to_string());
        let _ = session::write_json(&dir.join("manifest.json"), &m);
    }
    result
}
fn seconds_ms(seconds: f64) -> Result<u64> {
    if !seconds.is_finite() || seconds < 0.0 {
        bail!("--at must be finite nonnegative seconds");
    }
    Ok((seconds * 1000.) as u64)
}
fn report(
    path: &Path,
    at: Option<f64>,
    analysis: Analysis,
    top: usize,
    json: bool,
    svg: Option<&Path>,
) -> Result<()> {
    let m = session::manifest(path)?;
    let index = if let Some(seconds) = at {
        if !seconds.is_finite() || seconds < 0.0 {
            bail!("--at must be finite nonnegative seconds");
        }
        session::at(path, &m, (seconds * 1000.0) as u64)?
    } else {
        m.frames.checked_sub(1).context("no samples")?
    };
    let frame = tmv_symbols::replay::frame(path, index, m.pid)?;
    if json {
        println!("{}", serde_json::to_string(&frame)?);
        return Ok(());
    }
    let root = match analysis {
        Analysis::Allocation => session::allocation_tree(&frame),
        Analysis::Layout => tmv_core::build_tree(
            frame
                .snapshot
                .as_ref()
                .context("layout unavailable at this sample; select an earlier --at")?,
            tmv_core::TreeOptions {
                metric: tmv_core::Metric::Rss,
                group_by: tmv_core::GroupBy::Category,
                sort: tmv_core::SortOrder::SizeDesc,
            },
        ),
    };
    if let Some(svg) = svg {
        fs::write(
            svg,
            tmv_tui::svg::render(
                &root,
                &tmv_tui::svg::SvgOptions {
                    title: root.label.clone(),
                    subtitle: format!(
                        "sample {:.3}s; {:?}; {}",
                        frame.elapsed_ms as f64 / 1000.,
                        analysis,
                        m.state
                    ),
                    ..Default::default()
                },
            ),
        )?;
    } else {
        println!(
            "sample {:.3}s · {:?} · {} · unknown frees {}",
            frame.elapsed_ms as f64 / 1000.,
            analysis,
            m.state,
            frame.unknown_frees
        );
        if matches!(analysis, Analysis::Allocation) {
            println!(
                "Observed live requested bytes, NOT RSS; records={}",
                frame.heap_records
            );
        }
        for n in root.children.iter().take(top) {
            println!(
                "{:>12} {:>6.2}%  {}",
                n.bytes,
                if root.bytes == 0 {
                    0.
                } else {
                    n.bytes as f64 * 100. / root.bytes as f64
                },
                n.label
            );
        }
    }
    Ok(())
}
fn view(path: &Path) -> Result<()> {
    tmv_tui::session::run(path).map_err(Into::into)
}
