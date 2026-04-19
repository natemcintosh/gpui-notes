//! Render a single block's markdown text as inline GPUI elements.
//!
//! v1 coverage: bold, italic, inline code, links (http/https open via
//! `cx.open_url`), headings, fenced code blocks, blockquotes. List events from
//! the markdown parser are ignored — outline composition owns bullet nesting.
//!
//! Extensions (see `InlineExtension`) are the single hook for downstream
//! tokens like `[[PageName]]` (#8), `((block-id))` (#10), `#tag` (#11). They
//! match byte ranges inside plain-text runs and produce their own elements at
//! render time.

use std::ops::Range;

use gpui::{
    div, px, AnyElement, App, ElementId, FontWeight, InteractiveElement, IntoElement,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window,
};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

pub mod theme {
    use gpui::rgb;
    use gpui::Rgba;

    #[must_use]
    pub fn fg() -> Rgba {
        rgb(0xe6e6e6)
    }
    #[must_use]
    pub fn fg_muted() -> Rgba {
        rgb(0x9a9a9a)
    }
    #[must_use]
    pub fn bg_subtle() -> Rgba {
        rgb(0x2a2a2a)
    }
    #[must_use]
    pub fn accent() -> Rgba {
        rgb(0x66b2ff)
    }
    #[must_use]
    pub fn code_bg() -> Rgba {
        rgb(0x1e1e1e)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InlineNode {
    Text {
        text: String,
        style: Style,
    },
    Code(String),
    Link {
        url: SharedString,
        children: Vec<InlineNode>,
    },
    Extension(ExtensionNode),
    SoftBreak,
    HardBreak,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExtensionNode {
    pub kind: SharedString,
    pub source: SharedString,
    /// Index into the `extensions` slice passed to `lower`/`render_block`.
    pub extension_idx: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockNode {
    Paragraph(Vec<InlineNode>),
    Heading {
        level: u8,
        children: Vec<InlineNode>,
    },
    CodeBlock {
        lang: Option<SharedString>,
        text: String,
    },
    Quote(Vec<BlockNode>),
}

/// A match produced by an `InlineExtension` on a single text run.
#[derive(Clone, Debug)]
pub struct ExtensionMatch {
    pub range: Range<usize>,
    pub kind: SharedString,
}

/// Hook for downstream token matchers (#8 links, #10 block refs, #11 tags).
pub trait InlineExtension {
    /// Return non-overlapping byte ranges within `text`. Overlaps across
    /// extensions are resolved by the lowerer — earlier extensions win.
    fn extract(&self, text: &str) -> Vec<ExtensionMatch>;

    /// Render a matched span as an element.
    fn render(&self, node: &ExtensionNode, window: &mut Window, cx: &mut App) -> AnyElement;
}

#[must_use]
#[allow(clippy::too_many_lines)] // Event dispatch is a flat state machine; splitting hurts readability.
pub fn lower(text: &str, extensions: &[&dyn InlineExtension]) -> Vec<BlockNode> {
    let options = Options::empty();
    let parser = Parser::new_ext(text, options);

    let mut blocks: Vec<BlockNode> = Vec::new();
    // Stack of block containers being built. Root holds finished blocks.
    let mut quote_stack: Vec<Vec<BlockNode>> = Vec::new();
    // Inline scratch buffer for the currently-open inline container (paragraph,
    // heading, or link children).
    let mut inline_stack: Vec<Vec<InlineNode>> = Vec::new();
    // Parallel stack describing what each inline buffer is for.
    let mut inline_ctx: Vec<InlineCtx> = Vec::new();
    let mut style_stack: Vec<Style> = vec![Style::default()];

    let mut code_block: Option<(Option<SharedString>, String)> = None;

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    inline_stack.push(Vec::new());
                    inline_ctx.push(InlineCtx::Paragraph);
                }
                Tag::Heading { level, .. } => {
                    inline_stack.push(Vec::new());
                    inline_ctx.push(InlineCtx::Heading(heading_level(level)));
                }
                Tag::BlockQuote(_) => {
                    quote_stack.push(Vec::new());
                }
                Tag::CodeBlock(kind) => {
                    let lang = match kind {
                        CodeBlockKind::Fenced(s) if !s.is_empty() => {
                            Some(SharedString::from(s.into_string()))
                        }
                        _ => None,
                    };
                    code_block = Some((lang, String::new()));
                }
                Tag::Emphasis => push_style(&mut style_stack, |s| s.italic = true),
                Tag::Strong => push_style(&mut style_stack, |s| s.bold = true),
                Tag::Link { dest_url, .. } => {
                    inline_stack.push(Vec::new());
                    inline_ctx.push(InlineCtx::Link(SharedString::from(dest_url.into_string())));
                }
                // Lists and items: outline owns bullet structure; ignore.
                // Other unsupported tags fall through to be rendered as their text children.
                _ => {}
            },
            Event::End(end) => match end {
                TagEnd::Paragraph => {
                    let children = inline_stack.pop().unwrap_or_default();
                    inline_ctx.pop();
                    push_block(
                        &mut blocks,
                        &mut quote_stack,
                        BlockNode::Paragraph(children),
                    );
                }
                TagEnd::Heading(_) => {
                    let children = inline_stack.pop().unwrap_or_default();
                    let level = match inline_ctx.pop() {
                        Some(InlineCtx::Heading(l)) => l,
                        _ => 1,
                    };
                    push_block(
                        &mut blocks,
                        &mut quote_stack,
                        BlockNode::Heading { level, children },
                    );
                }
                TagEnd::BlockQuote => {
                    let children = quote_stack.pop().unwrap_or_default();
                    push_block(&mut blocks, &mut quote_stack, BlockNode::Quote(children));
                }
                TagEnd::CodeBlock => {
                    if let Some((lang, text)) = code_block.take() {
                        push_block(
                            &mut blocks,
                            &mut quote_stack,
                            BlockNode::CodeBlock { lang, text },
                        );
                    }
                }
                TagEnd::Emphasis | TagEnd::Strong => {
                    style_stack.pop();
                    if style_stack.is_empty() {
                        style_stack.push(Style::default());
                    }
                }
                TagEnd::Link => {
                    let children = inline_stack.pop().unwrap_or_default();
                    let url = match inline_ctx.pop() {
                        Some(InlineCtx::Link(u)) => u,
                        _ => SharedString::default(),
                    };
                    push_inline(&mut inline_stack, InlineNode::Link { url, children });
                }
                _ => {}
            },
            Event::Text(s) => {
                if let Some((_, buf)) = code_block.as_mut() {
                    buf.push_str(&s);
                    continue;
                }
                let style = style_stack.last().copied().unwrap_or_default();
                let text: String = s.into_string();
                for node in apply_extensions(&text, style, extensions) {
                    push_inline(&mut inline_stack, node);
                }
            }
            Event::Code(s) => {
                push_inline(&mut inline_stack, InlineNode::Code(s.into_string()));
            }
            Event::SoftBreak => push_inline(&mut inline_stack, InlineNode::SoftBreak),
            Event::HardBreak => push_inline(&mut inline_stack, InlineNode::HardBreak),
            Event::Html(s) | Event::InlineHtml(s) => {
                let style = style_stack.last().copied().unwrap_or_default();
                push_inline(
                    &mut inline_stack,
                    InlineNode::Text {
                        text: s.into_string(),
                        style,
                    },
                );
            }
            _ => {}
        }
    }

    blocks
}

enum InlineCtx {
    Paragraph,
    Heading(u8),
    Link(SharedString),
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn push_style(stack: &mut Vec<Style>, mutate: impl FnOnce(&mut Style)) {
    let mut next = stack.last().copied().unwrap_or_default();
    mutate(&mut next);
    stack.push(next);
}

fn push_inline(stack: &mut [Vec<InlineNode>], node: InlineNode) {
    if let Some(top) = stack.last_mut() {
        top.push(node);
    }
    // Top-level text outside any paragraph (rare with pulldown-cmark) is dropped.
}

fn push_block(roots: &mut Vec<BlockNode>, quote_stack: &mut [Vec<BlockNode>], block: BlockNode) {
    if let Some(top) = quote_stack.last_mut() {
        top.push(block);
    } else {
        roots.push(block);
    }
}

fn apply_extensions(
    text: &str,
    style: Style,
    extensions: &[&dyn InlineExtension],
) -> Vec<InlineNode> {
    // Collect all matches across extensions, resolving overlap by keeping the
    // earliest-starting (then earliest-registered) match.
    let mut matches: Vec<(Range<usize>, usize, SharedString)> = Vec::new();
    for (idx, ext) in extensions.iter().enumerate() {
        for m in ext.extract(text) {
            matches.push((m.range, idx, m.kind));
        }
    }
    matches.sort_by_key(|(r, idx, _)| (r.start, *idx));

    let mut resolved: Vec<(Range<usize>, usize, SharedString)> = Vec::with_capacity(matches.len());
    let mut cursor = 0usize;
    for (range, idx, kind) in matches {
        if range.start < cursor || range.end > text.len() || range.start >= range.end {
            continue;
        }
        cursor = range.end;
        resolved.push((range, idx, kind));
    }

    let mut out: Vec<InlineNode> = Vec::new();
    let mut cursor = 0usize;
    for (range, idx, kind) in resolved {
        if range.start > cursor {
            out.push(InlineNode::Text {
                text: text[cursor..range.start].to_string(),
                style,
            });
        }
        out.push(InlineNode::Extension(ExtensionNode {
            kind,
            source: SharedString::from(text[range.clone()].to_string()),
            extension_idx: idx,
        }));
        cursor = range.end;
    }
    if cursor < text.len() {
        out.push(InlineNode::Text {
            text: text[cursor..].to_string(),
            style,
        });
    }
    out
}

pub fn render_block(
    text: &str,
    extensions: &[&dyn InlineExtension],
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let blocks = lower(text, extensions);
    let mut ctx = RenderCtx {
        link_counter: 0,
        extensions,
    };
    let mut root = div().flex().flex_col().gap_1().text_color(theme::fg());
    for block in blocks {
        root = root.child(render_block_node(block, &mut ctx, window, cx));
    }
    root.into_any_element()
}

struct RenderCtx<'a> {
    link_counter: usize,
    extensions: &'a [&'a dyn InlineExtension],
}

fn render_block_node(
    block: BlockNode,
    ctx: &mut RenderCtx<'_>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    match block {
        BlockNode::Paragraph(children) => render_inlines(children, ctx, window, cx),
        BlockNode::Heading { level, children } => {
            let size = heading_size(level);
            div()
                .flex()
                .flex_wrap()
                .font_weight(FontWeight::BOLD)
                .text_size(px(size))
                .child(render_inlines(children, ctx, window, cx))
                .into_any_element()
        }
        BlockNode::CodeBlock { lang: _, text } => div()
            .bg(theme::code_bg())
            .text_color(theme::fg())
            .font_family("monospace")
            .p_2()
            .rounded_sm()
            .child(text)
            .into_any_element(),
        BlockNode::Quote(children) => {
            let mut wrap = div()
                .border_l_2()
                .border_color(theme::fg_muted())
                .pl_2()
                .text_color(theme::fg_muted())
                .flex()
                .flex_col()
                .gap_1();
            for child in children {
                wrap = wrap.child(render_block_node(child, ctx, window, cx));
            }
            wrap.into_any_element()
        }
    }
}

fn heading_size(level: u8) -> f32 {
    match level {
        1 => 22.0,
        2 => 19.0,
        3 => 17.0,
        _ => 15.0,
    }
}

fn render_inlines(
    nodes: Vec<InlineNode>,
    ctx: &mut RenderCtx<'_>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let mut wrap = div().flex().flex_wrap();
    for node in nodes {
        wrap = wrap.child(render_inline(node, ctx, window, cx));
    }
    wrap.into_any_element()
}

fn render_inline(
    node: InlineNode,
    ctx: &mut RenderCtx<'_>,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    match node {
        InlineNode::Text { text, style } => styled_span(text, style),
        InlineNode::Code(text) => div()
            .font_family("monospace")
            .bg(theme::bg_subtle())
            .px_1()
            .rounded_sm()
            .child(text)
            .into_any_element(),
        InlineNode::Link { url, children } => {
            ctx.link_counter += 1;
            let id = ElementId::named_usize("md-link", ctx.link_counter);
            let href = url.clone();
            let mut wrap = div()
                .id(id)
                .cursor_pointer()
                .text_color(theme::accent())
                .underline()
                .on_click(move |_, _window, cx| {
                    if href.starts_with("http://") || href.starts_with("https://") {
                        cx.open_url(&href);
                    }
                });
            for child in children {
                wrap = wrap.child(render_inline(child, ctx, window, cx));
            }
            wrap.into_any_element()
        }
        InlineNode::Extension(node) => {
            let ext = ctx.extensions.get(node.extension_idx);
            match ext {
                Some(e) => e.render(&node, window, cx),
                None => styled_span(node.source.to_string(), Style::default()),
            }
        }
        InlineNode::SoftBreak => styled_span(" ".into(), Style::default()),
        InlineNode::HardBreak => div().w_full().into_any_element(),
    }
}

fn styled_span(text: String, style: Style) -> AnyElement {
    let mut d = div().child(SharedString::from(text));
    if style.bold {
        d = d.font_weight(FontWeight::BOLD);
    }
    if style.italic {
        d = d.italic();
    }
    d.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lower_no_ext(s: &str) -> Vec<BlockNode> {
        lower(s, &[])
    }

    fn paragraph(blocks: Vec<BlockNode>) -> Vec<InlineNode> {
        match blocks.into_iter().next() {
            Some(BlockNode::Paragraph(c)) => c,
            other => panic!("expected paragraph, got {other:?}"),
        }
    }

    fn plain(text: &str) -> InlineNode {
        InlineNode::Text {
            text: text.into(),
            style: Style::default(),
        }
    }

    fn styled(text: &str, bold: bool, italic: bool) -> InlineNode {
        InlineNode::Text {
            text: text.into(),
            style: Style { bold, italic },
        }
    }

    #[test]
    fn plain_text_is_single_text_node() {
        let inlines = paragraph(lower_no_ext("hello world"));
        assert_eq!(inlines, vec![plain("hello world")]);
    }

    #[test]
    fn bold_toggles_style() {
        let inlines = paragraph(lower_no_ext("hello **world**"));
        assert_eq!(inlines, vec![plain("hello "), styled("world", true, false)]);
    }

    #[test]
    fn italic_toggles_style() {
        let inlines = paragraph(lower_no_ext("*hi* there"));
        assert_eq!(inlines, vec![styled("hi", false, true), plain(" there")]);
    }

    #[test]
    fn nested_bold_italic_stacks() {
        let inlines = paragraph(lower_no_ext("***yo***"));
        // pulldown-cmark emits strong(emphasis(text)) or emphasis(strong(text));
        // either ordering must produce a single bold+italic span.
        assert_eq!(inlines, vec![styled("yo", true, true)]);
    }

    #[test]
    fn inline_code_becomes_code_node() {
        let inlines = paragraph(lower_no_ext("see `foo` now"));
        assert_eq!(
            inlines,
            vec![plain("see "), InlineNode::Code("foo".into()), plain(" now"),]
        );
    }

    #[test]
    fn link_wraps_children() {
        let inlines = paragraph(lower_no_ext("[ex](https://x.y)"));
        assert_eq!(
            inlines,
            vec![InlineNode::Link {
                url: SharedString::from("https://x.y"),
                children: vec![plain("ex")],
            }]
        );
    }

    #[test]
    fn heading_lowers_with_level() {
        let blocks = lower_no_ext("## Title");
        assert_eq!(
            blocks,
            vec![BlockNode::Heading {
                level: 2,
                children: vec![plain("Title")]
            }]
        );
    }

    #[test]
    fn fenced_code_block_preserves_text_and_lang() {
        let blocks = lower_no_ext("```rust\nfn x() {}\n```\n");
        assert_eq!(
            blocks,
            vec![BlockNode::CodeBlock {
                lang: Some("rust".into()),
                text: "fn x() {}\n".into(),
            }]
        );
    }

    #[test]
    fn blockquote_nests_children() {
        let blocks = lower_no_ext("> quoted");
        let BlockNode::Quote(inner) = &blocks[0] else {
            panic!("expected quote, got {:?}", blocks[0]);
        };
        assert_eq!(inner.len(), 1);
        assert!(matches!(inner[0], BlockNode::Paragraph(_)));
    }

    struct MockTag;
    impl InlineExtension for MockTag {
        fn extract(&self, text: &str) -> Vec<ExtensionMatch> {
            let mut out = Vec::new();
            let bytes = text.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'#' {
                    let start = i;
                    i += 1;
                    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_')
                    {
                        i += 1;
                    }
                    if i > start + 1 {
                        out.push(ExtensionMatch {
                            range: start..i,
                            kind: "tag".into(),
                        });
                    }
                } else {
                    i += 1;
                }
            }
            out
        }

        fn render(&self, _node: &ExtensionNode, _window: &mut Window, _cx: &mut App) -> AnyElement {
            unreachable!("not used in lower() tests")
        }
    }

    #[test]
    fn extension_splices_into_plain_text_with_style_preserved() {
        let ext = MockTag;
        let exts: [&dyn InlineExtension; 1] = [&ext];
        let inlines = paragraph(lower("see **#rust** later", &exts));
        assert_eq!(
            inlines,
            vec![
                plain("see "),
                InlineNode::Extension(ExtensionNode {
                    kind: "tag".into(),
                    source: "#rust".into(),
                    extension_idx: 0,
                }),
                plain(" later"),
            ]
        );
    }

    #[test]
    fn overlapping_extensions_resolved_by_earliest_then_first_registered() {
        struct First;
        struct Second;
        impl InlineExtension for First {
            fn extract(&self, _text: &str) -> Vec<ExtensionMatch> {
                vec![ExtensionMatch {
                    range: 0..3,
                    kind: "a".into(),
                }]
            }
            fn render(&self, _: &ExtensionNode, _: &mut Window, _: &mut App) -> AnyElement {
                unreachable!()
            }
        }
        impl InlineExtension for Second {
            fn extract(&self, _text: &str) -> Vec<ExtensionMatch> {
                vec![ExtensionMatch {
                    range: 1..4,
                    kind: "b".into(),
                }]
            }
            fn render(&self, _: &ExtensionNode, _: &mut Window, _: &mut App) -> AnyElement {
                unreachable!()
            }
        }
        let a = First;
        let b = Second;
        let exts: [&dyn InlineExtension; 2] = [&a, &b];
        let inlines = paragraph(lower("abcdef", &exts));
        assert_eq!(
            inlines,
            vec![
                InlineNode::Extension(ExtensionNode {
                    kind: "a".into(),
                    source: "abc".into(),
                    extension_idx: 0,
                }),
                plain("def"),
            ]
        );
    }
}
