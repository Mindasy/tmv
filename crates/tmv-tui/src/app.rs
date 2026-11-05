//! TUI 状态机：焦点路径、选中项、指标/分组/排序切换与键位处理。

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use tmv_core::{
    build_tree, demo, diff, pagemap, Accounting, Diff, GroupBy, MemNode, Metric, PageDelta,
    PageScan, ProcessSnapshot, ProcessWatch, SortOrder, TargetGeometry, TreeOptions, Vma,
};
use tmv_ptrace::{self as ptrace, ThreadInfo, ThreadStack};
use tmv_symbols::{Resolved, SymbolEngine};

/// 页图最多采集的样本数，兼顾大映射下的响应速度。
const MAX_PAGE_SAMPLES: usize = 4096;

/// 内存查看器一次读取的字节数（32 行，每行 16 字节）。
const MEMORY_WINDOW: usize = 512;

/// 设置界面里可选的刷新间隔（秒）。
pub const AUTO_INTERVALS: [u64; 8] = [1, 2, 5, 10, 15, 30, 60, 120];

/// 设置界面的条目数。
pub const SETTING_COUNT: usize = 4;

/// 默认刷新间隔。
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(5);

/// 双击 r 的判定窗口。
const DOUBLE_PRESS: Duration = Duration::from_millis(400);

/// 后台页图扫描请求。
struct PageRequest {
    generation: u64,
    pid: i32,
    geometry: TargetGeometry,
    start: u64,
    end: u64,
    privileged: bool,
}

/// 后台页图扫描结果。
struct PageResult {
    generation: u64,
    result: std::io::Result<PageScan>,
    page_error: Option<String>,
}

/// 输入框当前服务的目标。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputTarget {
    /// `/`：解析十六进制地址。
    Address,
    /// `f`：按路径 / 标签过滤映射。
    Filter,
}

/// 主视图：treemap 或按度量排序的映射列表。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Treemap,
    List,
}

impl ViewMode {
    pub fn label(self) -> &'static str {
        match self {
            ViewMode::Treemap => "treemap",
            ViewMode::List => "列表",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// 内存查看器的状态，按 16 字节一行展示。
#[derive(Debug, Clone)]
pub struct MemoryView {
    pub base: u64,
    pub bytes: Vec<u8>,
    /// 首行行号（每行 16 字节）。
    pub scroll: usize,
    /// 读取失败时的原因；此时 `bytes` 为空。
    pub error: Option<String>,
}

impl MemoryView {
    /// 每行展示的字节数。
    pub const LINE: usize = 16;

    pub fn line_count(&self) -> usize {
        self.bytes.len().div_ceil(Self::LINE)
    }

    /// 读取失败的空视图。
    fn failed(base: u64, error: String) -> Self {
        Self {
            base,
            bytes: Vec::new(),
            scroll: 0,
            error: Some(error),
        }
    }
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
    /// 同一区间上一次扫描的结果，用于计算页级变化。
    previous_page: Option<PageScan>,
    /// 当前页图相对上一次扫描的变化，区间或样本不一致时为空。
    pub page_delta: Option<PageDelta>,
    pub page_error: Option<String>,
    pub show_pages: bool,
    pub symbols: SymbolEngine,
    /// 选中节点起始地址的符号解析结果。
    pub resolved: Option<Resolved>,
    /// `/` 地址查找的结果。
    pub search_result: Option<Resolved>,
    /// 地址输入框内容，Some 时占用全部按键。
    pub input: Option<String>,
    /// 输入框当前是解析地址还是设置过滤。
    pub input_target: InputTarget,
    /// 生效中的映射过滤关键字（小写比较），None 表示不过滤。
    pub filter: Option<String>,
    /// 当前主视图。
    pub view: ViewMode,
    /// 列表视图中的选中行（对应过滤后的映射）。
    pub list_index: usize,
    /// 上一份快照，用于计算差值。
    pub previous: Option<ProcessSnapshot>,
    pub diff: Diff,
    pub diff_enabled: bool,
    /// 定时刷新间隔。
    pub interval: Duration,
    /// 定时刷新是否开启（rr 或设置界面切换）。
    pub auto_refresh: bool,
    /// 是否正在后台采集快照。
    pub loading: bool,
    /// 页图是否正在后台扫描。
    pub page_loading: bool,
    /// 设置界面是否打开。
    pub show_settings: bool,
    /// 帮助面板是否打开。
    pub show_help: bool,
    /// 设置界面当前选中项。
    pub settings_index: usize,
    /// 账务面板是否打开。
    pub show_accounting: bool,
    /// 内存查看器；`x` 以当前选中项起始地址打开。
    pub memory: Option<MemoryView>,
    /// 最近一次采集的账务信息。
    pub accounting: Option<Accounting>,
    pub last_refresh: Instant,
    last_r: Option<Instant>,
    snapshot_tx: Option<Sender<()>>,
    snapshot_rx: Option<Receiver<Result<ProcessSnapshot, String>>>,
    page_tx: Option<Sender<PageRequest>>,
    page_rx: Option<Receiver<PageResult>>,
    page_gen: u64,
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
    /// 目标进程存活检查；demo 模式没有真实进程。
    watch: Option<ProcessWatch>,
    /// 目标进程是否已退出；退出后停止采样并保留最后一次快照。
    pub process_exited: bool,
    /// 回放模式：快照来自 JSON 文件，没有可采样的目标进程。
    pub replay: bool,
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
        // 真实进程的采集放到后台线程，避免大进程阻塞界面；demo 数据是瞬时的，保持同步。
        let (snapshot_tx, snapshot_rx, page_tx, page_rx) = match (demo, pid) {
            (false, Some(pid)) => {
                let (request, results) = spawn_snapshot_worker(pid);
                let (page_request, page_results) = spawn_page_worker();
                (
                    Some(request),
                    Some(results),
                    Some(page_request),
                    Some(page_results),
                )
            }
            _ => (None, None, None, None),
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
            previous_page: None,
            page_delta: None,
            page_error: None,
            show_pages: true,
            symbols,
            resolved: None,
            search_result: None,
            input: None,
            input_target: InputTarget::Address,
            filter: None,
            view: ViewMode::Treemap,
            list_index: 0,
            previous: None,
            diff: Diff::default(),
            diff_enabled: true,
            interval: DEFAULT_INTERVAL,
            auto_refresh: false,
            loading: false,
            page_loading: false,
            show_settings: false,
            show_help: false,
            settings_index: 0,
            show_accounting: false,
            memory: None,
            accounting: None,
            last_refresh: Instant::now(),
            last_r: None,
            snapshot_tx,
            snapshot_rx,
            page_tx,
            page_rx,
            page_gen: 0,
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
            // demo 没有真实进程；真实进程从启动的 pid 开始监听。
            watch: if demo {
                None
            } else {
                pid.map(ProcessWatch::new)
            },
            process_exited: false,
            replay: false,
            should_quit: false,
        };
        app.rebuild();
        app
    }

    /// 进入回放模式：快照来自 JSON 文件，不采样、不 attach 进程。
    pub fn set_replay(&mut self, replay: bool) {
        self.replay = replay;
        if replay {
            self.auto_refresh = false;
            self.status = "回放模式：快照来自文件，r / 页图 / 线程栈不可用".to_string();
        }
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
        if self.show_help {
            // 帮助面板是纯说明，任意按键关闭。
            self.show_help = false;
            return;
        }
        if self.show_settings {
            self.handle_settings_key(key);
            return;
        }
        if self.show_accounting {
            match key.code {
                KeyCode::Esc | KeyCode::Char('a') => self.show_accounting = false,
                KeyCode::Char('r') => self.refresh_accounting(),
                KeyCode::Char('q') => self.should_quit = true,
                _ => {}
            }
            return;
        }
        if self.memory.is_some() {
            self.handle_memory_key(key);
            return;
        }
        match key.code {
            KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Esc => {
                if self.list_active() {
                    self.toggle_view();
                } else if self.focus.is_empty() {
                    self.should_quit = true;
                } else {
                    self.back();
                }
            }
            KeyCode::Backspace => self.back(),
            KeyCode::Enter => {
                if self.list_active() {
                    self.toggle_view();
                } else {
                    self.enter();
                }
            }
            KeyCode::Left | KeyCode::Char('h') => {
                if self.list_active() {
                    self.move_list(-1);
                } else {
                    self.move_selection(Dir::Left);
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if self.list_active() {
                    self.move_list(1);
                } else {
                    self.move_selection(Dir::Right);
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if self.list_active() {
                    self.move_list(-1);
                } else {
                    self.move_selection(Dir::Up);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.list_active() {
                    self.move_list(1);
                } else {
                    self.move_selection(Dir::Down);
                }
            }
            KeyCode::Home => {
                if self.list_active() {
                    self.list_index = 0;
                    self.move_list(0);
                } else {
                    self.selected = 0;
                    self.refresh_details();
                }
            }
            KeyCode::End => {
                if self.list_active() {
                    self.list_index = self.visible_vmas().len().saturating_sub(1);
                    self.move_list(0);
                } else {
                    self.selected = self.children().len().saturating_sub(1);
                    self.refresh_details();
                }
            }
            KeyCode::Char('v') => self.toggle_view(),
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('x') => self.open_memory(),
            KeyCode::Char('/') => {
                self.input = Some(String::new());
                self.input_target = InputTarget::Address;
            }
            KeyCode::Char('f') => {
                self.input = Some(self.filter.clone().unwrap_or_default());
                self.input_target = InputTarget::Filter;
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
            KeyCode::Char('s') => {
                self.show_settings = true;
                self.settings_index = 0;
            }
            KeyCode::Char('a') => self.toggle_accounting(),
            KeyCode::Char('r') => {
                // 两次快速按 r 切换定时刷新；单次 r 立即刷新一次。
                let now = Instant::now();
                let double = self
                    .last_r
                    .is_some_and(|last| now.duration_since(last) < DOUBLE_PRESS);
                self.last_r = if double { None } else { Some(now) };
                if double {
                    self.toggle_auto();
                } else {
                    self.refresh();
                }
            }
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
            .is_some_and(|node| !node.children.is_empty());
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
                // 用 is_some_and 取反，避免依赖 Rust 1.82 才稳定的 is_none_or。
                if !best.is_some_and(|(best_score, _)| score >= best_score) {
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
                match self.input_target {
                    InputTarget::Address => self.lookup_address(&text),
                    InputTarget::Filter => self.apply_filter(&text),
                }
            }
            KeyCode::Backspace => {
                if let Some(input) = self.input.as_mut() {
                    input.pop();
                }
            }
            KeyCode::Char(character) => {
                // 过滤关键字可以是任意可打印字符；地址输入只接受十六进制。
                let accept = match self.input_target {
                    InputTarget::Filter => !character.is_control(),
                    InputTarget::Address => {
                        character.is_ascii_hexdigit() || character == 'x' || character == 'X'
                    }
                };
                if accept {
                    if let Some(input) = self.input.as_mut() {
                        input.push(character);
                    }
                }
            }
            _ => {}
        }
    }

    /// 应用映射过滤条件；空字符串表示清除。
    pub fn apply_filter(&mut self, text: &str) {
        let trimmed = text.trim();
        self.filter = (!trimmed.is_empty()).then(|| trimmed.to_lowercase());
        self.focus.clear();
        self.selected = 0;
        self.list_index = 0;
        self.last_layout.clear();
        self.rebuild();
        self.status = match &self.filter {
            Some(filter) => {
                let (matched, total) = self.filter_counts();
                format!("过滤 “{filter}”：{matched} / {total} 个映射")
            }
            None => format!("已清除过滤，共 {} 个映射", self.snapshot.vmas.len()),
        };
    }

    /// 过滤条件与命中数量，供状态行显示。
    pub fn filter_summary(&self) -> Option<(String, usize, usize)> {
        let filter = self.filter.as_ref()?;
        let (matched, total) = self.filter_counts();
        Some((filter.clone(), matched, total))
    }

    fn filter_counts(&self) -> (usize, usize) {
        let matched = match &self.filter {
            Some(needle) => self
                .snapshot
                .vmas
                .iter()
                .filter(|vma| vma_matches(vma, needle))
                .count(),
            None => self.snapshot.vmas.len(),
        };
        (matched, self.snapshot.vmas.len())
    }

    /// 应用过滤后的快照；无过滤时直接复用当前快照。
    fn visible_snapshot(&self) -> ProcessSnapshot {
        let Some(needle) = &self.filter else {
            return self.snapshot.clone();
        };
        let mut snapshot = self.snapshot.clone();
        snapshot.vmas.retain(|vma| vma_matches(vma, needle));
        snapshot
    }

    /// 过滤后按当前度量降序排列的映射，列表视图直接使用。
    pub fn visible_vmas(&self) -> Vec<&Vma> {
        let mut vmas: Vec<&Vma> = match &self.filter {
            Some(needle) => self
                .snapshot
                .vmas
                .iter()
                .filter(|vma| vma_matches(vma, needle))
                .collect(),
            None => self.snapshot.vmas.iter().collect(),
        };
        vmas.sort_by(|a, b| match self.sort {
            SortOrder::SizeDesc => b
                .metric(self.metric)
                .cmp(&a.metric(self.metric))
                .then(a.start.cmp(&b.start)),
            SortOrder::Address => a.start.cmp(&b.start),
        });
        vmas
    }

    /// 切换 treemap / 列表视图，并把列表选中项对齐到当前 treemap 选择。
    pub fn toggle_view(&mut self) {
        self.view = match self.view {
            ViewMode::Treemap => ViewMode::List,
            ViewMode::List => ViewMode::Treemap,
        };
        self.last_layout.clear();
        if self.view == ViewMode::List {
            self.list_index = 0;
            if let Some((start, _)) = self.selected_ranges().first().copied() {
                if let Some(index) = self
                    .visible_vmas()
                    .iter()
                    .position(|vma| vma.start == start)
                {
                    self.list_index = index;
                }
            }
        }
        self.status = format!("主视图：{}", self.view.label());
    }

    /// 列表视图上下移动；选中项同步到 treemap 焦点与详情。
    pub fn move_list(&mut self, delta: isize) {
        let count = self.visible_vmas().len();
        if count == 0 {
            return;
        }
        self.list_index = (self.list_index as isize + delta).rem_euclid(count as isize) as usize;
        if let Some(start) = self
            .visible_vmas()
            .get(self.list_index)
            .map(|vma| vma.start)
        {
            self.jump_to_address(start);
        }
    }

    fn list_active(&self) -> bool {
        self.view == ViewMode::List
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
            self.status = if self.filter.is_some() {
                format!("0x{address:x} 不在当前过滤结果中")
            } else {
                format!("0x{address:x} 不在地址空间内")
            };
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
    ///
    /// demo 数据同步生成，真实进程交给后台线程，避免大区间扫描卡住界面。
    pub fn refresh_page_scan(&mut self) {
        // 上一次的结果留作对比基线，界面上先清空，避免显示过期页图。
        self.previous_page = self.page_scan.take();
        self.page_delta = None;
        self.page_error = None;
        self.page_gen += 1;
        self.page_loading = false;
        if !self.show_pages {
            return;
        }
        if self.replay {
            self.page_error = Some("回放模式没有页级数据".to_string());
            return;
        }
        let Some(node) = self.selected_node() else {
            return;
        };
        let (start, end) = (node.start, node.end);
        if end <= start {
            return;
        }

        if self.demo {
            let scan = demo::page_scan(
                start,
                end,
                self.snapshot.geometry.page_size,
                MAX_PAGE_SAMPLES,
            );
            self.apply_page_scan(scan);
            return;
        }

        if self.process_exited {
            return;
        }
        let Some(tx) = &self.page_tx else {
            return;
        };
        self.page_loading = true;
        let _ = tx.send(PageRequest {
            generation: self.page_gen,
            pid: self.snapshot.pid,
            geometry: self.snapshot.geometry,
            start,
            end,
            privileged: self.privileged,
        });
    }

    /// 保存新的页扫描结果，并与同一区间的上一次扫描比较。
    fn apply_page_scan(&mut self, scan: PageScan) {
        self.page_delta = self
            .previous_page
            .as_ref()
            .and_then(|before| pagemap::delta(before, &scan));
        self.previous_page = None;
        self.page_scan = Some(scan);
    }

    /// 请求一次快照刷新：demo 同步生成，真实进程提交给后台线程。
    pub fn refresh(&mut self) {
        self.last_refresh = Instant::now();
        if self.replay {
            self.status = "回放模式：快照来自文件，不能重新采样".to_string();
            return;
        }
        if self.process_exited {
            self.status = "目标进程已退出，无法再次采样".to_string();
            return;
        }
        if self.demo {
            let mut snapshot = demo::snapshot();
            grow_demo(&mut snapshot);
            self.apply_snapshot(snapshot);
            self.status = "已重新生成示例数据".to_string();
            return;
        }
        if self.loading {
            return;
        }
        let Some(tx) = &self.snapshot_tx else {
            return;
        };
        self.loading = true;
        self.status = "采集中…".to_string();
        let _ = tx.send(());
    }

    /// 应用一份新快照，并重建符号、差值与树。
    fn apply_snapshot(&mut self, snapshot: ProcessSnapshot) {
        self.previous = Some(std::mem::replace(&mut self.snapshot, snapshot));
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
        if self.show_accounting {
            self.refresh_accounting();
        }
    }

    /// 设置自动采样间隔；Some 同时开启定时刷新。
    pub fn set_interval(&mut self, interval: Option<Duration>) {
        match interval {
            Some(value) => {
                self.interval = value;
                self.auto_refresh = true;
            }
            None => self.auto_refresh = false,
        }
        self.last_refresh = Instant::now();
    }

    /// 由命令行指定度量；smaps 不可读时强制退回虚拟大小。
    pub fn set_metric(&mut self, metric: Metric) {
        if !self.snapshot.has_smaps && metric != Metric::Virtual {
            self.metric = Metric::Virtual;
            self.status = "smaps 不可读，只能使用虚拟大小".to_string();
        } else {
            self.metric = metric;
        }
        self.reset_focus();
    }

    /// 设置对比基线，TUI 启动后即可看到差值。
    pub fn set_baseline(&mut self, baseline: ProcessSnapshot) {
        self.previous = Some(baseline);
        self.rebuild();
    }

    /// 开关账务面板；打开时立即采集一次。
    pub fn toggle_accounting(&mut self) {
        self.show_accounting = !self.show_accounting;
        if self.show_accounting {
            self.refresh_accounting();
        }
    }

    /// 采集账务信息：demo 用合成数据，真实进程读 /proc 与 /sys。
    pub fn refresh_accounting(&mut self) {
        if self.replay {
            self.status = "回放模式没有账务数据".to_string();
            return;
        }
        if self.process_exited {
            self.status = "目标进程已退出，账务信息保持最后一次结果".to_string();
            return;
        }
        self.accounting = Some(if self.demo {
            Accounting::demo(&self.snapshot)
        } else {
            Accounting::collect(self.snapshot.pid)
        });
        self.status = "账务信息已更新".to_string();
    }

    /// 以当前选中项的起始地址打开内存查看器。
    pub fn open_memory(&mut self) {
        let base = self.selected_node().map(|node| node.start).unwrap_or(0);
        self.memory = Some(match self.memory_source(base) {
            Ok(bytes) => MemoryView {
                base,
                bytes,
                scroll: 0,
                error: None,
            },
            Err(reason) => MemoryView::failed(base, reason),
        });
    }

    /// 按当前模式决定内存内容从哪里来。
    fn memory_source(&self, base: u64) -> Result<Vec<u8>, String> {
        if self.replay {
            return Err("回放模式没有内存内容".to_string());
        }
        if self.process_exited {
            return Err("目标进程已退出，无法读取内存".to_string());
        }
        if self.demo {
            return Ok(demo::memory(base, MEMORY_WINDOW));
        }
        ptrace::read_memory(self.snapshot.pid, base, MEMORY_WINDOW).map_err(|err| describe_io(&err))
    }

    fn handle_memory_key(&mut self, key: KeyEvent) {
        if matches!(
            key.code,
            KeyCode::Esc | KeyCode::Char('x') | KeyCode::Char('q')
        ) {
            self.memory = None;
            return;
        }
        let Some(view) = self.memory.as_mut() else {
            return;
        };
        let last = view.line_count().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => view.scroll = view.scroll.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => view.scroll = (view.scroll + 1).min(last),
            KeyCode::PageUp => view.scroll = view.scroll.saturating_sub(MemoryView::LINE),
            KeyCode::PageDown => view.scroll = (view.scroll + MemoryView::LINE).min(last),
            KeyCode::Home => view.scroll = 0,
            KeyCode::End => view.scroll = last,
            _ => {}
        }
    }

    /// 切换定时刷新开关。
    pub fn toggle_auto(&mut self) {
        if self.replay {
            self.status = "回放模式不支持定时刷新".to_string();
            return;
        }
        self.auto_refresh = !self.auto_refresh;
        self.last_refresh = Instant::now();
        self.status = if self.auto_refresh {
            format!("定时刷新开，间隔 {}s", self.interval.as_secs())
        } else {
            "定时刷新关".to_string()
        };
    }

    fn handle_settings_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('s') => self.show_settings = false,
            KeyCode::Up | KeyCode::Char('k') => {
                self.settings_index = (self.settings_index + SETTING_COUNT - 1) % SETTING_COUNT;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.settings_index = (self.settings_index + 1) % SETTING_COUNT;
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('-') => self.adjust_setting(-1),
            KeyCode::Right
            | KeyCode::Char('l')
            | KeyCode::Char('+')
            | KeyCode::Enter
            | KeyCode::Char(' ') => self.adjust_setting(1),
            _ => {}
        }
    }

    fn adjust_setting(&mut self, delta: isize) {
        match self.settings_index {
            0 => self.toggle_auto(),
            1 => self.step_interval(delta),
            2 => {
                self.show_pages = !self.show_pages;
                if self.show_pages {
                    self.show_threads = false;
                }
                self.refresh_details();
            }
            3 => self.diff_enabled = !self.diff_enabled,
            _ => {}
        }
    }

    fn step_interval(&mut self, delta: isize) {
        let current = self.interval.as_secs();
        let next = if delta > 0 {
            AUTO_INTERVALS
                .iter()
                .copied()
                .find(|value| *value > current)
        } else {
            AUTO_INTERVALS
                .iter()
                .rev()
                .copied()
                .find(|value| *value < current)
        };
        if let Some(value) = next {
            self.interval = Duration::from_secs(value);
            self.last_refresh = Instant::now();
            self.status = format!("刷新间隔 {}s", value);
        }
    }

    /// 由主循环周期性调用：接收后台结果、检查目标存活，并按间隔触发定时刷新。
    pub fn tick(&mut self) {
        self.poll_background();
        self.check_target();
        if self.auto_refresh
            && !self.process_exited
            && !self.loading
            && self.last_refresh.elapsed() >= self.interval
        {
            self.refresh();
        }
    }

    /// 主循环等待按键的时长：采集期间更短以便尽快取回结果，进程退出后放长以省电。
    pub fn poll_timeout(&self) -> Duration {
        if self.loading || self.page_loading {
            Duration::from_millis(60)
        } else if self.process_exited {
            Duration::from_secs(1)
        } else if self.auto_refresh {
            self.interval
                .saturating_sub(self.last_refresh.elapsed())
                .clamp(Duration::from_millis(50), Duration::from_millis(250))
        } else {
            Duration::from_millis(250)
        }
    }

    /// 目标进程退出后停止一切采样，保留最后一次快照供查看。
    fn check_target(&mut self) {
        if self.process_exited {
            return;
        }
        let Some(watch) = self.watch.as_mut() else {
            return;
        };
        if watch.is_alive() {
            return;
        }
        self.process_exited = true;
        self.auto_refresh = false;
        self.loading = false;
        self.page_loading = false;
        self.status = format!("目标进程 {} 已退出，显示最后一次快照", self.snapshot.pid);
    }

    fn poll_background(&mut self) {
        let mut snapshots = Vec::new();
        if let Some(rx) = &self.snapshot_rx {
            while let Ok(result) = rx.try_recv() {
                snapshots.push(result);
            }
        }
        for result in snapshots {
            self.loading = false;
            match result {
                Ok(snapshot) => {
                    let count = snapshot.vmas.len();
                    self.apply_snapshot(snapshot);
                    self.status = format!("已刷新 {count} 个映射");
                }
                Err(err) => self.status = format!("刷新失败: {err}"),
            }
        }

        let mut pages = Vec::new();
        if let Some(rx) = &self.page_rx {
            while let Ok(result) = rx.try_recv() {
                pages.push(result);
            }
        }
        for result in pages {
            if result.generation != self.page_gen {
                continue;
            }
            self.page_loading = false;
            match result.result {
                Ok(scan) => {
                    self.apply_page_scan(scan);
                    self.page_error = result.page_error;
                }
                Err(err) => self.page_error = Some(err.to_string()),
            }
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

        if self.replay {
            self.stack_error = Some("回放模式不能 attach 目标进程".to_string());
            return;
        }

        if self.process_exited {
            self.stack_error = Some("目标进程已退出".to_string());
            return;
        }

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
            ptrace::unwind_thread(
                self.snapshot.pid,
                tid,
                &self.snapshot.geometry,
                Some(&mut self.symbols),
                48,
            )
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
            &self.visible_snapshot(),
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
        self.list_index = self
            .list_index
            .min(self.visible_vmas().len().saturating_sub(1));
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

/// 后台快照线程：连续请求只保留最新一次，避免排队时重复采集。
fn spawn_snapshot_worker(pid: i32) -> (Sender<()>, Receiver<Result<ProcessSnapshot, String>>) {
    let (request_tx, request_rx) = mpsc::channel::<()>();
    let (result_tx, result_rx) = mpsc::channel::<Result<ProcessSnapshot, String>>();
    thread::spawn(move || {
        while request_rx.recv().is_ok() {
            while request_rx.try_recv().is_ok() {}
            let result = ProcessSnapshot::collect(pid).map_err(|err| err.to_string());
            if result_tx.send(result).is_err() {
                break;
            }
        }
    });
    (request_tx, result_rx)
}

/// 后台页图线程：同样只处理最新请求，过期结果由调用方按 generation 丢弃。
fn spawn_page_worker() -> (Sender<PageRequest>, Receiver<PageResult>) {
    let (request_tx, request_rx) = mpsc::channel::<PageRequest>();
    let (result_tx, result_rx) = mpsc::channel::<PageResult>();
    thread::spawn(move || {
        while let Ok(mut request) = request_rx.recv() {
            while let Ok(newer) = request_rx.try_recv() {
                request = newer;
            }
            let payload = match pagemap::scan(
                request.pid,
                &request.geometry,
                request.start,
                request.end,
                MAX_PAGE_SAMPLES,
            ) {
                Ok(mut scan) => {
                    let page_error = if request.privileged {
                        pagemap::enrich_with_kpage(&mut scan)
                            .err()
                            .map(|err| format!("kpage 不可用: {err}"))
                    } else {
                        None
                    };
                    PageResult {
                        generation: request.generation,
                        result: Ok(scan),
                        page_error,
                    }
                }
                Err(err) => PageResult {
                    generation: request.generation,
                    result: Err(err),
                    page_error: None,
                },
            };
            if result_tx.send(payload).is_err() {
                break;
            }
        }
    });
    (request_tx, result_rx)
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

/// 过滤关键字（小写）是否命中该映射。
fn vma_matches(vma: &Vma, needle: &str) -> bool {
    vma.label().to_lowercase().contains(needle)
        || vma.pathname.to_lowercase().contains(needle)
        || vma.perms.as_str().contains(needle)
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
    fn double_r_toggles_auto_refresh() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        assert!(!app.auto_refresh);

        press(&mut app, KeyCode::Char('r'));
        press(&mut app, KeyCode::Char('r'));
        assert!(app.auto_refresh, "rr 应开启定时刷新");

        press(&mut app, KeyCode::Char('r'));
        press(&mut app, KeyCode::Char('r'));
        assert!(!app.auto_refresh, "再次 rr 应关闭定时刷新");
    }

    #[test]
    fn settings_adjusts_interval_and_toggles() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        assert_eq!(app.interval, DEFAULT_INTERVAL);

        press(&mut app, KeyCode::Char('s'));
        assert!(app.show_settings);

        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Right);
        assert_eq!(app.interval.as_secs(), 10, "5s 的下一档应为 10s");
        press(&mut app, KeyCode::Left);
        assert_eq!(app.interval.as_secs(), 5);

        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Enter);
        assert!(app.auto_refresh, "设置界面可切换定时刷新");

        press(&mut app, KeyCode::Esc);
        assert!(!app.show_settings);
    }

    #[test]
    fn settings_blocks_treemap_keys() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        let before = app.metric;

        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('m'));

        assert_eq!(app.metric, before, "设置界面打开时不应触发指标切换");
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

    #[test]
    fn page_delta_compares_consecutive_scans() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        let node = app.selected_node().expect("应有选中项");
        let (start, end) = (node.start, node.end);

        // 构造一份「全部未驻留」的旧扫描，再扫描同一区间应看到新驻留页。
        let mut baseline = demo::page_scan(
            start,
            end,
            app.snapshot.geometry.page_size,
            MAX_PAGE_SAMPLES,
        );
        baseline.states.fill(tmv_core::PageState::default());
        app.page_scan = Some(baseline);

        app.refresh_page_scan();

        let delta = app.page_delta.expect("同一区间应给出变化");
        assert!(delta.became_present > 0);
        assert!(app.page_scan.is_some());
    }

    #[test]
    fn accounting_panel_toggles_with_demo_data() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        assert!(!app.show_accounting);

        press(&mut app, KeyCode::Char('m'));
        let metric = app.metric;

        press(&mut app, KeyCode::Char('a'));
        assert!(app.show_accounting);
        assert!(app.accounting.is_some(), "打开时应采集账务信息");

        press(&mut app, KeyCode::Char('m'));
        assert_eq!(app.metric, metric, "面板打开时不应切换度量");

        press(&mut app, KeyCode::Esc);
        assert!(!app.show_accounting);
    }

    #[cfg(unix)]
    #[test]
    fn tick_marks_exited_process() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("启动 sleep");
        let pid = child.id() as i32;
        let mut snapshot = demo::snapshot();
        snapshot.pid = pid;

        let mut app = App::new(snapshot, Some(pid), false, false);
        app.tick();
        assert!(!app.process_exited, "子进程仍在运行");

        child.kill().expect("结束子进程");
        child.wait().expect("回收子进程");
        app.tick();

        assert!(app.process_exited);
        assert!(app.status.contains("已退出"), "实际: {}", app.status);
        assert!(!app.auto_refresh, "退出后应停止定时刷新");
    }

    #[test]
    fn filter_limits_visible_mappings() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        let total = app.snapshot.vmas.len();

        // 切到按文件分组，便于直接检查树的一级标签。
        press(&mut app, KeyCode::Char('g'));
        app.apply_filter("libc");

        let (filter, matched, count) = app.filter_summary().expect("过滤已生效");
        assert_eq!(filter, "libc");
        assert!(matched > 0 && matched < total);
        assert_eq!(count, total);
        assert!(!app.root.children.is_empty());
        assert!(app
            .root
            .children
            .iter()
            .all(|node| node.label.to_lowercase().contains("libc")));
        assert!(app.status.contains("过滤"));

        app.apply_filter("");
        assert!(app.filter.is_none());
        assert!(app.filter_summary().is_none());
        assert!(app.status.contains("已清除过滤"));
    }

    #[test]
    fn filter_input_accepts_non_hex_characters() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        press(&mut app, KeyCode::Char('f'));
        assert_eq!(app.input_target, InputTarget::Filter);

        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Char('b'));
        assert_eq!(app.input.as_deref(), Some("lib"));

        press(&mut app, KeyCode::Enter);
        assert!(app.input.is_none());
        assert_eq!(app.filter.as_deref(), Some("lib"));
    }

    #[test]
    fn filtered_out_address_reports_reason() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        let outside = app
            .snapshot
            .vmas
            .iter()
            .find(|vma| !vma.pathname.to_lowercase().contains("libc"))
            .map(|vma| vma.start)
            .expect("demo 数据应包含非 libc 映射");

        app.apply_filter("libc");
        app.lookup_address(&format!("{outside:x}"));

        assert!(
            app.status.contains("不在当前过滤结果中"),
            "实际: {}",
            app.status
        );
    }

    #[test]
    fn list_view_syncs_selection_with_treemap() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        assert_eq!(app.view, ViewMode::Treemap);

        press(&mut app, KeyCode::Char('v'));
        assert_eq!(app.view, ViewMode::List);
        assert!(app.status.contains("列表"));

        let rows = app.visible_vmas();
        assert!(rows.len() > 2);
        assert!(
            rows[0].metric(app.metric) >= rows[1].metric(app.metric),
            "列表按当前度量降序"
        );

        let second_start = rows[1].start;
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.list_index, 1);
        assert_eq!(
            app.selected_ranges().first().map(|range| range.0),
            Some(second_start),
            "列表选中项应同步到 treemap 与详情"
        );

        press(&mut app, KeyCode::Enter);
        assert_eq!(app.view, ViewMode::Treemap);
    }

    #[test]
    fn list_view_respects_filter() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        app.apply_filter("libc");
        press(&mut app, KeyCode::Char('v'));

        assert_eq!(app.view, ViewMode::List);
        let rows = app.visible_vmas();
        assert!(!rows.is_empty());
        assert!(rows
            .iter()
            .all(|vma| vma.pathname.to_lowercase().contains("libc")));
    }

    #[test]
    fn help_opens_and_closes_on_any_key() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        assert!(!app.show_help);

        press(&mut app, KeyCode::Char('?'));
        assert!(app.show_help);

        // 帮助打开时任何按键只用于关闭，不会触发其他操作。
        let metric = app.metric;
        press(&mut app, KeyCode::Char('m'));
        assert!(!app.show_help);
        assert_eq!(app.metric, metric, "关闭帮助的那次按键不应切换度量");
    }

    #[test]
    fn list_view_follows_sort_order() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        assert_eq!(app.sort, SortOrder::SizeDesc);

        press(&mut app, KeyCode::Char('o'));
        assert_eq!(app.sort, SortOrder::Address);

        let starts: Vec<u64> = app.visible_vmas().iter().map(|vma| vma.start).collect();
        let mut sorted = starts.clone();
        sorted.sort_unstable();
        assert_eq!(starts, sorted, "地址排序下列表应按起始地址升序");
    }

    #[test]
    fn exited_target_freezes_sampling() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        assert_eq!(app.poll_timeout(), Duration::from_millis(250));

        app.process_exited = true;
        assert_eq!(app.poll_timeout(), Duration::from_secs(1));

        app.refresh();
        assert!(
            app.status.contains("已退出"),
            "退出后刷新应提示不再采样，实际: {}",
            app.status
        );

        let threads_before = app.threads.len();
        app.refresh_threads();
        assert_eq!(app.threads.len(), threads_before, "退出后不应枚举线程");
        assert_eq!(app.stack_error.as_deref(), Some("目标进程已退出"));
    }

    #[test]
    fn replay_mode_blocks_sampling_and_attachment() {
        let snapshot = demo::snapshot();
        let expected = snapshot.vmas.len();
        let mut app = App::new(snapshot, None, false, false);
        app.set_replay(true);

        assert!(app.replay);
        assert!(!app.auto_refresh, "回放模式应关闭定时刷新");

        app.refresh();
        assert!(app.status.contains("回放"), "实际: {}", app.status);
        assert_eq!(app.snapshot.vmas.len(), expected, "回放不应改动快照");

        app.toggle_auto();
        assert!(!app.auto_refresh, "回放模式不能开启定时刷新");
        assert!(app.status.contains("回放"));

        app.refresh_accounting();
        assert!(app.accounting.is_none(), "回放模式没有账务数据");

        app.refresh_threads();
        assert!(
            app.stack_error
                .as_deref()
                .is_some_and(|err| err.contains("回放")),
            "实际: {:?}",
            app.stack_error
        );

        app.refresh_page_scan();
        assert!(
            app.page_error
                .as_deref()
                .is_some_and(|err| err.contains("回放")),
            "实际: {:?}",
            app.page_error
        );
    }

    #[test]
    fn memory_view_shows_demo_bytes_and_closes() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        assert!(app.memory.is_none());

        app.open_memory();
        let line_count = {
            let view = app.memory.as_ref().expect("应打开内存视图");
            assert!(view.error.is_none(), "实际: {:?}", view.error);
            assert_eq!(view.bytes.len(), MEMORY_WINDOW);
            assert!(view.base > 0, "基准地址应来自选中映射");
            view.line_count()
        };

        // 滚到底部后继续按 PgDn 不应越界。
        app.handle_key(KeyEvent::from(KeyCode::End));
        let last = app.memory.as_ref().unwrap().scroll;
        assert_eq!(last, line_count - 1);
        app.handle_key(KeyEvent::from(KeyCode::PageDown));
        assert_eq!(app.memory.as_ref().unwrap().scroll, last);

        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.memory.is_none());
    }

    #[test]
    fn memory_view_reports_replay_mode() {
        let mut app = App::new(demo::snapshot(), None, false, false);
        app.set_replay(true);
        app.open_memory();

        let view = app.memory.as_ref().expect("回放也应打开面板以显示原因");
        assert!(view.bytes.is_empty());
        assert!(
            view.error
                .as_deref()
                .is_some_and(|err| err.contains("回放")),
            "实际: {:?}",
            view.error
        );
    }
}
