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
        let remaining: f64 = items[start..].iter().map(|(_, area)| *area).sum();
        let remaining_thickness = if horizontal {
            remaining / free.w
        } else {
            remaining / free.h
        };
        if remaining_thickness < 1.0 && items.len() - start > 1 {
            layout_row(&mut free, &items[start..], area, &mut out);
            break;
        }

        let mut end = start + 1;
        let mut best = worst_ratio(&items[start..end], side);
        while end < items.len() {
            let candidate = worst_ratio(&items[start..end + 1], side);
            if candidate > best {
                break;
            }
            // 新行必须装得进剩余空间，否则先铺的行会把后面的行挤掉。
            let sum: f64 = items[start..end + 1].iter().map(|(_, area)| *area).sum();
            let thickness = if horizontal {
                sum / free.w
            } else {
                sum / free.h
            };
            let limit = if horizontal { free.h } else { free.w };
            if thickness > limit {
                break;
            }
            best = candidate;
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

/// 当前行/列的最差长宽比，值越小越接近正方形。
fn worst_ratio(row: &[(usize, f64)], side: f64) -> f64 {
    if row.is_empty() || side <= 0.0 {
        return f64::MAX;
    }
    let sum: f64 = row.iter().map(|(_, area)| *area).sum();
    if sum <= 0.0 {
        return f64::MAX;
    }
    let max = row.iter().map(|(_, area)| *area).fold(0.0_f64, f64::max);
    let min = row.iter().map(|(_, area)| *area).fold(f64::MAX, f64::min);
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
}
