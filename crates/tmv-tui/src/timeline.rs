//! Timeline uses elapsed time, not sample index; missing RSS is never plotted as zero.
use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};
use tmv_core::{session, Metric};
#[derive(Clone, Debug)]
pub struct Point {
    pub ms: u64,
    pub heap: Option<u64>,
    pub rss: Option<u64>,
}
impl Point {
    pub fn from_sample(f: &session::Frame) -> Self {
        Self {
            ms: f.elapsed_ms,
            heap: (f.heap_records > 0).then(|| f.allocations.iter().map(|a| a.size).sum()),
            rss: f
                .snapshot
                .as_ref()
                .filter(|s| s.has_smaps)
                .map(|s| s.total(Metric::Rss)),
        }
    }
    pub fn value(&self, rss: bool) -> Option<u64> {
        if rss {
            self.rss
        } else {
            self.heap
        }
    }
}
pub fn column(points: &[Point], index: usize, width: usize) -> usize {
    if points.len() < 2 || width < 2 {
        return 0;
    }
    let span = points
        .last()
        .unwrap()
        .ms
        .saturating_sub(points[0].ms)
        .max(1);
    ((u128::from(points[index].ms.saturating_sub(points[0].ms)) * (width - 1) as u128)
        / u128::from(span)) as usize
}
pub fn pick(points: &[Point], col: usize, width: usize) -> usize {
    if points.len() < 2 || width < 2 {
        return 0;
    }
    let first = points[0].ms;
    let last = points.last().unwrap().ms;
    let ms = first
        + ((u128::from(last.saturating_sub(first)) * col.min(width - 1) as u128)
            / (width - 1) as u128) as u64;
    points.partition_point(|p| p.ms <= ms).saturating_sub(1)
}
pub fn peak(points: &[Point], rss: bool) -> Option<usize> {
    points
        .iter()
        .enumerate()
        .filter_map(|(i, p)| p.value(rss).map(|v| (i, v)))
        .max_by_key(|(_, v)| *v)
        .map(|(i, _)| i)
}
pub fn draw(frame: &mut Frame, area: Rect, points: &[Point], selected: usize, rss: bool) {
    if area.width < 3 || area.height < 3 || points.is_empty() {
        return;
    }
    let value = points[selected]
        .value(rss)
        .map(crate::ui::human_bytes)
        .unwrap_or_else(|| "未采集".into());
    let title = format!(
        "{}：{} @ {:.3}s | M 切换 T 时间 P 峰值 · 按住左右拖动 · 列内峰值/·未采集",
        if rss {
            "RSS"
        } else {
            "存活申请（非RSS）"
        },
        value,
        points[selected].ms as f64 / 1000.
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .title_bottom(format!(
            "{:.3}s → {:.3}s · 阶梯保持采样值，列内取峰值，黄色为选中采样；时间从记录开始计",
            points[0].ms as f64 / 1000.,
            points.last().unwrap().ms as f64 / 1000.
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let width = inner.width as usize;
    let mut bins: Vec<Option<u64>> = (0..width)
        .map(|col| points[pick(points, col, width)].value(rss))
        .collect();
    for (i, p) in points.iter().enumerate() {
        let col = column(points, i, width);
        if let Some(v) = p.value(rss) {
            bins[col] = Some(bins[col].unwrap_or(0).max(v));
        }
    }
    let max = bins.iter().flatten().copied().max().unwrap_or(0).max(1);
    let selected_column = column(points, selected, width);
    let glyphs = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let lines = (0..inner.height)
        .map(|row| {
            Line::from(
                bins.iter()
                    .enumerate()
                    .map(|(col, value)| {
                        let glyph = if let Some(v) = value {
                            let level = (u128::from(*v) * u128::from(inner.height) * 8
                                / u128::from(max)) as u64;
                            let base = u64::from(inner.height - 1 - row) * 8;
                            glyphs[level.saturating_sub(base).min(8) as usize]
                        } else if row == inner.height - 1 {
                            "·"
                        } else {
                            " "
                        };
                        Span::styled(
                            glyph,
                            Style::default()
                                .fg(if col == selected_column {
                                    Color::Yellow
                                } else {
                                    Color::Cyan
                                })
                                .bg(if col == selected_column {
                                    Color::DarkGray
                                } else {
                                    Color::Reset
                                }),
                        )
                    })
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), inner);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn irregular_time_and_missing_samples() {
        let p = vec![
            Point {
                ms: 0,
                heap: Some(10),
                rss: None,
            },
            Point {
                ms: 1000,
                heap: Some(40),
                rss: Some(9),
            },
            Point {
                ms: 10000,
                heap: Some(0),
                rss: None,
            },
        ];
        assert_eq!(column(&p, 1, 11), 1);
        assert_eq!(pick(&p, 5, 11), 1);
        assert_eq!(pick(&p, 10, 11), 2);
        assert_eq!(peak(&p, false), Some(1));
        assert_eq!(peak(&p, true), Some(1));
    }
}

/// Dragging is captured only if it starts inside the offline timeline.
#[derive(Default)]
pub struct Scrubber {
    pub active: bool,
}
impl Scrubber {
    pub fn handle(
        &mut self,
        event: &crossterm::event::MouseEvent,
        area: Rect,
        points: &[Point],
    ) -> Option<usize> {
        use crossterm::event::{MouseButton, MouseEventKind};
        if points.is_empty() || area.width < 3 {
            self.active = false;
            return None;
        }
        match event.kind {
            MouseEventKind::Down(MouseButton::Left)
                if area.contains((event.column, event.row).into()) =>
            {
                self.active = true
            }
            MouseEventKind::Drag(MouseButton::Left) if self.active => {}
            MouseEventKind::Up(MouseButton::Left) if self.active => self.active = false,
            _ => return None,
        }
        Some(pick(
            points,
            event.column.saturating_sub(area.x + 1) as usize,
            area.width.saturating_sub(2) as usize,
        ))
    }
}
#[cfg(test)]
mod drag_tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    #[test]
    fn drag_selects_samples_and_clamps_outside_plot() {
        let points = vec![
            Point {
                ms: 0,
                heap: Some(1),
                rss: None,
            },
            Point {
                ms: 1000,
                heap: Some(2),
                rss: None,
            },
            Point {
                ms: 2000,
                heap: Some(3),
                rss: None,
            },
        ];
        let area = Rect::new(10, 20, 13, 5);
        let mut s = Scrubber::default();
        let e = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            s.handle(
                &e(MouseEventKind::Drag(MouseButton::Left), 16, 22),
                area,
                &points
            ),
            None
        );
        assert_eq!(
            s.handle(
                &e(MouseEventKind::Down(MouseButton::Left), 16, 22),
                area,
                &points
            ),
            Some(1)
        );
        assert_eq!(
            s.handle(
                &e(MouseEventKind::Drag(MouseButton::Left), 100, 10),
                area,
                &points
            ),
            Some(2)
        );
        assert_eq!(
            s.handle(
                &e(MouseEventKind::Drag(MouseButton::Left), 0, 10),
                area,
                &points
            ),
            Some(0)
        );
        assert_eq!(
            s.handle(
                &e(MouseEventKind::Up(MouseButton::Left), 16, 22),
                area,
                &points
            ),
            Some(1)
        );
        assert!(!s.active);
    }
}
