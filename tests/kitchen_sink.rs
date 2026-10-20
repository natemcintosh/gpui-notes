//! Keeps `docs/fixtures/kitchen-sink.md` honest: it must survive a save
//! unchanged, and every feature its blocks claim to show must actually lower.

use std::collections::BTreeSet;

use gpui_notes::block_render::{lower, BlockNode, InlineExtension, InlineNode};
use gpui_notes::outline::Outline;
use gpui_notes::page_links::PageLinkExt;

const FIXTURE: &str = include_str!("../docs/fixtures/kitchen-sink.md");

#[test]
fn fixture_round_trips_byte_for_byte() {
    let outline = Outline::parse(FIXTURE);
    assert!(outline.preamble.starts_with("title:: kitchen-sink\n"));
    assert_eq!(outline.serialize(), FIXTURE);
}

/// Names for each kind of node in `nodes`, for the coverage check below.
fn inline_features(nodes: &[InlineNode], out: &mut BTreeSet<String>) {
    for node in nodes {
        match node {
            InlineNode::Text { style, .. } => {
                if style.bold {
                    out.insert("bold".into());
                }
                if style.italic {
                    out.insert("italic".into());
                }
                if style.bold && style.italic {
                    out.insert("bold italic".into());
                }
                if style.strikethrough {
                    out.insert("strikethrough".into());
                }
            }
            InlineNode::Code(_) => {
                out.insert("inline code".into());
            }
            InlineNode::Link { url, children } => {
                let scheme = if url.starts_with("https://") {
                    "http"
                } else {
                    "other"
                };
                out.insert(format!("{scheme} link"));
                inline_features(children, out);
            }
            InlineNode::Extension(ext) => {
                out.insert(ext.kind.to_string());
            }
            InlineNode::SoftBreak => {
                out.insert("soft break".into());
            }
            InlineNode::HardBreak => {
                out.insert("hard break".into());
            }
        }
    }
}

fn block_features(blocks: &[BlockNode], depth: usize, out: &mut BTreeSet<String>) {
    for block in blocks {
        match block {
            BlockNode::Paragraph(children) => inline_features(children, out),
            BlockNode::Heading { level, children } => {
                out.insert(format!("h{level}"));
                inline_features(children, out);
            }
            BlockNode::CodeBlock { lang, .. } => {
                out.insert(format!("code {}", lang.as_ref().map_or("-", AsRef::as_ref)));
            }
            BlockNode::Quote(children) => {
                out.insert(format!("quote depth {}", depth + 1));
                block_features(children, depth + 1, out);
            }
        }
    }
}

#[test]
fn fixture_covers_every_rendered_feature() {
    let outline = Outline::parse(FIXTURE);
    let extensions: [&dyn InlineExtension; 1] = [&PageLinkExt];
    let mut found = BTreeSet::new();
    for block in outline.preorder() {
        let lowered = lower(&block.text, &extensions);
        if lowered
            .iter()
            .filter(|b| matches!(b, BlockNode::Paragraph(_)))
            .count()
            > 1
        {
            found.insert("multiple paragraphs".to_string());
        }
        if block.text.is_empty() {
            found.insert("empty block".into());
        }
        block_features(&lowered, 0, &mut found);
    }
    let depth = |b: &gpui_notes::outline::Block| b.children.iter().any(|c| !c.children.is_empty());
    if outline.roots.iter().any(depth) {
        found.insert("three levels".into());
    }

    let expected = [
        "bold",
        "italic",
        "bold italic",
        "strikethrough",
        "inline code",
        "http link",
        "other link",
        "page-link",
        "soft break",
        "hard break",
        "h1",
        "h2",
        "h3",
        "quote depth 1",
        "quote depth 2",
        "code rust",
        "code python",
        "code nosuchlang",
        "code calc",
        "multiple paragraphs",
        "empty block",
        "three levels",
    ];
    let missing: Vec<_> = expected.iter().filter(|f| !found.contains(**f)).collect();
    assert!(missing.is_empty(), "fixture no longer shows {missing:?}");
}
