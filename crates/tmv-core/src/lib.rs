//! 进程虚拟地址空间的数据模型与采集实现。

pub mod demo;
pub mod diff;
pub mod geometry;
pub mod pagemap;
pub mod proc;
pub mod tree;
pub mod vma;

pub use diff::Diff;
pub use geometry::TargetGeometry;
pub use pagemap::{PageScan, PageState, PageSummary};
pub use proc::ProcessSnapshot;
pub use tree::{build_tree, GroupBy, MemNode, SortOrder, TreeOptions};
pub use vma::{Category, Metric, Perms, SmapsStats, Vma};
