use std::collections::BTreeMap;
use std::io;
use std::sync::LazyLock;

use chrono::NaiveDate;
use gpui::{
    div, px, App, BorrowAppContext, ElementId, Global, InteractiveElement, IntoElement,
    MouseButton, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window,
};
use regex::Regex;

use crate::block_render::{
    lower, theme, BlockNode, ExtensionMatch, ExtensionNode, InlineExtension, InlineNode,
};
use crate::outline::{Block, BlockId, Outline};
use crate::page::Page;
use crate::store::NotesStore;

static TAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s(,.:;!?\-])#([A-Za-z0-9_][A-Za-z0-9_\-/]*)").expect("tag regex is valid")
});

#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = gpui_notes, no_json)]
pub struct OpenTag {
    pub name: SharedString,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TagSource {
    Page(SharedString),
    Journal(NaiveDate),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct TagHit {
    pub source: TagSource,
    pub block_id: BlockId,
    pub preview: SharedString,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TagSummary {
    pub display: SharedString,
    pub count: usize,
}

#[derive(Clone, Debug)]
struct IndexedHit {
    hit: TagHit,
    display: SharedString,
    order: u64,
}

#[derive(Clone, Debug, Default)]
struct TagEntry {
    hits: BTreeMap<(TagSource, BlockId), IndexedHit>,
}

#[derive(Default)]
pub struct TagIndex {
    entries: BTreeMap<String, TagEntry>,
    next_order: u64,
}

impl Global for TagIndex {}

impl TagIndex {
    #[must_use]
    pub fn blocks_for_tag(&self, tag: &str) -> Vec<TagHit> {
        let key = tag_key(tag);
        let Some(entry) = self.entries.get(&key) else {
            return Vec::new();
        };
        let mut hits: Vec<&IndexedHit> = entry.hits.values().collect();
        hits.sort_by_key(|hit| hit.order);
        hits.into_iter().map(|hit| hit.hit.clone()).collect()
    }

    #[must_use]
    pub fn all_tags(&self) -> Vec<TagSummary> {
        let mut summaries: Vec<TagSummary> = self
            .entries
            .values()
            .filter_map(|entry| {
                let display = entry
                    .hits
                    .values()
                    .min_by_key(|hit| hit.order)
                    .map(|hit| hit.display.clone())?;
                Some(TagSummary {
                    display,
                    count: entry.hits.len(),
                })
            })
            .collect();
        summaries.sort_by_key(|summary| summary.display.to_ascii_lowercase());
        summaries
    }

    pub fn reindex_page(&mut self, source: TagSource, outline: &Outline) {
        self.remove_source(&source);

        for block in all_blocks(&outline.roots) {
            let tags = tags_in_text(&block.text);
            if tags.is_empty() {
                continue;
            }
            for (key, display) in tags {
                let entry = self.entries.entry(key).or_default();
                let hit_key = (source.clone(), block.id);
                entry.hits.entry(hit_key).or_insert_with(|| {
                    let order = self.next_order;
                    self.next_order += 1;
                    IndexedHit {
                        hit: TagHit {
                            source: source.clone(),
                            block_id: block.id,
                            preview: SharedString::from(block.text.clone()),
                        },
                        display,
                        order,
                    }
                });
            }
        }
    }

    /// # Errors
    /// Returns the first I/O error from listing or reading pages/journals.
    pub fn rebuild_from(store: &NotesStore) -> io::Result<Self> {
        let mut index = Self::default();
        for name in store.list()? {
            let body = store.read(&name)?;
            let outline = Outline::parse(&body);
            index.reindex_page(TagSource::Page(SharedString::from(name)), &outline);
        }
        for date in store.list_journals()? {
            let body = store.read_journal(date)?;
            let outline = Outline::parse(&body);
            index.reindex_page(TagSource::Journal(date), &outline);
        }
        Ok(index)
    }

    fn remove_source(&mut self, source: &TagSource) {
        self.entries.retain(|_, entry| {
            entry.hits.retain(|(hit_source, _), _| hit_source != source);
            !entry.hits.is_empty()
        });
    }
}

pub struct TagExt;

impl InlineExtension for TagExt {
    fn extract(&self, text: &str) -> Vec<ExtensionMatch> {
        TAG_RE
            .captures_iter(text)
            .filter_map(|captures| {
                let body = captures.get(1)?;
                let start = body.start().checked_sub(1)?;
                Some(ExtensionMatch {
                    range: start..body.end(),
                    kind: "tag".into(),
                })
            })
            .collect()
    }

    fn render(
        &self,
        node: &ExtensionNode,
        _window: &mut Window,
        _cx: &mut App,
    ) -> gpui::AnyElement {
        let display = node.source.trim_start_matches('#').to_string();
        let tag_name = SharedString::from(display.clone());
        div()
            .id(ElementId::Name(SharedString::from(format!(
                "tag-{display}"
            ))))
            .cursor_pointer()
            .bg(theme::bg_subtle())
            .text_color(theme::accent())
            .border_1()
            .border_color(theme::accent())
            .rounded_sm()
            .px_1()
            .py_0p5()
            .min_h(px(18.0))
            .min_w(px(24.0))
            .child(SharedString::from(display))
            .on_mouse_down(MouseButton::Left, |_, window, cx| {
                window.prevent_default();
                cx.stop_propagation();
            })
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                window.dispatch_action(
                    Box::new(OpenTag {
                        name: tag_name.clone(),
                    }),
                    cx,
                );
            })
            .into_any_element()
    }
}

pub fn reindex_global_for_page(page: &gpui::Entity<Page>, source: TagSource, cx: &mut App) {
    if !cx.has_global::<TagIndex>() {
        return;
    }
    let outline = page.read(cx).outline().clone();
    cx.update_global::<TagIndex, ()>(|index, _| {
        index.reindex_page(source, &outline);
    });
}

#[must_use]
pub fn tag_key(tag: &str) -> String {
    tag.trim_start_matches('#').to_ascii_lowercase()
}

#[must_use]
pub fn source_label(source: &TagSource) -> SharedString {
    match source {
        TagSource::Page(name) => name.clone(),
        TagSource::Journal(date) => SharedString::from(date.format("%Y-%m-%d").to_string()),
    }
}

#[must_use]
pub fn truncated_preview(preview: &str) -> String {
    let mut out: String = preview.chars().take(80).collect();
    if preview.chars().count() > 80 {
        out.push_str("...");
    }
    out
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

fn tags_in_text(text: &str) -> BTreeMap<String, SharedString> {
    let ext = TagExt;
    let extensions: [&dyn InlineExtension; 1] = [&ext];
    let blocks = lower(text, &extensions);
    let mut tags = BTreeMap::new();
    for block in blocks {
        collect_tags_from_block(&block, &mut tags);
    }
    tags
}

fn collect_tags_from_block(block: &BlockNode, out: &mut BTreeMap<String, SharedString>) {
    match block {
        BlockNode::Paragraph(children) | BlockNode::Heading { children, .. } => {
            collect_tags_from_inlines(children, out);
        }
        BlockNode::Quote(children) => {
            for child in children {
                collect_tags_from_block(child, out);
            }
        }
        BlockNode::CodeBlock { .. } => {}
    }
}

fn collect_tags_from_inlines(nodes: &[InlineNode], out: &mut BTreeMap<String, SharedString>) {
    for node in nodes {
        match node {
            InlineNode::Extension(node) if node.kind.as_ref() == "tag" => {
                let display = node.source.trim_start_matches('#').to_string();
                out.entry(display.to_ascii_lowercase())
                    .or_insert_with(|| SharedString::from(display));
            }
            InlineNode::Link { children, .. } => collect_tags_from_inlines(children, out),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block_render::{lower, render_block, BlockNode};
    use gpui::{
        point, Context, Entity, FocusHandle, Focusable, Modifiers, Render, TestAppContext,
        VisualTestContext,
    };
    use tempfile::TempDir;

    fn ranges(text: &str) -> Vec<(std::ops::Range<usize>, String)> {
        TagExt
            .extract(text)
            .into_iter()
            .map(|m| (m.range.clone(), text[m.range].to_string()))
            .collect()
    }

    fn extension_count(text: &str) -> usize {
        let ext = TagExt;
        let extensions: [&dyn InlineExtension; 1] = [&ext];
        fn count_inlines(nodes: &[InlineNode]) -> usize {
            nodes
                .iter()
                .map(|node| match node {
                    InlineNode::Extension(_) => 1,
                    InlineNode::Link { children, .. } => count_inlines(children),
                    _ => 0,
                })
                .sum()
        }
        lower(text, &extensions)
            .iter()
            .map(|block| match block {
                BlockNode::Paragraph(children) | BlockNode::Heading { children, .. } => {
                    count_inlines(children)
                }
                BlockNode::Quote(_) | BlockNode::CodeBlock { .. } => 0,
            })
            .sum()
    }

    #[test]
    fn tag_ext_extracts_expected_tags() {
        assert_eq!(ranges("#rust"), vec![(0..5, "#rust".into())]);
        assert_eq!(
            ranges("see #proj/sub-task"),
            vec![(4..18, "#proj/sub-task".into())],
        );
        assert_eq!(
            ranges("foo#bar"),
            Vec::<(std::ops::Range<usize>, String)>::new()
        );
        assert_eq!(ranges("#"), Vec::<(std::ops::Range<usize>, String)>::new());
        assert_eq!(
            ranges("#a #b,#c"),
            vec![
                (0..2, "#a".into()),
                (3..5, "#b".into()),
                (6..8, "#c".into()),
            ],
        );
        assert_eq!(ranges("##"), Vec::<(std::ops::Range<usize>, String)>::new());
        assert_eq!(ranges("é #tag"), vec![(3..7, "#tag".into())]);
    }

    #[test]
    fn lower_does_not_extract_tags_from_code_or_urls() {
        assert_eq!(extension_count("see `#code`"), 0);
        assert_eq!(extension_count("```txt\n#code\n```\n"), 0);
        assert_eq!(extension_count("# heading"), 0);
        assert_eq!(extension_count("https://example.test/#frag"), 0);
    }

    #[test]
    fn index_rebuilds_pages_and_journals_case_insensitively() {
        let tmp = TempDir::new().unwrap();
        let store = NotesStore::new(tmp.path()).unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 4, 18).unwrap();
        store
            .write("PageA", "- one #Rust #rust\n- two #work\n")
            .unwrap();
        store.write_journal(date, "- today #RUST\n").unwrap();

        let index = TagIndex::rebuild_from(&store).unwrap();
        let rust = index.blocks_for_tag("#rust");
        assert_eq!(rust.len(), 2, "duplicate tags in one block count once");
        assert_eq!(index.blocks_for_tag("RUST"), rust);

        let summaries = index.all_tags();
        assert_eq!(
            summaries
                .iter()
                .find(|summary| summary.display.as_ref() == "Rust")
                .map(|summary| summary.count),
            Some(2),
        );
        assert!(rust
            .iter()
            .any(|hit| matches!(hit.source, TagSource::Journal(d) if d == date)));
    }

    #[test]
    fn reindex_removes_stale_tags() {
        let mut index = TagIndex::default();
        let source = TagSource::Page("PageA".into());
        let old = Outline::parse("- old #gone\n- keep #stay\n");
        index.reindex_page(source.clone(), &old);
        assert_eq!(index.blocks_for_tag("gone").len(), 1);

        let new = Outline::parse("- keep #stay\n");
        index.reindex_page(source, &new);
        assert!(index.blocks_for_tag("gone").is_empty());
        assert_eq!(index.blocks_for_tag("stay").len(), 1);
    }

    #[derive(Default)]
    struct OpenTagRecorder {
        opened: Option<SharedString>,
    }
    impl Global for OpenTagRecorder {}

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
            let tag_ext = TagExt;
            let extensions: [&dyn InlineExtension; 1] = [&tag_ext];
            div()
                .track_focus(&self.focus_handle)
                .key_context("TagHost")
                .on_action(|action: &OpenTag, _window, cx| {
                    cx.update_global::<OpenTagRecorder, _>(|recorder, _| {
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
        let (view, vcx) = cx.add_window_view(move |_window, cx| {
            cx.set_global(OpenTagRecorder::default());
            HostView::new(text, cx)
        });
        vcx.update(|window, cx| {
            window.activate_window();
            let handle = view.read(cx).focus_handle.clone();
            window.focus(&handle, cx);
        });
        vcx.run_until_parked();
        (view, vcx)
    }

    #[gpui::test]
    fn clicking_rendered_tag_dispatches_open_tag(cx: &mut TestAppContext) {
        let (_view, cx) = mount_host(cx, "#alpha alpha alpha");

        cx.simulate_click(point(px(8.0), px(10.0)), Modifiers::default());
        cx.run_until_parked();

        cx.read(|cx| {
            assert_eq!(
                cx.global::<OpenTagRecorder>()
                    .opened
                    .as_ref()
                    .map(AsRef::as_ref),
                Some("alpha"),
            );
        });
    }
}
