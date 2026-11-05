//! demo 模式下的合成线程与调用栈，用于在非 Linux 环境预览。

use tmv_core::ProcessSnapshot;

use crate::{StackFrame, ThreadInfo, ThreadStack, UnwindMethod};

pub const THREAD_NAMES: [&str; 4] = ["main", "worker", "jit", "io"];

pub fn threads() -> Vec<ThreadInfo> {
    THREAD_NAMES
        .iter()
        .enumerate()
        .map(|(index, name)| ThreadInfo {
            tid: 4242 + index as i32,
            name: name.to_string(),
            state: if index == 0 { 'R' } else { 'S' },
        })
        .collect()
}

pub fn stack(snapshot: &ProcessSnapshot, index: usize) -> ThreadStack {
    let execs: Vec<&tmv_core::Vma> = snapshot.vmas.iter().filter(|vma| vma.perms.exec).collect();

    let mut frames = Vec::new();
    for (position, vma) in execs.iter().rev().take(5).enumerate() {
        let offset = 0x40 * (index as u64 + position as u64 + 1);
        frames.push(StackFrame {
            pc: vma.start + offset,
        });
    }
    if frames.is_empty() {
        frames.push(StackFrame {
            pc: snapshot.vmas.first().map_or(0, |vma| vma.start),
        });
    }

    ThreadStack {
        tid: 4242 + index as i32,
        name: THREAD_NAMES[index % THREAD_NAMES.len()].to_string(),
        tls: Some(0x7f00_0000_0000 + index as u64 * 0x1000),
        frames,
        method: UnwindMethod::FramePointer,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_threads_and_stacks_are_consistent() {
        let threads = threads();
        assert_eq!(threads.len(), 4);

        let snapshot = tmv_core::demo::snapshot();
        for (index, thread) in threads.iter().enumerate() {
            let stack = stack(&snapshot, index);
            assert_eq!(stack.tid, thread.tid);
            assert!(!stack.frames.is_empty());
            assert!(stack.frames.iter().all(|frame| frame.pc != 0));
        }
    }
}
