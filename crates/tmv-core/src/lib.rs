//! 进程虚拟地址空间的数据模型与采集实现。

pub mod accounting;
pub mod demo;
pub mod diff;
pub mod export;
pub mod geometry;
pub mod pagemap;
pub mod proc;
pub mod tree;
pub mod vma;
pub mod watch;

pub use accounting::Accounting;
pub use diff::Diff;
pub use export::{report, DiffEntry, DiffSummary, SnapshotDocument};
pub use geometry::TargetGeometry;
pub use pagemap::{PageDelta, PageScan, PageState, PageSummary};
pub use proc::ProcessSnapshot;
pub use tree::{build_tree, GroupBy, MemNode, SortOrder, TreeOptions};
pub use vma::{Category, Metric, Perms, SmapsStats, Vma};
pub use watch::ProcessWatch;
