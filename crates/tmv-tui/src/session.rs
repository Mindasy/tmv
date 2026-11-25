//! Sample-based session browser. Original layout App remains the layout renderer.
use crate::{timeline, treemap, ui, App};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    MouseButton, MouseEventKind,
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};
use std::{
    io,
    path::Path,
    time::{Duration, Instant},
};
use tmv_core::{session, MemNode, ProcessSnapshot};

pub fn run(path: &Path) -> io::Result<()> {
    run_at(path, None)
}
pub fn run_at(path: &Path, at: Option<u64>) -> io::Result<()> {
    let mut manifest = session::manifest(path)?;
    // Fixed for this viewer: a live viewer does not grow a replay timeline on exit.
    let offline = manifest.state != "recording";
    if !offline && at.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--at 仅适用于已结束记录的离线回放",
        ));
    }
    if manifest.frames == 0 {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "session has no samples",
        ));
    }
    let mut index = if let Some(time) = at {
        session::at(path, &manifest, time)?
    } else {
        manifest.frames - 1
    };
    let mut sample = tmv_symbols::replay::frame(path, index, manifest.pid)?;
    if manifest.state != "recording" && at.is_none() {
        while index > 0 && sample.snapshot.is_none() {
            index -= 1;
            sample = tmv_symbols::replay::frame(path, index, manifest.pid)?;
        }
    }
    let mut app = App::new(
        sample
            .snapshot
            .clone()
            .unwrap_or_else(|| ProcessSnapshot::from_vmas(manifest.pid as i32, vec![], false)),
        None,
        false,
        false,
    );
    app.pid = Some(manifest.pid as i32);
    app.set_replay(true);
    let mut allocation = false;
    let mut selected = 0usize;
    let mut group: Option<String> = None;
    let mut list = false;
    let mut blockmap = false;
    let mut following = manifest.state == "recording" && at.is_none();
    let mut playing = false;
    let mut last_step = Instant::now();
    let mut stack_view: Option<Vec<String>> = None;
    let mut session_help = false;
    let mut stack_scroll = 0usize;
    let mut trend = Vec::new();
    if offline {
        for i in 0..manifest.frames {
            trend.push(timeline::Point::from_sample(&session::frame(path, i)?));
        }
    }
    let mut trend_rss = false;
    let mut time_input: Option<String> = None;
    let mut time_error = String::new();
    let mut timeline_area = Rect::default();
    let mut scrubber = timeline::Scrubber::default();
    let mut block_hits = Vec::new();
    let controls = crate::mouse::MouseState::default();
    let mut side_area = Rect::default();
    let mut last_click: Option<(Instant, usize, Option<String>)> = None;
    let mut selection_drag = false;
    let mut side_scroll = 0usize;
    let mut side_key = None;
    let mut terminal = ratatui::try_init()?;
    let result = (|| -> io::Result<()> {
        crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
        loop {
            let root = session::allocation_tree(&sample);
            if group
                .as_ref()
                .is_some_and(|label| !root.children.iter().any(|n| &n.label == label))
            {
                group = None;
                selected = 0;
            }
            let focused = group
                .as_ref()
                .and_then(|label| root.children.iter().find(|n| &n.label == label))
                .unwrap_or(&root);
            selected = selected.min(focused.children.len().saturating_sub(1));
            let current_key = (index, group.clone(), selected);
            if side_key.as_ref() != Some(&current_key) {
                side_scroll = 0;
                side_key = Some(current_key);
            }
            let side_lines = selected_stack(&sample, group.as_deref(), focused, selected);
            terminal.draw(|frame| {
                controls.hits.borrow_mut().clear();
                let area=frame.area();
                    use crate::mouse::Action;
                    let mut items=vec![("布局/分配",Action::Key(KeyCode::Tab)),("返回",Action::Key(KeyCode::Backspace)),("列表",Action::Key(KeyCode::Char('v'))),("地址图",Action::Key(KeyCode::Char('b'))),("栈",Action::Key(KeyCode::Char('t'))),("帮助",Action::Key(KeyCode::Char('?'))),("退出",Action::Key(KeyCode::Char('q')))];
                    if allocation && list && !blockmap {items.insert(0,(app.list_unit.button(),Action::Key(KeyCode::Char('U'))));}
                    if offline {items.extend([("前一",Action::Key(KeyCode::Char('['))),("后一",Action::Key(KeyCode::Char(']'))),("播放",Action::Key(KeyCode::Char(' '))),("峰值",Action::Key(KeyCode::Char('P'))),("时间",Action::Key(KeyCode::Char('T'))),("度量",Action::Key(KeyCode::Char('M')))]);}else{items.push(("暂停跟随",Action::Key(KeyCode::Char(' '))));}
                let rows=crate::mouse::button_rows(area.width,&items).min(area.height);
                let main=Rect::new(area.x,area.y,area.width,area.height.saturating_sub(rows + if offline {5}else{0}));
                timeline_area=Rect::new(area.x,area.y+main.height,area.width,area.height.saturating_sub(main.height).saturating_sub(rows));
                if allocation {
                    let (left,right)=allocation_columns(main);
                    side_area=right;
                    block_hits=draw_allocations(frame,focused,selected,list.then_some(app.list_unit),blockmap,sample.heap_records,left);
                    side_scroll=draw_stack_side(frame,right,&side_lines,side_scroll);
                } else if sample.snapshot.is_none() {
                    frame.render_widget(Paragraph::new(format!("此采样点没有布局数据：{}\n按 [ 查看前一采样；Tab 查看分配归因。",sample.layout_error.as_deref().unwrap_or("未采集"))),main);
                } else {ui::draw_in(frame,&mut app,main);}
                if offline {timeline::draw(frame,timeline_area,&trend,index,trend_rss);}
                let area=frame.area();
                if area.height>0 && app.memory.is_none() && !app.show_help && !app.show_settings && !app.show_accounting && app.input.is_none() {
                    crate::mouse::buttons(frame,Rect::new(area.x,area.bottom()-rows,area.width,rows),&items,&controls);
                }
                if let Some(input)=&time_input {
                    controls.hits.borrow_mut().clear();
                    frame.render_widget(Paragraph::new(format!("距记录开始的秒数 [{:.3}～{:.3}] 当前 {:.3}s: {input}  Enter确认 Esc取消  {time_error}",trend.first().map_or(0.,|p|p.ms as f64/1000.),trend.last().map_or(0.,|p|p.ms as f64/1000.),sample.elapsed_ms as f64/1000.)).style(Style::default().fg(Color::Yellow)),Rect::new(area.x,area.bottom().saturating_sub(1),area.width,1));
                }
                if time_input.is_some() {crate::mouse::buttons(frame,Rect::new(area.right().saturating_sub(26),area.bottom().saturating_sub(1),26,1),&[("确认",crate::mouse::Action::Key(KeyCode::Enter)),("取消",crate::mouse::Action::Key(KeyCode::Esc))],&controls);}
                if let Some(lines)=&stack_view {
                    controls.hits.borrow_mut().clear();draw_stack_popup(frame,lines,stack_scroll,session_help);
                    crate::mouse::buttons(frame,Rect::new(area.right().saturating_sub(12),area.y+1,12,1),&[("关闭",crate::mouse::Action::Key(KeyCode::Esc))],&controls);
                }
            })?;
            let mut next = index;
            if event::poll(Duration::from_millis(200))? {
                let mut incoming = event::read()?;
                if let Event::Mouse(mouse) = incoming.clone() {
                    if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                        && controls.hit(mouse.column, mouse.row).is_some()
                    {
                        if let Some(crate::mouse::Action::Key(key)) =
                            controls.hit(mouse.column, mouse.row)
                        {
                            incoming = Event::Key(key.into());
                        }
                    } else if stack_view.is_some() {
                        match mouse.kind {
                            MouseEventKind::ScrollDown => {
                                stack_scroll = (stack_scroll + 3)
                                    .min(stack_view.as_ref().unwrap().len().saturating_sub(1))
                            }
                            MouseEventKind::ScrollUp => {
                                stack_scroll = stack_scroll.saturating_sub(3)
                            }
                            MouseEventKind::Down(MouseButton::Right) => stack_view = None,
                            _ => {}
                        }
                    } else if time_input.is_some() {
                        if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
                            time_input = None;
                        }
                    } else if !allocation
                        && (app.memory.is_some()
                            || app.show_settings
                            || app.show_help
                            || app.show_accounting
                            || app.input.is_some())
                    {
                        app.handle_mouse(mouse);
                    } else if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                        && controls.hit(mouse.column, mouse.row).is_some()
                    {
                        if let Some(crate::mouse::Action::Key(key)) =
                            controls.hit(mouse.column, mouse.row)
                        {
                            incoming = Event::Key(key.into());
                        }
                    } else if offline && scrubber.handle(&mouse, timeline_area, &trend).is_some() {
                        // Scrubber state changes once per event; derive target from its current x.
                        next = timeline::pick(
                            &trend,
                            mouse.column.saturating_sub(timeline_area.x + 1) as usize,
                            timeline_area.width.saturating_sub(2) as usize,
                        );
                        following = false;
                        playing = false;
                    } else if !allocation {
                        app.handle_mouse(mouse);
                    } else {
                        match mouse.kind {
                            MouseEventKind::Down(MouseButton::Right) => {
                                group = None;
                                selected = 0;
                            }
                            MouseEventKind::ScrollDown
                                if side_area.contains((mouse.column, mouse.row).into()) =>
                            {
                                side_scroll = side_scroll.saturating_add(3)
                            }
                            MouseEventKind::ScrollUp
                                if side_area.contains((mouse.column, mouse.row).into()) =>
                            {
                                side_scroll = side_scroll.saturating_sub(3)
                            }
                            MouseEventKind::ScrollDown => {
                                selected =
                                    (selected + 1).min(focused.children.len().saturating_sub(1))
                            }
                            MouseEventKind::ScrollUp => selected = selected.saturating_sub(1),
                            MouseEventKind::Up(MouseButton::Left) => selection_drag = false,
                            MouseEventKind::Down(MouseButton::Left)
                            | MouseEventKind::Drag(MouseButton::Left) => {
                                let down = mouse.kind == MouseEventKind::Down(MouseButton::Left);
                                if down || selection_drag {
                                    if let Some(hit) = block_hits
                                        .iter()
                                        .position(|r| r.contains((mouse.column, mouse.row).into()))
                                    {
                                        selected = hit;
                                        selection_drag = true;
                                        let now = Instant::now();
                                        let double = down
                                            && last_click.as_ref().is_some_and(
                                                |(time, index, path)| {
                                                    *index == hit
                                                        && *path == group
                                                        && now.duration_since(*time)
                                                            < Duration::from_millis(400)
                                                },
                                            );
                                        last_click = if down && !double {
                                            Some((now, hit, group.clone()))
                                        } else {
                                            None
                                        };
                                        if double && group.is_none() {
                                            group =
                                                focused.children.get(hit).map(|n| n.label.clone());
                                            selected = 0;
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                if let Event::Key(key) = incoming {
                    if key.kind != KeyEventKind::Press {
                        continue;
                    }
                    if let Some(input) = time_input.as_mut() {
                        match key.code {
                            KeyCode::Esc => {
                                time_input = None;
                                time_error.clear();
                            }
                            KeyCode::Backspace => {
                                input.pop();
                            }
                            KeyCode::Char(c) if c.is_ascii_digit() || c == '.' => input.push(c),
                            KeyCode::Enter => {
                                match input
                                    .parse::<f64>()
                                    .ok()
                                    .filter(|v| {
                                        v.is_finite()
                                            && *v >= 0.
                                            && *v
                                                <= trend.last().map_or(0., |p| p.ms as f64 / 1000.)
                                    })
                                    .map(|v| session::at(path, &manifest, (v * 1000.) as u64))
                                {
                                    Some(Ok(target)) => {
                                        index = target;
                                        sample =
                                            tmv_symbols::replay::frame(path, index, manifest.pid)?;
                                        if let Some(snapshot) = &sample.snapshot {
                                            app.set_session_snapshot(snapshot.clone());
                                        }
                                        time_input = None;
                                        time_error.clear();
                                    }
                                    _ => time_error = "请输入显示范围内的秒数".into(),
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }
                    if stack_view.is_some() {
                        match key.code {
                            KeyCode::Esc | KeyCode::Char('t') | KeyCode::Char('q') => {
                                stack_view = None
                            }
                            KeyCode::Up => stack_scroll = stack_scroll.saturating_sub(1),
                            KeyCode::Down => {
                                stack_scroll = (stack_scroll + 1)
                                    .min(stack_view.as_ref().unwrap().len().saturating_sub(1))
                            }
                            KeyCode::PageDown => {
                                stack_scroll = (stack_scroll + 10)
                                    .min(stack_view.as_ref().unwrap().len().saturating_sub(1))
                            }
                            KeyCode::PageUp => stack_scroll = stack_scroll.saturating_sub(10),
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                break
                            }
                            _ => {}
                        }
                        continue;
                    }
                    if !allocation
                        && (app.memory.is_some()
                            || app.input.is_some()
                            || app.show_help
                            || app.show_settings
                            || app.show_accounting)
                    {
                        app.handle_key(key);
                        continue;
                    }
                    match key.code {
                        KeyCode::Char('q') => break,
                        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            break
                        }
                        KeyCode::Char('?') => {
                            let mut lines = vec![
                                "Tab 布局/分配；v 列表；b 地址图；列表内 U 单位".to_owned(),
                                "[ / ] 上一/下一采样；End 最新；q 退出".to_owned(),
                            ];
                            lines.push(if allocation { "方向键选择，Enter 下钻，Backspace 返回，t 展开分配栈，PgUp/PgDn 滚动" } else { "1 方块 / 2 地址图 / 3 映射列表；m 度量，g 分组，o 排序" }.into());
                            lines.push(
                                if offline {
                                    "Space 播放/暂停；T 输入经过秒数，M 切换曲线，P 跳到峰值"
                                } else {
                                    "Space 暂停/恢复跟随"
                                }
                                .into(),
                            );
                            session_help = true;
                            stack_view = Some(lines);
                            stack_scroll = 0;
                        }
                        KeyCode::Tab => allocation = !allocation,
                        KeyCode::Char('M') if offline => trend_rss = !trend_rss,
                        KeyCode::Char('T') if offline => {
                            time_input = Some(String::new());
                            following = false;
                            playing = false;
                        }
                        KeyCode::Char('P') if offline => {
                            next = timeline::peak(&trend, trend_rss).unwrap_or(index);
                            following = false;
                            playing = false;
                        }
                        KeyCode::PageDown if allocation => {
                            side_scroll = side_scroll.saturating_add(8)
                        }
                        KeyCode::PageUp if allocation => {
                            side_scroll = side_scroll.saturating_sub(8)
                        }
                        KeyCode::Char('t') if allocation => {
                            if focused.children.get(selected).is_some() {
                                session_help = false;
                                stack_view = Some(side_lines.clone());
                                stack_scroll = 0;
                                following = false;
                                playing = false;
                            }
                        }
                        KeyCode::Char('[') => {
                            next = index.saturating_sub(1);
                            following = false;
                            playing = false;
                        }
                        KeyCode::Char(']') => {
                            next = (index + 1).min(manifest.frames - 1);
                            following = false;
                            playing = false;
                        }
                        KeyCode::End => {
                            next = manifest.frames - 1;
                            following = manifest.state == "recording";
                        }
                        KeyCode::Char(' ') => {
                            if manifest.state == "recording" {
                                following = !following;
                            } else {
                                playing = !playing;
                                last_step = Instant::now();
                            }
                        }
                        KeyCode::Char('b') if allocation => blockmap = !blockmap,
                        KeyCode::Char('U') if allocation && list && !blockmap => {
                            app.list_unit.next()
                        }
                        KeyCode::Char('v') if allocation => {
                            list = !list || blockmap;
                            blockmap = false;
                        }
                        KeyCode::Down | KeyCode::Right if allocation => {
                            selected = (selected + 1).min(focused.children.len().saturating_sub(1))
                        }
                        KeyCode::Up | KeyCode::Left if allocation => {
                            selected = selected.saturating_sub(1)
                        }
                        KeyCode::Enter if allocation => {
                            if group.is_none() {
                                group = focused.children.get(selected).map(|n| n.label.clone());
                                selected = 0;
                            }
                        }
                        KeyCode::Backspace | KeyCode::Esc if allocation => {
                            group = None;
                            selected = 0;
                        }
                        _ if !allocation => app.handle_key(key),
                        _ => {}
                    }
                }
            }
            if app.should_quit {
                break;
            }
            manifest = session::manifest(path)?;
            if offline {
                for i in trend.len()..manifest.frames {
                    trend.push(timeline::Point::from_sample(&session::frame(path, i)?));
                }
            }
            if following {
                next = manifest.frames - 1;
            }
            if playing && last_step.elapsed() >= Duration::from_millis(manifest.interval_ms) {
                next = (index + 1).min(manifest.frames - 1);
                last_step = Instant::now();
                if next == index {
                    playing = false;
                }
            }
            if next != index {
                index = next;
                sample = tmv_symbols::replay::frame(path, index, manifest.pid)?;
                if let Some(snapshot) = &sample.snapshot {
                    app.set_session_snapshot(snapshot.clone());
                }
            }
        }
        Ok(())
    })();
    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}
fn selected_stack(
    sample: &session::Frame,
    group: Option<&str>,
    focused: &MemNode,
    selected: usize,
) -> Vec<String> {
    if let Some(node) = focused.children.get(selected) {
        session::stack_lines(
            sample,
            group.unwrap_or(&node.label),
            group.map(|_| node.start),
        )
    } else {
        vec!["请选择一个有存活分配的方块".into()]
    }
}
fn allocation_columns(area: Rect) -> (Rect, Rect) {
    let areas = if area.width >= 96 {
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).split(area)
    } else {
        Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).split(area)
    };
    (areas[0], areas[1])
}
fn draw_stack_side(
    frame: &mut ratatui::Frame,
    area: Rect,
    lines: &[String],
    scroll: usize,
) -> usize {
    use unicode_width::UnicodeWidthStr;
    let block = Block::default()
        .borders(Borders::ALL)
        .title("所选方块 · 分配调用栈")
        .title_bottom("PgUp/PgDn 滚动 · t 展开");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let rows: usize = lines
        .iter()
        .map(|line| {
            line.width()
                .div_ceil(usize::from(inner.width).max(1))
                .max(1)
        })
        .sum();
    let scroll = scroll
        .min(rows.saturating_sub(inner.height as usize))
        .min(u16::MAX as usize);
    frame.render_widget(
        Paragraph::new(lines.join("\n"))
            .wrap(Wrap { trim: false })
            .scroll((scroll as u16, 0)),
        inner,
    );
    scroll
}

fn draw_stack_popup(frame: &mut ratatui::Frame, lines: &[String], scroll: usize, help: bool) {
    let area = frame.area();
    let rect = Rect::new(
        area.x + 1,
        area.y + 1,
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    frame.render_widget(Clear, rect);
    let block = Block::default().borders(Borders::ALL).title(if help {
        "会话帮助 · ↑↓ 滚动，Esc 关闭"
    } else {
        "分配时调用栈（非线程当前栈） ↑↓ PgUp/PgDn 滚动，t/Esc 关闭"
    });
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(
        Paragraph::new(
            lines
                .iter()
                .skip(scroll)
                .take(inner.height as usize)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        inner,
    );
}

fn draw_allocations(
    frame: &mut ratatui::Frame,
    node: &MemNode,
    selected: usize,
    list: Option<ui::ListUnit>,
    blockmap: bool,
    records: u64,
    area: Rect,
) -> Vec<Rect> {
    let mut hits = vec![Rect::default(); node.children.len()];
    let chunks = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(area);
    frame.render_widget(Paragraph::new(format!("分配归因 · {} · {} bytes\n已追踪存活申请字节（不是 RSS），{} 条事件；Enter 下钻，t 分配栈，Backspace 返回",node.label,node.bytes,records)),chunks[0]);
    let block = Block::default().borders(Borders::ALL).title(if blockmap {
        "地址顺序 · 每区间独立缩放 · 空洞折叠"
    } else {
        "Treemap / 占用排行"
    });
    let inner = block.inner(chunks[1]);
    frame.render_widget(block, chunks[1]);
    if records == 0 {
        frame.render_widget(
            Paragraph::new("没有分配采集数据；不能据此判断进程没有堆占用"),
            inner,
        );
        return hits;
    }
    if blockmap && node.children.iter().any(|n| !n.children.is_empty()) {
        frame.render_widget(
            Paragraph::new(
                "先 Enter 下钻到具体调用位置，再按 b 查看分配地址；业务分类不是连续地址区间。",
            ),
            inner,
        );
    } else if blockmap {
        hits = ui::draw_address_blocks(frame, &node.children, selected, inner);
    } else if let Some(unit) = list {
        let offset = selected.saturating_sub(inner.height as usize / 2);
        let lines = node
            .children
            .iter()
            .enumerate()
            .skip(offset)
            .take(inner.height as usize)
            .map(|(i, n)| {
                format!(
                    "{} {:>12} {:>6.2}% {}",
                    if i == selected { ">" } else { " " },
                    unit.format(n.bytes),
                    if node.bytes == 0 {
                        0.
                    } else {
                        100. * n.bytes as f64 / node.bytes as f64
                    },
                    if n.children.is_empty() {
                        format!("0x{:x}", n.start)
                    } else {
                        n.label.clone()
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        for (row, i) in (offset..node.children.len())
            .take(inner.height as usize)
            .enumerate()
        {
            hits[i] = Rect::new(inner.x, inner.y + row as u16, inner.width, 1);
        }
        frame.render_widget(Paragraph::new(lines), inner);
    } else {
        let weights = node
            .children
            .iter()
            .map(|n| n.bytes as f64)
            .collect::<Vec<_>>();
        for (i, (n, rect)) in node
            .children
            .iter()
            .zip(treemap::squarify(&weights, inner))
            .enumerate()
        {
            hits[i] = rect;
            ui::draw_block(frame, n, rect, i == selected, None);
        }
    }
    if let Some(n) = node.children.get(selected) {
        frame.render_widget(
            Paragraph::new(format!(
                "{}\n{} bytes · {} 个子项{}",
                n.label,
                n.bytes,
                n.children.len(),
                if n.start != 0 {
                    format!(" · 0x{:x}..0x{:x}", n.start, n.end)
                } else {
                    String::new()
                }
            )),
            chunks[2],
        );
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allocation_treemap_and_missing_data_are_distinct() {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        let root = MemNode {
            label: "Observed".into(),
            bytes: 1024,
            children: vec![MemNode {
                label: "Cache".into(),
                bytes: 1024,
                ..MemNode::empty()
            }],
            ..MemNode::empty()
        };
        terminal
            .draw(|f| {
                draw_allocations(f, &root, 0, None, false, 2, f.area());
            })
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(text.contains("Cache"));
        assert!(text.replace(' ', "").contains("不是RSS"));
        terminal.clear().unwrap();
        terminal
            .draw(|f| {
                draw_allocations(f, &root, 0, None, false, 0, f.area());
            })
            .unwrap();
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(
            text.replace(' ', "").contains("没有分配采集数据"),
            "{text:?}"
        );
    }
}

#[cfg(test)]
mod sidebar_tests {
    use super::*;
    #[test]
    fn selection_updates_right_hand_stack_without_popup() {
        let sample = session::Frame {
            elapsed_ms: 1000,
            snapshot: None,
            layout_error: None,
            heap_records: 10,
            unknown_frees: 0,
            heap_ended: false,
            allocations: vec![
                session::Allocation {
                    address: 4096,
                    size: 64,
                    caller: 1,
                    label: "Cache".into(),
                    stack: vec![session::StackFrame {
                        address: 1,
                        symbol: "cache_create".into(),
                        location: None,
                    }],
                    resize_stack: vec![],
                },
                session::Allocation {
                    address: 8192,
                    size: 32,
                    caller: 2,
                    label: "Session".into(),
                    stack: vec![session::StackFrame {
                        address: 2,
                        symbol: "session_create".into(),
                        location: None,
                    }],
                    resize_stack: vec![],
                },
            ],
        };
        let root = session::allocation_tree(&sample);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 35)).unwrap();
        for (selected, expected, absent) in [
            (0, "cache_create", "session_create"),
            (1, "session_create", "cache_create"),
        ] {
            terminal.clear().unwrap();
            let lines = selected_stack(&sample, None, &root, selected);
            terminal
                .draw(|f| {
                    let (_, right) = allocation_columns(f.area());
                    draw_stack_side(f, right, &lines, 0);
                })
                .unwrap();
            let buf = terminal.backend().buffer();
            let text = (0..35)
                .flat_map(|y| (77..140).map(move |x| (x, y)))
                .map(|(x, y)| buf[(x, y)].symbol())
                .collect::<String>();
            assert!(text.contains(expected));
            assert!(!text.contains(absent));
        }
    }
}
