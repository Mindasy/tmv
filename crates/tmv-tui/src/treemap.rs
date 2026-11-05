//! Squarified treemap 布局：把权重列表铺满矩形区域，并尽量让每块接近正方形。
//!
//! 纯函数，不依赖终端状态，便于单测。

use ratatui::layout::Rect;

/// 输入权重（正数；0/NaN 会被忽略）与可用区域，返回与 `weights` 一一对应的矩形。
pub fn squarify(weights: &[f64], area: Rect) -> Vec<Rect> {
    let mut out = vec![Rect::new(area.x, area.y, 0, 0); weights.len()];
    if weights.is_empty() || area.width == 0 || area.height == 0 {
        return out;
    }

    let total: f64 = weights.iter().filter(|w| w.is_finite() && **w > 0.0).sum();
    if total <= 0.0 {
        return out;
    }

    let cells = area.width as f64 * area.height as f64;
    // 极小权重会被四舍五入成 0 格而彻底消失，这里抬到「约 2 格」的可见下限。
    let floor = total * 2.0 / cells;
    let scaled_total: f64 = weights
        .iter()
        .filter(|w| w.is_finite() && **w > 0.0)
        .map(|w| w.max(floor))
        .sum();
    let scale = cells / scaled_total;

    let items: Vec<(usize, f64)> = weights
        .iter()
        .enumerate()
        .filter_map(|(index, weight)| {
            if weight.is_finite() && *weight > 0.0 {
                Some((index, weight.max(floor) * scale))
            } else {
                None
            }
        })
        .collect();

    let mut free = F64Rect {
        x: area.x as f64,
        y: area.y as f64,
        w: area.width as f64,
        h: area.height as f64,
    };

    // 后缀和：外层循环每轮都要判断「剩下的项还放得下吗」，逐次求和会退化成 O(n²)。
    let mut suffix = vec![0.0; items.len() + 1];
    for (index, (_, item_area)) in items.iter().enumerate().rev() {
        suffix[index] = suffix[index + 1] + item_area;
    }

    let mut start = 0;
    while start < items.len() {
        // 行是沿较长边铺开的，长宽比也必须用这条边的长度来计算。
        let horizontal = free.w >= free.h;
        let side = free.w.max(free.h);
        if side <= 0.0 {
            break;
        }

        // 剩余项总高度不足 1 格时，说明它们只能共用最后一条带，整体打包，
        // 否则会被逐个取整成 1 行而把更小的项挤没。
        let remaining = suffix[start];
        let remaining_thickness = if horizontal {
            remaining / free.w
        } else {
            remaining / free.h
        };
        if remaining_thickness < 1.0 && items.len() - start > 1 {
            layout_row(&mut free, &items[start..], area, &mut out);
            break;
        }

        // 行内累加和与小/大值随候选一起更新，避免每加一块就重扫整行。
        let first_area = items[start].1;
        let mut row_sum = first_area;
        let mut row_min = first_area;
        let mut row_max = first_area;
        let mut best = ratio(row_sum, row_min, row_max, side);
        let mut end = start + 1;
        let limit = if horizontal { free.h } else { free.w };
        while end < items.len() {
            let item_area = items[end].1;
            let next_sum = row_sum + item_area;
            let next_min = row_min.min(item_area);
            let next_max = row_max.max(item_area);
            let candidate = ratio(next_sum, next_min, next_max, side);
            if candidate > best {
                break;
            }
            // 新行必须装得进剩余空间，否则先铺的行会把后面的行挤掉。
            let thickness = if horizontal {
                next_sum / free.w
            } else {
                next_sum / free.h
            };
            if thickness > limit {
                break;
            }
            best = candidate;
            row_sum = next_sum;
            row_min = next_min;
            row_max = next_max;
            end += 1;
        }

        layout_row(&mut free, &items[start..end], area, &mut out);
        start = end;
    }

    out
}

#[derive(Clone, Copy, Debug)]
struct F64Rect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

/// 给定一行的面积和与极值，计算该行最差长宽比，值越小越接近正方形。
fn ratio(sum: f64, min: f64, max: f64, side: f64) -> f64 {
    if side <= 0.0 || sum <= 0.0 || min <= 0.0 {
        return f64::MAX;
    }
    let sum_sq = sum * sum;
    let side_sq = side * side;
    (side_sq * max / sum_sq).max(sum_sq / (side_sq * min))
}

fn layout_row(free: &mut F64Rect, row: &[(usize, f64)], bounds: Rect, out: &mut [Rect]) {
    let sum: f64 = row.iter().map(|(_, area)| *area).sum();
    if sum <= 0.0 {
        return;
    }

    if free.w >= free.h {
        // 沿较长的宽边铺一行，厚度在 y 方向。
        let thickness = (sum / free.w).min(free.h);
        if thickness <= 0.0 {
            return;
        }
        let y0 = free.y.round().max(bounds.y as f64) as u16;
        let limit = bounds.y + bounds.height;
        // 向下取整：避免先铺的块把小数行「吃掉」，让后面的小块也能分到可见空间。
        let mut y1 = ((free.y + thickness).floor() as u16).min(limit);
        if y1 <= y0 {
            y1 = (y0 + 1).min(limit);
        }
        let height = y1.saturating_sub(y0);
        let x_limit = bounds.x + bounds.width;

        let mut accumulated = 0.0;
        let mut prev_x = (free.x.round() as u16).min(x_limit);
        for (index, item_area) in row {
            accumulated += item_area;
            let x1 = ((free.x + accumulated / thickness).round() as u16)
                .clamp(prev_x, x_limit.max(prev_x));
            if x1 > prev_x && height > 0 {
                out[*index] = Rect::new(prev_x, y0, x1 - prev_x, height);
            }
            prev_x = x1;
        }

        // 对齐到整数行，避免小数误差累积后把后面的块挤出布局。
        free.y = y1 as f64;
        free.h = (limit - y1) as f64;
    } else {
        // 沿较长的高边铺一列，厚度在 x 方向。
        let thickness = (sum / free.h).min(free.w);
        if thickness <= 0.0 {
            return;
        }
        let x0 = free.x.round().max(bounds.x as f64) as u16;
        let limit = bounds.x + bounds.width;
        let mut x1 = ((free.x + thickness).floor() as u16).min(limit);
        if x1 <= x0 {
            x1 = (x0 + 1).min(limit);
        }
        let width = x1.saturating_sub(x0);
        let y_limit = bounds.y + bounds.height;

        let mut accumulated = 0.0;
        let mut prev_y = (free.y.round() as u16).min(y_limit);
        for (index, item_area) in row {
            accumulated += item_area;
            let y1 = ((free.y + accumulated / thickness).round() as u16)
                .clamp(prev_y, y_limit.max(prev_y));
            if y1 > prev_y && width > 0 {
                out[*index] = Rect::new(x0, prev_y, width, y1 - prev_y);
            }
            prev_y = y1;
        }

        free.x = x1 as f64;
        free.w = (limit - x1) as f64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(width: u16, height: u16) -> Rect {
        Rect::new(0, 0, width, height)
    }

    fn cells(rect: Rect) -> u64 {
        rect.width as u64 * rect.height as u64
    }

    #[test]
    fn single_item_fills_area() {
        let out = squarify(&[1.0], area(80, 20));
        assert_eq!(out, vec![area(80, 20)]);
    }

    #[test]
    fn zero_weights_are_ignored() {
        let out = squarify(&[0.0, 0.0], area(10, 10));
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|rect| cells(*rect) == 0));
    }

    #[test]
    fn areas_are_proportional_to_weights() {
        let weights = [1.0, 2.0, 3.0, 4.0];
        let out = squarify(&weights, area(100, 60));
        let total = 100.0 * 60.0;
        for (weight, rect) in weights.iter().zip(out.iter()) {
            let expected = total * weight / 10.0;
            let actual = cells(*rect) as f64;
            assert!(
                (actual - expected).abs() / expected < 0.2,
                "权重 {weight} 的面积偏差过大: {actual} vs {expected}"
            );
        }
    }

    #[test]
    fn blocks_do_not_overlap() {
        let weights: Vec<f64> = (1..=40).map(|i| i as f64).collect();
        let rects = squarify(&weights, area(97, 41));
        for i in 0..rects.len() {
            for j in (i + 1)..rects.len() {
                let overlap = rects[i].intersection(rects[j]);
                assert_eq!(cells(overlap), 0, "{:?} 与 {:?} 重叠", rects[i], rects[j]);
            }
        }
    }

    #[test]
    fn covers_most_of_the_area() {
        let weights = [5.0, 3.0, 2.0, 2.0, 1.0, 1.0];
        let rect = area(60, 30);
        let covered: u64 = squarify(&weights, rect).iter().map(|r| cells(*r)).sum();
        assert!(covered as f64 >= 0.85 * cells(rect) as f64);
    }

    #[test]
    fn degenerate_areas_are_safe() {
        assert!(squarify(&[1.0, 2.0], area(0, 10))
            .iter()
            .all(|r| cells(*r) == 0));
        assert!(squarify(&[f64::NAN, -1.0], area(10, 10))
            .iter()
            .all(|r| cells(*r) == 0));
    }

    #[test]
    fn tiny_items_stay_visible() {
        let rects = squarify(&[1000.0, 0.001, 0.001], area(80, 20));
        assert!(rects[1..].iter().all(|rect| cells(*rect) >= 1));
    }

    #[test]
    fn every_item_gets_a_cell_under_skewed_weights() {
        let mut weights = vec![1000.0];
        weights.extend((1..=30).map(|i| (31 - i) as f64));
        let rects = squarify(&weights, area(60, 20));
        assert!(
            rects.iter().all(|rect| cells(*rect) >= 1),
            "存在不可见块: {:?}",
            rects
                .iter()
                .filter(|rect| cells(**rect) == 0)
                .collect::<Vec<_>>()
        );
    }

    /// 大进程基准：10 万 VMA 的建树与布局耗时。
    ///
    /// 手动运行以对比优化前后的量级：
    /// `cargo test --release -p tmv-tui -- --ignored --nocapture large_snapshot`
    #[test]
    #[ignore = "性能基准，需手动在 release 下运行"]
    fn large_snapshot_build_and_layout() {
        use std::time::Instant;

        use tmv_core::{
            build_tree, demo, GroupBy, Metric, Perms, SmapsStats, SortOrder, TreeOptions, Vma,
        };

        const COUNT: usize = 100_000;
        const PAGE: u64 = 4096;
        let mut vmas = Vec::with_capacity(COUNT);
        for index in 0..COUNT {
            let start = 0x1000 + index as u64 * PAGE * 4;
            let size = PAGE * (1 + (index % 16) as u64);
            let pathname = match index % 3 {
                0 => format!("/usr/lib/aarch64-linux-gnu/lib{}.so", index % 512),
                1 => String::new(),
                _ => format!("[anon:{index}]"),
            };
            vmas.push(Vma {
                start,
                end: start + size,
                perms: Perms {
                    read: true,
                    write: index % 2 == 0,
                    exec: index % 3 == 0,
                    shared: index % 2 == 1,
                },
                offset: 0,
                dev: "08:01".to_string(),
                inode: index as u64,
                pathname,
                stats: Some(SmapsStats {
                    size,
                    rss: size / 2,
                    pss: size / 2,
                    ..SmapsStats::default()
                }),
            });
        }
        let mut snapshot = demo::snapshot();
        snapshot.vmas = vmas;
        snapshot.has_smaps = true;

        let start = Instant::now();
        let grouped = build_tree(
            &snapshot,
            TreeOptions {
                metric: Metric::Pss,
                group_by: GroupBy::Category,
                sort: SortOrder::SizeDesc,
            },
        );
        let build = start.elapsed();

        let start = Instant::now();
        let flat = build_tree(
            &snapshot,
            TreeOptions {
                metric: Metric::Pss,
                group_by: GroupBy::Map,
                sort: SortOrder::SizeDesc,
            },
        );
        let build_flat = start.elapsed();

        // 典型 200x60 终端区域下铺 10 万块。
        let weights: Vec<f64> = flat
            .children
            .iter()
            .map(|child| child.bytes as f64)
            .collect();
        let start = Instant::now();
        let rects = squarify(&weights, area(200, 60));
        let layout = start.elapsed();

        println!(
            "10 万 VMA：类别建树 {build:?}，单层建树 {build_flat:?}，布局 {layout:?}，分组 {}",
            grouped.children.len()
        );

        assert_eq!(rects.len(), COUNT);
        assert!(
            build < std::time::Duration::from_secs(10),
            "类别建树耗时异常: {build:?}"
        );
        assert!(
            build_flat < std::time::Duration::from_secs(10),
            "单层建树耗时异常: {build_flat:?}"
        );
        assert!(
            layout < std::time::Duration::from_secs(2),
            "布局耗时异常: {layout:?}"
        );
    }
}
