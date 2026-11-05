//! 把 treemap 导出为 SVG：两层布局（分组 → 叶子），带标签与配色。
//!
//! 与 TUI 共用 [`crate::treemap::squarify`]，所以导出的形状和界面一致。

use std::fmt::Write;

use ratatui::layout::Rect;
use tmv_core::{Category, MemNode};

use crate::treemap::squarify;
use crate::ui::human_bytes;

/// 导出尺寸与标题。
#[derive(Debug, Clone)]
pub struct SvgOptions {
    pub width: u32,
    pub height: u32,
    /// 画布顶部的标题，通常包含 pid 与度量。
    pub title: String,
}

impl Default for SvgOptions {
    fn default() -> Self {
        Self {
            width: 1600,
            height: 1000,
            title: String::new(),
        }
    }
}

/// 渲染整棵树，返回独立的 SVG 文本。
pub fn render(root: &MemNode, options: &SvgOptions) -> String {
    let width = options.width.clamp(320, u16::MAX as u32);
    let height = options.height.clamp(200, u16::MAX as u32);
    let mut out = String::with_capacity(8192);
    let _ = writeln!(out, r#"<?xml version="1.0" encoding="UTF-8"?>"#);
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}" viewBox="0 0 {width} {height}" font-family="monospace">"#
    );
    let _ = writeln!(
        out,
        r##"<rect width="100%" height="100%" fill="#111418"/>"##
    );
    if !options.title.is_empty() {
        let _ = writeln!(
            out,
            r##"<text x="12" y="22" fill="#e6e6e6" font-size="15">{}</text>"##,
            escape(&options.title)
        );
    }

    let area = Rect::new(
        10,
        34,
        (width - 20) as u16,
        height.saturating_sub(46) as u16,
    );
    if root.children.is_empty() || area.width == 0 || area.height == 0 {
        let _ = writeln!(
            out,
            r##"<text x="12" y="60" fill="#888888">没有可展示的数据</text>"##
        );
        let _ = writeln!(out, "</svg>");
        return out;
    }

    let weights: Vec<f64> = root
        .children
        .iter()
        .map(|child| child.bytes as f64)
        .collect();
    for (child, rect) in root.children.iter().zip(squarify(&weights, area)) {
        draw_group(&mut out, child, rect);
    }
    let _ = writeln!(out, "</svg>");
    out
}

/// 一级分组：底色块 + 标题，内部再铺一层叶子。
fn draw_group(out: &mut String, node: &MemNode, rect: Rect) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let fill = category_hex(node.category);
    let _ = writeln!(
        out,
        r##"<g><rect x="{}" y="{}" width="{}" height="{}" fill="{fill}" fill-opacity="0.4" stroke="#0b0d10"/>"##,
        rect.x, rect.y, rect.width, rect.height
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

    // 头部留给分组标题，剩下区域铺子块。
    let inner = Rect::new(
        rect.x + 2,
        rect.y + 20,
        rect.width.saturating_sub(4),
        rect.height.saturating_sub(24),
    );
    if inner.width > 2 && inner.height > 2 && !node.children.is_empty() {
        let weights: Vec<f64> = node
            .children
            .iter()
            .map(|child| child.bytes as f64)
            .collect();
        for (child, child_rect) in node.children.iter().zip(squarify(&weights, inner)) {
            draw_leaf(out, child, child_rect);
        }
    }
    let _ = writeln!(out, "</g>");
}

fn draw_leaf(out: &mut String, node: &MemNode, rect: Rect) {
    if rect.width < 2 || rect.height < 2 {
        return;
    }
    let fill = category_hex(node.category);
    let _ = writeln!(
        out,
        r##"<rect x="{}" y="{}" width="{}" height="{}" fill="{fill}" stroke="#0b0d10"/>"##,
        rect.x, rect.y, rect.width, rect.height
    );
    if rect.width >= 70 && rect.height >= 26 {
        let label = format!("{}  {}", node.label, human_bytes(node.bytes));
        let _ = writeln!(
            out,
            r##"<text x="{}" y="{}" fill="#eaeaea" font-size="11">{}</text>"##,
            rect.x + 3,
            rect.y + rect.height / 2 + 4,
            escape(&truncate(&label, rect.width as usize / 6))
        );
    }
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
            _ => out.push(character),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tmv_core::{build_tree, demo, GroupBy, Metric, SortOrder, TreeOptions};

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

    #[test]
    fn renders_svg_with_group_and_leaf_rects() {
        let root = tree();
        let svg = render(
            &root,
            &SvgOptions {
                title: "pid 4242 · PSS".to_string(),
                ..SvgOptions::default()
            },
        );

        assert!(svg.starts_with(r#"<?xml version="1.0" encoding="UTF-8"?>"#));
        assert!(svg.trim_end().ends_with("</svg>"));
        assert!(svg.contains("pid 4242"));
        assert_eq!(
            svg.matches("<g>").count(),
            root.children.len(),
            "每个一级分组一个 <g>"
        );
        assert!(svg.matches("<rect").count() > root.children.len());
        assert!(svg.contains("font-family=\"monospace\""));
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
