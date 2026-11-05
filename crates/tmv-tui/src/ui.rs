//! 界面绘制：状态行、treemap、地址条、详情面板与键位提示。

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};
use tmv_core::{Category, MemNode, PageState, PageSummary, SortOrder};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, InputTarget, MemoryView, ViewMode};
use crate::treemap::squarify;

pub fn draw(frame: &mut Frame, app: &mut App) {
    let area = frame.area();
    if area.width < 40 || area.height < 8 {
        frame.render_widget(Paragraph::new("终端尺寸过小，请放大窗口"), area);
        return;
    }

    let bottom_height = if app.show_threads {
        12
    } else if app.show_pages && area.height >= 30 {
        9
    } else {
        0
    };
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(5),
        Constraint::Length(bottom_height),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(area);

    draw_header(frame, app, chunks[0]);

    if chunks[1].width >= 72 {
        let main =
            Layout::horizontal([Constraint::Min(30), Constraint::Length(40)]).split(chunks[1]);
        let layout = if app.view == ViewMode::List {
            app.last_layout.clear();
            draw_list(frame, app, main[0]);
            Vec::new()
        } else {
            draw_treemap(frame, app, main[0])
        };
        app.last_layout = layout;
        draw_detail(frame, app, main[1]);
    } else {
        let layout = if app.view == ViewMode::List {
            app.last_layout.clear();
            draw_list(frame, app, chunks[1]);
            Vec::new()
        } else {
            draw_treemap(frame, app, chunks[1])
        };
        app.last_layout = layout;
    }

    if app.show_threads {
        draw_stack(frame, app, chunks[2]);
    } else {
        draw_pages(frame, app, chunks[2]);
    }
    draw_strip(frame, app, chunks[3]);
    draw_footer(frame, app, chunks[4]);
    if app.show_settings {
        draw_settings(frame, app, area);
    }
    if app.show_help {
        draw_help(frame, area);
    }
    if app.show_accounting {
        draw_accounting(frame, app, area);
    }
    if app.memory.is_some() {
        draw_memory(frame, app, area);
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let name = if app.demo {
        "demo".to_string()
    } else if app.replay {
        format!("回放 pid {}", app.pid.unwrap_or_default())
    } else {
        format!("pid {}", app.pid.unwrap_or_default())
    };
    let geometry = format!(
        "{}bit/{}",
        app.snapshot.geometry.va_bits,
        page_label(app.snapshot.geometry.page_size)
    );
    let privilege = if app.privileged { "root" } else { "非 root" };

    let mut spans = vec![
        Span::styled(
            " tmv ",
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {name} "),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{} {} ", app.metric.label(), human_bytes(app.root.bytes)),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(
                "分组:{} 排序:{} {geometry} ",
                app.group_by.label(),
                app.sort.label()
            ),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!(" {privilege} "),
            if app.privileged {
                Style::default().fg(Color::Black).bg(Color::Green)
            } else {
                Style::default().fg(Color::Black).bg(Color::Yellow)
            },
        ),
    ];
    if !app.snapshot.has_smaps {
        spans.push(Span::styled(
            " smaps 不可读，仅虚拟大小 ",
            Style::default().fg(Color::Black).bg(Color::Red),
        ));
    }
    if app.process_exited {
        spans.push(Span::styled(
            " 进程已退出 ",
            Style::default().fg(Color::Black).bg(Color::Red),
        ));
    }
    if let Some((filter, matched, total)) = app.filter_summary() {
        spans.push(Span::styled(
            format!(" 过滤:{filter} {matched}/{total} "),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ));
    }
    if app.diff_enabled && app.diff.has_baseline {
        let color = if app.diff.total > 0 {
            Color::Red
        } else if app.diff.total < 0 {
            Color::Cyan
        } else {
            Color::DarkGray
        };
        spans.push(Span::styled(
            format!(" Δ{} ", signed_bytes(app.diff.total)),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    if app.auto_refresh {
        spans.push(Span::styled(
            format!(" 定时{}s ", app.interval.as_secs()),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ));
    } else if app.process_exited {
        spans.push(Span::styled(" 已停止 ", Style::default().fg(Color::Red)));
    } else {
        spans.push(Span::styled(" 手动 ", Style::default().fg(Color::DarkGray)));
    }
    if app.loading {
        spans.push(Span::styled(
            " 采集中… ",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if !app.status.is_empty() {
        spans.push(Span::styled(
            format!(" {}", app.status),
            Style::default().fg(Color::DarkGray),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_treemap(frame: &mut Frame, app: &App, area: Rect) -> Vec<Rect> {
    let focused = app.focused();
    let title = format!(" {}  ({} 项) ", app.breadcrumb(), focused.children.len());
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if focused.children.is_empty() {
        frame.render_widget(
            Paragraph::new("没有可展示的数据")
                .style(Style::default().fg(Color::DarkGray))
                .centered(),
            inner,
        );
        return Vec::new();
    }

    let weights: Vec<f64> = focused
        .children
        .iter()
        .map(|child| child.bytes as f64)
        .collect();
    let rects = squarify(&weights, inner);
    for (index, (child, rect)) in focused.children.iter().zip(rects.iter()).enumerate() {
        let delta = (app.diff_enabled && app.diff.has_baseline).then_some(child.delta);
        draw_block(frame, child, *rect, index == app.selected, delta);
    }
    rects
}

fn draw_block(frame: &mut Frame, node: &MemNode, rect: Rect, selected: bool, delta: Option<i64>) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }

    let color = category_color(node.category);
    // 留 1 格间隙做视觉分隔；过窄的块不缩，避免直接消失。
    let width = if rect.width > 2 {
        rect.width - 1
    } else {
        rect.width
    };
    let height = if rect.height > 2 {
        rect.height - 1
    } else {
        rect.height
    };
    let block_rect = Rect::new(rect.x, rect.y, width, height);

    frame.render_widget(
        Block::default().style(Style::default().bg(color)),
        block_rect,
    );

    if selected {
        // 只画左/右/下三边，避免白框覆盖块标题。
        frame.render_widget(
            Block::default()
                .borders(Borders::LEFT | Borders::RIGHT | Borders::BOTTOM)
                .border_style(Style::default().fg(Color::White)),
            block_rect,
        );
    }

    if block_rect.width >= 5 {
        if let Some(label) = block_label(node, block_rect.width, selected, delta) {
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(label, style))),
                Rect::new(block_rect.x, block_rect.y, block_rect.width, 1),
            );
        }
    } else if selected {
        frame.render_widget(
            Block::default().style(Style::default().bg(Color::White)),
            Rect::new(block_rect.x, block_rect.y, 1, 1),
        );
    }
}

fn block_label(node: &MemNode, width: u16, selected: bool, delta: Option<i64>) -> Option<String> {
    let marker = if selected { "▶ " } else { "" };
    let change = delta.filter(|value| *value != 0).map(signed_bytes);
    let suffix = match &change {
        Some(change) => format!(" {} {change}", human_bytes(node.bytes)),
        None => format!(" {}", human_bytes(node.bytes)),
    };
    let budget = width as usize;
    let fixed = UnicodeWidthStr::width(marker) + UnicodeWidthStr::width(suffix.as_str());
    if budget <= fixed + 2 {
        return None;
    }
    let name = truncate_display(&node.label, budget - fixed);
    Some(format!("{marker}{name}{suffix}"))
}

/// 映射列表：按当前度量降序，展示地址区间、各度量、权限与路径。
fn draw_list(frame: &mut Frame, app: &App, area: Rect) {
    if area.height < 3 || area.width == 0 {
        return;
    }
    let rows = app.visible_vmas();
    let order = match app.sort {
        SortOrder::SizeDesc => format!("按 {} 降序", app.metric.label()),
        SortOrder::Address => "按地址升序".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" 映射列表 · {} 项 · {order} ", rows.len()));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let header = format!(
        "{:<25} {:>8} {:>8} {:>8} {:>8} {:<5} {}",
        "地址区间", "虚拟", "RSS", "PSS", "Swap", "权限", "路径"
    );
    let mut lines = vec![Line::from(Span::styled(
        truncate_display(&header, inner.width as usize),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    ))];

    let capacity = inner.height.saturating_sub(1) as usize;
    if capacity > 0 && !rows.is_empty() {
        let selected = app.list_index.min(rows.len() - 1);
        let offset = selected.saturating_sub(capacity - 1);
        for (index, vma) in rows.iter().enumerate().skip(offset).take(capacity) {
            let stats = vma.stats.as_ref();
            let text = format!(
                "{:<25} {:>8} {:>8} {:>8} {:>8} {:<5} {}",
                format!("0x{:x}-0x{:x}", vma.start, vma.end),
                human_bytes(vma.end.saturating_sub(vma.start)),
                human_bytes(stats.map_or(0, |stats| stats.rss)),
                human_bytes(stats.map_or(0, |stats| stats.pss)),
                human_bytes(stats.map_or(0, |stats| stats.swap)),
                vma.perms.as_str(),
                vma.label()
            );
            let style = if index == selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(
                truncate_display(&text, inner.width as usize),
                style,
            )));
        }
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_detail(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(" 详情 ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(node) = app.selected_node() else {
        return;
    };

    let mut lines = vec![
        Line::from(Span::styled(
            node.label.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled(
                node.category.label().to_string(),
                Style::default().fg(category_color(node.category)),
            ),
            Span::raw(format!("  {}", human_bytes(node.bytes))),
        ]),
        Line::from(format!("地址 0x{:012x}-0x{:012x}", node.start, node.end)),
        Line::from(format!(
            "跨度 {}",
            human_bytes(node.end.saturating_sub(node.start))
        )),
    ];

    if let Some(resolved) = &app.resolved {
        if let Some(symbol) = &resolved.symbol {
            lines.push(Line::from(format!(
                "符号 {}+0x{:x}",
                symbol, resolved.offset
            )));
        }
        if let Some(location) = resolved.location() {
            lines.push(Line::from(format!("源码 {location}")));
        }
    }

    if app.diff_enabled && app.diff.has_baseline {
        let before = (node.bytes as i64 - node.delta).max(0) as u64;
        lines.push(Line::from(format!(
            "变化 {}  ({} → {})",
            signed_bytes(node.delta),
            human_bytes(before),
            human_bytes(node.bytes)
        )));
    }

    if let Some(vma) = &node.vma {
        lines.push(Line::from(format!("权限 {}", vma.perms.as_str())));
        if vma.offset != 0 {
            lines.push(Line::from(format!("偏移 0x{:x}", vma.offset)));
        }
        if !vma.pathname.is_empty() {
            lines.push(Line::from(vma.pathname.clone()));
        }
        if let Some(stats) = &vma.stats {
            lines.push(Line::from(""));
            let rows = [
                ("Size", stats.size),
                ("RSS", stats.rss),
                ("PSS", stats.pss),
                ("独占", stats.private()),
                ("共享", stats.shared()),
                ("Swap", stats.swap),
                ("AnonHuge", stats.anon_huge),
            ];
            for (key, value) in rows {
                lines.push(Line::from(format!("{key:<10}{}", human_bytes(value))));
            }
        }
    } else {
        lines.push(Line::from(format!("含 {} 个子项", node.children.len())));
        lines.push(Line::from(""));
        let mut top: Vec<&MemNode> = node.children.iter().collect();
        top.sort_by_key(|node| std::cmp::Reverse(node.bytes));
        for child in top.iter().take(8) {
            let change = if app.diff_enabled && app.diff.has_baseline && child.delta != 0 {
                format!(" {}", signed_bytes(child.delta))
            } else {
                String::new()
            };
            lines.push(Line::from(format!(
                "{}  {}{change}",
                truncate_display(&child.label, 18),
                human_bytes(child.bytes)
            )));
        }
    }

    if let Some(scan) = &app.page_scan {
        let summary = &scan.summary;
        lines.push(Line::from(""));
        lines.push(Line::from(format!(
            "页 {} 个，采样 {}",
            scan.pages, summary.samples
        )));
        lines.push(Line::from(format!(
            "驻留 {}%  Swap {}%  未映射 {}%",
            pct(summary.present, summary.samples),
            pct(summary.swapped, summary.samples),
            absent_pct(summary)
        )));
        lines.push(Line::from(format!(
            "文件 {}%  共享 {}%  THP {}%",
            pct(summary.file, summary.samples),
            pct(summary.shared, summary.samples),
            pct(summary.thp, summary.samples)
        )));
    }

    if let Some(result) = &app.search_result {
        let mut block = vec![
            Line::from(Span::styled(
                format!("查找 0x{:x}", result.address),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(format!("模块 {}", result.module)),
        ];
        if let Some(symbol) = &result.symbol {
            block.push(Line::from(format!("符号 {}+0x{:x}", symbol, result.offset)));
        }
        if let Some(location) = result.location() {
            block.push(Line::from(format!("源码 {location}")));
        }
        block.push(Line::from(""));
        lines.splice(0..0, block);
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

const PAGE_ANON: Color = Color::Indexed(33);
const PAGE_FILE: Color = Color::Indexed(35);
const PAGE_SHARED: Color = Color::Indexed(170);
const PAGE_SWAP: Color = Color::Indexed(178);
const PAGE_ABSENT: Color = Color::Indexed(236);
const PAGE_THP: Color = Color::Indexed(48);

fn page_color(state: &PageState) -> Color {
    if !state.present {
        return if state.swapped {
            PAGE_SWAP
        } else {
            PAGE_ABSENT
        };
    }
    if state.shared {
        PAGE_SHARED
    } else if state.thp {
        PAGE_THP
    } else if state.file {
        PAGE_FILE
    } else {
        PAGE_ANON
    }
}

fn draw_pages(frame: &mut Frame, app: &App, area: Rect) {
    if area.height < 3 || area.width == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(page_title(app));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let heat_rows = inner.height.saturating_sub(1);
    if heat_rows > 0 {
        match (&app.page_scan, &app.page_error) {
            (Some(scan), _) if !scan.states.is_empty() => {
                let cells = inner.width as usize * heat_rows as usize;
                let total = scan.states.len();
                let count = cells.min(total);
                for index in 0..count {
                    // 样本多于格子时均匀抽点，少于格子时按地址顺序铺开。
                    let sample = if total > cells {
                        index * total / cells
                    } else {
                        index
                    };
                    let state = scan.states[sample];
                    let x = inner.x + (index % inner.width as usize) as u16;
                    let y = inner.y + (index / inner.width as usize) as u16;
                    frame.buffer_mut().set_string(
                        x,
                        y,
                        " ",
                        Style::default().bg(page_color(&state)),
                    );
                }
            }
            (_, Some(err)) => {
                frame.render_widget(
                    Paragraph::new(format!("页图不可用: {err}"))
                        .style(Style::default().fg(Color::Red)),
                    Rect::new(inner.x, inner.y, inner.width, heat_rows),
                );
            }
            _ if app.page_loading => {
                frame.render_widget(
                    Paragraph::new("页状态扫描中…").style(Style::default().fg(Color::DarkGray)),
                    Rect::new(inner.x, inner.y, inner.width, heat_rows),
                );
            }
            _ => {
                frame.render_widget(
                    Paragraph::new("页状态不可用").style(Style::default().fg(Color::DarkGray)),
                    Rect::new(inner.x, inner.y, inner.width, heat_rows),
                );
            }
        }
    }

    if inner.height >= 2 {
        let legend_area = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
        frame.render_widget(Paragraph::new(page_legend()), legend_area);
    }
}

fn page_legend() -> Line<'static> {
    Line::from(vec![
        Span::styled("█ ", Style::default().fg(PAGE_ANON)),
        Span::raw("匿名 "),
        Span::styled("█ ", Style::default().fg(PAGE_FILE)),
        Span::raw("文件 "),
        Span::styled("█ ", Style::default().fg(PAGE_SHARED)),
        Span::raw("共享 "),
        Span::styled("█ ", Style::default().fg(PAGE_SWAP)),
        Span::raw("Swap "),
        Span::styled("█ ", Style::default().fg(PAGE_ABSENT)),
        Span::raw("未映射 "),
        Span::styled("█ ", Style::default().fg(PAGE_THP)),
        Span::raw("THP"),
    ])
}

fn draw_stack(frame: &mut Frame, app: &App, area: Rect) {
    if area.height < 3 || area.width == 0 {
        return;
    }
    let block = Block::default()
        .borders(Borders::ALL)
        .title(stack_title(app));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if let Some(err) = &app.stack_error {
        frame.render_widget(
            Paragraph::new(format!("回溯不可用: {err}"))
                .wrap(Wrap { trim: true })
                .style(Style::default().fg(Color::Red)),
            inner,
        );
        return;
    }

    let mut lines = Vec::new();
    for (index, (pc, resolved)) in app.stack_frames.iter().enumerate() {
        if index >= inner.height as usize {
            break;
        }
        let mut text = format!("#{index} 0x{pc:012x}");
        if let Some(resolved) = resolved {
            if let Some(symbol) = &resolved.symbol {
                text.push_str(&format!("  {}+0x{:x}", symbol, resolved.offset));
            }
            if let Some(location) = resolved.location() {
                text.push_str(&format!("  {location}"));
            }
        }
        lines.push(Line::from(truncate_display(&text, inner.width as usize)));
    }
    if lines.is_empty() {
        lines.push(Line::from("没有可回溯的帧"));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn stack_title(app: &App) -> String {
    let Some(stack) = &app.stack else {
        return match app.threads.get(app.thread_index) {
            Some(thread) => format!(
                " 线程 {}/{} · {} ({}) ",
                app.thread_index + 1,
                app.threads.len(),
                thread.name,
                thread.tid
            ),
            None => " 线程 · 不可用 ".to_string(),
        };
    };
    let tls = stack
        .tls
        .map(|tls| format!(" · TLS 0x{tls:x}"))
        .unwrap_or_default();
    let mode = if app.demo {
        "demo"
    } else {
        "attach 期间暂停"
    };
    format!(
        " 线程 {}/{} · {} ({}){tls} · {} 帧 · {} · {mode} ",
        app.thread_index + 1,
        app.threads.len(),
        stack.name,
        stack.tid,
        stack.frames.len(),
        stack.method.label()
    )
}

fn page_title(app: &App) -> String {
    let Some(scan) = &app.page_scan else {
        if app.page_loading {
            return " 页图 · 扫描中… ".to_string();
        }
        return match &app.page_error {
            Some(_) => " 页图 · 不可用 ".to_string(),
            None => " 页图 ".to_string(),
        };
    };
    let summary = &scan.summary;
    let mut title = format!(" 页图 · {} 页", scan.pages);
    if scan.sampled {
        title.push_str(&format!(" · 采样 {}", scan.states.len()));
    }
    if summary.samples > 0 {
        title.push_str(&format!(
            " · 驻留 {}% · Swap {}%",
            pct(summary.present, summary.samples),
            pct(summary.swapped, summary.samples)
        ));
        if summary.shared > 0 {
            title.push_str(&format!(
                " · 共享 {}%",
                pct(summary.shared, summary.samples)
            ));
        }
        if summary.thp > 0 {
            title.push_str(&format!(" · THP {}%", pct(summary.thp, summary.samples)));
        }
    }
    if !summary.pfn_available && summary.present > 0 {
        title.push_str(" · PFN 需 root");
    }
    if let Some(delta) = &app.page_delta {
        let mut parts = Vec::new();
        if delta.became_present > 0 {
            parts.push(format!("驻留+{}", delta.became_present));
        }
        if delta.became_swapped > 0 {
            parts.push(format!("换出+{}", delta.became_swapped));
        }
        if delta.became_absent > 0 {
            parts.push(format!("取消+{}", delta.became_absent));
        }
        if delta.became_shared > 0 {
            parts.push(format!("共享+{}", delta.became_shared));
        }
        if delta.became_exclusive > 0 {
            parts.push(format!("独占+{}", delta.became_exclusive));
        }
        if !parts.is_empty() {
            title.push_str(&format!(" · 变化 {}", parts.join(" ")));
        }
    }
    title
}

fn pct(part: u64, total: u64) -> u64 {
    // 大映射下 part * 100 可能溢出，用饱和乘法；total 为 0 时按分母 1 处理。
    part.saturating_mul(100).saturating_add(total / 2) / total.max(1)
}

/// 未映射百分比取补数，避免三项四舍五入后合计超过 100%。
fn absent_pct(summary: &PageSummary) -> u64 {
    let occupied = pct(summary.present, summary.samples) + pct(summary.swapped, summary.samples);
    100u64.saturating_sub(occupied)
}

fn draw_strip(frame: &mut Frame, app: &App, area: Rect) {
    if area.height < 3 || area.width == 0 {
        return;
    }
    let vmas = &app.snapshot.vmas;
    if vmas.is_empty() {
        return;
    }
    let total: u64 = vmas.iter().map(|vma| vma.end - vma.start).sum();
    if total == 0 {
        return;
    }

    let selected = app.selected_ranges();
    let mut accumulated = 0u64;
    let mut prev = 0u16;
    for vma in vmas {
        accumulated += vma.end - vma.start;
        let x1 = (((accumulated as f64 / total as f64) * area.width as f64).round() as u16)
            .clamp(prev, area.width);
        if x1 > prev {
            let rect = Rect::new(area.x + prev, area.y, x1 - prev, 1);
            frame.render_widget(
                Block::default().style(Style::default().bg(category_color(vma.category()))),
                rect,
            );
            let hit = selected
                .iter()
                .any(|(start, end)| vma.start < *end && vma.end > *start);
            if hit {
                frame.render_widget(
                    Block::default().style(Style::default().bg(Color::White)),
                    Rect::new(area.x + prev, area.y + 1, x1 - prev, 1),
                );
            }
        }
        prev = x1;
    }

    let first = vmas.first().map_or(0, |vma| vma.start);
    let last = vmas.last().map_or(0, |vma| vma.end);
    let info = format!(
        "地址序 0x{first:012x} → 0x{last:012x}   面积 = {}",
        app.metric.label()
    );
    frame.render_widget(
        Paragraph::new(info).style(Style::default().fg(Color::DarkGray)),
        Rect::new(area.x, area.y + 2, area.width, 1),
    );
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let (text, color) = match &app.input {
        Some(input) if app.input_target == InputTarget::Filter => (
            format!(" 过滤(路径/权限): {input}▏  Enter 应用  Esc 取消"),
            Color::Cyan,
        ),
        Some(input) if app.input_target == InputTarget::Regex => (
            format!(" 正则过滤(路径/标签): {input}▏  Enter 应用  Esc 取消"),
            Color::Magenta,
        ),
        Some(input) => (
            format!(" 地址(十六进制): {input}▏  Enter 解析  Esc 取消"),
            Color::Yellow,
        ),
        None if app.show_settings => (
            " ↑↓ 选择  ←→ 调整  Enter 切换  Esc 关闭".to_string(),
            Color::Yellow,
        ),
        None if app.show_accounting => (
            " r 重新采集  Esc 关闭账务面板".to_string(),
            Color::Yellow,
        ),
        None if app.memory.is_some() => (
            " ↑↓ 滚动  PgUp/PgDn 翻页  Home/End 首尾  Esc 关闭".to_string(),
            Color::Yellow,
        ),
        None if app.show_threads => (
            " [ ] 切换线程  u 重新回溯  t 关闭栈  / 地址  s 设置  r 刷新  rr 定时  q 退出"
                .to_string(),
            Color::DarkGray,
        ),
        None => (
            " ←→↑↓ 选择  Enter 下钻  Esc 返回  v 视图  / 地址  f 过滤  F 正则  m 指标  g 分组  o 排序  p 页图  t 栈  a 账务  x 内存  s 设置  ? 帮助  r 刷新  rr 定时  q 退出"
                .to_string(),
            Color::DarkGray,
        ),
    };
    frame.render_widget(Paragraph::new(text).style(Style::default().fg(color)), area);
}

/// 帮助面板里的按键说明，按「分类 / 按键 / 说明」预排版，避免中英混排错位。
const HELP_LINES: [&str; 14] = [
    " 移动   ←→↑↓ / hjkl    选择相邻块；列表视图逐行移动",
    " 导航   Enter / Esc     下钻 / 返回；列表视图切回 treemap",
    " 视图   v               treemap ↔ 映射列表",
    " 过滤   f               路径 / 标签 / 权限，Enter 应用",
    " 过滤   F               同一范围的正则过滤，Enter 应用",
    " 地址   /               解析十六进制地址并跳转",
    " 度量   m               PSS / RSS / 虚拟 / 独占 / 共享 / Swap",
    " 分组   g               类别 / 文件 / 映射",
    " 排序   o               大小 / 地址（同时作用于列表）",
    " 面板   p t a           页图 / 线程栈 / 账务",
    " 内存   x               十六进制查看当前映射内容",
    " 刷新   r rr s          立即刷新 / 定时刷新 / 设置",
    " 进程   Esc q           根层 Esc 退出；q 随时退出",
    " 关闭   任意键          关闭本帮助",
];

fn draw_help(frame: &mut Frame, area: Rect) {
    let width = 62.min(area.width.saturating_sub(4));
    let height = (HELP_LINES.len() as u16 + 2).min(area.height.saturating_sub(2));
    if width < 40 || height < 6 {
        return;
    }
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 帮助 ")
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let lines: Vec<Line> = HELP_LINES
        .iter()
        .take(inner.height as usize)
        .map(|line| {
            Line::from(Span::styled(
                truncate_display(line, inner.width as usize),
                Style::default(),
            ))
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_settings(frame: &mut Frame, app: &App, area: Rect) {
    let width = 46.min(area.width.saturating_sub(4));
    let height = 9.min(area.height.saturating_sub(2));
    if width < 20 || height < 6 {
        return;
    }
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 设置 ")
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.height < 3 || inner.width == 0 {
        return;
    }

    let items = [
        ("定时刷新", if app.auto_refresh { "开" } else { "关" }),
        ("刷新间隔", ""),
        ("页图", if app.show_pages { "开" } else { "关" }),
        ("变化标记", if app.diff_enabled { "开" } else { "关" }),
    ];
    for (index, (label, value)) in items.iter().enumerate() {
        if index as u16 >= inner.height.saturating_sub(1) {
            break;
        }
        let selected = index == app.settings_index;
        let marker = if selected { "▶ " } else { "  " };
        let value = if *label == "刷新间隔" {
            format!("{}s", app.interval.as_secs())
        } else {
            (*value).to_string()
        };
        let style = if selected {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        frame.render_widget(
            Paragraph::new(format!("{marker}{label}  {value}")).style(style),
            Rect::new(inner.x, inner.y + index as u16, inner.width, 1),
        );
    }

    let hint = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
    frame.render_widget(
        Paragraph::new("↑↓ 选择  ←→ 调整  Enter 切换  Esc 关闭")
            .style(Style::default().fg(Color::DarkGray)),
        hint,
    );
}

fn draw_accounting(frame: &mut Frame, app: &App, area: Rect) {
    let Some(accounting) = &app.accounting else {
        return;
    };
    let lines = tmv_core::accounting::lines(accounting, &app.snapshot);
    let width = 104.min(area.width.saturating_sub(4));
    let height = (lines.len() as u16 + 3).min(area.height.saturating_sub(2));
    if width < 48 || height < 6 {
        return;
    }
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 内存账务 ")
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.height < 2 || inner.width == 0 {
        return;
    }

    let body_height = inner.height.saturating_sub(1);
    for (index, line) in lines.iter().enumerate() {
        if index as u16 >= body_height {
            break;
        }
        let style = if line.starts_with("差异") {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::Gray)
        };
        frame.render_widget(
            Paragraph::new(truncate_display(line, inner.width as usize)).style(style),
            Rect::new(inner.x, inner.y + index as u16, inner.width, 1),
        );
    }

    let hint = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
    frame.render_widget(
        Paragraph::new("r 重新采集  Esc 关闭").style(Style::default().fg(Color::DarkGray)),
        hint,
    );
}

fn category_color(category: Category) -> Color {
    match category {
        Category::Heap => Color::Indexed(178),
        Category::Stack => Color::Indexed(167),
        Category::JitExec => Color::Indexed(99),
        Category::Vdso => Color::Indexed(244),
        Category::AnonShared => Color::Indexed(73),
        Category::AnonPrivate => Color::Indexed(67),
        Category::FileShared => Color::Indexed(108),
        Category::FilePrivate => Color::Indexed(66),
        Category::Special => Color::Indexed(240),
    }
}

fn page_label(size: u64) -> String {
    if size >= 1024 && size % 1024 == 0 {
        format!("{}K", size / 1024)
    } else {
        format!("{size}B")
    }
}

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "K", "M", "G", "T"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes}B")
    } else {
        format!("{value:.1}{}", UNITS[unit])
    }
}

/// 带增减方向的字节数，例如 "▲4.0M"。
fn signed_bytes(delta: i64) -> String {
    let marker = if delta >= 0 { '▲' } else { '▼' };
    format!("{marker}{}", human_bytes(delta.unsigned_abs()))
}

/// 内存查看器：地址 + 十六进制 + ASCII，类似 gdb 的 `x` 命令。
fn draw_memory(frame: &mut Frame, app: &App, area: Rect) {
    let Some(view) = &app.memory else {
        return;
    };
    // 16 列地址 + 47 列 hex + 18 列 ASCII + 间距 ≈ 88 列。
    let width = 92.min(area.width.saturating_sub(2));
    let height = 24.min(area.height.saturating_sub(2));
    if width < 60 || height < 5 {
        return;
    }
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, rect);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " 内存 · pid {} · 0x{:x} · ↑↓ PgUp/PgDn，Esc 关闭 ",
            app.pid.unwrap_or_default(),
            view.base
        ))
        .style(Style::default().bg(Color::Black));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    if let Some(error) = &view.error {
        let line = Line::from(Span::styled(
            truncate_display(&format!("无法读取: {error}"), inner.width as usize),
            Style::default().fg(Color::Red),
        ));
        frame.render_widget(Paragraph::new(vec![line]), inner);
        return;
    }

    let visible = inner.height as usize;
    let first = view.scroll.min(view.line_count().saturating_sub(1));
    let mut lines = Vec::with_capacity(visible);
    for (index, chunk) in view
        .bytes
        .chunks(MemoryView::LINE)
        .enumerate()
        .skip(first)
        .take(visible)
    {
        let address = view.base + (index * MemoryView::LINE) as u64;
        lines.push(memory_line(address, chunk, inner.width as usize));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

/// 一行 hexdump。
fn memory_line(address: u64, bytes: &[u8], width: usize) -> Line<'static> {
    let mut hex = String::with_capacity(MemoryView::LINE * 3);
    for index in 0..MemoryView::LINE {
        if index > 0 {
            hex.push(' ');
        }
        match bytes.get(index) {
            Some(byte) => hex.push_str(&format!("{byte:02x}")),
            None => hex.push_str("  "),
        }
    }
    let ascii: String = (0..MemoryView::LINE)
        .map(|index| match bytes.get(index) {
            Some(byte) if byte.is_ascii_graphic() || *byte == b' ' => *byte as char,
            Some(_) => '.',
            None => ' ',
        })
        .collect();

    Line::from(Span::styled(
        truncate_display(&format!("{address:016x}  {hex}  |{ascii}|"), width),
        Style::default(),
    ))
}

fn truncate_display(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        // 预留 1 格给省略号
        if used + width + 1 > max_width {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};
    use tmv_core::demo;

    fn screen_text(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    fn dump_screen(terminal: &Terminal<TestBackend>) {
        if std::env::var_os("TMV_DUMP").is_none() {
            return;
        }
        let buffer = terminal.backend().buffer();
        let width = buffer.area.width as usize;
        for row in buffer.content().chunks(width) {
            println!(
                "{}",
                row.iter().map(|cell| cell.symbol()).collect::<String>()
            );
        }
    }

    #[test]
    fn memory_modal_renders_hexdump_and_errors() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        app.open_memory();
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        dump_screen(&terminal);

        let text: String = screen_text(&terminal)
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        assert!(text.contains("内存"), "内存面板标题未渲染: {text}");
        assert!(text.contains('|'), "ASCII 栏未渲染");

        // 回放模式打开同一面板时，应显示不可读原因。
        let mut replay = App::new(demo::snapshot(), None, false, false);
        replay.set_replay(true);
        replay.open_memory();
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut replay)).unwrap();
        let text: String = screen_text(&terminal)
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        assert!(text.contains("无法读取"), "未显示读取失败原因: {text}");
    }

    #[test]
    fn replay_header_marks_mode() {
        let mut app = App::new(demo::snapshot(), None, false, false);
        app.pid = Some(app.snapshot.pid);
        app.set_replay(true);
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        dump_screen(&terminal);

        let text: String = screen_text(&terminal)
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect();
        assert!(text.contains("回放pid"), "状态行未标注回放: {text}");
    }

    #[test]
    fn demo_renders_treemap_and_detail() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let text = screen_text(&terminal);
        assert!(text.contains("tmv"), "状态行未渲染");
        assert!(text.contains("[heap]"), "treemap 未渲染 heap 块");
        assert!(text.contains("PSS"), "缺少度量标注");
        assert!(text.contains('页'), "页图面板未渲染");
        assert!(app.page_scan.is_some(), "示例数据应产生页级扫描结果");

        let backgrounds: std::collections::HashSet<Color> = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.bg)
            .collect();
        assert!(backgrounds.len() >= 3, "页图单元格未着色: {backgrounds:?}");

        let invisible: Vec<String> = app
            .children()
            .iter()
            .zip(app.last_layout.iter())
            .filter(|(_, rect)| rect.width == 0 || rect.height == 0)
            .map(|(child, rect)| format!("{} {rect:?}", child.label))
            .collect();
        assert!(invisible.is_empty(), "存在不可见块: {invisible:?}");

        if std::env::var_os("TMV_DUMP").is_some() {
            dump_screen(&terminal);
        }
    }

    #[test]
    fn diff_marker_renders_after_refresh() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let text = screen_text(&terminal);
        assert!(text.contains('▲'), "刷新后应显示增长标记");
        assert!(text.contains('变'), "详情应显示变化区间");
        dump_screen(&terminal);
    }

    #[test]
    fn settings_modal_renders_interval() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let text = screen_text(&terminal);
        assert!(text.contains('设'), "设置弹窗未渲染");
        assert!(text.contains('隔'), "缺少间隔配置项");
        assert!(text.contains("5s"), "应显示默认间隔");
        assert!(text.contains('手'), "未开启定时刷新时应显示手动");
        dump_screen(&terminal);
    }

    #[test]
    fn thread_stack_panel_renders() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let text = screen_text(&terminal);
        assert!(text.contains('线'), "线程面板未渲染");
        assert!(text.contains("#0"), "缺少回溯帧");
        assert!(text.contains("TLS"), "缺少 TLS 信息");
        dump_screen(&terminal);
    }

    #[test]
    fn accounting_modal_renders_sections() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));

        let backend = TestBackend::new(140, 44);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let text = screen_text(&terminal);
        assert!(text.contains('账'), "账务面板未渲染");
        assert!(text.contains("KSM"), "缺少 KSM 段");
        assert!(text.contains("cgroup"), "缺少 cgroup 段");
        assert!(text.contains("NUMA"), "缺少 NUMA 段");
        dump_screen(&terminal);
    }

    #[test]
    fn page_title_reports_delta() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        app.page_delta = Some(tmv_core::PageDelta {
            became_present: 3,
            became_swapped: 2,
            ..Default::default()
        });

        let backend = TestBackend::new(150, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let text = screen_text(&terminal);
        assert!(text.contains('变'), "页图标题未显示变化");
        assert!(text.contains("+3"), "缺少新驻留页计数");
    }

    #[test]
    fn list_view_renders_rows_and_filter_badge() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        app.apply_filter("libc");
        app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));

        let backend = TestBackend::new(140, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        dump_screen(&terminal);

        // 宽字符在缓冲区里占两格，拼接时中间会插入空格，比较前先去掉空白。
        let text: String = screen_text(&terminal)
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        assert!(text.contains("映射列表"), "列表视图未渲染");
        assert!(text.contains("RSS"), "缺少 RSS 列");
        assert!(text.contains("过滤:libc"), "状态行未显示过滤条件");
        assert!(text.contains("libc"), "列表行应包含 libc 路径");
        assert!(
            !app.last_layout.iter().any(|rect| rect.width > 0),
            "列表视图不应产生 treemap 布局"
        );
    }

    #[test]
    fn help_modal_renders_key_reference() {
        let mut app = App::new(demo::snapshot(), None, true, false);
        app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        dump_screen(&terminal);

        let text: String = screen_text(&terminal)
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        assert!(text.contains("帮助"), "帮助面板未渲染");
        assert!(text.contains("映射列表"), "缺少视图切换说明");
        assert!(text.contains("关闭本帮助"), "缺少关闭提示");
    }
}
