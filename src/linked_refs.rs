//! The "N Linked References" section shown beneath a page's own blocks.
//!
//! Every block elsewhere in the vault that contains a `[[This Page]]` link is
//! listed here together with its descendants, grouped under the page or
//! journal it came from. The rows are real `OutlineView`s scoped to that one
//! subtree (`OutlineView::subtree`), so a reference is editable in place and
//! the edit lands in the *source* page.
//!
//! `LinkIndex` only records which sources reference a target; the referencing
//! block ids are resolved here from the live `Page`, which is what keeps them
//! from going stale (see `page_links::blocks_linking_to`).
//!
//! Resolving them is expensive (it opens and walks every referencing page), so
//! the grouping is *cached* and rebuilt only when it can have changed: the
//! `LinkIndex` observer covers the set of sources, and one observer per opened
//! source page covers the blocks within that source. `render` itself only
//! walks the cache — it runs on every keystroke anywhere in the window.

use std::collections::{HashMap, HashSet};
use std::io;

use gpui::{
    div, px, AppContext, BorrowAppContext, Context, ElementId, Entity, InteractiveElement,
    IntoElement, MouseButton, ParentElement, Render, SharedString, Styled, Subscription, Window,
};

use crate::block_render::theme;
use crate::errors;
use crate::history;
use crate::outline::BlockId;
use crate::outline_view::OutlineView;
use crate::page::Page;
use crate::page_links::{self, LinkIndex};
use crate::registry::PageRegistry;
use crate::tags::{source_label, TagSource};

/// How many references are mounted at once, and how many more each "Show
/// more" click adds. Every reference is a live `OutlineView` plus a
/// `BlockView` per block and descendant, so an uncapped section on a
/// heavily-referenced page mounts hundreds of views that all re-render
/// together.
const REFS_PER_PAGE: usize = 20;

pub struct LinkedRefsView {
    page: Entity<Page>,
    /// One subtree view per *visible* referencing block, kept across renders
    /// so an in-progress edit survives. Pruned to what is on screen.
    refs: HashMap<(TagSource, BlockId), Entity<OutlineView>>,
    /// The full grouping, in display order. Rebuilt only when invalidated.
    groups: Vec<(TagSource, Vec<BlockId>)>,
    /// Per-source referencing block ids, so invalidating one source (an edit
    /// in a reference lands in *its* page) does not re-walk the whole vault.
    /// A source that failed to open caches an empty list, which is also what
    /// stops it from re-reporting the error on every rebuild.
    blocks: HashMap<TagSource, Vec<BlockId>>,
    /// One observer per opened source page: its outline is the only thing
    /// that can change that source's block list.
    source_subs: HashMap<TagSource, Subscription>,
    needs_rebuild: bool,
    /// How many references are shown; grows by `REFS_PER_PAGE` per click.
    shown: usize,
    #[cfg(test)]
    rebuilds: usize,
    _link_index_sub: Option<Subscription>,
}

impl LinkedRefsView {
    pub fn new(page: Entity<Page>, cx: &mut Context<Self>) -> Self {
        // The index only decides *which* sources reference this page; the
        // blocks within a source are covered by its own page observer, so
        // this leaves the per-source cache alone.
        let sub = cx.has_global::<LinkIndex>().then(|| {
            cx.observe_global::<LinkIndex>(|this, cx| {
                this.needs_rebuild = true;
                cx.notify();
            })
        });
        Self {
            page,
            refs: HashMap::new(),
            groups: Vec::new(),
            blocks: HashMap::new(),
            source_subs: HashMap::new(),
            needs_rebuild: true,
            shown: REFS_PER_PAGE,
            #[cfg(test)]
            rebuilds: 0,
            _link_index_sub: sub,
        }
    }

    /// Every reference to this page, grouped and ordered for display —
    /// including the ones beyond the current `REFS_PER_PAGE` cap.
    #[must_use]
    pub fn groups(&self) -> &[(TagSource, Vec<BlockId>)] {
        &self.groups
    }

    /// How many times the grouping has been recomputed. Backs the regression
    /// test that editing the current page does not recompute it.
    #[cfg(test)]
    #[must_use]
    pub fn rebuild_count(&self) -> usize {
        self.rebuilds
    }

    /// Recomputes `groups` from the index, reusing every per-source block
    /// list that has not been invalidated.
    fn rebuild_groups(&mut self, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            self.rebuilds += 1;
        }

        let sources = self.ordered_sources(cx);
        let live: HashSet<TagSource> = sources.iter().cloned().collect();
        self.blocks.retain(|source, _| live.contains(source));
        self.source_subs.retain(|source, _| live.contains(source));

        self.groups = Vec::new();
        for source in sources {
            if !self.blocks.contains_key(&source) {
                let blocks = self.referencing_blocks(&source, cx);
                self.blocks.insert(source.clone(), blocks);
            }
            let blocks = self.blocks[&source].clone();
            if !blocks.is_empty() {
                self.groups.push((source, blocks));
            }
        }
        self.needs_rebuild = false;
    }

    /// The leading `shown` references, still grouped by source.
    fn visible_groups(&self) -> Vec<(TagSource, Vec<BlockId>)> {
        let mut budget = self.shown;
        let mut visible = Vec::new();
        for (source, blocks) in &self.groups {
            if budget == 0 {
                break;
            }
            let take = budget.min(blocks.len());
            visible.push((source.clone(), blocks[..take].to_vec()));
            budget -= take;
        }
        visible
    }

    /// The sources that reference this page, ordered for display: journals
    /// newest first, then pages alphabetically. `TagSource`'s derived `Ord`
    /// sorts the other way around, hence the explicit comparator.
    fn ordered_sources(&self, cx: &mut Context<Self>) -> Vec<TagSource> {
        let target = self.page.read(cx).name().clone();
        let own_source = {
            let page = self.page.clone();
            cx.update_global::<PageRegistry, TagSource>(|reg, cx| reg.source_for_page(&page, cx))
        };

        let mut sources: Vec<TagSource> = cx
            .global::<LinkIndex>()
            .backlinks(&target)
            .iter()
            .filter(|source| **source != own_source)
            .cloned()
            .collect();
        sources.sort_by(|a, b| match (a, b) {
            (TagSource::Journal(a), TagSource::Journal(b)) => b.cmp(a),
            (TagSource::Journal(_), TagSource::Page(_)) => std::cmp::Ordering::Less,
            (TagSource::Page(_), TagSource::Journal(_)) => std::cmp::Ordering::Greater,
            (TagSource::Page(a), TagSource::Page(b)) => a.cmp(b),
        });
        sources
    }

    fn open_source(source: &TagSource, cx: &mut Context<Self>) -> io::Result<Entity<Page>> {
        cx.update_global::<PageRegistry, _>(|reg, cx| match source {
            TagSource::Page(name) => reg.open(name.as_ref(), cx),
            TagSource::Journal(date) => reg.open_or_create_journal(*date, cx),
        })
    }

    /// Opens `source` through the registry and returns the ids of its blocks
    /// that link to this page, starting to observe the source page so an edit
    /// there — including one made in a reference row — refreshes the list.
    fn referencing_blocks(&mut self, source: &TagSource, cx: &mut Context<Self>) -> Vec<BlockId> {
        let target = self.page.read(cx).name().clone();
        match Self::open_source(source, cx) {
            Ok(page) => {
                self.watch_source(source, &page, cx);
                page_links::blocks_linking_to(page.read(cx).outline(), &target)
            }
            Err(err) => {
                errors::report(format!("open {}: {err}", source_label(source)), cx);
                Vec::new()
            }
        }
    }

    fn watch_source(&mut self, source: &TagSource, page: &Entity<Page>, cx: &mut Context<Self>) {
        if self.source_subs.contains_key(source) {
            return;
        }
        let invalidated = source.clone();
        let sub = cx.observe(page, move |this, _, cx| {
            this.blocks.remove(&invalidated);
            this.needs_rebuild = true;
            cx.notify();
        });
        self.source_subs.insert(source.clone(), sub);
    }

    fn ref_view(
        &mut self,
        source: &TagSource,
        block_id: BlockId,
        cx: &mut Context<Self>,
    ) -> Option<Entity<OutlineView>> {
        if let Some(existing) = self.refs.get(&(source.clone(), block_id)) {
            return Some(existing.clone());
        }
        let page = Self::open_source(source, cx).ok()?;
        let view = cx.new(|cx| OutlineView::subtree(page, block_id, cx));
        self.refs.insert((source.clone(), block_id), view.clone());
        Some(view)
    }

    fn render_show_more(remaining: usize, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("linked-refs-show-more")
            .w_full()
            .px_1()
            .rounded_sm()
            .cursor_pointer()
            .text_size(px(13.))
            .text_color(theme::accent())
            .hover(Styled::underline)
            .debug_selector(|| "linked-refs-show-more".to_string())
            .child(format!("Show {} more", remaining.min(REFS_PER_PAGE)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.shown += REFS_PER_PAGE;
                    cx.notify();
                }),
            )
    }

    fn render_source_label(source: &TagSource, cx: &mut Context<Self>) -> impl IntoElement {
        let label = source_label(source);
        let target = source.clone();
        div()
            .id(ElementId::Name(SharedString::from(format!(
                "linked-ref-source-{label}"
            ))))
            .w_full()
            .px_1()
            .rounded_sm()
            .cursor_pointer()
            .text_size(px(13.))
            .text_color(theme::accent())
            .hover(Styled::underline)
            .debug_selector({
                let label = label.clone();
                move || format!("linked-ref-source-{label}")
            })
            .child(label)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_, _, _, cx| {
                    if let Err(err) = history::navigate_to(&target, cx) {
                        errors::report(format!("open {}: {err}", source_label(&target)), cx);
                    }
                }),
            )
    }
}

impl Render for LinkedRefsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !cx.has_global::<LinkIndex>() || !cx.has_global::<PageRegistry>() {
            return div();
        }

        if self.needs_rebuild {
            self.rebuild_groups(cx);
        }
        let groups = self.visible_groups();

        // Drop views for references that have since been edited away or
        // scrolled out of the shown set, so a stale subtree cannot linger
        // with an open editor.
        let live: HashSet<(TagSource, BlockId)> = groups
            .iter()
            .flat_map(|(source, blocks)| blocks.iter().map(|id| (source.clone(), *id)))
            .collect();
        self.refs.retain(|key, _| live.contains(key));

        let count: usize = self.groups.iter().map(|(_, blocks)| blocks.len()).sum();
        if count == 0 {
            return div();
        }

        let mut section = div()
            .flex()
            .flex_col()
            .w_full()
            .gap_2()
            .pt_4()
            .mt_4()
            .border_t_1()
            .border_color(theme::overlay_border())
            .child(
                div()
                    .text_size(px(13.))
                    .text_color(theme::header_fg())
                    .child(format!("{count} Linked References")),
            );

        for (source, blocks) in groups {
            let mut group = div()
                .flex()
                .flex_col()
                .w_full()
                .gap_1()
                .p_2()
                .rounded_sm()
                .bg(theme::row_bg())
                .child(Self::render_source_label(&source, cx));
            for (nth, id) in blocks.into_iter().enumerate() {
                if let Some(view) = self.ref_view(&source, id, cx) {
                    let label = source_label(&source);
                    group = group.child(
                        // The wrapper exists to give each reference a stable
                        // name for tests; `debug_selector` compiles away
                        // outside them.
                        div()
                            .w_full()
                            .debug_selector(move || format!("linked-ref-block-{label}-{nth}"))
                            .child(view),
                    );
                }
            }
            section = section.child(group);
        }

        if count > self.shown {
            section = section.child(Self::render_show_more(count - self.shown, cx));
        }
        section
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::LastError;
    use crate::history::NavHistory;
    use crate::registry::{set_current_page, CurrentPage};
    use crate::store::NotesStore;
    use crate::tags::TagIndex;
    use crate::{block_view, text_input};
    use chrono::NaiveDate;
    use gpui::{point, size, Modifiers, TestAppContext, VisualTestContext};
    use tempfile::TempDir;

    const OLDER: (i32, u32, u32) = (2026, 7, 21);
    const NEWER: (i32, u32, u32) = (2026, 7, 23);

    fn date(ymd: (i32, u32, u32)) -> NaiveDate {
        NaiveDate::from_ymd_opt(ymd.0, ymd.1, ymd.2).expect("valid date")
    }

    /// Mounts a page-scoped `OutlineView` over `Target` with a fully wired
    /// registry and index, i.e. the same globals `main` sets up. Two journals
    /// and one page reference `Target`.
    fn mount<'a>(
        cx: &'a mut TestAppContext,
        tmp: &TempDir,
    ) -> (Entity<OutlineView>, &'a mut VisualTestContext) {
        let root = tmp.path().to_path_buf();
        let (ov, vcx) = cx.add_window_view(move |_window, cx| {
            text_input::bind_keys(cx);
            block_view::bind_keys(cx);
            crate::outline_view::bind_keys(cx);

            let store = NotesStore::new(&root).unwrap();
            store.write("Target", "- the page itself\n").unwrap();
            store.write("Other", "- unrelated\n").unwrap();
            store
                .write("Elsewhere", "- a page ref [[Target]]\n")
                .unwrap();
            store
                .write_journal(date(NEWER), "- issue [[Target]]\n\t- detail\n")
                .unwrap();
            store
                .write_journal(date(OLDER), "- earlier [[Target]]\n")
                .unwrap();

            cx.set_global(TagIndex::rebuild_from(&store).unwrap());
            cx.set_global(LinkIndex::rebuild_from(&store).unwrap());
            cx.set_global(PageRegistry::new(store));
            cx.set_global(CurrentPage::default());
            cx.set_global(LastError::default());
            cx.set_global(NavHistory::default());
            set_current_page("Target", cx).unwrap();

            let page = cx.global::<CurrentPage>().get().cloned().unwrap();
            OutlineView::new(page, cx)
        });
        vcx.update(|window, _| window.activate_window());
        vcx.run_until_parked();
        // The first render opens the referencing pages, which reindexes them
        // and schedules one more pass; settle before asserting.
        vcx.run_until_parked();
        (ov, vcx)
    }

    fn refs_view(ov: &Entity<OutlineView>, cx: &mut VisualTestContext) -> Entity<LinkedRefsView> {
        cx.read(|cx| {
            ov.read(cx)
                .linked_refs()
                .cloned()
                .expect("references section mounted")
        })
    }

    #[gpui::test]
    fn groups_references_journals_newest_first_then_pages(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (ov, cx) = mount(cx, &tmp);
        let refs = refs_view(&ov, cx);

        cx.read(|cx| {
            let groups = refs.read(cx).groups();
            let sources: Vec<TagSource> = groups.iter().map(|(s, _)| s.clone()).collect();
            assert_eq!(
                sources,
                vec![
                    TagSource::Journal(date(NEWER)),
                    TagSource::Journal(date(OLDER)),
                    TagSource::Page("Elsewhere".into()),
                ],
                "journals newest first, then pages",
            );
            assert!(
                groups.iter().all(|(_, blocks)| blocks.len() == 1),
                "one referencing block per source here",
            );
        });
    }

    #[gpui::test]
    fn reference_renders_the_block_and_its_children(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (ov, cx) = mount(cx, &tmp);
        let refs = refs_view(&ov, cx);

        cx.read(|cx| {
            let refs = refs.read(cx);
            let (source, blocks) = &refs.groups()[0];
            let subtree = refs
                .refs
                .get(&(source.clone(), blocks[0]))
                .expect("subtree view for the newest journal")
                .read(cx);
            // "issue [[Target]]" plus its "detail" child.
            assert!(subtree.block_view(blocks[0]).is_some());
            assert_eq!(
                subtree.rendered_block_count(),
                2,
                "the child of a referencing block is shown too",
            );
        });
    }

    #[gpui::test]
    fn editing_a_reference_writes_back_to_its_source_page(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (ov, cx) = mount(cx, &tmp);
        let refs = refs_view(&ov, cx);

        cx.read(|cx| {
            assert_eq!(refs.read(cx).groups()[0].0, TagSource::Journal(date(NEWER)));
        });

        // The production trigger is a click on the block, so drive the click.
        let bounds = cx
            .debug_bounds("linked-ref-block-2026-07-23-0")
            .expect("reference subtree painted");
        // Just right of the bullet on the first row: the block text, not the
        // empty space a `w_full` row leaves to its right.
        cx.simulate_click(bounds.origin + point(px(30.), px(8.)), Modifiers::none());
        cx.run_until_parked();
        cx.simulate_input("!");
        cx.run_until_parked();

        let journal = cx.update(|_, cx| {
            cx.update_global::<PageRegistry, _>(|reg, cx| {
                reg.open_or_create_journal(date(NEWER), cx).unwrap()
            })
        });
        cx.read(|cx| {
            let page = journal.read(cx);
            assert!(page.dirty(), "editing a reference dirties its source page");
            assert!(
                page.body().contains('!'),
                "edit must land in the source page: {:?}",
                page.body(),
            );
        });
    }

    #[gpui::test]
    fn clicking_a_source_label_navigates_to_it(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (ov, cx) = mount(cx, &tmp);
        let _ = refs_view(&ov, cx);

        let bounds = cx
            .debug_bounds("linked-ref-source-2026-07-23")
            .expect("source label painted");
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();

        cx.read(|cx| {
            let current = cx.global::<CurrentPage>().get().cloned().expect("a page");
            assert_eq!(current.read(cx).name().as_ref(), "2026-07-23");
        });
    }

    /// The grouping is O(whole vault) to compute, and `render` runs on every
    /// keystroke anywhere in the window — so editing the page you are looking
    /// at must not recompute it. Nothing the current page's outline does can
    /// change who links *to* it until it is saved and reindexed.
    #[gpui::test]
    fn editing_the_current_page_does_not_rebuild_the_grouping(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (ov, cx) = mount(cx, &tmp);
        let refs = refs_view(&ov, cx);

        let before = cx.read(|cx| refs.read(cx).rebuild_count());
        assert!(before > 0, "precondition: the grouping was built once");

        // Focus is precondition setup; typing is the behaviour under test.
        cx.update(|window, cx| ov.update(cx, |ov, cx| ov.focus_first_block(window, cx)));
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.simulate_input("edited");
        cx.run_until_parked();

        let page = cx.read(|cx| cx.global::<CurrentPage>().get().cloned().unwrap());
        assert!(
            cx.read(|cx| page.read(cx).body().contains("edited")),
            "precondition: the keystrokes landed in the page",
        );
        assert_eq!(
            cx.read(|cx| refs.read(cx).rebuild_count()),
            before,
            "typing on the current page must not recompute the grouping",
        );
    }

    /// Mounts `count` journals that each reference `Target`, one block apiece.
    fn mount_many<'a>(
        cx: &'a mut TestAppContext,
        tmp: &TempDir,
        count: u32,
    ) -> (Entity<OutlineView>, &'a mut VisualTestContext) {
        let root = tmp.path().to_path_buf();
        let (ov, vcx) = cx.add_window_view(move |_window, cx| {
            text_input::bind_keys(cx);
            block_view::bind_keys(cx);
            crate::outline_view::bind_keys(cx);

            let store = NotesStore::new(&root).unwrap();
            store.write("Target", "- the page itself\n").unwrap();
            for day in 1..=count {
                let date = NaiveDate::from_ymd_opt(2026, 7, day).expect("July has 31 days");
                store
                    .write_journal(date, &format!("- ref {day} [[Target]]\n"))
                    .unwrap();
            }

            cx.set_global(TagIndex::rebuild_from(&store).unwrap());
            cx.set_global(LinkIndex::rebuild_from(&store).unwrap());
            cx.set_global(PageRegistry::new(store));
            cx.set_global(CurrentPage::default());
            cx.set_global(LastError::default());
            cx.set_global(NavHistory::default());
            set_current_page("Target", cx).unwrap();

            let page = cx.global::<CurrentPage>().get().cloned().unwrap();
            OutlineView::new(page, cx)
        });
        // Tall enough that the whole section paints, so the "Show more" row
        // is hittable by a real click.
        vcx.simulate_resize(size(px(900.), px(3000.)));
        vcx.update(|window, _| window.activate_window());
        vcx.run_until_parked();
        vcx.run_until_parked();
        (ov, vcx)
    }

    #[gpui::test]
    fn many_references_mount_only_the_capped_number(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let extra = 5;
        let (ov, cx) = mount_many(cx, &tmp, u32::try_from(REFS_PER_PAGE).unwrap() + extra);
        let refs = refs_view(&ov, cx);

        cx.read(|cx| {
            let view = refs.read(cx);
            assert_eq!(
                view.groups().len(),
                REFS_PER_PAGE + extra as usize,
                "every reference is grouped, capped or not",
            );
            assert_eq!(
                view.refs.len(),
                REFS_PER_PAGE,
                "only the capped number of subtree views is mounted",
            );
        });

        let bounds = cx
            .debug_bounds("linked-refs-show-more")
            .expect("show-more row painted");
        cx.simulate_click(bounds.center(), Modifiers::none());
        cx.run_until_parked();

        cx.read(|cx| {
            assert_eq!(
                refs.read(cx).refs.len(),
                REFS_PER_PAGE + extra as usize,
                "Show more mounts the rest",
            );
        });
        assert!(
            cx.debug_bounds("linked-refs-show-more").is_none(),
            "nothing left to show",
        );
    }

    #[gpui::test]
    fn a_page_does_not_list_itself(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let (ov, vcx) = cx.add_window_view(move |_window, cx| {
            let store = NotesStore::new(&root).unwrap();
            store.write("Target", "- links to [[Target]]\n").unwrap();
            cx.set_global(LinkIndex::rebuild_from(&store).unwrap());
            cx.set_global(TagIndex::rebuild_from(&store).unwrap());
            cx.set_global(PageRegistry::new(store));
            cx.set_global(CurrentPage::default());
            cx.set_global(LastError::default());
            cx.set_global(NavHistory::default());
            set_current_page("Target", cx).unwrap();
            OutlineView::new(cx.global::<CurrentPage>().get().cloned().unwrap(), cx)
        });
        vcx.update(|window, _| window.activate_window());
        vcx.run_until_parked();

        let refs = refs_view(&ov, vcx);
        vcx.read(|cx| {
            assert!(
                refs.read(cx).groups().is_empty(),
                "a self-link is not a linked reference",
            );
        });
    }
}
