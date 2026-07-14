use std::collections::HashMap;
use std::io;

use chrono::NaiveDate;
use gpui::{App, AppContext, BorrowAppContext, Entity, Global, SharedString};

use crate::page::Page;
use crate::store::NotesStore;
use crate::tags::{self, TagSource};

/// Cache key for open pages. A regular page literally named `2026-07-13`
/// and that date's journal are distinct pages living in different
/// directories; keying the cache by display name let whichever opened
/// first win and routed the other's saves to the wrong directory (#40).
/// The typed key makes that collision unrepresentable.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum PageKey {
    Page(SharedString),
    Journal(NaiveDate),
}

impl PageKey {
    fn source(&self) -> TagSource {
        match self {
            Self::Page(name) => TagSource::Page(name.clone()),
            Self::Journal(date) => TagSource::Journal(*date),
        }
    }

    fn display_name(&self) -> SharedString {
        match self {
            Self::Page(name) => name.clone(),
            Self::Journal(date) => date.format("%Y-%m-%d").to_string().into(),
        }
    }
}

pub struct PageRegistry {
    store: NotesStore,
    open: HashMap<PageKey, Entity<Page>>,
}

impl Global for PageRegistry {}

impl PageRegistry {
    #[must_use]
    pub fn new(store: NotesStore) -> Self {
        Self {
            store,
            open: HashMap::new(),
        }
    }

    /// Opens an existing page.
    ///
    /// # Errors
    /// Returns `NotFound` if the page does not exist on disk, or a store I/O error.
    pub fn open(&mut self, name: &str, cx: &mut App) -> io::Result<Entity<Page>> {
        let key = PageKey::Page(name.to_string().into());
        if let Some(entity) = self.open.get(&key) {
            return Ok(entity.clone());
        }
        let body = self.store.read(name)?;
        Ok(self.insert(key, &body, cx))
    }

    /// Opens a page, creating it (empty) on disk if it does not yet exist.
    ///
    /// # Errors
    /// Returns any I/O error from the underlying store other than `NotFound`
    /// (which triggers creation).
    pub fn open_or_create(&mut self, name: &str, cx: &mut App) -> io::Result<Entity<Page>> {
        let key = PageKey::Page(name.to_string().into());
        if let Some(entity) = self.open.get(&key) {
            return Ok(entity.clone());
        }
        let body = match self.store.read(name) {
            Ok(body) => body,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                self.store.write(name, "")?;
                String::new()
            }
            Err(err) => return Err(err),
        };
        Ok(self.insert(key, &body, cx))
    }

    /// Opens the journal for `date`, creating it (empty) on disk if it does not
    /// yet exist. The page's display name is the ISO-8601 date (`YYYY-MM-DD`),
    /// and saves route to the store's journal directory.
    ///
    /// # Errors
    /// Returns any I/O error from the underlying store other than `NotFound`
    /// (which triggers creation).
    pub fn open_or_create_journal(
        &mut self,
        date: NaiveDate,
        cx: &mut App,
    ) -> io::Result<Entity<Page>> {
        let key = PageKey::Journal(date);
        if let Some(entity) = self.open.get(&key) {
            return Ok(entity.clone());
        }
        let body = match self.store.read_journal(date) {
            Ok(body) => body,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                self.store.write_journal(date, "")?;
                String::new()
            }
            Err(err) => return Err(err),
        };
        Ok(self.insert(key, &body, cx))
    }

    /// Lists all page names available on disk.
    ///
    /// # Errors
    /// Returns an I/O error if the pages directory cannot be read.
    pub fn list(&self) -> io::Result<Vec<SharedString>> {
        Ok(self
            .store
            .list()?
            .into_iter()
            .map(SharedString::from)
            .collect())
    }

    /// Lists all journal dates available on disk, ascending.
    ///
    /// # Errors
    /// Returns an I/O error if the journals directory cannot be read.
    pub fn list_journals(&self) -> io::Result<Vec<NaiveDate>> {
        self.store.list_journals()
    }

    /// Writes the page's body to disk if dirty and clears the dirty flag.
    ///
    /// # Errors
    /// Returns an I/O error if the write fails.
    pub fn save(&mut self, page: &Entity<Page>, cx: &mut App) -> io::Result<()> {
        let (name, body, dirty) = {
            let page = page.read(cx);
            (page.name().clone(), page.body().clone(), page.dirty())
        };
        if !dirty {
            return Ok(());
        }
        let key = self
            .key_for(page)
            .unwrap_or_else(|| PageKey::Page(name.clone()));
        match &key {
            PageKey::Page(name) => self.store.write(name, &body)?,
            PageKey::Journal(date) => self.store.write_journal(*date, &body)?,
        }
        page.update(cx, Page::mark_saved);
        tags::reindex_global_for_page(page, &key.source(), cx);
        Ok(())
    }

    /// Reverse lookup: the cache key under which `page`'s entity was opened.
    /// The map is small (open pages only), so a linear scan is fine.
    fn key_for(&self, page: &Entity<Page>) -> Option<PageKey> {
        self.open
            .iter()
            .find_map(|(key, entity)| (entity.entity_id() == page.entity_id()).then(|| key.clone()))
    }

    /// Saves every dirty open page. Attempts all pages even if one write
    /// fails; the first error is returned after the sweep so a single bad
    /// page cannot block the rest from persisting (#43).
    ///
    /// # Errors
    /// Returns the first I/O error encountered, if any.
    pub fn save_all(&mut self, cx: &mut App) -> io::Result<()> {
        let pages: Vec<Entity<Page>> = self.open.values().cloned().collect();
        let mut first_err = None;
        for page in &pages {
            if let Err(err) = self.save(page, cx) {
                first_err.get_or_insert(err);
            }
        }
        first_err.map_or(Ok(()), Err)
    }

    #[must_use]
    pub fn source_for_page(&self, page: &Entity<Page>, cx: &App) -> TagSource {
        self.key_for(page).map_or_else(
            || TagSource::Page(page.read(cx).name().clone()),
            |key| key.source(),
        )
    }

    fn insert(&mut self, key: PageKey, body: &str, cx: &mut App) -> Entity<Page> {
        let source = key.source();
        let page = cx.new(|cx| Page::new(key.display_name(), body, cx));
        self.open.insert(key, page.clone());
        tags::reindex_global_for_page(&page, &source, cx);
        page
    }
}

#[derive(Default)]
pub struct CurrentPage {
    current: Option<Entity<Page>>,
}

impl Global for CurrentPage {}

impl CurrentPage {
    #[must_use]
    pub fn get(&self) -> Option<&Entity<Page>> {
        self.current.as_ref()
    }

    pub fn set(&mut self, page: Option<Entity<Page>>) {
        self.current = page;
    }
}

/// Picks the next page name to cycle to after `current`, wrapping at the end.
/// Returns `None` for an empty `names` slice. If `current` isn't in `names`
/// (or is `None`), returns the first entry.
#[must_use]
pub fn pick_next<'a>(
    names: &'a [SharedString],
    current: Option<&SharedString>,
) -> Option<&'a SharedString> {
    if names.is_empty() {
        return None;
    }
    let idx = current
        .and_then(|c| names.iter().position(|n| n == c))
        .map_or(0, |i| (i + 1) % names.len());
    Some(&names[idx])
}

/// Registers an `on_app_quit` observer that saves every dirty open page.
/// Quit is the one exit path with no other flush (#43): ctrl-s and page
/// switch cover the rest. On Linux the default `QuitMode` quits when the
/// last window closes, so this fires for both the window close button and
/// explicit app quit.
pub fn save_all_on_quit(cx: &mut App) {
    cx.on_app_quit(|cx| {
        let result = cx.update_global::<PageRegistry, io::Result<()>>(PageRegistry::save_all);
        if let Err(err) = result {
            eprintln!("quit-save failed: {err}");
        }
        async {}
    })
    .detach();
}

/// Saves the outgoing current page if dirty, opens `name` (creating if missing),
/// and sets it as the current page.
///
/// # Errors
/// Returns any I/O error from saving the outgoing page or opening the incoming one.
pub fn set_current_page(name: &str, cx: &mut App) -> io::Result<()> {
    let outgoing = cx.global::<CurrentPage>().current.clone();
    let page = cx.update_global::<PageRegistry, io::Result<Entity<Page>>>(|reg, cx| {
        if let Some(outgoing) = &outgoing {
            reg.save(outgoing, cx)?;
        }
        reg.open_or_create(name, cx)
    })?;
    cx.update_global::<CurrentPage, ()>(|current, _| {
        current.current = Some(page);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::PageEvent;
    use crate::tags::TagIndex;
    use gpui::TestAppContext;
    use tempfile::TempDir;

    fn make_store(tmp: &TempDir) -> NotesStore {
        NotesStore::new(tmp.path()).expect("store")
    }

    #[gpui::test]
    fn open_or_create_caches_entity_by_name(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            let mut reg = PageRegistry::new(make_store(&tmp));
            let a = reg.open_or_create("foo", cx).unwrap();
            let b = reg.open_or_create("foo", cx).unwrap();
            assert_eq!(a.entity_id(), b.entity_id());
        });
    }

    #[gpui::test]
    fn save_persists_body_across_registries(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        cx.update(|cx| {
            let mut reg = PageRegistry::new(NotesStore::new(&root).unwrap());
            let page = reg.open_or_create("foo", cx).unwrap();
            page.update(cx, |p, cx| p.set_body_for_test("hello", cx));
            reg.save(&page, cx).unwrap();
            assert!(!page.read(cx).dirty());
        });

        cx.update(|cx| {
            let mut reg = PageRegistry::new(NotesStore::new(&root).unwrap());
            let page = reg.open("foo", cx).unwrap();
            assert_eq!(page.read(cx).body().as_ref(), "- hello\n");
        });
    }

    #[gpui::test]
    fn save_clears_dirty(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            let mut reg = PageRegistry::new(make_store(&tmp));
            let page = reg.open_or_create("foo", cx).unwrap();
            page.update(cx, |p, cx| p.set_body_for_test("x", cx));
            assert!(page.read(cx).dirty());
            reg.save(&page, cx).unwrap();
            assert!(!page.read(cx).dirty());
        });
    }

    struct SavedRecorder {
        count: usize,
        _sub: gpui::Subscription,
    }

    impl SavedRecorder {
        fn new(page: &Entity<crate::page::Page>, cx: &mut gpui::Context<Self>) -> Self {
            let sub = cx.subscribe(page, |this: &mut Self, _, event: &PageEvent, _| {
                if matches!(event, PageEvent::Saved) {
                    this.count += 1;
                }
            });
            Self {
                count: 0,
                _sub: sub,
            }
        }
    }

    #[gpui::test]
    fn save_emits_saved_event(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (recorder, page, mut reg) = cx.update(|cx| {
            let mut reg = PageRegistry::new(make_store(&tmp));
            let page = reg.open_or_create("foo", cx).unwrap();
            let recorder = cx.new(|cx| SavedRecorder::new(&page, cx));
            (recorder, page, reg)
        });

        cx.update(|cx| {
            page.update(cx, |p, cx| p.set_body_for_test("x", cx));
            reg.save(&page, cx).unwrap();
        });
        cx.run_until_parked();

        cx.read(|cx| assert_eq!(recorder.read(cx).count, 1));
    }

    #[gpui::test]
    fn save_is_noop_when_not_dirty(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            let mut reg = PageRegistry::new(make_store(&tmp));
            let page = reg.open_or_create("foo", cx).unwrap();
            reg.save(&page, cx).unwrap();
            assert!(!page.read(cx).dirty());
        });
    }

    #[gpui::test]
    fn open_and_save_refresh_tag_index(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        cx.update(|cx| {
            cx.set_global(TagIndex::default());
            let store = NotesStore::new(&root).unwrap();
            store.write("foo", "- old #stale\n").unwrap();
            let mut reg = PageRegistry::new(store);

            let page = reg.open("foo", cx).unwrap();
            assert_eq!(cx.global::<TagIndex>().blocks_for_tag("stale").len(), 1);

            let first = page.read(cx).outline().first_block_id().unwrap();
            page.update(cx, |p, cx| p.set_block_text(first, "new #fresh", cx));
            reg.save(&page, cx).unwrap();

            let index = cx.global::<TagIndex>();
            assert!(index.blocks_for_tag("stale").is_empty());
            assert_eq!(index.blocks_for_tag("fresh").len(), 1);
        });
    }

    use rstest::rstest;

    #[rstest]
    #[case::empty_no_current(&[], None, None)]
    #[case::empty_with_current(&[], Some("foo"), None)]
    #[case::wraps_after_last(&["a", "b", "c"], Some("c"), Some("a"))]
    #[case::advances_from_first(&["a", "b", "c"], Some("a"), Some("b"))]
    #[case::advances_from_middle(&["a", "b", "c"], Some("b"), Some("c"))]
    #[case::orphan_current_falls_back_to_first(&["a", "b"], Some("zzz"), Some("a"))]
    #[case::no_current_falls_back_to_first(&["a", "b"], None, Some("a"))]
    fn pick_next_cases(
        #[case] names: &[&str],
        #[case] current: Option<&str>,
        #[case] expected: Option<&str>,
    ) {
        let ns: Vec<SharedString> = names
            .iter()
            .map(|s| SharedString::from(s.to_string()))
            .collect();
        let current = current.map(|s| SharedString::from(s.to_string()));
        let picked = pick_next(&ns, current.as_ref());
        assert_eq!(picked.map(AsRef::as_ref), expected);
    }

    #[gpui::test]
    fn journal_save_routes_to_journals_dir(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let date = NaiveDate::from_ymd_opt(2026, 4, 18).unwrap();

        cx.update(|cx| {
            let mut reg = PageRegistry::new(NotesStore::new(&root).unwrap());
            let page = reg.open_or_create_journal(date, cx).unwrap();
            assert_eq!(page.read(cx).name().as_ref(), "2026-04-18");
            page.update(cx, |p, cx| p.set_body_for_test("hello", cx));
            reg.save(&page, cx).unwrap();
        });

        assert!(root.join("journals/2026-04-18.md").is_file());
        assert!(!root.join("pages/2026-04-18.md").exists());

        cx.update(|cx| {
            let mut reg = PageRegistry::new(NotesStore::new(&root).unwrap());
            let page = reg.open_or_create_journal(date, cx).unwrap();
            assert_eq!(page.read(cx).body().as_ref(), "- hello\n");
        });
    }

    /// Unit test for #43: `save_all` sweeps every open page, persisting the
    /// dirty ones and leaving clean ones untouched on disk.
    #[gpui::test]
    fn save_all_saves_every_dirty_page(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        cx.update(|cx| {
            let store = NotesStore::new(&root).unwrap();
            store.write("clean", "- untouched\n").unwrap();
            let mut reg = PageRegistry::new(store);
            let a = reg.open_or_create("a", cx).unwrap();
            let b = reg.open_or_create("b", cx).unwrap();
            let clean = reg.open("clean", cx).unwrap();
            a.update(cx, |p, cx| p.set_body_for_test("from-a", cx));
            b.update(cx, |p, cx| p.set_body_for_test("from-b", cx));

            reg.save_all(cx).unwrap();

            assert!(!a.read(cx).dirty());
            assert!(!b.read(cx).dirty());
            assert!(!clean.read(cx).dirty());
        });

        let a = std::fs::read_to_string(root.join("pages/a.md")).unwrap();
        let b = std::fs::read_to_string(root.join("pages/b.md")).unwrap();
        let clean = std::fs::read_to_string(root.join("pages/clean.md")).unwrap();
        assert_eq!(a, "- from-a\n");
        assert_eq!(b, "- from-b\n");
        assert_eq!(clean, "- untouched\n");
    }

    /// Regression test for #40: a regular page literally named like an ISO
    /// date and that date's journal must be distinct cache entries — each
    /// opens its own entity and saves to its own directory.
    #[gpui::test]
    fn page_named_like_date_does_not_collide_with_journal(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let date = NaiveDate::from_ymd_opt(2026, 1, 2).unwrap();

        cx.update(|cx| {
            let mut reg = PageRegistry::new(NotesStore::new(&root).unwrap());
            let page = reg.open_or_create("2026-01-02", cx).unwrap();
            let journal = reg.open_or_create_journal(date, cx).unwrap();
            assert_ne!(
                page.entity_id(),
                journal.entity_id(),
                "page and journal with the same display name must be distinct entities",
            );

            page.update(cx, |p, cx| p.set_body_for_test("page-body", cx));
            journal.update(cx, |p, cx| p.set_body_for_test("journal-body", cx));
            reg.save(&page, cx).unwrap();
            reg.save(&journal, cx).unwrap();
        });

        let page = std::fs::read_to_string(root.join("pages/2026-01-02.md")).unwrap();
        let journal = std::fs::read_to_string(root.join("journals/2026-01-02.md")).unwrap();
        assert_eq!(page, "- page-body\n");
        assert_eq!(journal, "- journal-body\n");
    }

    /// The opposite open order of the collision test above: the journal
    /// opens first, then the page. Both directions used to hit the same
    /// stale cache entry.
    #[gpui::test]
    fn journal_opened_first_does_not_shadow_page(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let date = NaiveDate::from_ymd_opt(2026, 1, 2).unwrap();

        cx.update(|cx| {
            let mut reg = PageRegistry::new(NotesStore::new(&root).unwrap());
            let journal = reg.open_or_create_journal(date, cx).unwrap();
            let page = reg.open_or_create("2026-01-02", cx).unwrap();
            assert_ne!(page.entity_id(), journal.entity_id());

            page.update(cx, |p, cx| p.set_body_for_test("page-body", cx));
            reg.save(&page, cx).unwrap();
        });

        assert_eq!(
            std::fs::read_to_string(root.join("pages/2026-01-02.md")).unwrap(),
            "- page-body\n",
        );
        assert_eq!(
            std::fs::read_to_string(root.join("journals/2026-01-02.md")).unwrap(),
            "",
            "journal file must not receive the page's body",
        );
    }

    #[gpui::test]
    fn set_current_page_autosaves_outgoing(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        cx.update(|cx| {
            cx.set_global(PageRegistry::new(NotesStore::new(&root).unwrap()));
            cx.set_global(CurrentPage::default());
            set_current_page("a", cx).unwrap();

            let page_a = cx.global::<CurrentPage>().get().unwrap().clone();
            page_a.update(cx, |p, cx| p.set_body_for_test("from-a", cx));

            set_current_page("b", cx).unwrap();
        });

        cx.update(|cx| {
            let mut reg = PageRegistry::new(NotesStore::new(&root).unwrap());
            let page = reg.open("a", cx).unwrap();
            assert_eq!(page.read(cx).body().as_ref(), "- from-a\n");
        });
    }
}
