//! 内存账务：smaps_rollup 对账、KSM、cgroup v2、NUMA 分布、THP 与 page_owner。
//!
//! 这些信息都不影响 treemap 布局，只用于解释「统计值从哪里来、和内核账本是否一致」，
//! 因此每一项都是尽力采集，失败时保持 `None`。

use std::fs;
use std::path::Path;

use crate::export::human;
use crate::proc::{self, ProcessSnapshot};
use crate::vma::SmapsStats;

/// 进程内存账务的补充信息。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Accounting {
    pub rollup: Option<SmapsStats>,
    pub ksm: Option<KsmInfo>,
    pub cgroup: Option<CgroupInfo>,
    pub numa: Option<NumaInfo>,
    pub thp: Option<ThpInfo>,
    /// `/sys/kernel/debug/page_owner` 是否存在（通常需要 root 且挂载 debugfs）。
    pub page_owner: bool,
}

/// 全局 KSM 状态与当前进程的 KSM 计数。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KsmInfo {
    pub run: bool,
    pub pages_shared: u64,
    pub pages_sharing: u64,
    pub pages_unshared: u64,
    pub full_scans: u64,
    /// `/proc/<pid>/ksm_stat` 的原始键值，不同内核字段有差异。
    pub process: Vec<(String, u64)>,
}

/// cgroup 内存账本，v1 / v2 自动识别。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CgroupInfo {
    /// 1 或 2，对应 cgroup v1 / v2。
    pub version: u8,
    pub path: String,
    pub current: u64,
    /// `memory.peak`（部分内核提供）。
    pub peak: Option<u64>,
    /// `memory.max`，`max` 表示不限。
    pub max: Option<u64>,
    pub swap_current: Option<u64>,
    pub stat: Vec<(String, u64)>,
}

/// numa_maps 的节点分布，页数为单位。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NumaInfo {
    pub nodes: Vec<(u32, u64)>,
    pub anon_huge_pages: u64,
}

/// 全局透明大页策略。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ThpInfo {
    pub enabled: String,
}

impl Accounting {
    pub fn collect(pid: i32) -> Self {
        Self {
            rollup: read_text(format!("/proc/{pid}/smaps_rollup"))
                .map(|text| proc::parse_rollup(&text)),
            ksm: collect_ksm(pid),
            cgroup: collect_cgroup(pid),
            numa: read_text(format!("/proc/{pid}/numa_maps")).map(|text| parse_numa_maps(&text)),
            thp: read_text("/sys/kernel/mm/transparent_hugepage/enabled").map(|text| ThpInfo {
                enabled: parse_thp_enabled(&text),
            }),
            page_owner: Path::new("/sys/kernel/debug/page_owner").exists(),
        }
    }

    /// 供 `--demo` 使用的合成账务信息，rollup 与 VMA 汇总保持一致。
    pub fn demo(snapshot: &ProcessSnapshot) -> Self {
        let process = vec![
            ("ksm_rmap_items".to_string(), 128),
            ("ksm_zero_pages".to_string(), 0),
        ];
        let stat = vec![
            ("anon".to_string(), 420 * 1024 * 1024),
            ("file".to_string(), 96 * 1024 * 1024),
            ("kernel".to_string(), 18 * 1024 * 1024),
            ("slab".to_string(), 12 * 1024 * 1024),
        ];
        Self {
            rollup: Some(vma_totals(snapshot)),
            ksm: Some(KsmInfo {
                run: true,
                pages_shared: 320,
                pages_sharing: 2_048,
                pages_unshared: 64,
                full_scans: 17,
                process,
            }),
            cgroup: Some(CgroupInfo {
                version: 2,
                path: "/user.slice/demo.scope".to_string(),
                current: 540 * 1024 * 1024,
                peak: Some(700 * 1024 * 1024),
                max: Some(1024 * 1024 * 1024),
                swap_current: Some(4 * 1024 * 1024),
                stat,
            }),
            numa: Some(NumaInfo {
                nodes: vec![(0, 12_000), (1, 64)],
                anon_huge_pages: 32,
            }),
            thp: Some(ThpInfo {
                enabled: "madvise".to_string(),
            }),
            page_owner: false,
        }
    }
}

/// 生成账务面板的正文行，TUI 与文本报告共用。
pub fn lines(accounting: &Accounting, snapshot: &ProcessSnapshot) -> Vec<String> {
    let page_size = snapshot.geometry.page_size.max(1);
    let mut out = Vec::new();

    let totals = vma_totals(snapshot);
    match &accounting.rollup {
        Some(rollup) => {
            out.push(format!(
                "smaps_rollup  RSS {} · PSS {} · Swap {}",
                human(rollup.rss),
                human(rollup.pss),
                human(rollup.swap)
            ));
            out.push(format!(
                "VMA 汇总      RSS {} · PSS {} · Swap {}",
                human(totals.rss),
                human(totals.pss),
                human(totals.swap)
            ));
            out.push(format!(
                "差异          RSS {} · PSS {} · Swap {}",
                signed_diff(rollup.rss, totals.rss),
                signed_diff(rollup.pss, totals.pss),
                signed_diff(rollup.swap, totals.swap)
            ));
        }
        None => out.push("smaps_rollup  不可用（需要同 uid 或 root）".to_string()),
    }

    let eligible = snapshot
        .vmas
        .iter()
        .filter(|vma| vma.stats.as_ref().is_some_and(|s| s.thp_eligible))
        .count();
    let mode = accounting
        .thp
        .as_ref()
        .map(|thp| thp.enabled.as_str())
        .unwrap_or("未知");
    out.push(format!(
        "THP           全局 {} · AnonHuge {} · FilePmd {} · ShmemPmd {} · 可合并 {} 个 VMA",
        mode,
        human(totals.anon_huge),
        human(totals.file_pmd_mapped),
        human(totals.shmem_pmd_mapped),
        eligible
    ));

    match &accounting.ksm {
        Some(ksm) => {
            let merged = ksm.pages_sharing * page_size;
            out.push(format!(
                "KSM           {} · 合并页 {}（≈{}） · 未合并 {} · 扫描 {} 轮",
                if ksm.run { "开启" } else { "关闭" },
                ksm.pages_sharing,
                human(merged),
                ksm.pages_unshared,
                ksm.full_scans
            ));
            if !ksm.process.is_empty() {
                let detail: Vec<String> = ksm
                    .process
                    .iter()
                    .take(4)
                    .map(|(key, value)| format!("{key}={value}"))
                    .collect();
                out.push(format!("KSM(进程)     {}", detail.join(" · ")));
            }
        }
        None => out.push("KSM           未启用（缺少 /sys/kernel/mm/ksm/run）".to_string()),
    }

    match &accounting.cgroup {
        Some(cgroup) => {
            let limit = match cgroup.max {
                Some(max) => format!("上限 {}", human(max)),
                None => "上限 不限".to_string(),
            };
            let peak = cgroup
                .peak
                .map(|peak| format!(" · 峰值 {}", human(peak)))
                .unwrap_or_default();
            out.push(format!(
                "cgroup v{}     {} · 当前 {}{} · {}",
                cgroup.version,
                cgroup.path,
                human(cgroup.current),
                peak,
                limit
            ));
            let swap = cgroup
                .swap_current
                .map(|swap| format!(" · swap {}", human(swap)))
                .unwrap_or_default();
            out.push(format!(
                "cgroup 明细   {}{}",
                cgroup_detail(&cgroup.stat),
                swap
            ));
        }
        None => out.push("cgroup        不可用（未启用 cgroup memory 控制器）".to_string()),
    }

    match &accounting.numa {
        Some(numa) if !numa.nodes.is_empty() => {
            let nodes: Vec<String> = numa
                .nodes
                .iter()
                .map(|(node, pages)| format!("N{node} {pages} 页（{}）", human(pages * page_size)))
                .collect();
            let huge = if numa.anon_huge_pages > 0 {
                format!(" · anon_huge {} 页", numa.anon_huge_pages)
            } else {
                String::new()
            };
            out.push(format!("NUMA          {}{}", nodes.join(" · "), huge));
        }
        Some(_) => out.push("NUMA          只有一个节点或没有分布信息".to_string()),
        None => out.push("NUMA          numa_maps 不可用".to_string()),
    }

    out.push(format!(
        "page_owner    {}",
        if accounting.page_owner {
            "可用（/sys/kernel/debug/page_owner）"
        } else {
            "不可用（需要 root 且挂载 debugfs）"
        }
    ));
    out
}

/// 生成文本报告中的账务段。
pub fn report(accounting: &Accounting, snapshot: &ProcessSnapshot) -> String {
    let mut out = String::from("\n内存账务\n");
    for line in lines(accounting, snapshot) {
        out.push_str("  ");
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn vma_totals(snapshot: &ProcessSnapshot) -> SmapsStats {
    let mut total = SmapsStats::default();
    for vma in &snapshot.vmas {
        if let Some(stats) = &vma.stats {
            total.merge(stats);
        }
    }
    total
}

/// cgroup 内存构成；v2 与 v1 的键名不同，按候选顺序取值。
fn cgroup_detail(stats: &[(String, u64)]) -> String {
    const ITEMS: [(&str, &[&str]); 4] = [
        ("anon", &["anon", "rss"]),
        ("file", &["file", "cache"]),
        ("kernel", &["kernel", "kernel_stack"]),
        ("slab", &["slab", "slab_reclaimable"]),
    ];
    let mut parts = Vec::new();
    for (label, names) in ITEMS {
        let value = names
            .iter()
            .find_map(|name| stats.iter().find(|(key, _)| key == name).map(|(_, v)| *v));
        if let Some(value) = value {
            parts.push(format!("{label} {}", human(value)));
        }
    }
    if parts.is_empty() {
        "无明细".to_string()
    } else {
        parts.join(" · ")
    }
}

fn signed_diff(rollup: u64, summed: u64) -> String {
    let delta = rollup as i64 - summed as i64;
    let sign = if delta >= 0 { '+' } else { '-' };
    format!("{sign}{}", human(delta.unsigned_abs()))
}

fn collect_ksm(pid: i32) -> Option<KsmInfo> {
    let base = Path::new("/sys/kernel/mm/ksm");
    let run = read_number(base.join("run"))?;
    Some(KsmInfo {
        run: run != 0,
        pages_shared: read_number(base.join("pages_shared")).unwrap_or(0),
        pages_sharing: read_number(base.join("pages_sharing")).unwrap_or(0),
        pages_unshared: read_number(base.join("pages_unshared")).unwrap_or(0),
        full_scans: read_number(base.join("full_scans")).unwrap_or(0),
        process: read_text(format!("/proc/{pid}/ksm_stat"))
            .map(|text| parse_kv(&text))
            .unwrap_or_default(),
    })
}

fn collect_cgroup(pid: i32) -> Option<CgroupInfo> {
    let cgroup = read_text(format!("/proc/{pid}/cgroup"))?;
    let mountinfo = read_text(format!("/proc/{pid}/mountinfo"))?;
    collect_cgroup_v2(&cgroup, &mountinfo).or_else(|| collect_cgroup_v1(&cgroup, &mountinfo))
}

/// cgroup v2：`0::/path` + `cgroup2` 挂载点 + `memory.*` 文件。
fn collect_cgroup_v2(cgroup: &str, mountinfo: &str) -> Option<CgroupInfo> {
    let path = parse_cgroup_path(cgroup)?;
    let mount = parse_cgroup2_mount(mountinfo)?;
    let dir = Path::new(&mount).join(path.trim_start_matches('/'));
    let current = read_number(dir.join("memory.current"))?;
    Some(CgroupInfo {
        version: 2,
        path,
        current,
        peak: read_number(dir.join("memory.peak")),
        max: read_number(dir.join("memory.max")),
        swap_current: read_number(dir.join("memory.swap.current")),
        stat: read_text(dir.join("memory.stat"))
            .map(|text| parse_kv(&text))
            .unwrap_or_default(),
    })
}

/// cgroup v1：`N:memory:...` + `- cgroup ... memory` 挂载点 + `memory.*` 文件。
fn collect_cgroup_v1(cgroup: &str, mountinfo: &str) -> Option<CgroupInfo> {
    let path = parse_cgroup_v1_memory(cgroup)?;
    let mount = parse_cgroup1_mount(mountinfo, "memory")?;
    let dir = Path::new(&mount).join(path.trim_start_matches('/'));
    let current = read_number(dir.join("memory.usage_in_bytes"))?;
    let max = read_number(dir.join("memory.limit_in_bytes")).filter(|limit| *limit < 1 << 60);
    let swap_current = read_number(dir.join("memory.memsw.usage_in_bytes"))
        .map(|memsw| memsw.saturating_sub(current));
    Some(CgroupInfo {
        version: 1,
        path,
        current,
        peak: read_number(dir.join("memory.max_usage_in_bytes")),
        max,
        swap_current,
        stat: read_text(dir.join("memory.stat"))
            .map(|text| parse_kv(&text))
            .unwrap_or_default(),
    })
}

fn read_text(path: impl AsRef<Path>) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

fn read_number(path: impl AsRef<Path>) -> Option<u64> {
    read_text(path)?.trim().parse().ok()
}

/// 解析 `/proc/<pid>/cgroup`，只接受 cgroup v2 的 `0::/path` 形式。
pub fn parse_cgroup_path(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
}

/// 从 mountinfo 中找出 cgroup2 的挂载点。
pub fn parse_cgroup2_mount(text: &str) -> Option<String> {
    for line in text.lines() {
        let Some((before, after)) = line.split_once(" - ") else {
            continue;
        };
        if after.split_whitespace().next() != Some("cgroup2") {
            continue;
        }
        let fields: Vec<&str> = before.split_whitespace().collect();
        if let Some(mount) = fields.get(4) {
            return Some((*mount).to_string());
        }
    }
    None
}

/// 从 v1 的 `/proc/<pid>/cgroup` 中取出 memory 控制器的路径。
pub fn parse_cgroup_v1_memory(text: &str) -> Option<String> {
    for line in text.lines() {
        let mut parts = line.splitn(3, ':');
        let _id = parts.next()?;
        let controllers = parts.next()?;
        let path = parts.next()?.trim();
        if controllers.split(',').any(|name| name == "memory") && !path.is_empty() {
            return Some(path.to_string());
        }
    }
    None
}

/// 从 mountinfo 中找出 v1 某个控制器的挂载点。
pub fn parse_cgroup1_mount(text: &str, controller: &str) -> Option<String> {
    for line in text.lines() {
        let Some((before, after)) = line.split_once(" - ") else {
            continue;
        };
        let mut fields = after.split_whitespace();
        if fields.next() != Some("cgroup") {
            continue;
        }
        let _source = fields.next();
        let super_options = fields.next().unwrap_or_default();
        if !super_options.split(',').any(|name| name == controller) {
            continue;
        }
        if let Some(mount_point) = before.split_whitespace().nth(4) {
            return Some(mount_point.to_string());
        }
    }
    None
}

/// 解析 `key value` 形式的文件（memory.stat、ksm_stat）。
pub fn parse_kv(text: &str) -> Vec<(String, u64)> {
    text.lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let key = fields.next()?;
            let value: u64 = fields.next()?.parse().ok()?;
            Some((key.to_string(), value))
        })
        .collect()
}

/// 解析 numa_maps：每行首字段是地址，`N<id>=页数` 表示节点分布。
pub fn parse_numa_maps(text: &str) -> NumaInfo {
    let mut info = NumaInfo::default();
    for line in text.lines() {
        for field in line.split_whitespace().skip(1) {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            let Ok(value) = value.parse::<u64>() else {
                continue;
            };
            if let Some(node) = key.strip_prefix('N').and_then(|id| id.parse::<u32>().ok()) {
                match info.nodes.iter_mut().find(|(id, _)| *id == node) {
                    Some(entry) => entry.1 += value,
                    None => info.nodes.push((node, value)),
                }
            } else if key == "anon_huge" {
                info.anon_huge_pages += value;
            }
        }
    }
    info.nodes.sort_by_key(|(node, _)| *node);
    info
}

/// 解析 `/sys/kernel/mm/transparent_hugepage/enabled`，取出方括号内的当前策略。
pub fn parse_thp_enabled(text: &str) -> String {
    let text = text.trim();
    match (text.find('['), text.find(']')) {
        (Some(start), Some(end)) if end > start => text[start + 1..end].to_string(),
        _ => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo;

    #[test]
    fn parses_cgroup_v2_path_and_mount() {
        let cgroup = "0::/user.slice/user-1000.slice/session-2.scope\n";
        assert_eq!(
            parse_cgroup_path(cgroup).as_deref(),
            Some("/user.slice/user-1000.slice/session-2.scope")
        );
        assert!(parse_cgroup_path("12:memory:/legacy\n").is_none());

        let mountinfo = "\
36 25 0:32 / /sys/fs/cgroup rw,nosuid - cgroup2 cgroup2 rw,nsdelegate
42 25 8:1 / / rw - ext4 /dev/sda1 rw
";
        assert_eq!(
            parse_cgroup2_mount(mountinfo).as_deref(),
            Some("/sys/fs/cgroup")
        );
        assert!(parse_cgroup2_mount("42 25 8:1 / / rw - ext4 /dev/sda1 rw\n").is_none());
    }

    #[test]
    fn parses_cgroup_v1_path_and_mount() {
        let cgroup = "\
12:memory:/user.slice/demo.scope
11:devices:/user.slice
0::/user.slice
";
        assert_eq!(
            parse_cgroup_v1_memory(cgroup).as_deref(),
            Some("/user.slice/demo.scope")
        );
        assert!(parse_cgroup_path(cgroup).is_some(), "v2 行仍然可解析");

        let mountinfo = "\
30 25 0:26 / /sys/fs/cgroup/memory rw,nosuid - cgroup cgroup rw,memory
31 25 0:27 / /sys/fs/cgroup/cpu rw,nosuid - cgroup cgroup rw,cpu
";
        assert_eq!(
            parse_cgroup1_mount(mountinfo, "memory").as_deref(),
            Some("/sys/fs/cgroup/memory")
        );
        assert!(parse_cgroup1_mount(mountinfo, "pids").is_none());
    }

    #[test]
    fn parses_kv_files() {
        let stat = "anon 4096\nfile 8192\ninactive_anon 0\nbroken line\n";
        let parsed = parse_kv(stat);
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0], ("anon".to_string(), 4096));
        assert_eq!(parsed[1], ("file".to_string(), 8192));
    }

    #[test]
    fn cgroup_detail_handles_v1_names() {
        let v1 = vec![
            ("rss".to_string(), 4096),
            ("cache".to_string(), 8192),
            ("kernel_stack".to_string(), 512),
            ("slab".to_string(), 1024),
        ];
        let text = cgroup_detail(&v1);
        assert!(text.contains("anon 4.0K"), "{text}");
        assert!(text.contains("file 8.0K"), "{text}");
        assert!(text.contains("kernel 512B"), "{text}");
        assert!(text.contains("slab 1.0K"), "{text}");

        assert_eq!(cgroup_detail(&[]), "无明细");
    }

    #[test]
    fn parses_numa_distribution() {
        let text = "\
55e6e0e00000 default anon=2 dirty=2 N0=2
55e6e1000000 file=/usr/lib/libc.so mapped=10 N0=8 N1=2
55e6e2000000 default anon_huge=4 N1=4
";
        let info = parse_numa_maps(text);
        assert_eq!(info.nodes, vec![(0, 10), (1, 6)]);
        assert_eq!(info.anon_huge_pages, 4);
    }

    #[test]
    fn parses_thp_mode() {
        assert_eq!(
            parse_thp_enabled("always [madvise] never\n"),
            "madvise".to_string()
        );
        assert_eq!(parse_thp_enabled("[always] madvise never"), "always");
        assert_eq!(parse_thp_enabled("never"), "never");
    }

    #[test]
    fn report_contains_accounting_sections() {
        let snapshot = demo::snapshot();
        let text = report(&Accounting::demo(&snapshot), &snapshot);
        for expected in [
            "内存账务",
            "smaps_rollup",
            "差异",
            "THP",
            "KSM",
            "cgroup",
            "NUMA",
        ] {
            assert!(text.contains(expected), "缺少 {expected}:\n{text}");
        }
    }

    #[test]
    fn unavailable_sources_are_explained() {
        let snapshot = demo::snapshot();
        let text = lines(&Accounting::default(), &snapshot).join("\n");
        assert!(text.contains("不可用"));
        assert!(text.contains("未启用"));
    }
}
