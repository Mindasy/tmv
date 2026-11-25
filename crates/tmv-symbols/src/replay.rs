//! Resolve missing allocation symbols using the recording's module identities.
use super::*;
use std::io;
use tmv_core::session::{self, Frame};

fn missing(s: &str) -> bool {
    s.strip_prefix("0x")
        .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        || s.contains("!?+0x")
}

pub fn frame(dir: &Path, index: usize, pid: u32) -> io::Result<Frame> {
    let mut frame = session::frame(dir, index)?;
    recover(dir, pid, &mut frame);
    let mut names = HashMap::new();
    for a in &mut frame.allocations {
        for f in a.stack.iter_mut().chain(&mut a.resize_stack) {
            if let Some((module, raw, offset)) = site(&f.symbol) {
                if raw.starts_with("_Z") {
                    let pretty = names.entry(raw.to_owned()).or_insert_with(|| demangle(raw));
                    f.symbol = format!("{module}!{pretty}+0x{offset}");
                }
            }
        }
        let direct = site(&a.label);
        if missing(&a.label) || direct.is_some() {
            let mut stack = a.stack.iter().filter_map(|f| site(&f.symbol));
            if let Some((module, raw, _)) = direct
                .into_iter()
                .chain(stack.clone())
                .filter(|(_, name, _)| *name != "?")
                .find(|(_, name, _)| !name.starts_with("operator new") && !name.starts_with("_Zn"))
                .or_else(|| direct.filter(|(_, n, _)| *n != "?"))
                .or_else(|| stack.find(|(_, n, _)| *n != "?"))
            {
                let pretty = names.entry(raw.to_owned()).or_insert_with(|| demangle(raw));
                a.label = format!("{module}!{pretty}");
            }
        }
    }
    Ok(frame)
}

fn site(symbol: &str) -> Option<(&str, &str, &str)> {
    let (module, rest) = symbol.split_once('!')?;
    let (name, offset) = rest.rsplit_once("+0x")?;
    (!offset.is_empty() && offset.bytes().all(|b| b.is_ascii_hexdigit()))
        .then_some((module, name, offset))
}

fn recover(dir: &Path, pid: u32, frame: &mut Frame) {
    if !frame.allocations.iter().any(|a| {
        missing(&a.label)
            || a.stack
                .iter()
                .chain(&a.resize_stack)
                .any(|f| missing(&f.symbol))
    }) {
        return;
    }
    let Ok(ids) = fs::read_to_string(dir.join(format!("heap.{pid}.ids"))) else {
        return;
    };
    let mut paths = HashMap::new();
    for (id, path) in ids.lines().filter_map(|line| line.split_once(' ')) {
        if !id.is_empty() && id.bytes().all(|b| b.is_ascii_hexdigit()) {
            paths.insert(PathBuf::from(path), id);
            if let Ok(real) = fs::canonicalize(path) {
                paths.insert(real, id);
            }
        }
    }
    let fallback;
    let snapshot = if let Some(snapshot) = &frame.snapshot {
        snapshot
    } else {
        let Ok(maps) = fs::read_to_string(dir.join(format!("heap.{pid}.maps"))) else {
            return;
        };
        fallback = ProcessSnapshot::from_vmas(pid as i32, tmv_core::proc::parse_maps(&maps), false);
        &fallback
    };
    let mut engine = SymbolEngine::for_snapshot(snapshot);
    engine.modules.retain_mut(|m| {
        let Some(id) = paths.get(&m.path) else {
            return false;
        };
        for candidate in [dir.join("symbols").join(id), m.path.clone()] {
            let Ok(data) = fs::read(&candidate) else {
                continue;
            };
            let Ok(obj) = object::File::parse(data.as_slice()) else {
                continue;
            };
            if obj.build_id().ok().flatten().is_some_and(|bytes| {
                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>() == **id
            }) {
                m.path = candidate;
                return true;
            }
        }
        false
    });
    let mut cache = HashMap::new();
    let mut lookup = |pc| {
        cache
            .entry(pc)
            .or_insert_with(|| {
                let r = engine.resolve(pc)?;
                let location = r.location();
                Some((
                    format!("{}!{}+0x{:x}", r.module, r.symbol?, r.offset),
                    location,
                ))
            })
            .clone()
    };
    for a in &mut frame.allocations {
        if missing(&a.label) {
            if let Some((name, _)) = lookup(a.caller) {
                a.label = name;
            }
        }
        for f in a.stack.iter_mut().chain(&mut a.resize_stack) {
            if missing(&f.symbol) {
                if let Some((name, loc)) = lookup(f.address.saturating_sub(1)) {
                    f.symbol = name;
                    if loc.is_some() {
                        f.location = loc;
                    }
                }
            }
        }
    }
}
