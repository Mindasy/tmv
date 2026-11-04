//! ptrace 侵入式能力：线程枚举、寄存器、FP 栈回溯与 TLS。
//!
//! 仅在 Linux 上真正实现；其他平台返回 `Unsupported`，demo 数据不依赖 ptrace。

use std::io;

pub mod demo;

#[derive(Debug, Clone)]
pub struct ThreadInfo {
    pub tid: i32,
    pub name: String,
    pub state: char,
}

#[derive(Debug, Clone)]
pub struct StackFrame {
    pub pc: u64,
}

#[derive(Debug, Clone)]
pub struct ThreadStack {
    pub tid: i32,
    pub name: String,
    /// arm64 的 TPIDR_EL0，读取失败时为 None。
    pub tls: Option<u64>,
    pub frames: Vec<StackFrame>,
}

#[cfg(target_os = "linux")]
mod platform {
    use std::fs;
    use std::io;
    use std::os::unix::fs::FileExt;

    use tmv_core::TargetGeometry;

    use super::{StackFrame, ThreadInfo, ThreadStack};

    // 自己声明 ptrace，避免 glibc/musl 对 request 参数类型（c_uint / c_int）的差异。
    const PTRACE_ATTACH: u32 = 16;
    const PTRACE_DETACH: u32 = 17;
    const PTRACE_GETREGSET: u32 = 0x4204;
    const NT_PRSTATUS: usize = 1;
    const NT_ARM_TLS: usize = 0x401;

    extern "C" {
        fn ptrace(request: u32, ...) -> libc::c_long;
        fn waitpid(pid: libc::pid_t, status: *mut libc::c_int, options: libc::c_int)
            -> libc::pid_t;
    }

    /// arm64 的 `struct user_pt_regs`。
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct UserPtRegs {
        regs: [u64; 31],
        sp: u64,
        pc: u64,
        pstate: u64,
    }

    pub fn list_threads(pid: i32) -> io::Result<Vec<ThreadInfo>> {
        let mut threads = Vec::new();
        for entry in fs::read_dir(format!("/proc/{pid}/task"))? {
            let entry = entry?;
            let Ok(tid) = entry.file_name().to_string_lossy().parse::<i32>() else {
                continue;
            };
            let path = entry.path();
            let name = fs::read_to_string(path.join("comm"))
                .map(|name| name.trim().to_string())
                .unwrap_or_default();
            let state = fs::read_to_string(path.join("stat"))
                .ok()
                .and_then(|stat| parse_state(&stat))
                .unwrap_or('?');
            threads.push(ThreadInfo { tid, name, state });
        }
        threads.sort_by_key(|thread| thread.tid);
        Ok(threads)
    }

    pub fn unwind_thread(
        pid: i32,
        tid: i32,
        geometry: &TargetGeometry,
        max_depth: usize,
    ) -> io::Result<ThreadStack> {
        attach(tid)?;
        let result = (|| {
            let regs = read_regs(tid)?;
            let tls = read_tls(tid).ok();
            let frames = unwind_frames(pid, &regs, geometry, max_depth);
            Ok(ThreadStack {
                tid,
                name: thread_name(pid, tid),
                tls,
                frames,
            })
        })();
        let _ = detach(tid);
        result
    }

    fn attach(tid: i32) -> io::Result<()> {
        // SAFETY: ptrace 是系统调用封装；attach 失败时直接返回错误。
        let rc = unsafe { ptrace(PTRACE_ATTACH, tid, 0, 0) };
        if rc == -1 {
            return Err(io::Error::last_os_error());
        }
        // attach 后必须等待目标进入 stop，否则后续读取寄存器可能失败。
        let mut status = 0;
        let waited = unsafe { waitpid(tid, &mut status, 0) };
        if waited == -1 {
            // 已经把目标挂上了，等待失败时也要恢复执行，避免目标一直停住。
            let _ = detach(tid);
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn detach(tid: i32) -> io::Result<()> {
        // SAFETY: 恢复目标执行，不修改其内存。
        let rc = unsafe { ptrace(PTRACE_DETACH, tid, 0, 0) };
        if rc == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn read_regs(tid: i32) -> io::Result<UserPtRegs> {
        let mut regs = UserPtRegs::default();
        let mut iov = libc::iovec {
            iov_base: &mut regs as *mut UserPtRegs as *mut libc::c_void,
            iov_len: std::mem::size_of::<UserPtRegs>(),
        };
        // SAFETY: iovec 指向本函数栈上的有效缓冲区。
        let rc = unsafe {
            ptrace(
                PTRACE_GETREGSET,
                tid,
                NT_PRSTATUS as *mut libc::c_void,
                &mut iov as *mut libc::iovec,
            )
        };
        if rc == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(regs)
    }

    fn read_tls(tid: i32) -> io::Result<u64> {
        let mut tls: u64 = 0;
        let mut iov = libc::iovec {
            iov_base: &mut tls as *mut u64 as *mut libc::c_void,
            iov_len: std::mem::size_of::<u64>(),
        };
        // SAFETY: 内核把 TPIDR_EL0 写入 iovec 指向的 8 字节缓冲区。
        let rc = unsafe {
            ptrace(
                PTRACE_GETREGSET,
                tid,
                NT_ARM_TLS as *mut libc::c_void,
                &mut iov as *mut libc::iovec,
            )
        };
        if rc == -1 {
            return Err(io::Error::last_os_error());
        }
        Ok(tls)
    }

    /// 沿 x29 帧指针链回溯；返回地址先剥离 PAC/TBI 顶层位。
    fn unwind_frames(
        pid: i32,
        regs: &UserPtRegs,
        geometry: &TargetGeometry,
        max_depth: usize,
    ) -> Vec<StackFrame> {
        let mut frames = vec![StackFrame {
            pc: geometry.canonicalize(regs.pc),
        }];
        let mut fp = regs.regs[29];
        let mut previous_fp = 0u64;

        for _ in 0..max_depth {
            if fp == 0 || fp <= previous_fp || fp % 8 != 0 {
                break;
            }
            let Ok(data) = read_memory(pid, fp, 16) else {
                break;
            };
            let next_fp = u64::from_le_bytes(data[0..8].try_into().expect("8 字节"));
            let link = u64::from_le_bytes(data[8..16].try_into().expect("8 字节"));
            let pc = geometry.canonicalize(link);
            if pc == 0 {
                break;
            }
            frames.push(StackFrame { pc });
            previous_fp = fp;
            fp = next_fp;
        }

        frames
    }

    /// 读取目标进程内存；必须在 ptrace attach 之后调用。
    fn read_memory(pid: i32, address: u64, len: usize) -> io::Result<Vec<u8>> {
        let mut buffer = vec![0u8; len];
        let file = fs::File::open(format!("/proc/{pid}/mem"))?;
        file.read_exact_at(&mut buffer, address)?;
        Ok(buffer)
    }

    fn thread_name(pid: i32, tid: i32) -> String {
        fs::read_to_string(format!("/proc/{pid}/task/{tid}/comm"))
            .map(|name| name.trim().to_string())
            .unwrap_or_default()
    }

    /// 从 `/proc/<pid>/task/<tid>/stat` 里取进程状态字符。
    fn parse_state(stat: &str) -> Option<char> {
        let close = stat.rfind(')')?;
        stat[close + 1..].trim_start().chars().next()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn parses_state_after_comm() {
            let stat = "1234 (a b) S 1 1234 1234 0 -1 4194560";
            assert_eq!(parse_state(stat), Some('S'));
        }

        #[test]
        fn rejects_stat_without_comm() {
            assert_eq!(parse_state("garbage"), None);
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use std::io;

    use tmv_core::TargetGeometry;

    use super::{ThreadInfo, ThreadStack};

    pub fn list_threads(_pid: i32) -> io::Result<Vec<ThreadInfo>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "当前平台不支持 ptrace",
        ))
    }

    pub fn unwind_thread(
        _pid: i32,
        _tid: i32,
        _geometry: &TargetGeometry,
        _max_depth: usize,
    ) -> io::Result<ThreadStack> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "当前平台不支持 ptrace",
        ))
    }
}

pub use platform::list_threads;
pub use platform::unwind_thread;

/// 判断错误是否更像权限问题，给用户一个可执行的提示。
pub fn permission_hint(err: &io::Error) -> Option<&'static str> {
    match err.kind() {
        io::ErrorKind::PermissionDenied => Some("需要 root，或把 kernel.yama.ptrace_scope 设为 0"),
        _ => None,
    }
}
