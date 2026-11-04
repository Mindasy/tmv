//! /proc/<pid>/maps 与 /proc/<pid>/smaps 的采集与解析。

use std::fs;
use std::io;

use crate::geometry::TargetGeometry;
use crate::vma::{Perms, SmapsStats, Vma};

/// 一次采集得到的进程地址空间快照。
#[derive(Debug, Clone)]
pub struct ProcessSnapshot {
    pub pid: i32,
    pub geometry: TargetGeometry,
    pub vmas: Vec<Vma>,
    /// smaps 不可读时退化为 maps，此时只有虚拟大小可用。
    pub has_smaps: bool,
}

impl ProcessSnapshot {
    pub fn collect(pid: i32) -> io::Result<Self> {
        match read_proc(&format!("/proc/{pid}/smaps")) {
            Ok(text) => Ok(Self::from_vmas(pid, parse_smaps(&text), true)),
            Err(_) => match read_proc(&format!("/proc/{pid}/maps")) {
                Ok(text) => Ok(Self::from_vmas(pid, parse_maps(&text), false)),
                Err(err) if err.kind() == io::ErrorKind::NotFound => {
                    // /proc 未挂载时也会是 NotFound，先确认目录本身是否存在。
                    if std::path::Path::new(&format!("/proc/{pid}")).is_dir() {
                        Err(err)
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::NotFound,
                            format!("进程 {pid} 不存在"),
                        ))
                    }
                }
                Err(err) => Err(err),
            },
        }
    }

    pub fn from_vmas(pid: i32, vmas: Vec<Vma>, has_smaps: bool) -> Self {
        let max_addr = vmas.iter().map(|v| v.end).max().unwrap_or(0);
        let geometry =
            TargetGeometry::new(page_size(), TargetGeometry::va_bits_for_max_addr(max_addr));
        Self {
            pid,
            geometry,
            vmas,
            has_smaps,
        }
    }

    pub fn total(&self, metric: crate::vma::Metric) -> u64 {
        self.vmas.iter().map(|v| v.metric(metric)).sum()
    }
}

fn read_proc(path: &str) -> io::Result<String> {
    // maps 中的路径可能不是合法 UTF-8，用 lossy 转换避免整次采集失败。
    let bytes = fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

#[cfg(target_os = "linux")]
fn page_size() -> u64 {
    // SAFETY: sysconf 只读系统常量，不会修改状态。
    let value = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if value > 0 {
        value as u64
    } else {
        4096
    }
}

#[cfg(not(target_os = "linux"))]
fn page_size() -> u64 {
    4096
}

struct Header<'a> {
    start: u64,
    end: u64,
    perms: Perms,
    offset: u64,
    dev: &'a str,
    inode: u64,
    pathname: &'a str,
}

fn parse_header(line: &str) -> Option<Header<'_>> {
    let mut rest = line;
    let mut fields = [""; 5];
    for field in fields.iter_mut() {
        rest = rest.trim_start();
        if rest.is_empty() {
            return None;
        }
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        *field = &rest[..end];
        rest = &rest[end..];
    }
    let pathname = rest.trim_start();

    let (start_text, end_text) = fields[0].split_once('-')?;
    let start = u64::from_str_radix(start_text, 16).ok()?;
    let end = u64::from_str_radix(end_text, 16).ok()?;
    if end <= start {
        return None;
    }
    Some(Header {
        start,
        end,
        perms: Perms::parse(fields[1])?,
        offset: u64::from_str_radix(fields[2], 16).ok()?,
        dev: fields[3],
        inode: fields[4].parse().ok()?,
        pathname,
    })
}

fn vma_from_header(header: Header<'_>, with_stats: bool) -> Vma {
    Vma {
        start: header.start,
        end: header.end,
        perms: header.perms,
        offset: header.offset,
        dev: header.dev.to_string(),
        inode: header.inode,
        pathname: header.pathname.to_string(),
        stats: with_stats.then(SmapsStats::default),
    }
}

/// 解析 smaps：头部行与 maps 相同，后面跟若干 "Key: value kB" 统计行。
pub fn parse_smaps(text: &str) -> Vec<Vma> {
    let mut vmas: Vec<Vma> = Vec::new();
    for line in text.lines() {
        if let Some(header) = parse_header(line) {
            vmas.push(vma_from_header(header, true));
        } else if let Some(stats) = vmas.last_mut().and_then(|v| v.stats.as_mut()) {
            apply_stat(stats, line);
        }
    }
    vmas
}

pub fn parse_maps(text: &str) -> Vec<Vma> {
    text.lines()
        .filter_map(parse_header)
        .map(|header| vma_from_header(header, false))
        .collect()
}

fn apply_stat(stats: &mut SmapsStats, line: &str) {
    let Some((key, value)) = line.trim().split_once(':') else {
        return;
    };
    let value = value.trim();
    let number = value.strip_suffix(" kB").unwrap_or(value).trim();
    let Ok(kb) = number.parse::<u64>() else {
        return;
    };
    let bytes = kb * 1024;
    match key.trim() {
        "Size" => stats.size = bytes,
        "Rss" => stats.rss = bytes,
        "Pss" => stats.pss = bytes,
        "Shared_Clean" => stats.shared_clean = bytes,
        "Shared_Dirty" => stats.shared_dirty = bytes,
        "Private_Clean" => stats.private_clean = bytes,
        "Private_Dirty" => stats.private_dirty = bytes,
        "Swap" => stats.swap = bytes,
        "Anonymous" => stats.anonymous = bytes,
        "AnonHugePages" => stats.anon_huge = bytes,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vma::Metric;

    const SMAPS: &str = "\
555555554000-555555555000 r--p 00000000 fd:01 1234   /usr/bin/demo
Size:                  4 kB
Rss:                   4 kB
Pss:                   2 kB
Private_Clean:         4 kB
VmFlags: rd mr mw me

555555555000-55555555a000 r-xp 00001000 fd:01 1234   /usr/bin/demo
Size:                 20 kB
Rss:                  20 kB
Pss:                  10 kB
Shared_Clean:         20 kB
AnonHugePages:         4 kB
";

    #[test]
    fn parses_smaps_headers_and_stats() {
        let vmas = parse_smaps(SMAPS);
        assert_eq!(vmas.len(), 2);

        assert_eq!(vmas[0].start, 0x5555_5555_4000);
        assert_eq!(vmas[0].end, 0x5555_5555_5000);
        assert_eq!(vmas[0].perms.as_str(), "r--p");
        assert_eq!(vmas[0].pathname, "/usr/bin/demo");
        assert_eq!(vmas[0].metric(Metric::Virtual), 4096);
        assert_eq!(vmas[0].metric(Metric::Private), 4096);
        assert_eq!(vmas[0].metric(Metric::Pss), 2048);

        assert_eq!(vmas[1].offset, 0x1000);
        assert_eq!(vmas[1].metric(Metric::Shared), 20 * 1024);
        assert_eq!(vmas[1].stats.as_ref().unwrap().anon_huge, 4096);
    }

    #[test]
    fn parses_maps_without_stats() {
        let vmas = parse_maps(SMAPS);
        assert_eq!(vmas.len(), 2);
        assert!(vmas.iter().all(|v| v.stats.is_none()));
        assert_eq!(vmas[1].metric(Metric::Virtual), 5 * 4096);
    }

    #[test]
    fn handles_pathnames_with_spaces() {
        let text = "7f0000000000-7f0000001000 rw-s 00000000 00:05 42 /dev/shm/a b c\n";
        let vmas = parse_smaps(text);
        assert_eq!(vmas[0].pathname, "/dev/shm/a b c");
    }

    #[test]
    fn ignores_garbage_lines() {
        let vmas = parse_smaps("not a mapping\nSize: 4 kB\n\n");
        assert!(vmas.is_empty());
    }

    #[test]
    fn missing_process_is_reported_as_not_found() {
        let err = ProcessSnapshot::collect(2_000_000_000).expect_err("不存在的 PID 必须报错");
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(err.to_string().contains("不存在"), "实际信息: {err}");
    }
}
