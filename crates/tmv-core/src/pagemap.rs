//! /proc/<pid>/pagemap 页级状态采集。
//!
//! 无 CAP_SYS_ADMIN 时内核会把 PFN 抹零，其余状态位仍可读。

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

use crate::geometry::TargetGeometry;

const PAGE_PRESENT: u64 = 1 << 63;
const PAGE_SWAPPED: u64 = 1 << 62;
const PAGE_FILE_OR_SHARED: u64 = 1 << 61;
const PAGE_EXCLUSIVE: u64 = 1 << 56;
const PAGE_SOFT_DIRTY: u64 = 1 << 55;
const PFN_MASK: u64 = (1 << 55) - 1;

/// kpageflags 中表示透明大页的位。
const KPF_THP: u64 = 1 << 22;
/// kpageflags 中表示 compound head 的位。
const KPF_COMPOUND_HEAD: u64 = 1 << 15;

/// 单页状态。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PageState {
    pub present: bool,
    pub swapped: bool,
    /// 页缓存页或共享匿名页。
    pub file: bool,
    /// mapcount > 1，需要 /proc/kpagecount 才能判定。
    pub shared: bool,
    pub exclusive: bool,
    pub soft_dirty: bool,
    /// 属于透明大页，需要 /proc/kpageflags 且通常需要 root。
    pub thp: bool,
    pub pfn: u64,
}

#[derive(Debug, Clone, Default)]
pub struct PageSummary {
    pub samples: u64,
    pub present: u64,
    pub swapped: u64,
    pub absent: u64,
    pub file: u64,
    pub anon: u64,
    pub shared: u64,
    pub thp: u64,
    pub pfn_available: bool,
}

/// 一段地址区间的页级扫描结果。
#[derive(Debug, Clone)]
pub struct PageScan {
    pub start: u64,
    pub end: u64,
    pub page_size: u64,
    /// 区间内实际页数。
    pub pages: u64,
    /// 是否按 stride 抽样。
    pub sampled: bool,
    pub states: Vec<PageState>,
    pub summary: PageSummary,
}

/// 同一区间两次页扫描之间的状态变化，单位样本数。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PageDelta {
    /// 之前非驻留、现在驻留（新触及或因换入回到内存）。
    pub became_present: u64,
    /// 之前驻留、现在被换出。
    pub became_swapped: u64,
    /// 之前驻留、现在既不在内存也未换出（取消映射或页被回收）。
    pub became_absent: u64,
    /// 之前独占、现在被多个映射共享。
    pub became_shared: u64,
    /// 之前共享、现在独占。
    pub became_exclusive: u64,
}

impl PageDelta {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// 比较两次针对同一区间的扫描；区间或样本数不一致时返回 None。
///
/// `states` 按相同步长抽样，因此可以逐下标对应。
pub fn delta(before: &PageScan, after: &PageScan) -> Option<PageDelta> {
    if before.start != after.start
        || before.end != after.end
        || before.page_size != after.page_size
        || before.states.len() != after.states.len()
    {
        return None;
    }

    let mut result = PageDelta::default();
    for (old, new) in before.states.iter().zip(&after.states) {
        match (old.present, new.present) {
            (false, true) => result.became_present += 1,
            (true, false) if new.swapped => result.became_swapped += 1,
            (true, false) => result.became_absent += 1,
            (true, true) => {
                if !old.shared && new.shared {
                    result.became_shared += 1;
                } else if old.shared && !new.shared {
                    result.became_exclusive += 1;
                }
            }
            _ => {}
        }
    }
    Some(result)
}

/// 解析一条 pagemap 记录。
pub fn decode_entry(raw: u64) -> PageState {
    PageState {
        present: raw & PAGE_PRESENT != 0,
        swapped: raw & PAGE_SWAPPED != 0,
        file: raw & PAGE_FILE_OR_SHARED != 0,
        exclusive: raw & PAGE_EXCLUSIVE != 0,
        soft_dirty: raw & PAGE_SOFT_DIRTY != 0,
        pfn: raw & PFN_MASK,
        ..Default::default()
    }
}

pub fn summarize(states: &[PageState]) -> PageSummary {
    let mut summary = PageSummary {
        samples: states.len() as u64,
        ..Default::default()
    };
    for state in states {
        if state.present {
            summary.present += 1;
            if state.file {
                summary.file += 1;
            } else {
                summary.anon += 1;
            }
            if state.pfn != 0 {
                summary.pfn_available = true;
            }
        } else if state.swapped {
            summary.swapped += 1;
        } else {
            summary.absent += 1;
        }
        if state.shared {
            summary.shared += 1;
        }
        if state.thp {
            summary.thp += 1;
        }
    }
    summary
}

/// 扫描 `[start, end)` 的页状态，最多返回 `max_samples` 个样本。
pub fn scan(
    pid: i32,
    geometry: &TargetGeometry,
    start: u64,
    end: u64,
    max_samples: usize,
) -> io::Result<PageScan> {
    let page_size = geometry.page_size;
    let first = start / page_size;
    let last = end.div_ceil(page_size);
    let pages = last.saturating_sub(first);

    if pages == 0 || max_samples == 0 {
        return Ok(PageScan {
            start: first * page_size,
            end: last * page_size,
            page_size,
            pages,
            sampled: false,
            states: Vec::new(),
            summary: PageSummary::default(),
        });
    }

    let stride = pages.div_ceil(max_samples as u64).max(1);
    let file = File::open(format!("/proc/{pid}/pagemap"))?;
    let mut states = Vec::with_capacity((pages / stride).min(max_samples as u64) as usize);

    if stride == 1 {
        // 全量扫描时按块 pread，减少系统调用。
        let mut buffer = vec![0u8; 32 * 1024];
        let mut index = first;
        while index < last {
            let chunk = ((last - index) as usize).min(buffer.len() / 8);
            let bytes = chunk * 8;
            file.read_exact_at(&mut buffer[..bytes], index * 8)?;
            for i in 0..chunk {
                let raw = u64::from_le_bytes(buffer[i * 8..i * 8 + 8].try_into().expect("8 字节"));
                states.push(decode_entry(raw));
            }
            index += chunk as u64;
        }
    } else {
        // 抽样时按需 pread，避免为一个样本读整段。
        let mut buffer = [0u8; 8];
        let mut index = first;
        while index < last {
            file.read_exact_at(&mut buffer, index * 8)?;
            states.push(decode_entry(u64::from_le_bytes(buffer)));
            index += stride;
        }
    }

    Ok(PageScan {
        start: first * page_size,
        end: last * page_size,
        page_size,
        pages,
        sampled: stride > 1,
        summary: summarize(&states),
        states,
    })
}

/// 补充 PFN 级信息：共享页（kpagecount）与透明大页（kpageflags）。
///
/// 需要 root；失败时保持原有状态不变。
pub fn enrich_with_kpage(scan: &mut PageScan) -> io::Result<()> {
    let flags = File::open("/proc/kpageflags")?;
    let counts = File::open("/proc/kpagecount")?;
    let mut flag_buffer = [0u8; 8];
    let mut count_buffer = [0u8; 8];

    for state in &mut scan.states {
        if !state.present || state.pfn == 0 {
            continue;
        }
        let offset = state.pfn * 8;
        if counts.read_exact_at(&mut count_buffer, offset).is_ok() {
            state.shared = u64::from_le_bytes(count_buffer) > 1;
        }
        if flags.read_exact_at(&mut flag_buffer, offset).is_ok() {
            let raw = u64::from_le_bytes(flag_buffer);
            state.thp = raw & (KPF_THP | KPF_COMPOUND_HEAD) != 0;
        }
    }

    scan.summary = summarize(&scan.states);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_pagemap_bits() {
        let raw = PAGE_PRESENT | PAGE_FILE_OR_SHARED | PAGE_EXCLUSIVE | PAGE_SOFT_DIRTY | 0x1234;
        let state = decode_entry(raw);
        assert!(state.present);
        assert!(state.file);
        assert!(state.exclusive);
        assert!(state.soft_dirty);
        assert!(!state.swapped);
        assert_eq!(state.pfn, 0x1234);
    }

    #[test]
    fn decodes_swapped_entry() {
        let state = decode_entry(PAGE_SWAPPED);
        assert!(state.swapped);
        assert!(!state.present);
        assert_eq!(state.pfn, 0);
    }

    #[test]
    fn summary_separates_anon_file_swap_absent() {
        let states = [
            PageState {
                present: true,
                pfn: 42,
                ..Default::default()
            },
            PageState {
                present: true,
                file: true,
                ..Default::default()
            },
            PageState {
                swapped: true,
                ..Default::default()
            },
            PageState::default(),
        ];
        let summary = summarize(&states);
        assert_eq!(summary.samples, 4);
        assert_eq!(summary.present, 2);
        assert_eq!(summary.anon, 1);
        assert_eq!(summary.file, 1);
        assert_eq!(summary.swapped, 1);
        assert_eq!(summary.absent, 1);
        assert!(summary.pfn_available);
    }

    fn scan_of(states: Vec<PageState>) -> PageScan {
        PageScan {
            start: 0x1000,
            end: 0x1000 + states.len() as u64 * 4096,
            page_size: 4096,
            pages: states.len() as u64,
            sampled: false,
            summary: summarize(&states),
            states,
        }
    }

    #[test]
    fn delta_classifies_state_transitions() {
        let present = PageState {
            present: true,
            ..Default::default()
        };
        let shared = PageState {
            present: true,
            shared: true,
            ..Default::default()
        };
        let swapped = PageState {
            swapped: true,
            ..Default::default()
        };
        let absent = PageState::default();

        let before = scan_of(vec![present, present, present, shared]);
        let after = scan_of(vec![present, swapped, absent, present]);
        let delta = delta(&before, &after).expect("区间一致时应能比较");

        assert_eq!(delta.became_present, 0);
        assert_eq!(delta.became_swapped, 1);
        assert_eq!(delta.became_absent, 1);
        assert_eq!(delta.became_exclusive, 1);
        assert_eq!(delta.became_shared, 0);
        assert!(!delta.is_empty());
    }

    #[test]
    fn delta_requires_same_range() {
        let before = scan_of(vec![PageState::default()]);
        let mut after = scan_of(vec![PageState::default()]);
        after.end += 4096;
        assert!(delta(&before, &after).is_none());

        let mut after = scan_of(vec![PageState::default()]);
        after.states.push(PageState::default());
        assert!(delta(&before, &after).is_none());
    }

    #[test]
    fn unchanged_scan_has_empty_delta() {
        let scan = scan_of(vec![PageState::default(), PageState::default()]);
        assert!(delta(&scan, &scan).expect("同一份扫描").is_empty());
    }
}
