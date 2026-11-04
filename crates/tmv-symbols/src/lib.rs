//! 地址 → 模块 / 符号 / 源码行解析。
//!
//! - 文件映射：读取 ELF 的 symtab + dynsym，按「映射地址 - 文件偏移」计算加载偏差；
//! - 源码行：用 addr2line 懒加载 DWARF；
//! - JIT：读取 perf 约定的 `/tmp/perf-<pid>.map`。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use addr2line::Loader;
use object::{Object, ObjectSymbol};
use tmv_core::ProcessSnapshot;

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
    /// 按 (路径, 加载偏差) 缓存解析结果；None 表示解析失败。
    cache: HashMap<(PathBuf, u64), Option<ModuleData>>,
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
            cache: HashMap::new(),
        }
    }

    /// 从进程快照构建模块索引，只记录元数据，ELF 在首次解析时懒加载。
    pub fn for_process(snapshot: &ProcessSnapshot) -> Self {
        let mut grouped: HashMap<(String, u64, String), (Vec<(u64, u64)>, u64)> = HashMap::new();
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
            cache: HashMap::new(),
        }
    }

    /// 合成数据用的假符号，让 `--demo` 也能展示解析结果。
    pub fn for_demo(snapshot: &ProcessSnapshot) -> Self {
        let mut jit = Vec::new();
        for vma in &snapshot.vmas {
            let parts = if vma.perms.exec { 3 } else { 1 };
            let span = vma.end.saturating_sub(vma.start);
            for index in 0..parts {
                let start = vma.start + span * index as u64 / parts;
                let end = vma.start + span * (index as u64 + 1) / parts;
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
            cache: HashMap::new(),
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
}

impl ModuleData {
    fn load(path: &Path, bias: u64) -> Option<Self> {
        let bytes = fs::read(path).ok()?;
        let object = object::File::parse(bytes.as_slice()).ok()?;

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

        Some(Self {
            symbols,
            loader: Loader::new(path).ok(),
            bias,
        })
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

fn find_jit<'a>(symbols: &'a [JitSymbol], address: u64) -> Option<&'a JitSymbol> {
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
}
