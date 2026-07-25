//! `[[Page Name]]` links and the forward/reverse index that backs them.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::io;
use std::sync::LazyLock;

use gpui::{
    div, App, BorrowAppContext, ElementId, Global, InteractiveElement, IntoElement, MouseButton,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window,
};
use regex::Regex;

use crate::block_render::{
    lower, theme, BlockNode, ExtensionMatch, ExtensionNode, InlineExtension, InlineNode,
};
use crate::outline::{Block, BlockId, Outline};
use crate::page::Page;
use crate::store::NotesStore;

static PAGE_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\[\]\n]+?)\]\]").expect("page-link regex is valid"));

pub type Backlink = (SharedString, BlockId);

static EMPTY_BACKLINKS: LazyLock<BTreeSet<Backlink>> = LazyLock::new(BTreeSet::new);
static EMPTY_TARGETS: LazyLock<HashSet<SharedString>> = LazyLock::new(HashSet::new);

#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = gpui_notes, no_json)]
pub struct OpenPage {
    pub name: SharedString,
}

/// Forward and reverse page-link lookups.
///
/// Page names are case-preserving and case-sensitive. The separate `pages`
/// set lets the renderer distinguish a missing target without touching disk.
#[derive(Default)]
pub struct LinkIndex {
    reverse: HashMap<String, BTreeSet<Backlink>>,
    forward: HashMap<String, HashSet<SharedString>>,
    pages: HashSet<String>,
}

impl Global for LinkIndex {}

impl LinkIndex {
    /// Replaces all indexed links for `page` with its current outline.
    pub fn reindex_page(&mut self, page: &Page) {
        self.reindex_outline(page.name(), page.outline());
    }

    /// Returns sources that link to `target`, ordered by page name then block.
    #[must_use]
    pub fn backlinks(&self, target: &str) -> &BTreeSet<Backlink> {
        self.reverse.get(target).unwrap_or(&EMPTY_BACKLINKS)
    }

    /// Returns the unique page names referenced by `source`.
    #[must_use]
    pub fn targets(&self, source: &str) -> &HashSet<SharedString> {
        self.forward.get(source).unwrap_or(&EMPTY_TARGETS)
    }

    #[must_use]
    pub fn page_exists(&self, name: &str) -> bool {
        self.pages.contains(name)
    }

    /// Every page name the index knows about, sorted case-insensitively.
    /// Backs the `[[` completion popup, which needs the list on every
    /// keystroke and so cannot afford to re-read the notes directory.
    #[must_use]
    pub fn page_names(&self) -> Vec<SharedString> {
        let mut names: Vec<SharedString> = self
            .pages
            .iter()
            .map(|name| SharedString::from(name.clone()))
            .collect();
        names.sort_by_key(|name| name.to_lowercase());
        names
    }

    /// Builds an index from every regular page currently on disk.
    ///
    /// # Errors
    /// Returns the first I/O error from listing or reading pages.
    pub fn rebuild_from(store: &NotesStore) -> io::Result<Self> {
        let mut index = Self::default();
        for name in store.list()? {
            let body = store.read(&name)?;
            let source = SharedString::from(name);
            index.reindex_outline(&source, &Outline::parse(&body));
        }
        Ok(index)
    }

    fn reindex_outline(&mut self, source: &SharedString, outline: &Outline) {
        let source_name = source.to_string();

        // The forward map identifies exactly which reverse entries can contain
        // this source, avoiding a sweep over unrelated targets.
        if let Some(old_targets) = self.forward.remove(&source_name) {
            for target in old_targets {
                let target = target.to_string();
                let remove_entry = self.reverse.get_mut(&target).is_some_and(|backlinks| {
                    backlinks.retain(|(page, _)| page.as_ref() != source.as_ref());
                    backlinks.is_empty()
                });
                if remove_entry {
                    self.reverse.remove(&target);
                }
            }
        }

        let mut targets = HashSet::new();
        for block in all_blocks(&outline.roots) {
            for target in page_links_in_text(&block.text) {
                targets.insert(target.clone());
                self.reverse
                    .entry(target.to_string())
                    .or_default()
                    .insert((source.clone(), block.id));
            }
        }

        self.pages.insert(source_name.clone());
        self.forward.insert(source_name, targets);
    }
}

pub struct PageLinkExt;

impl InlineExtension for PageLinkExt {
    fn extract(&self, text: &str) -> Vec<ExtensionMatch> {
        PAGE_LINK_RE
            .captures_iter(text)
            .filter_map(|captures| {
                let whole = captures.get(0)?;
                let name = captures.get(1)?.as_str().trim();
                (!name.is_empty()).then(|| ExtensionMatch {
                    range: whole.range(),
                    kind: "page-link".into(),
                })
            })
            .collect()
    }

    fn render(&self, node: &ExtensionNode, _window: &mut Window, cx: &mut App) -> gpui::AnyElement {
        let Some(name) = page_name(&node.source) else {
            return div().child(node.source.clone()).into_any_element();
        };
        let name = SharedString::from(name.to_string());
        let exists = cx.has_global::<LinkIndex>() && cx.global::<LinkIndex>().page_exists(&name);

        let mut link = div()
            .id(ElementId::Name(SharedString::from(format!(
                "page-link-{name}"
            ))))
            .cursor_pointer()
            .text_color(if exists {
                theme::accent()
            } else {
                theme::fg_muted()
            })
            .hover(Styled::underline)
            .child(name.clone())
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            });
        if !exists {
            link = link.italic();
        }
        link.on_click(move |_, window, cx| {
            cx.stop_propagation();
            window.dispatch_action(Box::new(OpenPage { name: name.clone() }), cx);
        })
        .into_any_element()
    }
}

/// Refreshes the global index after a regular page is opened, created, or saved.
pub fn reindex_global_for_page(page: &gpui::Entity<Page>, cx: &mut App) {
    if !cx.has_global::<LinkIndex>() {
        return;
    }
    let (name, outline) = {
        let page = page.read(cx);
        (page.name().clone(), page.outline().clone())
    };
    cx.update_global::<LinkIndex, ()>(|index, _| {
        index.reindex_outline(&name, &outline);
    });
}

fn page_name(source: &str) -> Option<&str> {
    let name = source.strip_prefix("[[")?.strip_suffix("]]")?.trim();
    (!name.is_empty()).then_some(name)
}

fn all_blocks(blocks: &[Block]) -> Vec<&Block> {
    fn walk<'a>(out: &mut Vec<&'a Block>, blocks: &'a [Block]) {
        for block in blocks {
            out.push(block);
            walk(out, &block.children);
        }
    }

    let mut out = Vec::new();
    walk(&mut out, blocks);
    out
}

fn page_links_in_text(text: &str) -> BTreeSet<SharedString> {
    let ext = PageLinkExt;
    let extensions: [&dyn InlineExtension; 1] = [&ext];
    let blocks = lower(text, &extensions);
    let mut links = BTreeSet::new();
    for block in &blocks {
        collect_links_from_block(block, &mut links);
    }
    links
}

fn collect_links_from_block(block: &BlockNode, out: &mut BTreeSet<SharedString>) {
    match block {
        BlockNode::Paragraph(children) | BlockNode::Heading { children, .. } => {
            collect_links_from_inlines(children, out);
        }
        BlockNode::Quote(children) => {
            for child in children {
                collect_links_from_block(child, out);
            }
        }
        BlockNode::CodeBlock { .. } => {}
    }
}

fn collect_links_from_inlines(nodes: &[InlineNode], out: &mut BTreeSet<SharedString>) {
    for node in nodes {
        match node {
            InlineNode::Extension(node) if node.kind.as_ref() == "page-link" => {
                if let Some(name) = page_name(&node.source) {
                    out.insert(SharedString::from(name.to_string()));
                }
            }
            InlineNode::Link { children, .. } => collect_links_from_inlines(children, out),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_render::{render_block, BlockNode};
    use gpui::{
        point, Context, Entity, FocusHandle, Focusable, Modifiers, Render, TestAppContext,
        VisualTestContext,
    };
    use tempfile::TempDir;

    fn matches(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
        PageLinkExt
            .extract(text)
            .into_iter()
            .map(|matched| {
                let range = matched.range;
                let source = text[range.clone()].to_string();
                (range, source)
            })
            .collect()
    }

    fn extension_count(text: &str) -> usize {
        fn count(nodes: &[InlineNode]) -> usize {
            nodes
                .iter()
                .map(|node| match node {
                    InlineNode::Extension(_) => 1,
                    InlineNode::Link { children, .. } => count(children),
                    _ => 0,
                })
                .sum()
        }

        let ext = PageLinkExt;
        let extensions: [&dyn InlineExtension; 1] = [&ext];
        lower(text, &extensions)
            .iter()
            .map(|block| match block {
                BlockNode::Paragraph(children) | BlockNode::Heading { children, .. } => {
                    count(children)
                }
                BlockNode::Quote(_) | BlockNode::CodeBlock { .. } => 0,
            })
            .sum()
    }

    #[test]
    fn extracts_page_link_syntax() {
        assert_eq!(matches("[[Page]]"), vec![(0..8, "[[Page]]".into())]);
        assert_eq!(
            matches("see [[One]] and [[Two Words]] now"),
            vec![(4..11, "[[One]]".into()), (16..29, "[[Two Words]]".into())],
        );
        assert_eq!(page_name("[[  Trim Me  ]]"), Some("Trim Me"));
    }

    #[test]
    fn rejects_malformed_or_empty_page_links() {
        for text in ["[ [foo] ]", "[[foo\nbar]]", "[[]]", "[[   ]]", "[[a[b]]"] {
            assert!(matches(text).is_empty(), "unexpected match in {text:?}");
        }
    }

    #[test]
    fn lower_recognizes_link_across_split_markdown_text_events() {
        assert_eq!(extension_count("before [[Page]] after"), 1);
    }

    #[test]
    fn lower_does_not_extract_page_links_from_code() {
        assert_eq!(extension_count("see `[[Code]]`"), 0);
        assert_eq!(extension_count("```txt\n[[Code]]\n```\n"), 0);
        assert_eq!(extension_count("[label](https://example.test/page)"), 0);
        assert_eq!(
            extension_count("[label](https://example.test/[[Hidden]])"),
            0,
        );
    }

    #[test]
    fn reindex_adds_and_removes_forward_and_reverse_links() {
        let mut index = LinkIndex::default();
        let first = Outline::parse("- [[B]] and [[C]]\n- another [[B]]\n");
        index.reindex_outline(&"A".into(), &first);

        assert_eq!(index.targets("A").len(), 2);
        assert_eq!(index.backlinks("B").len(), 2);
        assert_eq!(index.backlinks("C").len(), 1);
        assert!(index.page_exists("A"));
        assert!(!index.page_exists("a"));
        assert!(!index.page_exists("B"));

        let second = Outline::parse("- only [[C]]\n");
        index.reindex_outline(&"A".into(), &second);
        assert!(index.backlinks("B").is_empty());
        assert_eq!(index.backlinks("C").len(), 1);
        assert_eq!(index.targets("A").len(), 1);
    }

    #[test]
    fn rebuild_from_indexes_cross_references_and_existing_pages() {
        let tmp = TempDir::new().unwrap();
        let store = NotesStore::new(tmp.path()).unwrap();
        store.write("A", "- [[B]] [[C]]\n- [[B]]\n").unwrap();
        store.write("B", "- [[A]] [[C]]\n").unwrap();
        store.write("C", "- [[A]]\n").unwrap();

        let index = LinkIndex::rebuild_from(&store).unwrap();
        assert_eq!(index.backlinks("A").len(), 2);
        assert_eq!(index.backlinks("B").len(), 2);
        assert_eq!(index.backlinks("C").len(), 2);
        assert_eq!(index.targets("A").len(), 2);
        assert!(index.page_exists("A"));
        assert!(index.page_exists("B"));
        assert!(index.page_exists("C"));
        assert!(!index.page_exists("Missing"));
    }

    #[derive(Default)]
    struct OpenPageRecorder {
        opened: Option<SharedString>,
    }

    impl Global for OpenPageRecorder {}

    struct HostView {
        text: SharedString,
        focus_handle: FocusHandle,
    }

    impl HostView {
        fn new(text: SharedString, cx: &mut Context<Self>) -> Self {
            Self {
                text,
                focus_handle: cx.focus_handle(),
            }
        }
    }

    impl Focusable for HostView {
        fn focus_handle(&self, _: &App) -> FocusHandle {
            self.focus_handle.clone()
        }
    }

    impl Render for HostView {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let ext = PageLinkExt;
            let extensions: [&dyn InlineExtension; 1] = [&ext];
            div()
                .track_focus(&self.focus_handle)
                .key_context("PageLinkHost")
                .on_action(|action: &OpenPage, _window, cx| {
                    cx.update_global::<OpenPageRecorder, _>(|recorder, _| {
                        recorder.opened = Some(action.name.clone());
                    });
                })
                .size_full()
                .child(render_block(&self.text, &extensions, window, cx))
        }
    }

    fn mount_host<'a>(
        cx: &'a mut TestAppContext,
        text: &str,
    ) -> (Entity<HostView>, &'a mut VisualTestContext) {
        let text = SharedString::from(text.to_string());
        let (view, visual) = cx.add_window_view(move |_window, cx| {
            cx.set_global(LinkIndex::default());
            cx.set_global(OpenPageRecorder::default());
            HostView::new(text, cx)
        });
        visual.update(|window, cx| {
            window.activate_window();
            let handle = view.read(cx).focus_handle.clone();
            window.focus(&handle, cx);
        });
        visual.run_until_parked();
        (view, visual)
    }

    #[gpui::test]
    fn clicking_rendered_page_link_dispatches_open_page(cx: &mut TestAppContext) {
        let (_view, cx) = mount_host(cx, "[[Alpha]] trailing text");

        cx.simulate_click(point(gpui::px(8.0), gpui::px(10.0)), Modifiers::default());
        cx.run_until_parked();

        cx.read(|cx| {
            assert_eq!(
                cx.global::<OpenPageRecorder>()
                    .opened
                    .as_ref()
                    .map(AsRef::as_ref),
                Some("Alpha"),
            );
        });
    }
}
