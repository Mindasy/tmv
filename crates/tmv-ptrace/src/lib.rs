//! ptrace 侵入式能力：线程枚举、寄存器、栈回溯与 TLS。
//!
//! 仅 Linux 上真正实现；其他平台返回 `Unsupported`，demo 数据不依赖 ptrace。
//! 回溯优先使用 DWARF CFI（`.eh_frame`），不可用时回退到 x29 帧指针链。

use std::io;

use tmv_symbols::{FrameRules, SavedRule};

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

/// 本次回溯实际使用的算法。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UnwindMethod {
    #[default]
    FramePointer,
    Dwarf,
}

impl UnwindMethod {
    pub fn label(self) -> &'static str {
        match self {
            UnwindMethod::FramePointer => "帧指针",
            UnwindMethod::Dwarf => "DWARF",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ThreadStack {
    pub tid: i32,
    pub name: String,
    /// arm64 的 TPIDR_EL0，读取失败时为 None。
    pub tls: Option<u64>,
    pub frames: Vec<StackFrame>,
    pub method: UnwindMethod,
}

/// 回溯需要的寄存器快照。
///
/// 只保留栈回溯实际用到的四个寄存器，取值统一走 [`Registers::get`]，
/// 这样 CFI 规则里的 DWARF 编号不必关心底层架构。
#[derive(Debug, Clone, Copy, Default)]
pub struct Registers {
    pub pc: u64,
    pub sp: u64,
    pub fp: u64,
    /// 返回地址寄存器（arm64 的 x30）；x86_64 没有链接寄存器，与 `pc` 相同。
    pub ra: u64,
}

// 各架构在 DWARF 里的寄存器编号。
#[cfg(target_arch = "x86_64")]
const DWARF_SP: u16 = 7;
#[cfg(target_arch = "x86_64")]
const DWARF_FP: u16 = 6;
#[cfg(target_arch = "x86_64")]
const DWARF_RA: u16 = 16;

#[cfg(not(target_arch = "x86_64"))]
const DWARF_SP: u16 = 31;
#[cfg(not(target_arch = "x86_64"))]
const DWARF_FP: u16 = 29;
#[cfg(not(target_arch = "x86_64"))]
const DWARF_RA: u16 = 30;

impl Registers {
    /// 按 DWARF 编号取寄存器值；只认当前的 SP / FP / RA，其余编号返回 None。
    pub fn get(&self, number: u16) -> Option<u64> {
        match number {
            DWARF_SP => Some(self.sp),
            DWARF_FP => Some(self.fp),
            DWARF_RA => Some(self.ra),
            _ => None,
        }
    }
}

/// 调用者的一帧。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameStep {
    pub pc: u64,
    pub sp: u64,
    pub fp: u64,
}

/// 按 CFI 规则计算调用者的一帧；规则不支持或读内存失败时返回 None。
pub fn step_frame(
    rules: &FrameRules,
    registers: &Registers,
    mut read: impl FnMut(u64) -> Option<u64>,
) -> Option<FrameStep> {
    let base = registers.get(rules.cfa_register)?;
    let cfa = add_signed(base, rules.cfa_offset);

    let pc = match rules.ra_rule {
        SavedRule::Offset(offset) => read(add_signed(cfa, offset))?,
        SavedRule::SameValue => registers.get(rules.ra_register)?,
    };
    // 没有规则表示寄存器未被修改，保持当前值。
    let fp = match rules.fp_rule {
        Some(SavedRule::Offset(offset)) => read(add_signed(cfa, offset)).unwrap_or(0),
        _ => rules
            .fp_register
            .and_then(|register| registers.get(register))
            .unwrap_or(0),
    };
    Some(FrameStep { pc, sp: cfa, fp })
}

fn add_signed(base: u64, offset: i64) -> u64 {
    if offset >= 0 {
        base.wrapping_add(offset as u64)
    } else {
        base.wrapping_sub(offset.unsigned_abs())
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::fs;
    use std::io;
    use std::os::unix::fs::FileExt;

    use tmv_core::TargetGeometry;
    use tmv_symbols::SymbolEngine;

    use super::{step_frame, Registers, StackFrame, ThreadInfo, ThreadStack, UnwindMethod};

    // 自己声明 ptrace，避免 glibc/musl 对 request 参数类型（c_uint / c_int）的差异。
    const PTRACE_ATTACH: u32 = 16;
    const PTRACE_DETACH: u32 = 17;
    const PTRACE_GETREGSET: u32 = 0x4204;
    const NT_PRSTATUS: usize = 1;
    #[cfg(target_arch = "aarch64")]
    const NT_ARM_TLS: usize = 0x401;

    extern "C" {
        fn ptrace(request: u32, ...) -> libc::c_long;
        fn waitpid(pid: libc::pid_t, status: *mut libc::c_int, options: libc::c_int)
            -> libc::pid_t;
    }

    /// arm64 的 `struct user_pt_regs`。
    #[cfg(target_arch = "aarch64")]
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct UserRegs {
        regs: [u64; 31],
        sp: u64,
        pc: u64,
        pstate: u64,
    }

    #[cfg(target_arch = "aarch64")]
    impl UserRegs {
        fn pc(&self) -> u64 {
            self.pc
        }

        fn sp(&self) -> u64 {
            self.sp
        }

        fn fp(&self) -> u64 {
            self.regs[29]
        }

        fn ra(&self) -> u64 {
            self.regs[30]
        }
    }

    /// x86_64 的 `struct user_regs_struct`（27 个 unsigned long）。
    #[cfg(target_arch = "x86_64")]
    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct UserRegs {
        raw: [u64; 27],
    }

    #[cfg(target_arch = "x86_64")]
    impl UserRegs {
        fn pc(&self) -> u64 {
            self.raw[16]
        }

        fn sp(&self) -> u64 {
            self.raw[19]
        }

        fn fp(&self) -> u64 {
            self.raw[4]
        }

        /// x86_64 没有链接寄存器，返回地址在栈上。
        fn ra(&self) -> u64 {
            self.pc()
        }

        /// `fs_base` 即 TLS 基址。
        fn tls(&self) -> u64 {
            self.raw[21]
        }
    }

    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    compile_error!("tmv-ptrace 的寄存器布局只实现了 aarch64 与 x86_64");

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
        engine: Option<&mut SymbolEngine>,
        max_depth: usize,
    ) -> io::Result<ThreadStack> {
        attach(tid)?;
        let result = (|| {
            let regs = read_regs(tid)?;
            let tls = read_tls(tid, &regs).ok();
            let mut memory = TargetMemory::new(pid);
            let (frames, method) = unwind(&mut memory, &regs, geometry, engine, max_depth);
            Ok(ThreadStack {
                tid,
                name: thread_name(pid, tid),
                tls,
                frames,
                method,
            })
        })();
        let _ = detach(tid);
        result
    }

    /// 先尝试 DWARF CFI，走不动时回退到帧指针链。
    fn unwind(
        memory: &mut TargetMemory,
        regs: &UserRegs,
        geometry: &TargetGeometry,
        engine: Option<&mut SymbolEngine>,
        max_depth: usize,
    ) -> (Vec<StackFrame>, UnwindMethod) {
        if let Some(engine) = engine {
            if let Some(frames) = unwind_with_cfi(memory, regs, geometry, engine, max_depth) {
                return (frames, UnwindMethod::Dwarf);
            }
        }
        (
            unwind_with_frame_pointer(memory, regs, geometry, max_depth),
            UnwindMethod::FramePointer,
        )
    }

    /// 按 `.eh_frame` 规则逐帧回溯；第一帧就走不动时返回 None，交由调用方回退。
    fn unwind_with_cfi(
        memory: &mut TargetMemory,
        regs: &UserRegs,
        geometry: &TargetGeometry,
        engine: &mut SymbolEngine,
        max_depth: usize,
    ) -> Option<Vec<StackFrame>> {
        let start = geometry.canonicalize(regs.pc());
        if start == 0 {
            return None;
        }

        let mut registers = Registers {
            pc: start,
            sp: regs.sp(),
            fp: regs.fp(),
            ra: regs.ra(),
        };
        let mut frames = vec![StackFrame { pc: start }];
        // 递归调用会重复同一个返回地址，只能按 (pc, sp) 去重。
        let mut seen = vec![(start, registers.sp)];

        for _ in 0..max_depth {
            // 返回地址可能落在 call 指令之后，查规则时退一个字节更稳。
            let probe = if frames.len() == 1 {
                registers.pc
            } else {
                registers.pc.saturating_sub(1)
            };
            let Some(rules) = engine.frame_rules(probe) else {
                break;
            };
            let Some(step) = step_frame(&rules, &registers, |address| memory.read_u64(address))
            else {
                break;
            };

            let pc = geometry.canonicalize(step.pc);
            // 叶子函数的 CFA 就是 sp 本身，允许相等；只拒绝栈指针回退。
            if pc == 0 || step.sp < registers.sp || seen.contains(&(pc, step.sp)) {
                break;
            }

            registers.sp = step.sp;
            registers.pc = pc;
            // 新一帧的返回地址寄存器就是本帧 pc。
            registers.ra = pc;
            if step.fp != 0 {
                registers.fp = step.fp;
            }
            frames.push(StackFrame { pc });
            seen.push((pc, step.sp));
        }

        (frames.len() > 1).then_some(frames)
    }

    /// 沿帧指针链（arm64 的 x29 / x86_64 的 rbp）回溯；返回地址先剥离 PAC/TBI 顶层位。
    fn unwind_with_frame_pointer(
        memory: &mut TargetMemory,
        regs: &UserRegs,
        geometry: &TargetGeometry,
        max_depth: usize,
    ) -> Vec<StackFrame> {
        let mut frames = vec![StackFrame {
            pc: geometry.canonicalize(regs.pc()),
        }];
        let mut fp = regs.fp();
        let mut previous_fp = 0u64;

        for _ in 0..max_depth {
            if fp == 0 || fp <= previous_fp || fp % 8 != 0 {
                break;
            }
            let Some((next_fp, link)) = memory.read_pair(fp) else {
                break;
            };
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

    /// 读取目标进程内存；文件句柄复用，避免逐帧重新打开 `/proc/<pid>/mem`。
    struct TargetMemory {
        pid: i32,
        file: Option<fs::File>,
    }

    impl TargetMemory {
        fn new(pid: i32) -> Self {
            Self { pid, file: None }
        }

        fn read(&mut self, address: u64, len: usize) -> Option<Vec<u8>> {
            let pid = self.pid;
            let file = match &mut self.file {
                Some(file) => file,
                slot => slot.insert(fs::File::open(format!("/proc/{pid}/mem")).ok()?),
            };
            let mut buffer = vec![0u8; len];
            file.read_exact_at(&mut buffer, address).ok()?;
            Some(buffer)
        }

        fn read_u64(&mut self, address: u64) -> Option<u64> {
            let data = self.read(address, 8)?;
            Some(u64::from_le_bytes(data[0..8].try_into().ok()?))
        }

        /// 读取帧指针对 `(next_fp, link)`。
        fn read_pair(&mut self, address: u64) -> Option<(u64, u64)> {
            let data = self.read(address, 16)?;
            let next_fp = u64::from_le_bytes(data[0..8].try_into().ok()?);
            let link = u64::from_le_bytes(data[8..16].try_into().ok()?);
            Some((next_fp, link))
        }
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

    fn read_regs(tid: i32) -> io::Result<UserRegs> {
        let mut regs = UserRegs::default();
        let mut iov = libc::iovec {
            iov_base: &mut regs as *mut UserRegs as *mut libc::c_void,
            iov_len: std::mem::size_of::<UserRegs>(),
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

    /// arm64 的 TLS 基址来自 TPIDR_EL0，需要单独发一次 ptrace。
    #[cfg(target_arch = "aarch64")]
    fn read_tls(tid: i32, _regs: &UserRegs) -> io::Result<u64> {
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

    /// x86_64 的 TLS 基址就是已经读到的 `fs_base`。
    #[cfg(target_arch = "x86_64")]
    fn read_tls(_tid: i32, regs: &UserRegs) -> io::Result<u64> {
        Ok(regs.tls())
    }

    /// 读取目标进程内存，返回实际读到的字节（遇到未映射区间会短读）。
    ///
    /// 与栈回溯不同，这里不 attach：`/proc/<pid>/mem` 的读权限由 ptrace 访问检查决定。
    pub fn read_memory(pid: i32, address: u64, len: usize) -> io::Result<Vec<u8>> {
        let file = fs::File::open(format!("/proc/{pid}/mem"))?;
        let mut buffer = vec![0u8; len];
        let read = file.read_at(&mut buffer, address)?;
        buffer.truncate(read);
        if buffer.is_empty() {
            return Err(io::Error::other(
                "地址不可读：可能未映射，或需要 ptrace 权限",
            ));
        }
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

        #[test]
        fn read_memory_reads_own_process() {
            let value: u64 = 0x1234_5678_9abc_def0;
            let address = &value as *const u64 as u64;
            let bytes = read_memory(std::process::id() as i32, address, 8).expect("读取自身内存");
            assert_eq!(bytes.len(), 8);
            assert_eq!(
                u64::from_le_bytes(bytes[..8].try_into().unwrap()),
                value,
                "读到的内容应与栈上的值一致"
            );
        }

        /// 未映射地址应返回明确的错误，而不是静默返回全 0。
        #[test]
        fn read_memory_rejects_unmapped_address() {
            // 用户态最低页通常不可映射，读取会失败。
            assert!(read_memory(std::process::id() as i32, 0, 8).is_err());
        }

        /// 结构体大小必须和内核的 `struct user_pt_regs` / `user_regs_struct` 一致，
        /// 否则 `PTRACE_GETREGSET` 会写越界或读不全。
        #[test]
        fn register_layout_matches_kernel() {
            #[cfg(target_arch = "aarch64")]
            assert_eq!(std::mem::size_of::<UserRegs>(), 34 * 8);
            #[cfg(target_arch = "x86_64")]
            assert_eq!(std::mem::size_of::<UserRegs>(), 27 * 8);
        }

        #[cfg(target_arch = "aarch64")]
        #[test]
        fn aarch64_registers_map_to_dwarf_numbers() {
            let mut regs = UserRegs::default();
            regs.pc = 0x1111;
            regs.sp = 0x2222;
            regs.regs[29] = 0x3333;
            regs.regs[30] = 0x4444;
            assert_eq!(regs.pc(), 0x1111);
            assert_eq!(regs.sp(), 0x2222);
            assert_eq!(regs.fp(), 0x3333);
            assert_eq!(regs.ra(), 0x4444);
        }

        #[cfg(target_arch = "x86_64")]
        #[test]
        fn x86_64_registers_map_to_dwarf_numbers() {
            let mut regs = UserRegs::default();
            regs.raw[16] = 0x1111; // rip
            regs.raw[19] = 0x2222; // rsp
            regs.raw[4] = 0x3333; // rbp
            regs.raw[21] = 0x5555; // fs_base
            assert_eq!(regs.pc(), 0x1111);
            assert_eq!(regs.sp(), 0x2222);
            assert_eq!(regs.fp(), 0x3333);
            // x86_64 没有链接寄存器，RA 与 PC 相同。
            assert_eq!(regs.ra(), 0x1111);
            assert_eq!(regs.tls(), 0x5555);
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use std::io;

    use tmv_core::TargetGeometry;
    use tmv_symbols::SymbolEngine;

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
        _engine: Option<&mut SymbolEngine>,
        _max_depth: usize,
    ) -> io::Result<ThreadStack> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "当前平台不支持 ptrace",
        ))
    }

    pub fn read_memory(_pid: i32, _address: u64, _len: usize) -> io::Result<Vec<u8>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "当前平台不支持读取进程内存",
        ))
    }
}

pub use platform::list_threads;
pub use platform::read_memory;
pub use platform::unwind_thread;

/// 判断错误是否更像权限问题，给用户一个可执行的提示。
pub fn permission_hint(err: &io::Error) -> Option<&'static str> {
    match err.kind() {
        io::ErrorKind::PermissionDenied => Some("需要 root，或把 kernel.yama.ptrace_scope 设为 0"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(cfa_register: u16, cfa_offset: i64, ra_rule: SavedRule) -> FrameRules {
        FrameRules {
            cfa_register,
            cfa_offset,
            ra_register: DWARF_RA,
            ra_rule,
            fp_register: Some(DWARF_FP),
            fp_rule: Some(SavedRule::Offset(-16)),
        }
    }

    #[test]
    fn registers_map_dwarf_numbers() {
        let registers = Registers {
            pc: 0x1000,
            sp: 0x2000,
            fp: 0x3000,
            ra: 0x4000,
        };
        assert_eq!(registers.get(DWARF_SP), Some(0x2000));
        assert_eq!(registers.get(DWARF_FP), Some(0x3000));
        assert_eq!(registers.get(DWARF_RA), Some(0x4000));
        // 其余编号不可用，CFI 规则命中时会回退到帧指针。
        assert_eq!(registers.get(DWARF_SP + 1), None);
    }

    #[test]
    fn steps_caller_from_cfa_offset() {
        let registers = Registers {
            sp: 0x1000,
            pc: 0x2000,
            fp: 0,
            ra: 0,
        };
        // CFA = sp + 32 = 0x1020，RA 在 CFA-8，FP 在 CFA-16。
        let rules = rules(DWARF_SP, 32, SavedRule::Offset(-8));
        let step = step_frame(&rules, &registers, |address| match address {
            0x1018 => Some(0xdead_beef),
            0x1010 => Some(0x3000),
            _ => None,
        })
        .expect("应能走出一帧");
        assert_eq!(
            step,
            FrameStep {
                pc: 0xdead_beef,
                sp: 0x1020,
                fp: 0x3000,
            }
        );
    }

    #[test]
    fn leaf_frame_keeps_link_register() {
        let registers = Registers {
            sp: 0x1000,
            pc: 0x2000,
            fp: 0x5000,
            ra: 0x4444,
        };
        let mut rules = rules(DWARF_SP, 16, SavedRule::SameValue);
        rules.fp_rule = None;

        let step = step_frame(&rules, &registers, |_| None).expect("叶子帧不需要读内存");
        assert_eq!(step.pc, 0x4444);
        assert_eq!(step.sp, 0x1010);
        assert_eq!(step.fp, 0x5000);
    }

    #[test]
    fn unknown_cfa_register_is_rejected() {
        let registers = Registers::default();
        // 40 不是当前架构的 sp / fp / ra 编号。
        let rules = rules(40, 0, SavedRule::Offset(0));
        assert!(step_frame(&rules, &registers, |_| Some(1)).is_none());
    }

    #[test]
    fn negative_cfa_offset_is_applied() {
        let registers = Registers {
            sp: 0x2000,
            pc: 0,
            fp: 0,
            ra: 0,
        };
        let rules = rules(DWARF_SP, -16, SavedRule::Offset(8));
        let step = step_frame(&rules, &registers, |address| {
            (address == 0x1ff8).then_some(0x1234)
        })
        .expect("负偏移应下移 CFA");
        assert_eq!(step.sp, 0x1ff0);
        assert_eq!(step.pc, 0x1234);
    }
}
