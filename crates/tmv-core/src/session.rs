//! Versioned, atomic, sample-based session storage. Heap samples are NOT RSS.
use crate::{Category, MemNode, ProcessSnapshot};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, io, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub version: u32,
    pub pid: u32,
    pub command: Vec<String>,
    pub state: String,
    pub frames: usize,
    pub interval_ms: u64,
    pub started_unix_ms: u64,
    pub error: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackFrame {
    pub address: u64,
    pub symbol: String,
    pub location: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Allocation {
    pub address: u64,
    pub size: u64,
    pub caller: u64,
    pub label: String,
    #[serde(default)]
    pub stack: Vec<StackFrame>,
    #[serde(default)]
    pub resize_stack: Vec<StackFrame>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub elapsed_ms: u64,
    pub snapshot: Option<ProcessSnapshot>,
    pub layout_error: Option<String>,
    pub allocations: Vec<Allocation>,
    pub heap_records: u64,
    pub unknown_frees: u64,
    pub heap_ended: bool,
}
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let temporary = path.with_extension("tmp");
    let bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, path)
}
pub fn manifest(dir: &Path) -> io::Result<Manifest> {
    let m: Manifest =
        serde_json::from_slice(&fs::read(dir.join("manifest.json"))?).map_err(io::Error::other)?;
    if m.format != "tmv.session" || m.version != 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported tmv session version",
        ));
    }
    Ok(m)
}
pub fn frame(dir: &Path, index: usize) -> io::Result<Frame> {
    serde_json::from_slice(&fs::read(
        dir.join("frames").join(format!("{index:08}.json")),
    )?)
    .map_err(io::Error::other)
}
pub fn at(dir: &Path, m: &Manifest, time_ms: u64) -> io::Result<usize> {
    if m.frames == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "session has no samples",
        ));
    }
    let (mut low, mut high) = (0, m.frames);
    while low < high {
        let mid = low + (high - low) / 2;
        if frame(dir, mid)?.elapsed_ms <= time_ms {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    if low == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "requested time precedes first sample",
        ));
    }
    Ok(low - 1)
}
pub fn allocation_tree(frame: &Frame) -> MemNode {
    let mut groups: BTreeMap<&str, MemNode> = BTreeMap::new();
    for a in &frame.allocations {
        let group = groups.entry(&a.label).or_insert_with(|| MemNode {
            label: a.label.clone(),
            category: Category::Special,
            ..MemNode::empty()
        });
        group.bytes += a.size;
        group.children.push(MemNode {
            label: format!("0x{:x} · {} B", a.address, a.size),
            start: a.address,
            end: a.address.saturating_add(a.size),
            bytes: a.size,
            category: Category::Special,
            ..MemNode::empty()
        });
    }
    let mut children: Vec<_> = groups.into_values().collect();
    children.sort_by_key(|n| std::cmp::Reverse(n.bytes));
    MemNode {
        label: "存活申请字节（非 RSS）".into(),
        bytes: children.iter().map(|n| n.bytes).sum(),
        children,
        ..MemNode::empty()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disjoint_allocations_do_not_become_fake_contiguous_mapping() {
        let f = Frame {
            elapsed_ms: 0,
            snapshot: None,
            layout_error: None,
            allocations: vec![
                Allocation {
                    address: 4096,
                    size: 16,
                    caller: 1,
                    label: "Cache".into(),
                    stack: vec![],
                    resize_stack: vec![],
                },
                Allocation {
                    address: 65536,
                    size: 32,
                    caller: 1,
                    label: "Cache".into(),
                    stack: vec![],
                    resize_stack: vec![],
                },
            ],
            heap_records: 3,
            unknown_frees: 0,
            heap_ended: false,
        };
        let root = allocation_tree(&f);
        assert_eq!(root.bytes, 48);
        assert_eq!(root.children[0].children.len(), 2);
        assert_eq!(root.children[0].start, 0);
    }
}

#[cfg(test)]
mod storage_tests {
    use super::*;
    #[test]
    fn historical_lookup_never_uses_a_future_sample() {
        let dir = std::env::temp_dir().join(format!(
            "tmv-session-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(dir.join("frames")).unwrap();
        let mut m = Manifest {
            format: "tmv.session".into(),
            version: 1,
            pid: 1,
            command: vec![],
            state: "complete".into(),
            frames: 3,
            interval_ms: 1000,
            started_unix_ms: 0,
            error: None,
        };
        for (i, elapsed_ms) in [100, 1100, 2300].into_iter().enumerate() {
            let f = Frame {
                elapsed_ms,
                snapshot: None,
                layout_error: None,
                allocations: vec![],
                heap_records: 0,
                unknown_frees: 0,
                heap_ended: false,
            };
            write_json(&dir.join("frames").join(format!("{i:08}.json")), &f).unwrap();
        }
        assert!(at(&dir, &m, 99).is_err());
        assert_eq!(at(&dir, &m, 1100).unwrap(), 1);
        assert_eq!(at(&dir, &m, 2200).unwrap(), 1);
        assert_eq!(at(&dir, &m, 9999).unwrap(), 2);
        m.version = 2;
        write_json(&dir.join("manifest.json"), &m).unwrap();
        assert!(manifest(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}

/// Group all matching live allocations by both origin and latest resize stack.
/// Never pretend one arbitrary stack represents an entire business category.
pub fn stack_lines(frame: &Frame, label: &str, address: Option<u64>) -> Vec<String> {
    type StackKey = (Vec<u64>, Vec<u64>);
    let mut groups: BTreeMap<StackKey, (u64, usize, &Allocation)> = BTreeMap::new();
    for a in frame
        .allocations
        .iter()
        .filter(|a| a.label == label && address.is_none_or(|p| a.address == p))
    {
        let key = (
            a.stack.iter().map(|f| f.address).collect(),
            a.resize_stack.iter().map(|f| f.address).collect(),
        );
        let entry = groups.entry(key).or_insert((0, 0, a));
        entry.0 += a.size;
        entry.1 += 1;
    }
    let mut groups: Vec<_> = groups.into_values().collect();
    groups.sort_by_key(|g| std::cmp::Reverse(g.0));
    let mut lines = vec![format!(
        "{label} · {} 种栈路径 · 采样 {:.3}s",
        groups.len(),
        frame.elapsed_ms as f64 / 1000.
    )];
    if groups.is_empty() {
        lines.push("此时刻没有匹配的存活分配（可能已经释放）".into());
    }
    for (bytes, count, a) in groups {
        lines.push(format!("--- {bytes} bytes / {count} 个存活分配 ---"));
        if a.stack.is_empty() {
            lines.push("未采集分配栈（旧记录或关闭了栈采集）；不能由当前线程栈还原".into());
        } else {
            lines.push(format!(
                "原始分配栈（{} 帧，内层 → 外层；可能截断）",
                a.stack.len()
            ));
            append_stack(&mut lines, &a.stack);
        }
        if !a.resize_stack.is_empty() {
            lines.push("最近一次成功 realloc 调用栈".into());
            append_stack(&mut lines, &a.resize_stack);
        }
    }
    lines
}
fn append_stack(lines: &mut Vec<String>, stack: &[StackFrame]) {
    for (index, f) in stack.iter().enumerate() {
        lines.push(format!(
            "#{index:<2} 0x{:016x} {}{}",
            f.address,
            f.symbol,
            f.location
                .as_ref()
                .map(|s| format!("  {s}"))
                .unwrap_or_default()
        ));
    }
}
pub fn peak_index(dir: &Path, m: &Manifest) -> io::Result<usize> {
    if m.frames == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "session has no samples",
        ));
    }
    let (mut index, mut max) = (0, 0u128);
    for i in 0..m.frames {
        let sum: u128 = frame(dir, i)?
            .allocations
            .iter()
            .map(|a| u128::from(a.size))
            .sum();
        if sum > max {
            max = sum;
            index = i;
        }
    }
    Ok(index)
}

#[cfg(test)]
mod stack_compatibility_tests {
    use super::*;
    #[test]
    fn legacy_sessions_do_not_invent_stacks() {
        let raw = r#"{"elapsed_ms":1000,"snapshot":null,"layout_error":null,"allocations":[{"address":4096,"size":32,"caller":99,"label":"Cache"}],"heap_records":2,"unknown_frees":0,"heap_ended":false}"#;
        let mut f: Frame = serde_json::from_str(raw).unwrap();
        assert!(f.allocations[0].stack.is_empty());
        assert!(stack_lines(&f, "Cache", None)
            .join("\n")
            .contains("未采集分配栈"));
        let mut second = f.allocations[0].clone();
        second.address = 8192;
        second.stack = vec![StackFrame {
            address: 99,
            symbol: "cache_create".into(),
            location: None,
        }];
        f.allocations.push(second);
        let text = stack_lines(&f, "Cache", None).join("\n");
        assert!(text.contains("2 种栈路径"));
        let text = stack_lines(&f, "Cache", Some(8192)).join("\n");
        assert!(text.contains("1 种栈路径"));
        assert!(!text.contains("未采集分配栈"));
    }
}
