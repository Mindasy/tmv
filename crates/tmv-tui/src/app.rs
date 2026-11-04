//! TUI 状态机：焦点路径、选中项、指标/分组/排序切换与键位处理。

use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use tmv_core::{
    build_tree, demo, diff, pagemap, Diff, GroupBy, MemNode, Metric, PageScan, ProcessSnapshot,
    SortOrder, TreeOptions,
};
use tmv_ptrace::{self as ptrace, ThreadInfo, ThreadStack};
use tmv_symbols::{Resolved, SymbolEngine};

/// 页图最多采集的样本数，兼顾大映射下的响应速度。
const MAX_PAGE_SAMPLES: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

pub struct App {
    pub snapshot: ProcessSnapshot,
    pub metric: Metric,
    pub group_by: GroupBy,
    pub sort: SortOrder,
    pub root: MemNode,
    /// 从根到当前聚焦节点的子节点下标路径。
    pub focus: Vec<usize>,
    pub selected: usize,
    /// 上一帧的块布局，用于方向键做几何导航。
    pub last_layout: Vec<Rect>,
    pub page_scan: Option<PageScan>,
    pub page_error: Option<String>,
    pub show_pages: bool,
    pub symbols: SymbolEngine,
    /// 选中节点起始地址的符号解析结果。
    pub resolved: Option<Resolved>,
    /// `/` 地址查找的结果。
    pub search_result: Option<Resolved>,
    /// 地址输入框内容，Some 时占用全部按键。
    pub input: Option<String>,
    /// 上一份快照，用于计算差值。
    pub previous: Option<ProcessSnapshot>,
    pub diff: Diff,
    pub diff_enabled: bool,
    /// 自动刷新间隔。
    pub interval: Option<Duration>,
    pub last_refresh: Instant,
    pub show_threads: bool,
    pub threads: Vec<ThreadInfo>,
    pub thread_index: usize,
    pub stack: Option<ThreadStack>,
    /// 回溯出的 PC 及对应符号解析。
    pub stack_frames: Vec<(u64, Option<Resolved>)>,
    pub stack_error: Option<String>,
    pub status: String,
    pub pid: Option<i32>,
    pub demo: bool,
    pub privileged: bool,
    pub should_quit: bool,
}

impl App {
    pub fn new(snapshot: ProcessSnapshot, pid: Option<i32>, demo: bool, privileged: bool) -> Self {
        let metric = if snapshot.has_smaps {
            Metric::Pss
        } else {
            Metric::Virtual
        };
        let symbols = if demo {
            SymbolEngine::for_demo(&snapshot)
        } else {
            SymbolEngine::for_process(&snapshot)
        };
        let mut app = Self {
            snapshot,
            metric,
            group_by: GroupBy::Category,
            sort: SortOrder::SizeDesc,
            root: MemNode::empty(),
            focus: Vec::new(),
            selected: 0,
            last_layout: Vec::new(),
            page_scan: None,
            page_error: None,
            show_pages: true,
            symbols,
            resolved: None,
            search_result: None,
            input: None,
            previous: None,
            diff: Diff::default(),
            diff_enabled: true,
            interval: None,
            last_refresh: Instant::now(),
            show_threads: false,
            threads: Vec::new(),
            thread_index: 0,
            stack: None,
            stack_frames: Vec::new(),
            stack_error: None,
            status: String::new(),
            pid,
            demo,
            privileged,
            should_quit: false,
        };
        app.rebuild();
        app
    }

    pub fn focused(&self) -> &MemNode {
        node_at(&self.root, &self.focus).unwrap_or(&self.root)
    }

    pub fn children(&self) -> &[MemNode] {
        &self.focused().children
    }

    pub fn selected_node(&self) -> Option<&MemNode> {
        self.children().get(self.selected)
    }

    pub fn breadcrumb(&self) -> String {
        let mut parts = vec![self.root.label.clone()];
        let mut node = &self.root;
        for &index in &self.focus {
            let Some(child) = node.children.get(index) else {
                break;
            };
            parts.push(child.label.clone());
            node = child;
        }
        parts.join(" › ")
    }

    /// 选中项对应的地址区间；聚合组返回所有子项的区间。
    pub fn selected_ranges(&self) -> Vec<(u64, u64)> {
        match self.selected_node() {
            Some(node) if node.children.is_empty() => vec![(node.start, node.end)],
            Some(node) => node
                .children
                .iter()
                .map(|child| (child.start, child.end))
                .collect(),
            None => Vec::new(),
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.should_quit = true;
            return;
        }
        if self.input.is_some() {
            self.handle_input_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => {
                if self.focus.is_empty() {
                    self.should_quit = true;
                } else {
                    self.back();
                }
            }
            KeyCode::Backspace => self.back(),
            KeyCode::Enter => self.enter(),
            KeyCode::Left | KeyCode::Char('h') => self.move_selection(Dir::Left),
            KeyCode::Right | KeyCode::Char('l') => self.move_selection(Dir::Right),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(Dir::Up),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(Dir::Down),
            KeyCode::Home => {
                self.selected = 0;
                self.refresh_details();
            }
            KeyCode::End => {
                self.selected = self.children().len().saturating_sub(1);
                self.refresh_details();
            }
            KeyCode::Char('/') => {
                self.input = Some(String::new());
            }
            KeyCode::Char('m') => self.cycle_metric(),
            KeyCode::Char('p') => {
                self.show_pages = !self.show_pages;
                if self.show_pages {
                    self.show_threads = false;
                }
                self.refresh_details();
            }
            KeyCode::Char('t') => self.toggle_threads(),
            KeyCode::Char('[') => {
                if self.show_threads {
                    self.cycle_thread(-1);
                }
            }
            KeyCode::Char(']') => {
                if self.show_threads {
                    self.cycle_thread(1);
                }
            }
            KeyCode::Char('u') => {
                if self.show_threads {
                    self.load_stack();
                }
            }
            KeyCode::Char('d') => {
                self.diff_enabled = !self.diff_enabled;
            }
            KeyCode::Char('g') => {
                self.group_by = self.group_by.next();
                self.reset_focus();
            }
            KeyCode::Char('o') => {
                self.sort = self.sort.next();
                self.rebuild();
            }
            KeyCode::Char('r') => self.refresh(),
            _ => {}
        }
    }

    fn cycle_metric(&mut self) {
        if !self.snapshot.has_smaps {
            self.status = "smaps 不可读，只能使用虚拟大小".to_string();
            return;
        }
        self.metric = self.metric.next();
        self.reset_focus();
    }

    fn reset_focus(&mut self) {
        self.focus.clear();
        self.selected = 0;
        self.rebuild();
    }

    fn enter(&mut self) {
        let has_children = self
            .selected_node()
            .map_or(false, |node| !node.children.is_empty());
        if has_children {
            self.focus.push(self.selected);
            self.selected = 0;
            self.last_layout.clear();
            self.refresh_details();
        }
    }

    fn back(&mut self) {
        if let Some(index) = self.focus.pop() {
            self.selected = index;
            self.last_layout.clear();
            self.refresh_details();
        }
    }

    fn move_selection(&mut self, dir: Dir) {
        let count = self.children().len();
        if count == 0 {
            return;
        }
        let previous = self.selected;

        if self.last_layout.len() != count {
            self.selected = match dir {
                Dir::Left | Dir::Up => (self.selected + count - 1) % count,
                Dir::Right | Dir::Down => (self.selected + 1) % count,
            };
        } else {
            let current = center(self.last_layout[self.selected]);
            let mut best: Option<(f64, usize)> = None;
            for (index, rect) in self.last_layout.iter().enumerate() {
                if index == self.selected {
                    continue;
                }
                let candidate = center(*rect);
                let (primary, secondary) = match dir {
                    Dir::Left => (current.0 - candidate.0, (current.1 - candidate.1).abs()),
                    Dir::Right => (candidate.0 - current.0, (current.1 - candidate.1).abs()),
                    Dir::Up => (current.1 - candidate.1, (current.0 - candidate.0).abs()),
                    Dir::Down => (candidate.1 - current.1, (current.0 - candidate.0).abs()),
                };
                if primary <= 0.0 {
                    continue;
                }
                let score = primary + secondary * 2.0;
                if best.map_or(true, |(best_score, _)| score < best_score) {
                    best = Some((score, index));
                }
            }
            if let Some((_, index)) = best {
                self.selected = index;
            }
        }

        if self.selected != previous {
            self.refresh_details();
        }
    }

    fn handle_input_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => self.input = None,
            KeyCode::Enter => {
                let text = self.input.take().unwrap_or_default();
                self.lookup_address(&text);
            }
            KeyCode::Backspace => {
                if let Some(input) = self.input.as_mut() {
                    input.pop();
                }
            }
            KeyCode::Char(character)
                if character.is_ascii_hexdigit() || character == 'x' || character == 'X' =>
            {
                if let Some(input) = self.input.as_mut() {
                    input.push(character);
                }
            }
            _ => {}
        }
    }

    /// 解析用户输入的十六进制地址，并跳到包含它的 VMA。
    fn lookup_address(&mut self, text: &str) {
        let trimmed = text.trim();
        let digits = trimmed
            .strip_prefix("0x")
            .or_else(|| trimmed.strip_prefix("0X"))
            .unwrap_or(trimmed);
        let Ok(address) = u64::from_str_radix(digits, 16) else {
            self.status = format!("地址无效: {trimmed}");
            self.search_result = None;
            return;
        };

        let resolved = self.symbols.resolve(address);
        self.status = match &resolved {
            Some(result) => {
                let symbol = result
                    .symbol
                    .clone()
                    .unwrap_or_else(|| result.module.clone());
                let location = result
                    .location()
                    .map(|location| format!("  {location}"))
                    .unwrap_or_default();
                format!("0x{address:x} → {symbol}+0x{:x}{location}", result.offset)
            }
            None => format!("0x{address:x} 没有对应的模块或 JIT 符号"),
        };
        self.search_result = resolved;
        self.jump_to_address(address);
    }

    fn jump_to_address(&mut self, address: u64) {
        let Some(path) = find_path(&self.root, address) else {
            return;
        };
        if path.is_empty() {
            self.focus.clear();
            self.selected = 0;
        } else {
            self.selected = *path.last().expect("路径非空");
            self.focus = path[..path.len() - 1].to_vec();
        }
        self.last_layout.clear();
        self.refresh_details();
    }

    /// 页图与符号都依赖当前选中项，统一在这里刷新。
    pub fn refresh_details(&mut self) {
        self.refresh_page_scan();
        self.resolved = match self.selected_node().map(|node| node.start) {
            Some(address) => self.symbols.resolve(address),
            None => None,
        };
    }

    /// 扫描当前选中节点的页状态；页图关闭时直接清空。
    pub fn refresh_page_scan(&mut self) {
        self.page_scan = None;
        self.page_error = None;
        if !self.show_pages {
            return;
        }
        let Some(node) = self.selected_node() else {
            return;
        };
        let (start, end) = (node.start, node.end);
        if end <= start {
            return;
        }

        let page_size = self.snapshot.geometry.page_size;
        let result = if self.demo {
            Ok(demo::page_scan(start, end, page_size, MAX_PAGE_SAMPLES))
        } else {
            pagemap::scan(
                self.snapshot.pid,
                &self.snapshot.geometry,
                start,
                end,
                MAX_PAGE_SAMPLES,
            )
        };

        match result {
            Ok(mut scan) => {
                if self.privileged && !self.demo {
                    if let Err(err) = pagemap::enrich_with_kpage(&mut scan) {
                        self.page_error = Some(format!("kpage 不可用: {err}"));
                    }
                }
                self.page_scan = Some(scan);
            }
            Err(err) => self.page_error = Some(err.to_string()),
        }
    }

    fn refresh(&mut self) {
        if self.demo {
            let mut snapshot = demo::snapshot();
            grow_demo(&mut snapshot);
            self.previous = Some(std::mem::replace(&mut self.snapshot, snapshot));
            self.status = "已重新生成示例数据".to_string();
        } else if let Some(pid) = self.pid {
            match ProcessSnapshot::collect(pid) {
                Ok(snapshot) => {
                    self.previous = Some(std::mem::replace(&mut self.snapshot, snapshot));
                    self.status = format!("已刷新 {} 个映射", self.snapshot.vmas.len());
                }
                Err(err) => self.status = format!("刷新失败: {err}"),
            }
        }
        if !self.snapshot.has_smaps && self.metric != Metric::Virtual {
            self.metric = Metric::Virtual;
        }
        // 映射变了，旧的模块索引和符号缓存都要重建。
        self.symbols = if self.demo {
            SymbolEngine::for_demo(&self.snapshot)
        } else {
            SymbolEngine::for_process(&self.snapshot)
        };
        self.search_result = None;
        self.last_refresh = Instant::now();
        self.rebuild();
        if self.show_threads {
            self.refresh_threads();
        }
    }

    /// 设置自动采样间隔。
    pub fn set_interval(&mut self, interval: Option<Duration>) {
        self.interval = interval;
        self.last_refresh = Instant::now();
    }

    /// 由主循环周期性调用，到达间隔就自动采样一次。
    pub fn tick(&mut self) {
        let Some(interval) = self.interval else {
            return;
        };
        if self.last_refresh.elapsed() >= interval {
            self.refresh();
        }
    }

    fn toggle_threads(&mut self) {
        self.show_threads = !self.show_threads;
        if self.show_threads {
            self.show_pages = false;
            self.refresh_threads();
        } else {
            self.stack = None;
            self.stack_frames.clear();
            self.stack_error = None;
        }
    }

    fn cycle_thread(&mut self, delta: isize) {
        if self.threads.is_empty() {
            return;
        }
        let len = self.threads.len() as isize;
        self.thread_index = (self.thread_index as isize + delta).rem_euclid(len) as usize;
        self.load_stack();
    }

    /// 枚举线程并回溯当前选中线程。
    pub fn refresh_threads(&mut self) {
        let previous_index = self.thread_index;
        self.threads.clear();
        self.stack = None;
        self.stack_frames.clear();
        self.stack_error = None;

        if self.demo {
            self.threads = ptrace::demo::threads();
        } else {
            match ptrace::list_threads(self.snapshot.pid) {
                Ok(threads) => self.threads = threads,
                Err(err) => self.stack_error = Some(describe_io(&err)),
            }
        }
        // 刷新后尽量保留原来选中的线程位次。
        self.thread_index = previous_index.min(self.threads.len().saturating_sub(1));
        self.load_stack();
    }

    fn load_stack(&mut self) {
        self.stack = None;
        self.stack_frames.clear();

        let Some(thread) = self.threads.get(self.thread_index) else {
            return;
        };
        let tid = thread.tid;
        let result = if self.demo {
            Ok(ptrace::demo::stack(&self.snapshot, self.thread_index))
        } else {
            ptrace::unwind_thread(self.snapshot.pid, tid, &self.snapshot.geometry, 48)
        };

        match result {
            Ok(stack) => {
                self.stack_frames = stack
                    .frames
                    .iter()
                    .map(|frame| (frame.pc, self.symbols.resolve(frame.pc)))
                    .collect();
                self.stack = Some(stack);
                self.stack_error = None;
            }
            Err(err) => self.stack_error = Some(describe_io(&err)),
        }
    }

    fn rebuild(&mut self) {
        self.diff = match &self.previous {
            Some(previous) => diff::diff(previous, &self.snapshot, self.metric),
            None => Diff::default(),
        };

        self.root = build_tree(
            &self.snapshot,
            TreeOptions {
                metric: self.metric,
                group_by: self.group_by,
                sort: self.sort,
            },
        );
        apply_deltas(&mut self.root, &self.diff);

        // 分组或指标变化后，旧的聚焦路径可能失效，截断到仍然有效的前缀。
        let depth = {
            let mut node = &self.root;
            let mut depth = 0;
            for &index in &self.focus {
                match node.children.get(index) {
                    Some(child) => {
                        node = child;
                        depth += 1;
                    }
                    None => break,
                }
            }
            depth
        };
        self.focus.truncate(depth);

        let count = self.focused().children.len();
        self.selected = self.selected.min(count.saturating_sub(1));
        self.last_layout.clear();
        self.refresh_details();
    }
}

/// 在树里查找包含该地址的叶子，返回从根到叶子的下标路径。
fn find_path(node: &MemNode, address: u64) -> Option<Vec<usize>> {
    if node.children.is_empty() {
        return (address >= node.start && address < node.end).then(Vec::new);
    }
    for (index, child) in node.children.iter().enumerate() {
        if address < child.start || address >= child.end {
            continue;
        }
        if let Some(mut path) = find_path(child, address) {
            path.insert(0, index);
            return Some(path);
        }
    }
    None
}

/// 把叶子差值汇总到每个节点上，返回值是子树总差值。
fn apply_deltas(node: &mut MemNode, diff: &Diff) -> i64 {
    if node.children.is_empty() {
        let delta = node
            .vma
            .as_ref()
            .and_then(|vma| diff.by_start.get(&vma.start))
            .copied()
            .unwrap_or(0);
        node.delta = delta;
        return delta;
    }

    let mut total = 0;
    for child in &mut node.children {
        total += apply_deltas(child, diff);
    }
    node.delta = total;
    total
}

/// demo 模式下每次刷新把 heap 的驻留抬高一点，方便预览 diff。
fn grow_demo(snapshot: &mut ProcessSnapshot) {
    if let Some(heap) = snapshot
        .vmas
        .iter_mut()
        .find(|vma| vma.pathname == "[heap]")
    {
        if let Some(stats) = heap.stats.as_mut() {
            let bump = 4 * 1024 * 1024;
            stats.rss += bump;
            stats.pss += bump;
            stats.private_dirty += bump;
        }
    }
}

fn describe_io(err: &std::io::Error) -> String {
    match ptrace::permission_hint(err) {
        Some(hint) => format!("{err}（{hint}）"),
        None => err.to_string(),
    }
}

fn node_at<'a>(root: &'a MemNode, path: &[usize]) -> Option<&'a MemNode> {
    let mut node = root;
    for &index in path {
        node = node.children.get(index)?;
    }
    Some(node)
}

fn center(rect: Rect) -> (f64, f64) {
    (
        rect.x as f64 + rect.width as f64 / 2.0,
        rect.y as f64 + rect.height as f64 / 2.0,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tmv_core::demo;

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn address_lookup_jumps_to_containing_vma() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        let target = app.snapshot.vmas[3].start + 0x20;

        press(&mut app, KeyCode::Char('/'));
        for character in format!("{target:x}").chars() {
            press(&mut app, KeyCode::Char(character));
        }
        press(&mut app, KeyCode::Enter);

        assert!(app.input.is_none(), "回车后应退出输入模式");
        assert!(app.search_result.is_some(), "地址应解析到符号");
        let node = app.selected_node().expect("应跳到某个 VMA");
        assert!(target >= node.start && target < node.end);
    }

    #[test]
    fn invalid_address_keeps_state_consistent() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        press(&mut app, KeyCode::Char('/'));
        for character in "zz".chars() {
            press(&mut app, KeyCode::Char(character));
        }
        press(&mut app, KeyCode::Enter);
        assert!(app.search_result.is_none());

        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Esc);
        assert!(app.input.is_none());
    }

    #[test]
    fn refresh_produces_growth_diff() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        assert!(!app.diff.has_baseline);

        press(&mut app, KeyCode::Char('r'));

        assert!(app.diff.has_baseline);
        assert_eq!(app.diff.total, 4 * 1024 * 1024);
        assert_eq!(app.diff.added, 0);
        assert_eq!(app.diff.removed, 0);
        // 差值应汇总到根节点和对应的类别组上
        assert!(app.root.delta > 0);
        let heap = app
            .root
            .children
            .iter()
            .find(|node| node.label == "heap")
            .expect("存在 heap 分组");
        assert_eq!(heap.delta, 4 * 1024 * 1024);
    }

    #[test]
    fn diff_can_be_toggled() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        assert!(app.diff_enabled);
        press(&mut app, KeyCode::Char('d'));
        assert!(!app.diff_enabled);
        press(&mut app, KeyCode::Char('d'));
        assert!(app.diff_enabled);
    }

    #[test]
    fn thread_panel_unwinds_demo_stack() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        assert!(!app.show_threads);

        press(&mut app, KeyCode::Char('t'));

        assert!(app.show_threads);
        assert!(!app.show_pages, "页图应让位给线程面板");
        assert_eq!(app.threads.len(), 4);
        assert!(app.stack.is_some());
        assert!(!app.stack_frames.is_empty());
        assert!(app
            .stack_frames
            .iter()
            .all(|(pc, resolved)| *pc != 0 && resolved.is_some()));

        let first = app.thread_index;
        press(&mut app, KeyCode::Char(']'));
        assert_ne!(app.thread_index, first);
        assert_eq!(app.stack.as_ref().unwrap().name, "worker");

        press(&mut app, KeyCode::Char('t'));
        assert!(!app.show_threads);
        assert!(app.stack.is_none());
    }
}
