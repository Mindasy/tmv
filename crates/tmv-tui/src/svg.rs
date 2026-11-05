//! 把 treemap 导出为 SVG。
//!
//! 支持两种布局：
//! - [`SvgFormat::Treemap`]：与 TUI 一致的 squarified 两层布局，点击分组可下钻
//! - [`SvgFormat::Flame`]：横向 icicle 火焰图，按层级堆叠，适合看「谁一路撑大」
//!
//! 输出是单文件、无外部依赖：悬停详情用 `<title>`（原生 tooltip），
//! 高亮用内联 CSS，下钻用几十行内联 JS 切换预计算好的层级。

use std::fmt::Write;

use ratatui::layout::Rect;
use tmv_core::{Category, MemNode};

use crate::treemap::squarify;
use crate::ui::human_bytes;

/// 导出图形尺寸与标题。
#[derive(Debug, Clone)]
pub struct SvgOptions {
    pub width: u32,
    pub height: u32,
    /// 顶部主标题，通常包含 pid。
    pub title: String,
    /// 标题下的第二行，放度量、合计与 VMA 数量。
    pub subtitle: String,
    pub format: SvgFormat,
}

impl Default for SvgOptions {
    fn default() -> Self {
        Self {
            width: 1600,
            height: 1000,
            title: String::new(),
            subtitle: String::new(),
            format: SvgFormat::Treemap,
        }
    }
}

/// 导出布局。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SvgFormat {
    #[default]
    Treemap,
    Flame,
}

impl SvgFormat {
    pub fn label(self) -> &'static str {
        match self {
            SvgFormat::Treemap => "treemap",
            SvgFormat::Flame => "flame",
        }
    }
}

/// 渲染整棵树，返回独立的 SVG 文本。
pub fn render(root: &MemNode, options: &SvgOptions) -> String {
    let width = options.width.clamp(320, u16::MAX as u32);
    let height = options.height.clamp(200, u16::MAX as u32);
    let mut out = String::with_capacity(16384);
    let _ = writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(
        out,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" font-family="monospace">"##
    );
    let _ = writeln!(
        out,
        r##"<style>
  .blk {{ stroke: #0b0d10; stroke-width: 1; }}
  .blk:hover {{ stroke: #ffffff; stroke-width: 2; }}
  .grp {{ cursor: zoom-in; }}
  .back {{ cursor: zoom-out; }}
  text {{ pointer-events: none; }}
</style>"##
    );
    let _ = writeln!(
        out,
        r##"<rect width="100%" height="100%" fill="#111418"/>"##
    );
    let _ = writeln!(
        out,
        r##"<text x="12" y="22" fill="#e6e6e6" font-size="15">{}</text>"##,
        escape(&options.title)
    );
    if !options.subtitle.is_empty() {
        let _ = writeln!(
            out,
            r##"<text x="12" y="40" fill="#9aa4b2" font-size="12">{}</text>"##,
            escape(&options.subtitle)
        );
    }

    let header: u32 = if options.subtitle.is_empty() { 30 } else { 48 };
    let legend: u32 = 26;
    let content = Rect::new(
        12,
        header as u16,
        width.saturating_sub(24) as u16,
        height.saturating_sub(header + legend + 8) as u16,
    );
    if root.children.is_empty() || content.width < 6 || content.height < 6 {
        let _ = writeln!(
            out,
            r##"<text x="12" y="70" fill="#888888">没有可展示的数据</text>"##
        );
        let _ = writeln!(out, "</svg>");
        return out;
    }

    match options.format {
        SvgFormat::Treemap => draw_treemap(&mut out, root, content),
        SvgFormat::Flame => draw_flame(&mut out, root, content),
    }
    draw_legend(&mut out, root, width, height, options.format);
    if options.format == SvgFormat::Treemap {
        // 只有 treemap 有层级可下钻；火焰图本身已经展开所有层级。
        draw_back_button(&mut out, width, header);
        draw_script(&mut out);
    }
    let _ = writeln!(out, "</svg>");
    out
}

/// 两层 treemap：一级分组 + 组内叶子；分组可点击下钻。
fn draw_treemap(out: &mut String, root: &MemNode, area: Rect) {
    let weights: Vec<f64> = root
        .children
        .iter()
        .map(|child| child.bytes as f64)
        .collect();
    let rects = squarify(&weights, area);

    let _ = writeln!(out, r#"<g id="lv0">"#);
    for (index, (node, rect)) in root.children.iter().zip(rects.iter()).enumerate() {
        if rect.width == 0 || rect.height == 0 {
            continue;
        }
        let drillable = !node.children.is_empty();
        let class = if drillable { "blk grp" } else { "blk" };
        let onclick = if drillable {
            format!(r#" onclick="zoom({index})""#)
        } else {
            String::new()
        };
        let _ = writeln!(
            out,
            r##"<rect class="{class}" x="{}" y="{}" width="{}" height="{}" fill="{}" fill-opacity="0.42"{onclick}><title>{}</title></rect>"##,
            rect.x,
            rect.y,
            rect.width,
            rect.height,
            category_hex(node.category),
            escape(&tooltip(node))
        );
        if rect.width >= 40 {
            let label = format!("{}  {}", node.label, human_bytes(node.bytes));
            let _ = writeln!(
                out,
                r##"<text x="{}" y="{}" fill="#f2f2f2" font-size="13" font-weight="bold">{}</text>"##,
                rect.x + 4,
                rect.y + 16,
                escape(&truncate(&label, rect.width as usize / 7))
            );
        }
        if drillable {
            let inner = Rect::new(
                rect.x + 2,
                rect.y + 20,
                rect.width.saturating_sub(4),
                rect.height.saturating_sub(24),
            );
            if inner.width > 2 && inner.height > 2 {
                let _ = writeln!(
                    out,
                    r#"<g id="z{index}" style="display:none" transform="{}">"#,
                    zoom_transform(inner, area)
                );
                let weights: Vec<f64> = node
                    .children
                    .iter()
                    .map(|child| child.bytes as f64)
                    .collect();
                for (child, child_rect) in node.children.iter().zip(squarify(&weights, inner)) {
                    draw_leaf(out, child, child_rect);
                }
                let _ = writeln!(out, "</g>");
            }
        }
        draw_delta_strip(out, node.delta, rect.x, rect.y, rect.width, rect.height);
    }
    let _ = writeln!(out, "</g>");
}

fn draw_leaf(out: &mut String, node: &MemNode, rect: Rect) {
    if rect.width < 2 || rect.height < 2 {
        return;
    }
    let _ = writeln!(
        out,
        r##"<rect class="blk" x="{}" y="{}" width="{}" height="{}" fill="{}"><title>{}</title></rect>"##,
        rect.x,
        rect.y,
        rect.width,
        rect.height,
        category_hex(node.category),
        escape(&tooltip(node))
    );
    if rect.width >= 70 && rect.height >= 24 {
        let label = format!("{}  {}", node.label, human_bytes(node.bytes));
        let _ = writeln!(
            out,
            r##"<text x="{}" y="{}" fill="#eaeaea" font-size="11">{}</text>"##,
            rect.x + 3,
            rect.y + rect.height / 2 + 4,
            escape(&truncate(&label, rect.width as usize / 6))
        );
    }
    draw_delta_strip(out, node.delta, rect.x, rect.y, rect.width, rect.height);
}

/// 横向火焰图：第一行是总量，第二行分组，第三行叶子。
fn draw_flame(out: &mut String, root: &MemNode, area: Rect) {
    let total = root.bytes.max(1) as f64;
    let row_height = (area.height as f64 / 3.0).max(8.0);
    let x0 = area.x as f64;
    let x1 = (area.x + area.width) as f64;

    let _ = writeln!(out, r#"<g id="flame">"#);
    flame_bar(out, root, x0, x1, area.y as f64, row_height, true);

    let mut x = x0;
    for group in &root.children {
        let group_width = group.bytes as f64 / total * (x1 - x0);
        let (gx0, gx1) = (x, x + group_width);
        flame_bar(
            out,
            group,
            gx0,
            gx1,
            area.y as f64 + row_height,
            row_height,
            true,
        );

        if !group.children.is_empty() {
            let group_total = group.bytes.max(1) as f64;
            let mut child_x = gx0;
            for child in &group.children {
                let width = child.bytes as f64 / group_total * (gx1 - gx0);
                flame_bar(
                    out,
                    child,
                    child_x,
                    child_x + width,
                    area.y as f64 + row_height * 2.0,
                    row_height,
                    width > 60.0,
                );
                child_x += width;
            }
        }
        x += group_width;
    }
    let _ = writeln!(out, "</g>");
}

#[allow(clippy::too_many_arguments)]
fn flame_bar(
    out: &mut String,
    node: &MemNode,
    x0: f64,
    x1: f64,
    y: f64,
    height: f64,
    show_label: bool,
) {
    let width = x1 - x0;
    if width < 0.5 {
        return;
    }
    let _ = writeln!(
        out,
        r##"<rect class="blk" x="{x0:.2}" y="{y:.2}" width="{width:.2}" height="{height:.2}" fill="{}" fill-opacity="0.85"><title>{}</title></rect>"##,
        category_hex(node.category),
        escape(&tooltip(node))
    );
    if show_label && width >= 70.0 {
        let label = format!("{}  {}", node.label, human_bytes(node.bytes));
        let _ = writeln!(
            out,
            r##"<text x="{:.2}" y="{:.2}" fill="#0b0d10" font-size="11" font-weight="bold">{}</text>"##,
            x0 + 4.0,
            y + height / 2.0 + 4.0,
            escape(&truncate(&label, (width / 6.2) as usize))
        );
    }
    if node.delta != 0 {
        let color = if node.delta > 0 { "#3fb950" } else { "#f85149" };
        let _ = writeln!(
            out,
            r##"<rect x="{x0:.2}" y="{:.2}" width="{width:.2}" height="3" fill="{color}"/>"##,
            y + height - 3.0
        );
    }
}

/// 底部图例：列出实际出现的类别与色块。
fn draw_legend(out: &mut String, root: &MemNode, width: u32, height: u32, format: SvgFormat) {
    let mut categories: Vec<Category> = Vec::new();
    for group in &root.children {
        let leaves = if group.children.is_empty() {
            std::slice::from_ref(group)
        } else {
            group.children.as_slice()
        };
        for leaf in leaves {
            if !categories.contains(&leaf.category) {
                categories.push(leaf.category);
            }
        }
    }

    let y = height.saturating_sub(20);
    let _ = writeln!(out, r#"<g id="legend">"#);
    let _ = writeln!(
        out,
        r##"<text x="12" y="{y}" fill="#9aa4b2" font-size="11">布局 {} · 悬停看详情{}</text>"##,
        format.label(),
        if format == SvgFormat::Treemap {
            " · 点击分组下钻"
        } else {
            ""
        }
    );
    let mut x = 12f64 + 210.0;
    for category in categories {
        if x > width as f64 - 120.0 {
            break;
        }
        let _ = writeln!(
            out,
            r##"<rect x="{x:.1}" y="{}" width="10" height="10" fill="{}"/>"##,
            y - 9,
            category_hex(category)
        );
        let _ = writeln!(
            out,
            r##"<text x="{:.1}" y="{y}" fill="#c9d1d9" font-size="11">{}</text>"##,
            x + 14.0,
            escape(category.label())
        );
        x += 14.0 + category.label().chars().count() as f64 * 12.0 + 14.0;
    }
    let _ = writeln!(out, "</g>");
}

/// 下钻后左上角的返回按钮。
fn draw_back_button(out: &mut String, width: u32, header: u32) {
    let _ = writeln!(
        out,
        r##"<g id="back" style="display:none" class="back" onclick="unzoom()">
  <rect x="{}" y="{}" width="76" height="22" rx="4" fill="#30363d"/>
  <text x="{}" y="{}" fill="#e6e6e6" font-size="12">← 返回</text>
</g>"##,
        width.saturating_sub(92),
        header.saturating_sub(26),
        width.saturating_sub(84),
        header.saturating_sub(10)
    );
}

fn draw_script(out: &mut String) {
    let _ = writeln!(
        out,
        r#"<script><![CDATA[
function zoom(i) {{
  document.getElementById('lv0').style.display = 'none';
  var all = document.querySelectorAll('[id^="z"]');
  for (var k = 0; k < all.length; k++) {{ all[k].style.display = 'none'; }}
  var target = document.getElementById('z' + i);
  if (target) {{ target.style.display = 'block'; }}
  document.getElementById('back').style.display = 'block';
}}
function unzoom() {{
  document.getElementById('lv0').style.display = 'block';
  var all = document.querySelectorAll('[id^="z"]');
  for (var k = 0; k < all.length; k++) {{ all[k].style.display = 'none'; }}
  document.getElementById('back').style.display = 'none';
}}
]]></script>"#
    );
}

/// 把子区域放大到整块内容区的变换（布局在 Rust 侧算好，JS 只切换显示）。
fn zoom_transform(from: Rect, to: Rect) -> String {
    let scale = (to.width as f64 / from.width.max(1) as f64)
        .min(to.height as f64 / from.height.max(1) as f64);
    let tx = to.x as f64 - from.x as f64 * scale;
    let ty = to.y as f64 - from.y as f64 * scale;
    format!("translate({tx:.2},{ty:.2}) scale({scale:.4})")
}

/// 变化量：涨用绿色、跌用红色，画在块的下边缘。
fn draw_delta_strip(out: &mut String, delta: i64, x: u16, y: u16, width: u16, height: u16) {
    if delta == 0 || height < 4 {
        return;
    }
    let color = if delta > 0 { "#3fb950" } else { "#f85149" };
    let _ = writeln!(
        out,
        r##"<rect x="{x}" y="{}" width="{width}" height="3" fill="{color}"/>"##,
        y + height - 3
    );
}

/// 悬停提示：名称、大小、地址区间与权限。
fn tooltip(node: &MemNode) -> String {
    let mut text = format!("{}  {}", node.label, human_bytes(node.bytes));
    if !node.children.is_empty() {
        text.push_str(&format!("\n{} 个子项", node.children.len()));
    }
    text.push_str(&format!("\n0x{:x} – 0x{:x}", node.start, node.end));
    if let Some(vma) = &node.vma {
        text.push_str(&format!("\n权限 {}", vma.perms.as_str()));
        if !vma.pathname.is_empty() {
            text.push_str(&format!("\n{}", vma.pathname));
        }
    }
    if node.delta != 0 {
        text.push_str(&format!("\n变化 {}", signed_bytes(node.delta)));
    }
    text
}

fn signed_bytes(delta: i64) -> String {
    let marker = if delta >= 0 { '+' } else { '-' };
    format!("{marker}{}", human_bytes(delta.unsigned_abs()))
}

/// 与 TUI 一致的类别配色（xterm 256 色近似值）。
fn category_hex(category: Category) -> &'static str {
    match category {
        Category::Heap => "#d7af5f",
        Category::Stack => "#d75f5f",
        Category::JitExec => "#875fd7",
        Category::Vdso => "#808080",
        Category::AnonShared => "#5fafaf",
        Category::AnonPrivate => "#5f87af",
        Category::FileShared => "#87af87",
        Category::FilePrivate => "#5f8787",
        Category::Special => "#585858",
    }
}

/// 按字符数截断，避免超长路径溢出块。
fn truncate(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(1);
    let mut out: String = text.chars().take(keep).collect();
    out.push('…');
    out
}

/// XML 转义：标签里可能出现 `&`、`<`、`>`。
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\n' => out.push_str("&#10;"),
            _ => out.push(character),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tmv_core::{apply_deltas, build_tree, demo, diff, GroupBy, Metric, SortOrder, TreeOptions};

    fn tree() -> MemNode {
        build_tree(
            &demo::snapshot(),
            TreeOptions {
                metric: Metric::Pss,
                group_by: GroupBy::Category,
                sort: SortOrder::SizeDesc,
            },
        )
    }

    fn options(format: SvgFormat) -> SvgOptions {
        SvgOptions {
            title: "tmv · pid 4242".to_string(),
            subtitle: "度量 PSS · 合计 600M".to_string(),
            format,
            ..SvgOptions::default()
        }
    }

    fn svg(format: SvgFormat) -> String {
        render(&tree(), &options(format))
    }

    #[test]
    fn treemap_is_interactive_and_has_tooltips() {
        let root = tree();
        let svg = svg(SvgFormat::Treemap);

        assert!(svg.contains(r#"onclick="zoom(0)""#), "分组应可点击下钻");
        assert!(svg.contains(r#"id="z0""#), "应预计算下一层");
        assert!(svg.contains("<title>"), "每块应有悬停提示");
        assert!(svg.contains("权限"), "提示里应含权限位");
        assert!(svg.contains("function unzoom()"));
        // 过窄的分组放不下子块时不会生成缩放组，这里只要求数量不超过分组数。
        let zoom_groups = svg.matches("<g id=\"z").count();
        assert!(zoom_groups >= 1 && zoom_groups <= root.children.len());
    }

    #[test]
    fn flame_layout_stacks_three_rows() {
        let root = tree();
        let svg = svg(SvgFormat::Flame);

        assert!(svg.contains(r#"<g id="flame">"#));
        assert!(!svg.contains(r#"onclick="zoom(0)""#), "火焰图不做下钻");
        // 根 + 分组 + 每个分组的子项；过窄的叶子会被跳过，所以是区间而不是精确值。
        let leaves: usize = root
            .children
            .iter()
            .map(|child| child.children.len().max(1))
            .sum();
        let bars = svg.matches(r#"class="blk""#).count();
        assert!(
            bars > root.children.len() && bars <= 1 + root.children.len() + leaves,
            "实际 {bars} 条"
        );
        assert!(svg.contains("布局 flame"));
    }

    #[test]
    fn legend_and_delta_strip_are_rendered() {
        let mut root = tree();
        let after = demo::snapshot();
        let mut before = demo::snapshot();
        // 让 heap 少 1 MiB，`after` 相对 `before` 就是 +1 MiB 的增长。
        if let Some(vma) = before.vmas.iter_mut().find(|vma| vma.pathname == "[heap]") {
            if let Some(stats) = vma.stats.as_mut() {
                stats.pss = stats.pss.saturating_sub(1024 * 1024);
            }
        }
        let changes = diff::diff(&before, &after, Metric::Pss);
        assert!(changes.total > 0, "构造的基线应产生正向差值");
        apply_deltas(&mut root, &changes);

        let svg = render(&root, &options(SvgFormat::Treemap));
        assert!(svg.contains(r#"<g id="legend">"#));
        assert!(svg.contains("布局 treemap"), "图例应标明布局");
        assert!(
            svg.contains("#3fb950") || svg.contains("#f85149"),
            "有变化时应画涨跌条带"
        );
    }

    #[test]
    fn escapes_xml_special_characters() {
        let mut root = tree();
        root.children[0].label = "a<b&c".to_string();
        let svg = render(&root, &SvgOptions::default());
        assert!(svg.contains("a&lt;b&amp;c"));
        assert!(!svg.contains("a<b&c"));
    }

    #[test]
    fn empty_tree_renders_placeholder() {
        let svg = render(&MemNode::empty(), &SvgOptions::default());
        assert!(svg.contains("没有可展示的数据"));
        assert!(svg.trim_end().ends_with("</svg>"));
    }
}
