use anyhow::{bail, Context, Result};
use object::Object;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    path::PathBuf,
    thread,
    time::Duration,
};
use tmv_core::{heap::Ledger, MemNode, ProcessSnapshot};
use tmv_symbols::SymbolEngine;

#[derive(Debug, clap::Args)]
pub struct HeapArgs {
    /// 读取 glibc 分配事件，统计仍存活的申请字节
    #[arg(long, value_name="FILE", conflicts_with_all=["demo","replay","baseline","save_baseline","metric","lookup","read","stack"])]
    pub heap_trace: Option<PathBuf>,
    /// 录制的 /proc/PID/maps 文件，默认将事件文件扩展名替换为 .maps
    #[arg(long, requires = "heap_trace")]
    heap_maps: Option<PathBuf>,
    /// 基于 Build ID 和调用函数的分配标定规则（JSON 数组）
    #[arg(long, requires = "heap_trace")]
    heap_rules: Option<PathBuf>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Rule {
    pub label: String,
    pub build_id: String,
    pub function: String,
    /// Return PC offset from the function start (not the call instruction).
    pub return_offset: Option<u64>,
}
#[derive(Serialize)]
struct Row {
    label: String,
    site: String,
    build_id: Option<String>,
    return_offset: Option<u64>,
    caller: u64,
    location: Option<String>,
    live_count: u64,
    live_bytes: u64,
    allocated_bytes: u64,
    freed_bytes: u64,
    peak_bytes: u64,
}
#[derive(Serialize)]
struct Report {
    format: &'static str,
    pid: u64,
    status: &'static str,
    scope: &'static str,
    records: u64,
    unknown_frees: u64,
    live_bytes: u64,
    rows: Vec<Row>,
}
pub(crate) fn build_id(path: &str) -> Option<String> {
    let data = fs::read(path).ok()?;
    let obj = object::File::parse(&*data).ok()?;
    Some(
        obj.build_id()
            .ok()??
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect(),
    )
}
pub fn run(args: &super::Args) -> Result<()> {
    let trace = args.heap.heap_trace.as_ref().unwrap();
    let mut file = File::open(trace).with_context(|| format!("open {}", trace.display()))?;
    let rules: Vec<Rule> = match &args.heap.heap_rules {
        Some(p) => serde_json::from_slice(&fs::read(p)?)?,
        None => Vec::new(),
    };
    for rule in &rules {
        if rule.label.is_empty() || rule.build_id.is_empty() || rule.function.is_empty() {
            bail!("heap rules require nonempty label, build_id and function");
        }
    }
    let mut ledger = Ledger::default();
    loop {
        ledger.read(&mut file)?;
        if ledger.records == 0 {
            bail!("trace has no complete START record; retry after collector initialization");
        }
        if let Some(pid) = args.pid {
            if i64::from(pid) != ledger.pid as i64 {
                bail!("PID does not match trace");
            }
        }
        let maps = args
            .heap
            .heap_maps
            .clone()
            .unwrap_or_else(|| trace.with_extension("maps"));
        let snapshot = if let Some(pid) = args.pid {
            ProcessSnapshot::collect(pid).or_else(|_| {
                fs::read_to_string(&maps)
                    .map(|s| ProcessSnapshot::from_vmas(pid, tmv_core::proc::parse_maps(&s), false))
            })?
        } else {
            let text = fs::read_to_string(&maps).with_context(|| {
                format!("read {}; supply --heap-maps or a live PID", maps.display())
            })?;
            ProcessSnapshot::from_vmas(ledger.pid as i32, tmv_core::proc::parse_maps(&text), false)
        };
        // Only symbolize files whose build ID matches the loaded image capture.
        let captured: BTreeMap<String, String> = fs::read_to_string(trace.with_extension("ids"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let (id, path) = line.split_once(' ')?;
                let path = fs::canonicalize(path).ok()?.to_string_lossy().into_owned();
                Some((path, id.to_string()))
            })
            .collect();
        let mut verified = snapshot.clone();
        let mut verified_ids = BTreeMap::new();
        verified.vmas.retain(|v| {
            let path = fs::canonicalize(&v.pathname)
                .ok()
                .map(|p| p.to_string_lossy().into_owned());
            let Some(path) = path else {
                return false;
            };
            let id = verified_ids
                .entry(path.clone())
                .or_insert_with(|| build_id(&path));
            id.is_some() && id.as_ref() == captured.get(&path)
        });
        let mut engine = SymbolEngine::for_process(&verified);
        let mut ids = BTreeMap::new();
        let mut rows = Vec::new();
        for site in ledger.sites.values() {
            let resolved = engine.resolve(site.caller);
            let path = verified
                .vmas
                .iter()
                .find(|v| site.caller >= v.start && site.caller < v.end)
                .map(|v| v.pathname.as_str());
            let id = path.and_then(|p| {
                ids.entry(p.to_string())
                    .or_insert_with(|| build_id(p))
                    .clone()
            });
            let symbol = resolved.as_ref().and_then(|r| r.symbol.as_deref());
            let offset = resolved
                .as_ref()
                .filter(|r| r.symbol.is_some())
                .map(|r| r.offset);
            let name = resolved
                .as_ref()
                .map(|r| {
                    format!(
                        "{}!{}+0x{:x}",
                        r.module,
                        r.symbol.as_deref().unwrap_or("?"),
                        if r.symbol.is_some() {
                            r.offset
                        } else {
                            site.caller
                        }
                    )
                })
                .unwrap_or_else(|| format!("unresolved@0x{:x}", site.caller));
            let matched: Vec<_> = rules
                .iter()
                .filter(|r| {
                    id.as_deref() == Some(r.build_id.as_str())
                        && symbol.is_some_and(|s| tmv_symbols::function_matches(s, &r.function))
                        && r.return_offset.is_none_or(|o| Some(o) == offset)
                })
                .collect();
            if matched.len() > 1 {
                bail!("conflicting heap rules at {name}");
            }
            rows.push(Row {
                label: matched
                    .first()
                    .map(|r| r.label.clone())
                    .unwrap_or_else(|| name.clone()),
                site: name,
                build_id: id,
                return_offset: offset,
                caller: site.caller,
                location: engine
                    .resolve(site.caller.saturating_sub(1))
                    .and_then(|r| r.location()),
                live_count: site.live_count,
                live_bytes: site.live_bytes,
                allocated_bytes: site.allocated_bytes,
                freed_bytes: site.freed_bytes,
                peak_bytes: site.peak_bytes,
            });
        }
        rows.sort_by(|a, b| {
            b.live_bytes
                .cmp(&a.live_bytes)
                .then(a.caller.cmp(&b.caller))
        });
        let report=Report {
            format:"tmv.heap.v1",pid:ledger.pid,
            status:if ledger.ended {"ended-at-collector-shutdown"}else if ledger.partial(){"partial-record/incomplete"}else{"open-or-incomplete"},
            scope:"observed glibc allocations since preload initialization; requested bytes, NOT RSS; no child-fork coverage",
            records:ledger.records,unknown_frees:ledger.unknown_frees,
            live_bytes:rows.iter().map(|r|r.live_bytes).sum(),rows,
        };
        if let Some(path) = &args.svg {
            let root = tree(&report);
            let options = tmv_tui::svg::SvgOptions {
                title: format!("pid {} · 存活申请字节（不是 RSS）", report.pid),
                subtitle: format!(
                    "{} bytes · {} · unknown frees {}",
                    report.live_bytes, report.status, report.unknown_frees
                ),
                format: args.svg_format.into(),
                ..Default::default()
            };
            fs::write(path, tmv_tui::svg::render(&root, &options))?;
        } else if args.json {
            println!("{}", serde_json::to_string(&report)?);
        } else {
            print_report(&report, args.top);
        }
        if args.interval == 0 || ledger.ended {
            break;
        }
        thread::sleep(Duration::from_secs(args.interval));
    }
    Ok(())
}
fn tree(report: &Report) -> MemNode {
    let mut groups: BTreeMap<String, MemNode> = BTreeMap::new();
    for row in &report.rows {
        if row.live_bytes == 0 {
            continue;
        }
        let group = groups.entry(row.label.clone()).or_insert_with(|| MemNode {
            label: row.label.clone(),
            ..MemNode::empty()
        });
        group.bytes += row.live_bytes;
        group.children.push(MemNode {
            label: format!("{} · {} allocations", row.site, row.live_count),
            bytes: row.live_bytes,
            ..MemNode::empty()
        });
    }
    let mut children: Vec<_> = groups.into_values().collect();
    children.sort_by_key(|a| std::cmp::Reverse(a.bytes));
    MemNode {
        label: "已追踪存活申请字节（非 RSS）".into(),
        bytes: report.live_bytes,
        children,
        ..MemNode::empty()
    }
}
fn print_report(report: &Report, top: usize) {
    println!(
        "\nHeap pid={} · {} · records={} · unknown frees={}",
        report.pid, report.status, report.records, report.unknown_frees
    );
    println!(
        "存活申请 {} bytes（不是 RSS；占比以已追踪存活字节为分母）",
        report.live_bytes
    );
    let mut groups: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for r in &report.rows {
        let g = groups.entry(&r.label).or_default();
        g.0 += r.live_bytes;
        g.1 += r.live_count;
    }
    let mut groups: Vec<_> = groups.into_iter().collect();
    groups.sort_by_key(|a| std::cmp::Reverse(a.1 .0));
    println!(
        "{:>14} {:>9} {:>8}  分类 / 分配位置",
        "bytes", "count", "share"
    );
    for (name, (bytes, count)) in groups.into_iter().take(top) {
        let pct = if report.live_bytes == 0 {
            0.0
        } else {
            bytes as f64 * 100.0 / report.live_bytes as f64
        };
        println!("{bytes:>14} {count:>9} {pct:>7.2}%  {name}");
    }
    println!("范围：{}", report.scope);
    if !report.status.starts_with("ended") {
        println!("尚无结束标记：可能仍在采集，也可能异常退出/写入失败。仅表示已读取事件。");
    }
}
