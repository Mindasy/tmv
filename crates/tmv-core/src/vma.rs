//! 单个 VMA（虚拟内存区域）及其 smaps 统计。

/// 映射权限，对应 /proc/pid/maps 第二个字段。
///
/// 序列化为 `"r-xp"` 这样的字符串，便于基线文件阅读与手工编辑。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Perms {
    pub read: bool,
    pub write: bool,
    pub exec: bool,
    pub shared: bool,
}

impl TryFrom<String> for Perms {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Perms::parse(&value).ok_or_else(|| format!("无效的权限字符串: {value}"))
    }
}

impl From<Perms> for String {
    fn from(value: Perms) -> Self {
        value.as_str()
    }
}

impl Perms {
    pub fn parse(field: &str) -> Option<Self> {
        let bytes = field.as_bytes();
        if bytes.len() != 4 {
            return None;
        }
        let read = match bytes[0] {
            b'r' => true,
            b'-' => false,
            _ => return None,
        };
        let write = match bytes[1] {
            b'w' => true,
            b'-' => false,
            _ => return None,
        };
        let exec = match bytes[2] {
            b'x' => true,
            b'-' => false,
            _ => return None,
        };
        let shared = match bytes[3] {
            b's' => true,
            b'p' => false,
            _ => return None,
        };
        Some(Self {
            read,
            write,
            exec,
            shared,
        })
    }

    pub fn as_str(&self) -> String {
        let mut out = String::with_capacity(4);
        out.push(if self.read { 'r' } else { '-' });
        out.push(if self.write { 'w' } else { '-' });
        out.push(if self.exec { 'x' } else { '-' });
        out.push(if self.shared { 's' } else { 'p' });
        out
    }
}

/// /proc/pid/smaps 中单个 VMA 的统计值，单位统一为字节。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SmapsStats {
    pub size: u64,
    pub rss: u64,
    pub pss: u64,
    pub shared_clean: u64,
    pub shared_dirty: u64,
    pub private_clean: u64,
    pub private_dirty: u64,
    pub swap: u64,
    pub anonymous: u64,
    pub anon_huge: u64,
    /// 换出页按共享比例摊销后的 PSS 值。
    pub swap_pss: u64,
    /// 文件透明大页映射的大小。
    pub file_pmd_mapped: u64,
    /// 共享内存透明大页映射的大小。
    pub shmem_pmd_mapped: u64,
    /// smaps 的 THPeligible 标志（0/1）。
    pub thp_eligible: bool,
}

impl SmapsStats {
    pub fn private(&self) -> u64 {
        self.private_clean + self.private_dirty
    }

    pub fn shared(&self) -> u64 {
        self.shared_clean + self.shared_dirty
    }

    /// 所有透明大页映射的合计（匿名 + 文件 + 共享内存）。
    pub fn thp_total(&self) -> u64 {
        self.anon_huge + self.file_pmd_mapped + self.shmem_pmd_mapped
    }

    pub fn merge(&mut self, other: &Self) {
        self.size += other.size;
        self.rss += other.rss;
        self.pss += other.pss;
        self.shared_clean += other.shared_clean;
        self.shared_dirty += other.shared_dirty;
        self.private_clean += other.private_clean;
        self.private_dirty += other.private_dirty;
        self.swap += other.swap;
        self.anonymous += other.anonymous;
        self.anon_huge += other.anon_huge;
        self.swap_pss += other.swap_pss;
        self.file_pmd_mapped += other.file_pmd_mapped;
        self.shmem_pmd_mapped += other.shmem_pmd_mapped;
        self.thp_eligible |= other.thp_eligible;
    }
}

/// treemap 面积所依据的度量。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Metric {
    Virtual,
    Rss,
    Pss,
    Private,
    Shared,
    Swap,
}

impl Metric {
    pub const ALL: [Metric; 6] = [
        Metric::Pss,
        Metric::Rss,
        Metric::Virtual,
        Metric::Private,
        Metric::Shared,
        Metric::Swap,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Metric::Virtual => "虚拟大小",
            Metric::Rss => "RSS",
            Metric::Pss => "PSS",
            Metric::Private => "独占",
            Metric::Shared => "共享",
            Metric::Swap => "Swap",
        }
    }

    pub fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|m| *m == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }
}

/// 区域类别，用于着色与一级聚合。
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Heap,
    Stack,
    JitExec,
    Vdso,
    AnonShared,
    AnonPrivate,
    FileShared,
    FilePrivate,
    Special,
}

impl Category {
    pub fn label(self) -> &'static str {
        match self {
            Category::Heap => "heap",
            Category::Stack => "stack",
            Category::JitExec => "JIT/可执行",
            Category::Vdso => "vdso",
            Category::AnonShared => "匿名共享",
            Category::AnonPrivate => "匿名私有",
            Category::FileShared => "文件共享",
            Category::FilePrivate => "文件私有",
            Category::Special => "特殊",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Vma {
    pub start: u64,
    pub end: u64,
    pub perms: Perms,
    pub offset: u64,
    pub dev: String,
    pub inode: u64,
    pub pathname: String,
    pub stats: Option<SmapsStats>,
}

impl Vma {
    pub fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.end <= self.start
    }

    /// 展示名：有路径用路径，匿名的按权限给一个可读名字。
    pub fn label(&self) -> &str {
        if !self.pathname.is_empty() {
            &self.pathname
        } else if self.perms.exec {
            "[anon-exec]"
        } else if self.perms.shared {
            "[anon-shared]"
        } else {
            "[anon]"
        }
    }

    pub fn category(&self) -> Category {
        let path = self.pathname.as_str();
        match path {
            "[heap]" => Category::Heap,
            "[vdso]" | "[vvar]" | "[vsyscall]" | "[vectors]" => Category::Vdso,
            "" => self.anon_category(),
            _ if path.starts_with("[stack") => Category::Stack,
            _ if path.starts_with("[anon:") => self.anon_category(),
            _ if path.starts_with('[') => Category::Special,
            _ if self.perms.shared => Category::FileShared,
            _ => Category::FilePrivate,
        }
    }

    fn anon_category(&self) -> Category {
        if self.perms.exec {
            Category::JitExec
        } else if self.perms.shared {
            Category::AnonShared
        } else {
            Category::AnonPrivate
        }
    }

    pub fn metric(&self, metric: Metric) -> u64 {
        match metric {
            Metric::Virtual => self.len(),
            Metric::Rss => self.stats.as_ref().map_or(0, |s| s.rss),
            Metric::Pss => self.stats.as_ref().map_or(0, |s| s.pss),
            Metric::Private => self.stats.as_ref().map_or(0, |s| s.private()),
            Metric::Shared => self.stats.as_ref().map_or(0, |s| s.shared()),
            Metric::Swap => self.stats.as_ref().map_or(0, |s| s.swap),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vma(path: &str, perms: &str) -> Vma {
        Vma {
            start: 0x1000,
            end: 0x4000,
            perms: Perms::parse(perms).unwrap(),
            offset: 0,
            dev: "fd:00".into(),
            inode: 0,
            pathname: path.into(),
            stats: None,
        }
    }

    #[test]
    fn perms_roundtrip() {
        for raw in ["rwxp", "r-xp", "rw-s", "r--p"] {
            assert_eq!(Perms::parse(raw).unwrap().as_str(), raw);
        }
        assert!(Perms::parse("rw").is_none());
        assert!(Perms::parse("rwxq").is_none());
    }

    #[test]
    fn categories_are_classified() {
        assert_eq!(vma("[heap]", "rw-p").category(), Category::Heap);
        assert_eq!(vma("[stack]", "rw-p").category(), Category::Stack);
        assert_eq!(vma("[vdso]", "r-xp").category(), Category::Vdso);
        assert_eq!(vma("", "rwxp").category(), Category::JitExec);
        assert_eq!(vma("", "rw-s").category(), Category::AnonShared);
        assert_eq!(vma("", "rw-p").category(), Category::AnonPrivate);
        assert_eq!(
            vma("/lib/libc.so", "r-xp").category(),
            Category::FilePrivate
        );
        assert_eq!(vma("/dev/shm/x", "rw-s").category(), Category::FileShared);
        assert_eq!(
            vma("[anon:jemalloc]", "rw-p").category(),
            Category::AnonPrivate
        );
    }

    #[test]
    fn anonymous_labels_are_readable() {
        assert_eq!(vma("", "rwxp").label(), "[anon-exec]");
        assert_eq!(vma("", "rw-s").label(), "[anon-shared]");
        assert_eq!(vma("", "rw-p").label(), "[anon]");
    }

    #[test]
    fn metrics_fall_back_to_zero_without_smaps() {
        assert_eq!(vma("[heap]", "rw-p").metric(Metric::Virtual), 0x3000);
        assert_eq!(vma("[heap]", "rw-p").metric(Metric::Pss), 0);
    }
}
