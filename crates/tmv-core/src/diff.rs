//! 两次进程快照的页级差值，用于定位内存增长来源。

use std::collections::HashMap;

use crate::proc::ProcessSnapshot;
use crate::vma::{Metric, Vma};

/// 两次快照按「起始地址 + 路径」匹配后的变化量，单位字节。
#[derive(Debug, Clone, Default)]
pub struct Diff {
    /// 按新快照中 VMA 的起始地址索引的差值。
    pub by_start: HashMap<u64, i64>,
    pub added: usize,
    pub removed: usize,
    pub total: i64,
    /// 没有上一份快照时为 false。
    pub has_baseline: bool,
}

pub fn diff(before: &ProcessSnapshot, after: &ProcessSnapshot, metric: Metric) -> Diff {
    let mut old: HashMap<(u64, &str), &Vma> = before
        .vmas
        .iter()
        .map(|vma| ((vma.start, vma.pathname.as_str()), vma))
        .collect();

    let mut result = Diff {
        has_baseline: true,
        ..Default::default()
    };

    for vma in &after.vmas {
        let after_bytes = vma.metric(metric) as i64;
        match old.remove(&(vma.start, vma.pathname.as_str())) {
            Some(previous) => {
                let delta = after_bytes - previous.metric(metric) as i64;
                if delta != 0 {
                    result.by_start.insert(vma.start, delta);
                    result.total += delta;
                }
            }
            None => {
                result.added += 1;
                result.total += after_bytes;
                if after_bytes != 0 {
                    result.by_start.insert(vma.start, after_bytes);
                }
            }
        }
    }

    for vma in old.values() {
        result.removed += 1;
        result.total -= vma.metric(metric) as i64;
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vma::{Perms, SmapsStats};

    fn vma(start: u64, len: u64, path: &str, pss: u64) -> Vma {
        Vma {
            start,
            end: start + len,
            perms: Perms::parse("rw-p").unwrap(),
            offset: 0,
            dev: "fd:01".into(),
            inode: 1,
            pathname: path.into(),
            stats: Some(SmapsStats {
                size: len,
                rss: pss,
                pss,
                ..Default::default()
            }),
        }
    }

    fn snapshot(vmas: Vec<Vma>) -> ProcessSnapshot {
        ProcessSnapshot::from_vmas(7, vmas, true)
    }

    #[test]
    fn detects_growth_shrink_add_and_remove() {
        let before = snapshot(vec![
            vma(0x1000, 0x1000, "a", 100),
            vma(0x5000, 0x1000, "b", 50),
        ]);
        let after = snapshot(vec![
            vma(0x1000, 0x1000, "a", 120),
            vma(0x9000, 0x1000, "c", 30),
        ]);

        let result = diff(&before, &after, Metric::Pss);
        assert!(result.has_baseline);
        assert_eq!(result.by_start.get(&0x1000), Some(&20));
        assert_eq!(result.by_start.get(&0x9000), Some(&30));
        assert_eq!(result.added, 1);
        assert_eq!(result.removed, 1);
        assert_eq!(result.total, 20 + 30 - 50);
    }

    #[test]
    fn address_reuse_with_different_path_is_add_plus_remove() {
        let before = snapshot(vec![vma(0x1000, 0x1000, "old", 60)]);
        let after = snapshot(vec![vma(0x1000, 0x1000, "new", 40)]);
        let result = diff(&before, &after, Metric::Pss);
        assert_eq!(result.added, 1);
        assert_eq!(result.removed, 1);
        assert_eq!(result.by_start.get(&0x1000), Some(&40));
        assert_eq!(result.total, -20);
    }

    #[test]
    fn unchanged_snapshot_has_zero_diff() {
        let snapshot = snapshot(vec![vma(0x1000, 0x1000, "a", 100)]);
        let result = diff(&snapshot, &snapshot, Metric::Pss);
        assert_eq!(result.total, 0);
        assert!(result.by_start.is_empty());
        assert_eq!(result.added, 0);
        assert_eq!(result.removed, 0);
    }
}
