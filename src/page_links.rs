//! `[[Page Name]]` links and the forward/reverse index that backs them.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io;
use std::sync::LazyLock;

use gpui::{App, BorrowAppContext, Global, SharedString};
use regex::Regex;

use crate::block_render::{
    lower, theme, BlockNode, ExtensionMatch, ExtensionNode, InlineExtension, InlineNode, InlineRun,
};
use crate::journal;
use crate::outline::{Block, BlockId, Outline};
use crate::page::Page;
use crate::source::NoteSource;
use crate::store::NotesStore;

static PAGE_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\[\]\n]+?)\]\]").expect("page-link regex is valid"));

static EMPTY_BACKLINKS: LazyLock<BTreeSet<NoteSource>> = LazyLock::new(BTreeSet::new);
static EMPTY_TARGETS: LazyLock<HashSet<SharedString>> = LazyLock::new(HashSet::new);

#[derive(Clone, Debug, PartialEq, gpui::Action)]
#[action(namespace = gpui_notes, no_json)]
pub struct OpenPage {
    pub name: SharedString,
}

/// Forward and reverse page-link lookups.
///
/// Page names are case-preserving and case-sensitive. The separate `pages`
/// set backs the `[[` completion list without touching disk.
///
/// The reverse map records only *which* sources link to a target, never the
/// referencing `BlockId`s: ids minted by `rebuild_from`'s throwaway
/// `Outline::parse` do not survive into the `Page` that is opened later, so
/// the linked-references UI resolves them from the live page instead
/// (`blocks_linking_to`).
#[derive(Default)]
pub struct LinkIndex {
    reverse: HashMap<String, BTreeSet<NoteSource>>,
    /// Keyed by `NoteSource` rather than by display name so that a page named
    /// `2026-07-23` cannot purge the reverse entries of that date's journal.
    forward: BTreeMap<NoteSource, HashSet<SharedString>>,
    pages: HashSet<String>,
}

impl Global for LinkIndex {}

impl LinkIndex {
    /// Replaces all indexed links for `page` with its current outline.
    pub fn reindex_page(&mut self, page: &Page, source: &NoteSource) {
        self.reindex_outline(source, page.outline());
    }

    /// Returns the sources that link to `target`.
    #[must_use]
    pub fn backlinks(&self, target: &str) -> &BTreeSet<NoteSource> {
        self.reverse.get(target).unwrap_or(&EMPTY_BACKLINKS)
    }

    /// Returns the unique page names referenced by `source`.
    #[must_use]
    pub fn targets(&self, source: &NoteSource) -> &HashSet<SharedString> {
        self.forward.get(source).unwrap_or(&EMPTY_TARGETS)
    }

    /// Every `[[target]]` linked from at least one page or journal, whether
    /// or not a file exists for it.
    pub fn link_targets(&self) -> impl Iterator<Item = &str> {
        self.reverse.keys().map(String::as_str)
    }

    #[must_use]
    pub fn page_exists(&self, name: &str) -> bool {
        self.pages.contains(name)
    }

    /// Every page name the index knows about — pages on disk plus link
    /// targets that are not journal titles — sorted case-insensitively.
    /// Backs the `[[` completion popup, which needs the list on every
    /// keystroke and so cannot afford to re-read the notes directory.
    #[must_use]
    pub fn page_names(&self) -> Vec<SharedString> {
        let linked = self
            .link_targets()
            .filter(|target| journal::parse_journal_name(target).is_none());
        let mut names: Vec<SharedString> = self
            .pages
            .iter()
            .map(String::as_str)
            .chain(linked)
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|name| SharedString::from(name.to_string()))
            .collect();
        names.sort_by_key(|name| name.to_lowercase());
        names
    }

    /// Builds an index from every page and journal currently on disk.
    ///
    /// Journals are indexed because they are where most references are
    /// written; they are deliberately kept out of `pages`, which drives the
    /// `[[` completion list.
    ///
    /// A file that fails to read (not UTF-8, no permission) is left out of
    /// the index and returned with its error instead, so one bad file cannot
    /// keep the app from starting (#67).
    ///
    /// # Errors
    /// Returns an I/O error if the pages or journals directory cannot be listed.
    pub fn rebuild_from(store: &NotesStore) -> io::Result<(Self, Vec<(NoteSource, io::Error)>)> {
        let mut index = Self::default();
        let mut skipped = Vec::new();
        for name in store.list()? {
            let source = NoteSource::Page(SharedString::from(name.clone()));
            match store.read(&name) {
                Ok(body) => index.reindex_outline(&source, &Outline::parse(&body)),
                Err(err) => skipped.push((source, err)),
            }
        }
        for date in store.list_journals()? {
            let source = NoteSource::Journal(date);
            match store.read_journal(date) {
                Ok(body) => index.reindex_outline(&source, &Outline::parse(&body)),
                Err(err) => skipped.push((source, err)),
            }
        }
        Ok((index, skipped))
    }

    fn reindex_outline(&mut self, source: &NoteSource, outline: &Outline) {
        // The forward map identifies exactly which reverse entries can contain
        // this source, avoiding a sweep over unrelated targets.
        if let Some(old_targets) = self.forward.remove(source) {
            for target in old_targets {
                let target = target.to_string();
                let remove_entry = self.reverse.get_mut(&target).is_some_and(|backlinks| {
                    backlinks.remove(source);
                    backlinks.is_empty()
                });
                if remove_entry {
                    self.reverse.remove(&target);
                }
            }
        }

        let mut targets = HashSet::new();
        for block in outline.preorder() {
            for target in page_links_in_text(&block.text) {
                targets.insert(target.clone());
                self.reverse
                    .entry(target.to_string())
                    .or_default()
                    .insert(source.clone());
            }
        }

        if let NoteSource::Page(name) = source {
            self.pages.insert(name.to_string());
        }
        self.forward.insert(source.clone(), targets);
    }
}

/// Ids of the blocks in `outline` whose text links to `target`, in document
/// order. Resolved against a live outline, so the ids are always current.
///
/// A hit's descendants are not searched: each id becomes a subtree row in
/// Linked References, so a linking descendant is already on screen (#69).
#[must_use]
pub fn blocks_linking_to(outline: &Outline, target: &str) -> Vec<BlockId> {
    fn walk(out: &mut Vec<BlockId>, blocks: &[Block], target: &str) {
        for block in blocks {
            // A block with no `[[` cannot link anywhere, and the lowering
            // below is a full markdown parse — by far the expensive part when
            // a page has hundreds of referencing sources. Survivors still
            // have to be parsed: a `[[Target]]` inside a ``` fence is not a
            // reference.
            let links = block.text.contains("[[")
                && page_links_in_text(&block.text)
                    .iter()
                    .any(|link| link.as_ref() == target);
            if links {
                out.push(block.id);
            } else {
                walk(out, &block.children, target);
            }
        }
    }

    let mut out = Vec::new();
    walk(&mut out, &outline.roots, target);
    out
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

    fn run(&self, node: &ExtensionNode, _cx: &mut App) -> InlineRun {
        let Some(name) = page_name(&node.source) else {
            return InlineRun::new(node.source.clone());
        };
        let name = SharedString::from(name.to_string());

        // Styled like Logseq: blue, with the brackets shown faintly. A page
        // that does not exist yet looks the same — following the link is how
        // it gets created.
        InlineRun {
            color: Some(theme::accent()),
            delimiters: Some(("[[", "]]")),
            action: Some(Box::new(OpenPage { name: name.clone() })),
            ..InlineRun::new(name)
        }
    }
}

/// What `[[name]]` opens. A journal title (`[[2026-10-09]]`) opens that date's
/// journal, as in Logseq; anything else is a regular page.
#[must_use]
pub fn link_target(name: &str) -> NoteSource {
    journal::parse_journal_name(name).map_or_else(
        || NoteSource::Page(SharedString::from(name.to_string())),
        NoteSource::Journal,
    )
}

/// Refreshes the global index after a page or journal is opened, created, or saved.
pub fn reindex_global_for_page(page: &gpui::Entity<Page>, source: &NoteSource, cx: &mut App) {
    if !cx.has_global::<LinkIndex>() {
        return;
    }
    let outline = page.read(cx).outline().clone();
    cx.update_global::<LinkIndex, ()>(|index, _| {
        index.reindex_outline(source, &outline);
    });
}

fn page_name(source: &str) -> Option<&str> {
    let name = source.strip_prefix("[[")?.strip_suffix("]]")?.trim();
    (!name.is_empty()).then_some(name)
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
    use chrono::NaiveDate;
    use gpui::{
        div, point, Context, Entity, FocusHandle, Focusable, Hsla, InteractiveElement, IntoElement,
        Modifiers, ParentElement, Render, Styled, TestAppContext, VisualTestContext, Window,
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

    fn page(name: &str) -> NoteSource {
        NoteSource::Page(SharedString::from(name.to_string()))
    }

    #[test]
    fn reindex_adds_and_removes_forward_and_reverse_links() {
        let mut index = LinkIndex::default();
        let first = Outline::parse("- [[B]] and [[C]]\n- another [[B]]\n");
        index.reindex_outline(&page("A"), &first);

        assert_eq!(index.targets(&page("A")).len(), 2);
        // Two blocks link to B, but a source is listed once.
        assert_eq!(
            index.backlinks("B").iter().collect::<Vec<_>>(),
            [&page("A")]
        );
        assert_eq!(index.backlinks("C").len(), 1);
        assert!(index.page_exists("A"));
        assert!(!index.page_exists("a"));
        assert!(!index.page_exists("B"));

        let second = Outline::parse("- only [[C]]\n");
        index.reindex_outline(&page("A"), &second);
        assert!(index.backlinks("B").is_empty());
        assert_eq!(index.backlinks("C").len(), 1);
        assert_eq!(index.targets(&page("A")).len(), 1);
    }

    #[test]
    fn rebuild_from_indexes_cross_references_and_existing_pages() {
        let tmp = TempDir::new().unwrap();
        let store = NotesStore::new(tmp.path()).unwrap();
        store.write("A", "- [[B]] [[C]]\n- [[B]]\n").unwrap();
        store.write("B", "- [[A]] [[C]]\n").unwrap();
        store.write("C", "- [[A]]\n").unwrap();

        let index = LinkIndex::rebuild_from(&store).unwrap().0;
        assert_eq!(index.backlinks("A").len(), 2);
        assert_eq!(
            index.backlinks("B").iter().collect::<Vec<_>>(),
            [&page("A")]
        );
        assert_eq!(index.backlinks("C").len(), 2);
        assert_eq!(index.targets(&page("A")).len(), 2);
        assert!(index.page_exists("A"));
        assert!(index.page_exists("B"));
        assert!(index.page_exists("C"));
        assert!(!index.page_exists("Missing"));
    }

    #[test]
    fn rebuild_from_skips_unreadable_files_instead_of_failing() {
        let tmp = TempDir::new().unwrap();
        let store = NotesStore::new(tmp.path()).unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 7, 23).unwrap();
        store.write("Good", "- [[Target]]\n").unwrap();
        store.write("Bad", "").unwrap();
        store.write_journal(date, "").unwrap();
        // Not valid UTF-8, so `read_to_string` fails.
        std::fs::write(tmp.path().join("pages/Bad.md"), [0xff, 0xfe]).unwrap();
        std::fs::write(tmp.path().join("journals/2026_07_23.md"), [0xff]).unwrap();

        let (index, skipped) = LinkIndex::rebuild_from(&store).unwrap();
        let skipped: Vec<&NoteSource> = skipped.iter().map(|(source, _)| source).collect();
        assert_eq!(skipped, [&page("Bad"), &NoteSource::Journal(date)]);
        assert_eq!(
            index.backlinks("Target").iter().collect::<Vec<_>>(),
            [&page("Good")],
            "readable files must still be indexed",
        );
    }

    #[test]
    fn rebuild_from_indexes_journals_without_listing_them_as_pages() {
        let tmp = TempDir::new().unwrap();
        let store = NotesStore::new(tmp.path()).unwrap();
        let date = NaiveDate::from_ymd_opt(2026, 7, 23).unwrap();
        store.write("Target", "- the page itself\n").unwrap();
        store
            .write_journal(date, "- [[NSSP]] issue [[Target]] on [[2026-07-01]]\n")
            .unwrap();

        let index = LinkIndex::rebuild_from(&store).unwrap().0;
        assert_eq!(
            index.backlinks("Target").iter().collect::<Vec<_>>(),
            [&NoteSource::Journal(date)],
            "a journal that links a page must be a backlink source",
        );
        assert_eq!(index.targets(&NoteSource::Journal(date)).len(), 3);

        // Journals must not leak into the page set: it drives the `[[`
        // completion list. A page that is only ever linked (no file, like
        // `NSSP`) is offered, as Logseq does; a journal title is not.
        assert!(!index.page_exists("2026-07-23"));
        assert_eq!(
            index.page_names(),
            vec![SharedString::from("NSSP"), SharedString::from("Target")],
        );
    }

    #[test]
    fn same_named_page_and_journal_do_not_purge_each_other() {
        let mut index = LinkIndex::default();
        let date = NaiveDate::from_ymd_opt(2026, 7, 23).unwrap();
        index.reindex_outline(&page("2026-07-23"), &Outline::parse("- [[Target]]\n"));
        index.reindex_outline(
            &NoteSource::Journal(date),
            &Outline::parse("- also [[Target]]\n"),
        );

        assert_eq!(index.backlinks("Target").len(), 2);
    }

    #[test]
    fn blocks_linking_to_finds_nested_blocks_and_skips_code() {
        let outline = Outline::parse(
            "- intro [[Target]]\n\t- child [[Other]]\n\t\t- deep [[Target]]\n- plain\n\t- nested [[Target]]\n- ```txt\n  [[Target]]\n  ```\n",
        );
        let ids = blocks_linking_to(&outline, "Target");
        let texts: Vec<&str> = ids
            .iter()
            .map(|id| outline.get(*id).unwrap_or_default())
            .collect();

        // `deep` is inside `intro`'s subtree, which its row already shows
        // (#69); the fenced link is not a reference at all.
        assert_eq!(texts.len(), 2, "{texts:?}");
        assert!(texts[0].starts_with("intro"));
        assert!(texts[1].starts_with("nested"));
        assert_eq!(blocks_linking_to(&outline, "Missing"), []);
    }

    #[test]
    fn journal_title_links_target_the_journal() {
        let date = NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        assert_eq!(link_target("2026-10-09"), NoteSource::Journal(date));
        // Only Logseq's title form; the on-disk `2026_10_09` is a page name.
        assert_eq!(link_target("2026_10_09"), page("2026_10_09"));
        assert_eq!(link_target("Target"), page("Target"));
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
    fn page_link_renders_blue_between_dim_brackets(cx: &mut TestAppContext) {
        let (_view, cx) = mount_host(cx, "[[Alpha]]");
        cx.run_until_parked();

        let runs = cx.update(|_, _| crate::block_render::take_recorded_runs());
        let styled: Vec<(usize, Hsla)> = runs.iter().map(|r| (r.len, r.color)).collect();
        let (dim, accent) = (theme::link_bracket().into(), theme::accent().into());
        assert!(
            styled.ends_with(&[(2, dim), (5, accent), (2, dim)]),
            "{styled:?}"
        );
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
