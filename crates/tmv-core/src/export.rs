//! 快照导出：JSON 文档、基线读写与文本报告。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::diff::{self, Diff};
use crate::proc::ProcessSnapshot;
use crate::vma::{Category, Metric, Vma};

/// 快照文件的格式标识。
pub const SNAPSHOT_FORMAT: &str = "tmv.snapshot";

/// 当前快照文件版本。
pub const SNAPSHOT_VERSION: u32 = 1;

/// 写入磁盘的快照文档，`--save-baseline` 与 `--json` 共用同一格式。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotDocument {
    pub format: String,
    pub version: u32,
    #[serde(flatten)]
    pub snapshot: ProcessSnapshot,
    /// 与基线对比时附带的摘要，单独保存基线时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffSummary>,
}

/// 两次快照差值的可序列化摘要。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffSummary {
    pub metric: Metric,
    pub total: i64,
    pub added: usize,
    pub removed: usize,
    /// 按变化绝对值排序的前若干项。
    pub changes: Vec<DiffEntry>,
}

/// 单个 VMA 的变化量。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffEntry {
    pub start: u64,
    pub end: u64,
    pub label: String,
    pub delta: i64,
}

impl SnapshotDocument {
    pub fn new(snapshot: ProcessSnapshot) -> Self {
        Self {
            format: SNAPSHOT_FORMAT.to_string(),
            version: SNAPSHOT_VERSION,
            snapshot,
            diff: None,
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|err| format!("{{\"error\":\"{err}\"}}"))
    }

    /// 解析并校验格式与版本。
    pub fn from_json(text: &str) -> Result<Self, String> {
        let document: Self =
            serde_json::from_str(text).map_err(|err| format!("JSON 解析失败: {err}"))?;
        if document.format != SNAPSHOT_FORMAT {
            return Err(format!("不支持的文件格式: {}", document.format));
        }
        if document.version > SNAPSHOT_VERSION {
            return Err(format!(
                "文件版本 {} 高于当前支持的 {}",
                document.version, SNAPSHOT_VERSION
            ));
        }
        Ok(document)
    }
}

impl DiffSummary {
    pub fn new(
        before: &ProcessSnapshot,
        after: &ProcessSnapshot,
        metric: Metric,
        top: usize,
    ) -> Self {
        let diff = diff::diff(before, after, metric);
        Self::from_diff(&diff, after, metric, top)
    }

    pub fn from_diff(diff: &Diff, after: &ProcessSnapshot, metric: Metric, top: usize) -> Self {
        let mut changes: Vec<DiffEntry> = after
            .vmas
            .iter()
            .filter_map(|vma| {
                let delta = *diff.by_start.get(&vma.start)?;
                Some(DiffEntry {
                    start: vma.start,
                    end: vma.end,
                    label: vma.label().to_string(),
                    delta,
                })
            })
            .collect();
        changes.sort_by_key(|change| std::cmp::Reverse(change.delta.abs()));
        changes.truncate(top);
        Self {
            metric,
            total: diff.total,
            added: diff.added,
            removed: diff.removed,
            changes,
        }
    }
}

/// 生成便于人读的文本报告。
pub fn report(
    snapshot: &ProcessSnapshot,
    metric: Metric,
    top: usize,
    baseline: Option<&ProcessSnapshot>,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("tmv report · pid {}\n", snapshot.pid));
    out.push_str(&format!(
        "几何 {}bit/{} · VMA {} · smaps {}\n",
        snapshot.geometry.va_bits,
        page_label(snapshot.geometry.page_size),
        snapshot.vmas.len(),
        if snapshot.has_smaps {
            "可用"
        } else {
            "不可用（仅虚拟大小）"
        }
    ));

    out.push_str("\n总量\n");
    for item in [
        Metric::Virtual,
        Metric::Rss,
        Metric::Pss,
        Metric::Private,
        Metric::Shared,
        Metric::Swap,
    ] {
        out.push_str(&format!(
            "  {:<8} {}\n",
            item.label(),
            human(snapshot.total(item))
        ));
    }

    let total = snapshot.total(metric);
    let mut categories: HashMap<Category, u64> = HashMap::new();
    for vma in &snapshot.vmas {
        *categories.entry(vma.category()).or_default() += vma.metric(metric);
    }
    let mut categories: Vec<(Category, u64)> = categories.into_iter().collect();
    categories.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    out.push_str(&format!("\n按类别（{}）\n", metric.label()));
    for (category, bytes) in categories {
        out.push_str(&format!(
            "  {:<10} {:>10}  {:>5.1}%\n",
            category.label(),
            human(bytes),
            percent(bytes, total)
        ));
    }

    let mut files: HashMap<&str, (u64, usize)> = HashMap::new();
    for vma in &snapshot.vmas {
        if vma.pathname.is_empty() || vma.pathname.starts_with('[') {
            continue;
        }
        let entry = files.entry(vma.pathname.as_str()).or_insert((0, 0));
        entry.0 += vma.metric(metric);
        entry.1 += 1;
    }
    let mut files: Vec<(&str, (u64, usize))> = files
        .into_iter()
        .filter(|(_, (bytes, _))| *bytes > 0)
        .collect();
    files.sort_by_key(|entry| std::cmp::Reverse(entry.1 .0));
    if !files.is_empty() {
        out.push_str(&format!(
            "\n按文件（{}，Top {}）\n",
            metric.label(),
            top.min(files.len())
        ));
        for (path, (bytes, mappings)) in files.iter().take(top) {
            out.push_str(&format!(
                "  {:>10}  {:>4} 映射  {}\n",
                human(*bytes),
                mappings,
                path
            ));
        }
    }

    let mut vmas: Vec<&Vma> = snapshot
        .vmas
        .iter()
        .filter(|vma| vma.metric(metric) > 0)
        .collect();
    vmas.sort_by_key(|vma| std::cmp::Reverse(vma.metric(metric)));
    out.push_str(&format!(
        "\nVMA Top {}（{}）\n",
        top.min(vmas.len()),
        metric.label()
    ));
    for vma in vmas.iter().take(top) {
        out.push_str(&format!(
            "  {:#014x}-{:#014x}  {:>10}  {}  {}\n",
            vma.start,
            vma.end,
            human(vma.metric(metric)),
            vma.perms.as_str(),
            vma.label()
        ));
    }

    if let Some(before) = baseline {
        let summary = DiffSummary::new(before, snapshot, metric, top);
        out.push_str(&format!("\n对比基线（{}）\n", metric.label()));
        out.push_str(&format!(
            "  总计 {} · 新增 {} · 移除 {}\n",
            signed(summary.total),
            summary.added,
            summary.removed
        ));
        for entry in &summary.changes {
            out.push_str(&format!(
                "  {:>10}  {:#014x}-{:#014x}  {}\n",
                signed(entry.delta),
                entry.start,
                entry.end,
                entry.label
            ));
        }
    }

    out
}

pub(crate) fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}B")
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

fn signed(delta: i64) -> String {
    let sign = if delta >= 0 { '+' } else { '-' };
    format!("{sign}{}", human(delta.unsigned_abs()))
}

fn percent(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 * 100.0 / total as f64
    }
}

fn page_label(size: u64) -> String {
    if size >= 1024 && size % 1024 == 0 {
        format!("{}K", size / 1024)
    } else {
        format!("{size}B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::demo;

    #[test]
    fn snapshot_json_roundtrip() {
        let snapshot = demo::snapshot();
        let document = SnapshotDocument::new(snapshot.clone());
        let json = document.to_json();
        let parsed = SnapshotDocument::from_json(&json).expect("应能解析自己写出的文件");

        assert_eq!(parsed.snapshot.pid, snapshot.pid);
        assert_eq!(parsed.snapshot.vmas, snapshot.vmas);
        assert_eq!(parsed.snapshot.geometry, snapshot.geometry);
        assert_eq!(parsed.snapshot.has_smaps, snapshot.has_smaps);
        assert_eq!(parsed.diff, None);
    }

    #[test]
    fn perms_serialize_as_string() {
        let perms = crate::vma::Perms::parse("r-xp").unwrap();
        assert_eq!(serde_json::to_string(&perms).unwrap(), "\"r-xp\"");
        let parsed: crate::vma::Perms = serde_json::from_str("\"rw-s\"").unwrap();
        assert_eq!(parsed.as_str(), "rw-s");
    }

    #[test]
    fn rejects_unknown_format_and_newer_version() {
        let mut document = SnapshotDocument::new(demo::snapshot());
        document.format = "other".into();
        assert!(SnapshotDocument::from_json(&document.to_json()).is_err());

        let mut document = SnapshotDocument::new(demo::snapshot());
        document.version = SNAPSHOT_VERSION + 1;
        assert!(SnapshotDocument::from_json(&document.to_json()).is_err());
    }

    #[test]
    fn diff_summary_reports_growth() {
        let before = demo::snapshot();
        let mut after = before.clone();
        if let Some(heap) = after.vmas.iter_mut().find(|vma| vma.pathname == "[heap]") {
            if let Some(stats) = heap.stats.as_mut() {
                stats.pss += 4 * 1024 * 1024;
            }
        }

        let summary = DiffSummary::new(&before, &after, Metric::Pss, 10);
        assert_eq!(summary.total, 4 * 1024 * 1024);
        assert_eq!(summary.changes.len(), 1);
        assert_eq!(summary.changes[0].label, "[heap]");
    }

    #[test]
    fn report_contains_sections() {
        let text = report(&demo::snapshot(), Metric::Pss, 5, None);
        assert!(text.contains("tmv report"));
        assert!(text.contains("总量"));
        assert!(text.contains("按类别"));
        assert!(text.contains("VMA Top"));
        assert!(text.contains("[heap]"));
    }
}
