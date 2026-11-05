//! 把 VMA 列表聚合成可渲染的树，treemap 只关心「标签 + 字节数 + 子节点」。

use std::collections::BTreeMap;

use crate::diff::Diff;
use crate::proc::ProcessSnapshot;
use crate::vma::{Category, Metric, Vma};

/// 一级分组方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupBy {
    /// 每个 VMA 一个块。
    Map,
    /// 按映射文件聚合。
    File,
    /// 按区域类别聚合。
    Category,
}

impl GroupBy {
    pub const ALL: [GroupBy; 3] = [GroupBy::Category, GroupBy::File, GroupBy::Map];

    pub fn label(self) -> &'static str {
        match self {
            GroupBy::Map => "映射",
            GroupBy::File => "文件",
            GroupBy::Category => "类别",
        }
    }

    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|g| *g == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }
}

/// 同级块的排序方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Address,
    SizeDesc,
}

impl SortOrder {
    pub fn label(self) -> &'static str {
        match self {
            SortOrder::Address => "地址",
            SortOrder::SizeDesc => "大小",
        }
    }

    pub fn next(self) -> Self {
        match self {
            SortOrder::Address => SortOrder::SizeDesc,
            SortOrder::SizeDesc => SortOrder::Address,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TreeOptions {
    pub metric: Metric,
    pub group_by: GroupBy,
    pub sort: SortOrder,
}

/// treemap 节点：叶子对应一个 VMA，中间节点对应聚合组。
#[derive(Debug, Clone)]
pub struct MemNode {
    pub label: String,
    pub start: u64,
    pub end: u64,
    pub bytes: u64,
    /// 与上一份快照相比的字节变化，未启用 diff 时为 0。
    pub delta: i64,
    pub category: Category,
    pub vma: Option<Vma>,
    pub children: Vec<MemNode>,
}

impl MemNode {
    pub fn empty() -> Self {
        Self {
            label: String::new(),
            start: 0,
            end: 0,
            bytes: 0,
            delta: 0,
            category: Category::Special,
            vma: None,
            children: Vec::new(),
        }
    }
}

pub fn build_tree(snapshot: &ProcessSnapshot, opts: TreeOptions) -> MemNode {
    let leaves: Vec<MemNode> = snapshot
        .vmas
        .iter()
        .filter_map(|vma| {
            let bytes = vma.metric(opts.metric);
            if bytes == 0 {
                return None;
            }
            Some(MemNode {
                label: vma.label().to_string(),
                start: vma.start,
                end: vma.end,
                bytes,
                delta: 0,
                category: vma.category(),
                vma: Some(vma.clone()),
                children: Vec::new(),
            })
        })
        .collect();

    let mut root = MemNode {
        label: format!("pid {}", snapshot.pid),
        start: leaves.iter().map(|n| n.start).min().unwrap_or(0),
        end: leaves.iter().map(|n| n.end).max().unwrap_or(0),
        bytes: leaves.iter().map(|n| n.bytes).sum(),
        delta: 0,
        category: Category::Special,
        vma: None,
        children: Vec::new(),
    };

    root.children = match opts.group_by {
        GroupBy::Map => {
            let mut nodes = leaves;
            sort_nodes(&mut nodes, opts.sort);
            nodes
        }
        GroupBy::File => group_by(leaves, opts.sort, |v| v.label().to_string()),
        GroupBy::Category => group_by(leaves, opts.sort, |v| v.category().label().to_string()),
    };
    root
}

fn group_by(leaves: Vec<MemNode>, sort: SortOrder, key: impl Fn(&Vma) -> String) -> Vec<MemNode> {
    let mut buckets: BTreeMap<String, Vec<MemNode>> = BTreeMap::new();
    for leaf in leaves {
        let key = key(leaf.vma.as_ref().expect("叶子节点必须带 VMA"));
        buckets.entry(key).or_default().push(leaf);
    }

    let mut groups: Vec<MemNode> = buckets
        .into_iter()
        .map(|(label, mut children)| {
            sort_nodes(&mut children, SortOrder::Address);
            let category = children
                .iter()
                .max_by_key(|c| c.bytes)
                .map(|c| c.category)
                .unwrap_or(Category::Special);
            MemNode {
                label,
                start: children.iter().map(|n| n.start).min().unwrap_or(0),
                end: children.iter().map(|n| n.end).max().unwrap_or(0),
                bytes: children.iter().map(|n| n.bytes).sum(),
                delta: 0,
                category,
                vma: None,
                children,
            }
        })
        .collect();
    sort_nodes(&mut groups, sort);
    groups
}

fn sort_nodes(nodes: &mut [MemNode], sort: SortOrder) {
    match sort {
        SortOrder::Address => nodes.sort_by_key(|n| n.start),
        SortOrder::SizeDesc => {
            nodes.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(a.start.cmp(&b.start)))
        }
    }
}

/// 把叶子差值汇总到每个节点上，返回值是子树总差值。
///
/// TUI 和 SVG 导出共用：改基线的块都会带上 `delta`，便于着色。
pub fn apply_deltas(node: &mut MemNode, diff: &Diff) -> i64 {
    if node.children.is_empty() {
        let delta = node
            .vma
            .as_ref()
            .and_then(|vma| diff.by_start.get(&vma.start))
            .copied()
            .unwrap_or(0);
        node.delta = delta;
        return delta;
    }

    let mut total = 0;
    for child in &mut node.children {
        total += apply_deltas(child, diff);
    }
    node.delta = total;
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vma::{Perms, SmapsStats};

    fn vma(start: u64, len: u64, path: &str, rss: u64) -> Vma {
        Vma {
            start,
            end: start + len,
            perms: Perms::parse("rw-p").unwrap(),
            offset: 0,
            dev: "fd:00".into(),
            inode: 0,
            pathname: path.into(),
            stats: Some(SmapsStats {
                size: len,
                rss,
                pss: rss / 2,
                ..Default::default()
            }),
        }
    }

    fn snapshot() -> ProcessSnapshot {
        ProcessSnapshot::from_vmas(
            7,
            vec![
                vma(0x1000, 0x3000, "/a", 0x2000),
                vma(0x5000, 0x3000, "/a", 0x1000),
                vma(0x9000, 0x1000, "/b", 0),
            ],
            true,
        )
    }

    fn opts(group_by: GroupBy, metric: Metric, sort: SortOrder) -> TreeOptions {
        TreeOptions {
            metric,
            group_by,
            sort,
        }
    }

    #[test]
    fn zero_metric_vmas_are_dropped() {
        let root = build_tree(
            &snapshot(),
            opts(GroupBy::Map, Metric::Rss, SortOrder::SizeDesc),
        );
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.bytes, 0x3000);
    }

    #[test]
    fn grouping_by_file_sums_children() {
        let root = build_tree(
            &snapshot(),
            opts(GroupBy::File, Metric::Virtual, SortOrder::SizeDesc),
        );
        assert_eq!(root.children.len(), 2);
        assert_eq!(root.children[0].label, "/a");
        assert_eq!(root.children[0].bytes, 0x6000);
        assert_eq!(root.children[0].children.len(), 2);
        assert_eq!(root.children[0].start, 0x1000);
        assert_eq!(root.children[0].end, 0x8000);
    }

    #[test]
    fn address_sort_keeps_stable_order() {
        let root = build_tree(
            &snapshot(),
            opts(GroupBy::Map, Metric::Virtual, SortOrder::Address),
        );
        let starts: Vec<u64> = root.children.iter().map(|c| c.start).collect();
        assert_eq!(starts, vec![0x1000, 0x5000, 0x9000]);
    }
}
