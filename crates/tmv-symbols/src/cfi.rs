//! DWARF CFI（`.eh_frame`）规则解析。
//!
//! 只覆盖「CFA = 寄存器 + 偏移」且返回地址 / 帧指针保存在 `CFA + 偏移` 的常见情形，
//! 其余规则（表达式、值偏移）返回 None，由调用方回退到帧指针回溯。

use std::path::Path;

use gimli::{
    BaseAddresses, CfaRule, EhFrame, Register, RegisterRule, RunTimeEndian, UnwindContext,
    UnwindSection, UnwindTableRow,
};
use object::{Object, ObjectSection};

/// 寄存器保存位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavedRule {
    /// 值保存在 `CFA + offset` 指向的内存。
    Offset(i64),
    /// 值仍在当前寄存器里（叶子函数常见）。
    SameValue,
}

/// 单帧的 unwind 规则。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRules {
    pub cfa_register: u16,
    pub cfa_offset: i64,
    pub ra_register: u16,
    pub ra_rule: SavedRule,
    pub fp_register: Option<u16>,
    pub fp_rule: Option<SavedRule>,
}

pub(crate) struct CfiData {
    eh_frame: Vec<u8>,
    eh_frame_addr: u64,
    endian: RunTimeEndian,
    ra_register: u16,
    fp_register: Option<u16>,
}

impl CfiData {
    pub(crate) fn load(path: &Path, bias: u64) -> Option<Self> {
        let bytes = std::fs::read(path).ok()?;
        Self::from_bytes(&bytes, bias)
    }

    pub(crate) fn from_bytes(bytes: &[u8], bias: u64) -> Option<Self> {
        let object = object::File::parse(bytes).ok()?;
        // Mach-O 的段名是 __eh_frame，ELF 是 .eh_frame。
        let section = object
            .section_by_name(".eh_frame")
            .or_else(|| object.section_by_name("__eh_frame"))?;
        let eh_frame = section.data().ok()?.to_vec();
        if eh_frame.is_empty() {
            return None;
        }
        let bias = if object.kind() == object::ObjectKind::Executable {
            0
        } else {
            bias
        };
        let (ra_register, fp_register) = registers(object.architecture());
        Some(Self {
            eh_frame,
            eh_frame_addr: section.address().wrapping_add(bias),
            endian: if object.is_little_endian() {
                RunTimeEndian::Little
            } else {
                RunTimeEndian::Big
            },
            ra_register,
            fp_register,
        })
    }

    /// 查询 `pc` 所在指令的 unwind 规则；没有可用 CFI 时返回 None。
    pub(crate) fn rules(&self, pc: u64) -> Option<FrameRules> {
        let eh_frame = EhFrame::new(&self.eh_frame, self.endian);
        let bases = BaseAddresses::default().set_eh_frame(self.eh_frame_addr);
        let mut context = UnwindContext::new();
        let row = eh_frame
            .unwind_info_for_address(&bases, &mut context, pc, EhFrame::cie_from_offset)
            .ok()?;

        let (cfa_register, cfa_offset) = match row.cfa() {
            CfaRule::RegisterAndOffset { register, offset } => (register.0, *offset),
            _ => return None,
        };
        // gimli 0.32 的 `register()` 会把「未提及」也报成 Undefined，
        // 这里用 `registers()` 区分「未修改」与「显式 undefined」。
        let ra_rule = match declared_rule(row, Register(self.ra_register)) {
            None => SavedRule::SameValue,
            Some(RegisterRule::Undefined) => return None,
            Some(rule) => saved_rule(rule)?,
        };
        let fp_rule = self
            .fp_register
            .and_then(|register| declared_rule(row, Register(register)).and_then(saved_rule));

        Some(FrameRules {
            cfa_register,
            cfa_offset,
            ra_register: self.ra_register,
            ra_rule,
            fp_register: self.fp_register,
            fp_rule,
        })
    }
}

fn saved_rule(rule: RegisterRule<usize>) -> Option<SavedRule> {
    match rule {
        RegisterRule::Offset(offset) => Some(SavedRule::Offset(offset)),
        RegisterRule::SameValue => Some(SavedRule::SameValue),
        _ => None,
    }
}

/// 只在该寄存器确实出现在 unwind 表里时返回值；未提及返回 None。
fn declared_rule(row: &UnwindTableRow<usize>, register: Register) -> Option<RegisterRule<usize>> {
    row.registers()
        .find(|(candidate, _)| *candidate == register)
        .map(|(_, rule)| rule.clone())
}

/// 各架构的返回地址 / 帧指针 DWARF 寄存器编号。
fn registers(architecture: object::Architecture) -> (u16, Option<u16>) {
    use object::Architecture::*;
    match architecture {
        Aarch64 | Aarch64_Ilp32 => (30, Some(29)),
        X86_64 | X86_64_X32 => (16, Some(6)),
        _ => (30, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use object::ObjectSymbol;

    fn current_exe_bytes() -> Vec<u8> {
        let exe = std::env::current_exe().expect("当前测试可执行文件");
        std::fs::read(exe).expect("读取自身")
    }

    #[test]
    fn loads_eh_frame_from_current_executable() {
        let bytes = current_exe_bytes();
        let cfi = CfiData::from_bytes(&bytes, 0).expect("自身应带 .eh_frame");
        assert!(!cfi.eh_frame.is_empty());
        assert!(cfi.ra_register == 16 || cfi.ra_register == 30);
    }

    #[test]
    fn rules_are_found_on_linux() {
        let bytes = current_exe_bytes();
        let Some(cfi) = CfiData::from_bytes(&bytes, 0) else {
            return;
        };
        let object = object::File::parse(bytes.as_slice()).expect("解析自身 ELF");

        // 只抽样前 200 个文本符号，避免在 macOS 上逐个线性扫描 .eh_frame 太慢。
        let symbols: Vec<u64> = object
            .symbols()
            .chain(object.dynamic_symbols())
            .filter(|symbol| symbol.kind() == object::SymbolKind::Text && symbol.address() != 0)
            .map(|symbol| symbol.address())
            .take(200)
            .collect();
        let hits = symbols
            .iter()
            .filter(|pc| cfi.rules(**pc).is_some())
            .count();

        if cfg!(target_os = "linux") {
            assert!(hits > 0, "Linux 上应有可用 CFI，实际 0/{}", symbols.len());
        }
        // 任何平台都不应崩溃；命中时规则结构必须自洽。
        if let Some(rules) = symbols.iter().find_map(|pc| cfi.rules(*pc)) {
            assert!(
                rules.cfa_register != rules.ra_register || rules.ra_rule == SavedRule::SameValue
            );
            assert!(rules.fp_register.is_some());
        }
    }

    #[test]
    fn rejects_bytes_without_eh_frame() {
        assert!(CfiData::from_bytes(b"not an elf", 0).is_none());
    }

    #[test]
    fn undefined_register_rule_cannot_be_restored() {
        assert_eq!(saved_rule(RegisterRule::Undefined), None);
        assert_eq!(
            saved_rule(RegisterRule::SameValue),
            Some(SavedRule::SameValue)
        );
        assert_eq!(
            saved_rule(RegisterRule::Offset(8)),
            Some(SavedRule::Offset(8))
        );
        assert_eq!(saved_rule(RegisterRule::ValOffset(8)), None);
    }
}
