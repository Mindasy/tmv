//! 地址 → 模块 / 符号 / 源码行解析。
//!
//! - 文件映射：读取 ELF 的 symtab + dynsym，按「映射地址 - 文件偏移」计算加载偏差；
//! - 源码行：用 addr2line 懒加载 DWARF；
//! - JIT：读取 perf 约定的 `/tmp/perf-<pid>.map`；
//! - 调试信息：支持 build-id / `.gnu_debuglink` 指向的独立调试文件；
//! - CFI：解析 `.eh_frame`，为 DWARF 栈回溯提供每帧规则。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use addr2line::Loader;
use object::{Object, ObjectSymbol};
use tmv_core::ProcessSnapshot;

pub mod cfi;

pub use cfi::{FrameRules, SavedRule};

/// 一次地址解析的结果。
#[derive(Debug, Clone, Default)]
pub struct Resolved {
    pub address: u64,
    pub module: String,
    pub symbol: Option<String>,
    /// 相对符号起点的偏移。
    pub offset: u64,
    pub file: Option<String>,
    pub line: Option<u32>,
}

impl Resolved {
    /// "src/main.rs:12" 形式的位置描述。
    pub fn location(&self) -> Option<String> {
        let file = self.file.as_ref()?;
        Some(match self.line {
            Some(line) => format!("{file}:{line}"),
            None => file.clone(),
        })
    }
}

pub struct SymbolEngine {
    modules: Vec<Module>,
    jit: Vec<JitSymbol>,
    jit_module: String,
    /// [vdso] 这类只在进程内存里存在的模块。
    mem_modules: Vec<MemModule>,
    /// 按 (路径, 加载偏差) 缓存解析结果；None 表示解析失败。
    cache: HashMap<(PathBuf, u64), Option<ModuleData>>,
    /// 按 (路径, 加载偏差) 缓存 `.eh_frame`。
    cfi_cache: HashMap<(PathBuf, u64), Option<cfi::CfiData>>,
    /// 按 PC 缓存 unwind 规则，避免同一帧重复扫描 `.eh_frame`。
    rules_cache: HashMap<u64, Option<FrameRules>>,
}

#[derive(Debug, Clone)]
struct Module {
    path: PathBuf,
    name: String,
    bias: u64,
    ranges: Vec<(u64, u64)>,
}

impl Module {
    fn contains(&self, address: u64) -> bool {
        self.ranges
            .iter()
            .any(|(start, end)| address >= *start && address < *end)
    }
}

#[derive(Debug, Clone)]
pub struct JitSymbol {
    pub start: u64,
    pub end: u64,
    pub name: String,
}

/// 只在进程内存在的模块（如 `[vdso]`），从 /proc/pid/mem 读出 ELF 后解析。
struct MemModule {
    name: String,
    start: u64,
    end: u64,
    data: ModuleData,
}

struct ModuleData {
    symbols: Vec<SymbolEntry>,
    loader: Option<Loader>,
    /// 实际用于换算的加载偏差：ET_DYN 为映射基址，ET_EXEC 的 st_value 已是绝对地址。
    bias: u64,
}

#[derive(Debug, Clone)]
struct SymbolEntry {
    start: u64,
    end: u64,
    name: String,
}

impl SymbolEngine {
    pub fn empty() -> Self {
        Self {
            modules: Vec::new(),
            jit: Vec::new(),
            jit_module: "[jit]".to_string(),
            mem_modules: Vec::new(),
            cache: HashMap::new(),
            cfi_cache: HashMap::new(),
            rules_cache: HashMap::new(),
        }
    }

    /// 从进程快照构建模块索引，只记录元数据，ELF 在首次解析时懒加载。
    pub fn for_process(snapshot: &ProcessSnapshot) -> Self {
        // 键：(路径, 加载偏差, build-id)；值：(地址区间列表, 区间并集大小)。
        type ModuleKey = (String, u64, String);
        type ModuleRanges = (Vec<(u64, u64)>, u64);
        let mut grouped: HashMap<ModuleKey, ModuleRanges> = HashMap::new();
        for vma in &snapshot.vmas {
            let path = normalize_path(&vma.pathname);
            if path.is_empty() || path.starts_with('[') {
                continue;
            }
            let bias = vma.start.saturating_sub(vma.offset);
            let entry = grouped
                .entry((vma.dev.clone(), vma.inode, path))
                .or_insert_with(|| (Vec::new(), bias));
            entry.0.push((vma.start, vma.end));
            entry.1 = entry.1.min(bias);
        }

        let mut modules: Vec<Module> = grouped
            .into_iter()
            .map(|((_dev, _inode, path), (mut ranges, bias))| {
                ranges.sort_unstable();
                Module {
                    name: short_name(&path),
                    path: PathBuf::from(path),
                    bias,
                    ranges,
                }
            })
            .collect();
        modules.sort_by_key(|module| module.ranges.first().map_or(0, |range| range.0));

        Self {
            modules,
            jit: load_jit_map(snapshot.pid),
            jit_module: "[jit]".to_string(),
            mem_modules: load_memory_modules(snapshot),
            cache: HashMap::new(),
            cfi_cache: HashMap::new(),
            rules_cache: HashMap::new(),
        }
    }

    /// 合成数据用的假符号，让 `--demo` 也能展示解析结果。
    pub fn for_demo(snapshot: &ProcessSnapshot) -> Self {
        let mut jit = Vec::new();
        for vma in &snapshot.vmas {
            let parts = if vma.perms.exec { 3 } else { 1 };
            let span = vma.end.saturating_sub(vma.start);
            for index in 0..parts {
                let start = vma.start + span * index / parts;
                let end = vma.start + span * (index + 1) / parts;
                jit.push(JitSymbol {
                    start,
                    end,
                    name: format!("{}::entry{}", vma.label(), index),
                });
            }
        }
        Self {
            modules: Vec::new(),
            jit,
            jit_module: "[demo]".to_string(),
            mem_modules: Vec::new(),
            cache: HashMap::new(),
            cfi_cache: HashMap::new(),
            rules_cache: HashMap::new(),
        }
    }

    pub fn resolve(&mut self, address: u64) -> Option<Resolved> {
        if let Some(symbol) = find_jit(&self.jit, address) {
            return Some(Resolved {
                address,
                module: self.jit_module.clone(),
                symbol: Some(symbol.name.clone()),
                offset: address - symbol.start,
                file: None,
                line: None,
            });
        }

        if let Some(module) = self
            .mem_modules
            .iter()
            .find(|module| address >= module.start && address < module.end)
        {
            let (symbol, offset) = match module.data.find_symbol(address) {
                Some(entry) => (Some(entry.name.clone()), address - entry.start),
                None => (None, 0),
            };
            return Some(Resolved {
                address,
                module: module.name.clone(),
                symbol,
                offset,
                file: None,
                line: None,
            });
        }

        let module = self
            .modules
            .iter()
            .find(|module| module.contains(address))?
            .clone();
        let key = (module.path.clone(), module.bias);
        if !self.cache.contains_key(&key) {
            let data = ModuleData::load(&module.path, module.bias);
            self.cache.insert(key.clone(), data);
        }
        let data = self.cache.get(&key)?.as_ref()?;

        let (symbol, offset) = match data.find_symbol(address) {
            Some(entry) => (Some(entry.name.clone()), address - entry.start),
            None => (None, 0),
        };
        let (file, line) = data.location(address);

        Some(Resolved {
            address,
            module: module.name,
            symbol,
            offset,
            file,
            line,
        })
    }

    /// 查询 `pc` 所在模块的 DWARF unwind 规则；没有可用 CFI 时返回 None。
    pub fn frame_rules(&mut self, pc: u64) -> Option<FrameRules> {
        if let Some(cached) = self.rules_cache.get(&pc) {
            return *cached;
        }
        let rules = self.lookup_frame_rules(pc);
        self.rules_cache.insert(pc, rules);
        rules
    }

    fn lookup_frame_rules(&mut self, pc: u64) -> Option<FrameRules> {
        let module = self
            .modules
            .iter()
            .find(|module| module.contains(pc))?
            .clone();
        let key = (module.path.clone(), module.bias);
        if !self.cfi_cache.contains_key(&key) {
            let data = load_cfi(&module.path, module.bias);
            self.cfi_cache.insert(key.clone(), data);
        }
        self.cfi_cache.get(&key)?.as_ref()?.rules(pc)
    }
}

impl ModuleData {
    fn load(path: &Path, bias: u64) -> Option<Self> {
        let bytes = fs::read(path).ok()?;
        let object = object::File::parse(bytes.as_slice()).ok()?;
        let mut data = Self::from_object(&object, bias, Some(path));

        // 主文件被 strip 时，尝试用 build-id / .gnu_debuglink 指向的独立调试文件补全。
        if data.symbols.is_empty() || data.loader.is_none() {
            if let Some(debug_path) = locate_debug_file(path, &object) {
                if let Ok(debug_bytes) = fs::read(&debug_path) {
                    if let Ok(debug_object) = object::File::parse(debug_bytes.as_slice()) {
                        let debug = Self::from_object(&debug_object, bias, Some(&debug_path));
                        if data.symbols.is_empty() {
                            data.symbols = debug.symbols;
                        }
                        if data.loader.is_none() {
                            data.loader = debug.loader;
                        }
                    }
                }
            }
        }

        Some(data)
    }

    /// 直接从内存中的 ELF 构建，用于 `[vdso]` 这类没有文件后端的模块。
    fn from_bytes(bytes: &[u8], bias: u64) -> Option<Self> {
        let object = object::File::parse(bytes).ok()?;
        Some(Self::from_object(&object, bias, None))
    }

    fn from_object(object: &object::File<'_>, bias: u64, path: Option<&Path>) -> Self {
        // 非 PIE 可执行文件的 st_value 已经是绝对地址，不能再加映射基址。
        let bias = if object.kind() == object::ObjectKind::Executable {
            0
        } else {
            bias
        };

        let mut symbols = Vec::new();
        for symbol in object.symbols().chain(object.dynamic_symbols()) {
            if symbol.is_undefined() || symbol.is_common() {
                continue;
            }
            if !matches!(
                symbol.kind(),
                object::SymbolKind::Text | object::SymbolKind::Data | object::SymbolKind::Unknown
            ) {
                continue;
            }
            let Ok(name) = symbol.name() else {
                continue;
            };
            if name.is_empty() || symbol.address() == 0 {
                continue;
            }
            let start = bias.wrapping_add(symbol.address());
            let end = start.saturating_add(symbol.size().max(1));
            symbols.push(SymbolEntry {
                start,
                end,
                name: demangle(name),
            });
        }

        symbols.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
        symbols.dedup_by(|a, b| a.start == b.start && a.name == b.name);

        Self {
            symbols,
            loader: path.and_then(|path| Loader::new(path).ok()),
            bias,
        }
    }

    fn find_symbol(&self, address: u64) -> Option<&SymbolEntry> {
        let index = self.symbols.partition_point(|entry| entry.start <= address);
        self.symbols[..index]
            .iter()
            .rev()
            .take(16)
            .find(|entry| address < entry.end)
    }

    fn location(&self, address: u64) -> (Option<String>, Option<u32>) {
        let Some(loader) = self.loader.as_ref() else {
            return (None, None);
        };
        match loader.find_location(address.saturating_sub(self.bias)) {
            Ok(Some(location)) => (location.file.map(str::to_string), location.line),
            _ => (None, None),
        }
    }
}

/// 加载模块的 `.eh_frame`；主文件没有时改用独立调试文件。
fn load_cfi(path: &Path, bias: u64) -> Option<cfi::CfiData> {
    if let Some(data) = cfi::CfiData::load(path, bias) {
        return Some(data);
    }
    let bytes = fs::read(path).ok()?;
    let object = object::File::parse(bytes.as_slice()).ok()?;
    let debug_path = locate_debug_file(path, &object)?;
    cfi::CfiData::load(&debug_path, bias)
}

/// build-id 对应的独立调试文件路径：`/usr/lib/debug/.build-id/ab/cdef....debug`。
pub fn build_id_debug_path(build_id: &[u8]) -> Option<PathBuf> {
    if build_id.len() < 2 {
        return None;
    }
    let hex: String = build_id.iter().map(|byte| format!("{byte:02x}")).collect();
    let (head, tail) = hex.split_at(2);
    Some(PathBuf::from(format!(
        "/usr/lib/debug/.build-id/{head}/{tail}.debug"
    )))
}

/// `.gnu_debuglink` 的候选路径：模块同目录、`/usr/lib/debug/<模块目录>`。
pub fn debuglink_candidates(module: &Path, link: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(parent) = module.parent() {
        candidates.push(parent.join(link));
        if let Ok(relative) = module.strip_prefix("/") {
            let debug = Path::new("/usr/lib/debug").join(relative);
            if let Some(parent) = debug.parent() {
                candidates.push(parent.join(link));
            }
        }
    }
    candidates
}

/// 找出模块对应的独立调试文件：优先 build-id，其次 `.gnu_debuglink`。
fn locate_debug_file(module: &Path, object: &object::File<'_>) -> Option<PathBuf> {
    let candidates = object
        .build_id()
        .ok()
        .flatten()
        .and_then(build_id_debug_path)
        .into_iter()
        .chain(
            object
                .gnu_debuglink()
                .ok()
                .flatten()
                .map(|(link, _)| String::from_utf8_lossy(link).into_owned())
                .map(|link| debuglink_candidates(module, &link))
                .unwrap_or_default(),
        );
    candidates.into_iter().find(|path| path.is_file())
}

/// 从 /proc/pid/mem 读取一段内存；需要 ptrace 权限，失败时返回错误。
fn read_process_memory(pid: i32, address: u64, len: usize) -> std::io::Result<Vec<u8>> {
    use std::os::unix::fs::FileExt;

    let mut buffer = vec![0u8; len];
    let file = fs::File::open(format!("/proc/{pid}/mem"))?;
    file.read_exact_at(&mut buffer, address)?;
    Ok(buffer)
}

/// 解析 `[vdso]` / `[vsyscall]` 这类只存在于进程内存的模块。
fn load_memory_modules(snapshot: &ProcessSnapshot) -> Vec<MemModule> {
    const MAX_SIZE: u64 = 256 * 1024;
    let mut modules = Vec::new();
    for vma in &snapshot.vmas {
        let name = normalize_path(&vma.pathname);
        if name != "[vdso]" && name != "[vsyscall]" {
            continue;
        }
        let size = vma.len().min(MAX_SIZE) as usize;
        if size == 0 {
            continue;
        }
        let Ok(bytes) = read_process_memory(snapshot.pid, vma.start, size) else {
            continue;
        };
        let Some(data) = ModuleData::from_bytes(&bytes, vma.start) else {
            continue;
        };
        modules.push(MemModule {
            name,
            start: vma.start,
            end: vma.end,
            data,
        });
    }
    modules
}

fn find_jit(symbols: &[JitSymbol], address: u64) -> Option<&JitSymbol> {
    let index = symbols.partition_point(|symbol| symbol.start <= address);
    symbols[..index]
        .iter()
        .rev()
        .take(8)
        .find(|symbol| address < symbol.end)
}

fn normalize_path(path: &str) -> String {
    path.strip_suffix(" (deleted)").unwrap_or(path).to_string()
}

fn short_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn demangle(name: &str) -> String {
    match rustc_demangle::try_demangle(name) {
        Ok(value) => format!("{value:#}"),
        Err(_) => name.to_string(),
    }
}

fn load_jit_map(pid: i32) -> Vec<JitSymbol> {
    let Ok(text) = fs::read_to_string(format!("/tmp/perf-{pid}.map")) else {
        return Vec::new();
    };
    parse_jit_map(&text)
}

/// 解析 perf 约定的 JIT map：每行 `起始地址 长度 名称`，地址为十六进制。
pub fn parse_jit_map(text: &str) -> Vec<JitSymbol> {
    let mut symbols: Vec<JitSymbol> = text
        .lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, ' ');
            let start = u64::from_str_radix(parts.next()?, 16).ok()?;
            let size = u64::from_str_radix(parts.next()?, 16).ok()?;
            let name = parts.next()?.trim();
            if name.is_empty() || size == 0 {
                return None;
            }
            Some(JitSymbol {
                start,
                end: start.saturating_add(size),
                name: name.to_string(),
            })
        })
        .collect();
    symbols.sort_by_key(|symbol| symbol.start);
    symbols
}

#[cfg(test)]
mod tests {
    use super::*;
    use tmv_core::ProcessSnapshot;
    use tmv_core::Vma;

    fn vma(start: u64, end: u64, offset: u64, path: &str) -> Vma {
        Vma {
            start,
            end,
            perms: tmv_core::Perms::parse("r-xp").unwrap(),
            offset,
            dev: "fd:01".into(),
            inode: 7,
            pathname: path.into(),
            stats: None,
        }
    }

    #[test]
    fn jit_map_parses_hex_and_spaces() {
        let text = "\
1000 20 jit_fn
2000 10 Scala::main
bad line
3000 0 empty
4000 ff name with spaces
";
        let symbols = parse_jit_map(text);
        assert_eq!(symbols.len(), 3);
        assert_eq!(symbols[0].start, 0x1000);
        assert_eq!(symbols[1].name, "Scala::main");
        assert_eq!(symbols[2].name, "name with spaces");
    }

    #[test]
    fn module_bias_comes_from_start_minus_offset() {
        let snapshot = ProcessSnapshot::from_vmas(
            42,
            vec![
                vma(0x10000, 0x11000, 0x1000, "/lib/libc.so.6"),
                vma(0x11000, 0x13000, 0x2000, "/lib/libc.so.6"),
            ],
            true,
        );
        let engine = SymbolEngine::for_process(&snapshot);
        assert_eq!(engine.modules.len(), 1);
        assert_eq!(engine.modules[0].bias, 0xf000);
        assert!(engine.modules[0].contains(0x12000));
        assert!(!engine.modules[0].contains(0x9000));
    }

    #[test]
    fn symbol_lookup_prefers_covering_entry() {
        let data = ModuleData {
            symbols: vec![
                SymbolEntry {
                    start: 0x1000,
                    end: 0x1100,
                    name: "foo".into(),
                },
                SymbolEntry {
                    start: 0x1100,
                    end: 0x1200,
                    name: "bar".into(),
                },
            ],
            loader: None,
            bias: 0,
        };
        assert_eq!(data.find_symbol(0x10ff).unwrap().name, "foo");
        assert_eq!(data.find_symbol(0x1100).unwrap().name, "bar");
        assert!(data.find_symbol(0x0fff).is_none());
        assert!(data.find_symbol(0x2000).is_none());
    }

    #[test]
    fn deleted_suffix_is_stripped() {
        assert_eq!(normalize_path("/tmp/lib.so (deleted)"), "/tmp/lib.so");
        assert_eq!(normalize_path("/tmp/lib.so"), "/tmp/lib.so");
    }

    #[test]
    fn demo_symbols_cover_vmas() {
        let snapshot = tmv_core::demo::snapshot();
        let mut engine = SymbolEngine::for_demo(&snapshot);
        let resolved = engine.resolve(snapshot.vmas[0].start + 0x10).unwrap();
        assert_eq!(resolved.module, "[demo]");
        assert!(resolved.symbol.unwrap().starts_with("/usr/bin/tmv"));
    }

    #[test]
    fn loads_symbols_from_current_executable() {
        let exe = std::env::current_exe().expect("当前测试可执行文件");
        let data = ModuleData::load(&exe, 0).expect("解析自身符号表");
        assert!(!data.symbols.is_empty(), "符号表为空");
    }

    #[test]
    fn build_id_maps_to_debug_path() {
        let path = build_id_debug_path(&[0xab, 0xcd, 0x01, 0x23]).expect("至少 2 字节");
        assert_eq!(
            path,
            PathBuf::from("/usr/lib/debug/.build-id/ab/cd0123.debug")
        );
        assert!(build_id_debug_path(&[0xab]).is_none());
    }

    #[test]
    fn debuglink_candidates_cover_module_and_debug_root() {
        let candidates = debuglink_candidates(Path::new("/usr/lib/libdemo.so.1"), "libdemo.debug");
        assert_eq!(
            candidates,
            vec![
                PathBuf::from("/usr/lib/libdemo.debug"),
                PathBuf::from("/usr/lib/debug/usr/lib/libdemo.debug"),
            ]
        );
    }

    #[test]
    fn parses_elf_from_bytes() {
        let exe = std::env::current_exe().expect("当前测试可执行文件");
        let bytes = std::fs::read(exe).expect("读取自身");
        let data = ModuleData::from_bytes(&bytes, 0).expect("从内存字节解析 ELF");
        assert!(!data.symbols.is_empty());
        assert!(
            data.loader.is_none(),
            "内存 ELF 没有文件路径，不应有 DWARF loader"
        );
    }

    #[test]
    fn vdso_is_skipped_when_memory_is_unreadable() {
        let snapshot = ProcessSnapshot::from_vmas(
            2_000_000_000,
            vec![vma(0x7fff_0000, 0x7fff_2000, 0, "[vdso]")],
            true,
        );
        assert!(load_memory_modules(&snapshot).is_empty());
    }
}
