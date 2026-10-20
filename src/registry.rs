use std::collections::HashMap;
use std::io;
use std::time::Duration;

use chrono::{Local, NaiveDate};
use gpui::{App, AppContext, BorrowAppContext, Entity, EntityId, Global, SharedString, Task};

use crate::errors;
use crate::page::Page;
use crate::page_links;
use crate::source::{source_label, NoteSource};
use crate::store::NotesStore;

pub struct PageRegistry {
    store: NotesStore,
    /// Keyed by `NoteSource`, not display name: a regular page literally
    /// named `2026-07-13` and that date's journal are distinct pages living
    /// in different directories; keying by name let whichever opened first
    /// win and routed the other's saves to the wrong directory (#40).
    open: HashMap<NoteSource, Entity<Page>>,
    /// Each open page's file as last read or written here. A save that finds
    /// the file no longer matching knows it was changed outside the app
    /// (Logseq, Syncthing) and keeps that version as a conflict copy (#65).
    on_disk: HashMap<NoteSource, String>,
}

impl Global for PageRegistry {}

impl PageRegistry {
    #[must_use]
    pub fn new(store: NotesStore) -> Self {
        Self {
            store,
            open: HashMap::new(),
            on_disk: HashMap::new(),
        }
    }

    /// The notes root every page is read from and saved to.
    #[must_use]
    pub fn root(&self) -> &std::path::Path {
        self.store.root()
    }

    /// Opens an existing page.
    ///
    /// # Errors
    /// Returns `NotFound` if the page does not exist on disk, or a store I/O error.
    pub fn open(&mut self, name: &str, cx: &mut App) -> io::Result<Entity<Page>> {
        let key = self.page_source(name)?;
        if let Some(entity) = self.open.get(&key) {
            return Ok(entity.clone());
        }
        let body = self.read_source(&key)?;
        Ok(self.insert(key, Some(&body), cx))
    }

    /// Opens a page, or a blank one in memory if it does not exist yet. The
    /// file is created by the first save with content, as in Logseq (#71):
    /// following a link or opening today's journal must not leave empty
    /// files behind, which Syncthing would turn into conflicts.
    ///
    /// # Errors
    /// Returns any I/O error from the underlying store other than `NotFound`.
    pub fn open_or_create(&mut self, name: &str, cx: &mut App) -> io::Result<Entity<Page>> {
        let key = self.page_source(name)?;
        self.open_or_blank(key, cx)
    }

    /// The page `name` refers to, spelled as the page already open or on
    /// disk under any case, so `[[helix]]` opens `Helix.md` rather than
    /// creating `helix.md` beside it (#66).
    fn page_source(&self, name: &str) -> io::Result<NoteSource> {
        let exact = NoteSource::Page(name.to_string().into());
        if self.open.contains_key(&exact) || self.store.exists(name) {
            return Ok(exact);
        }
        let key = page_links::page_key(name);
        let open = self.open.keys().find(
            |source| matches!(source, NoteSource::Page(open) if page_links::page_key(open) == key),
        );
        if let Some(source) = open {
            return Ok(source.clone());
        }
        let on_disk = self
            .store
            .list()?
            .into_iter()
            .find(|page| page_links::page_key(page) == key);
        Ok(on_disk.map_or(exact, |page| NoteSource::Page(page.into())))
    }

    /// Opens the journal for `date`, as `open_or_create` does a page. The
    /// page's display name is the ISO-8601 date (`YYYY-MM-DD`), and saves
    /// route to the store's journal directory.
    ///
    /// # Errors
    /// Returns any I/O error from the underlying store other than `NotFound`.
    pub fn open_or_create_journal(
        &mut self,
        date: NaiveDate,
        cx: &mut App,
    ) -> io::Result<Entity<Page>> {
        self.open_or_blank(NoteSource::Journal(date), cx)
    }

    fn open_or_blank(&mut self, key: NoteSource, cx: &mut App) -> io::Result<Entity<Page>> {
        if let Some(entity) = self.open.get(&key) {
            return Ok(entity.clone());
        }
        let body = match self.read_source(&key) {
            Ok(body) => Some(body),
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(err),
        };
        Ok(self.insert(key, body.as_deref(), cx))
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
    /// If the file changed outside the app since it was read, the app's
    /// version still wins, but the outside version is first written to a
    /// conflict page (`<name> (conflict <time>)`) and reported, so neither
    /// is lost.
    ///
    /// # Errors
    /// Returns an I/O error if a read or write fails.
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
            .unwrap_or_else(|| NoteSource::Page(name.clone()));
        // Never saved and still blank: the file is only created once there
        // is something to keep (#71).
        let blank = page
            .read(cx)
            .outline()
            .preorder()
            .iter()
            .all(|b| b.text.trim().is_empty());
        if blank && !self.on_disk.contains_key(&key) {
            page.update(cx, Page::mark_saved);
            return Ok(());
        }
        self.keep_outside_change(&key, cx)?;
        match &key {
            NoteSource::Page(name) => self.store.write(name, &body)?,
            NoteSource::Journal(date) => self.store.write_journal(*date, &body)?,
        }
        self.on_disk.insert(key.clone(), body.to_string());
        page.update(cx, Page::mark_saved);
        page_links::reindex_global_for_page(page, &key, cx);
        Ok(())
    }

    /// If `key`'s file no longer matches what was last read or written here,
    /// copies it to a new conflict file beside it before it gets overwritten.
    /// A missing file has nothing to keep.
    fn keep_outside_change(&self, key: &NoteSource, cx: &mut App) -> io::Result<()> {
        let current = match self.read_source(key) {
            Ok(current) => current,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err),
        };
        if self.on_disk.get(key) == Some(&current) {
            return Ok(());
        }
        let suffix = format!(" (conflict {})", Local::now().format("%Y-%m-%d %H-%M-%S"));
        let copy = match key {
            NoteSource::Page(name) => {
                let copy = format!("{name}{suffix}");
                self.store.write(&copy, &current)?;
                copy
            }
            // Not a page, and not listed as a journal, so name the file.
            NoteSource::Journal(date) => format!(
                "journals/{}",
                self.store.write_journal_copy(*date, &suffix, &current)?
            ),
        };
        errors::report(
            format!(
                "{} changed outside the app; kept that version as \"{copy}\"",
                source_label(key)
            ),
            cx,
        );
        Ok(())
    }

    /// Reverse lookup: the cache key under which `page`'s entity was opened.
    /// The map is small (open pages only), so a linear scan is fine.
    fn key_for(&self, page: &Entity<Page>) -> Option<NoteSource> {
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

    /// The open page for `source`, if any. Unlike `open`, never reads disk.
    #[must_use]
    pub fn get_open(&self, source: &NoteSource) -> Option<&Entity<Page>> {
        self.open.get(source)
    }

    /// The markdown on disk for `source`, whether or not it is open.
    ///
    /// # Errors
    /// Returns `NotFound` if the note does not exist, or another I/O error.
    pub fn read_source(&self, source: &NoteSource) -> io::Result<String> {
        match source {
            NoteSource::Page(name) => self.store.read(name),
            NoteSource::Journal(date) => self.store.read_journal(*date),
        }
    }

    #[must_use]
    pub fn source_for_page(&self, page: &Entity<Page>, cx: &App) -> NoteSource {
        self.key_for(page)
            .unwrap_or_else(|| NoteSource::Page(page.read(cx).name().clone()))
    }

    /// Opens `key` with its file's `body`, or blank when it has no file yet.
    /// A blank page stays out of `on_disk` and the link index until saved,
    /// so it doesn't count as existing.
    fn insert(&mut self, key: NoteSource, body: Option<&str>, cx: &mut App) -> Entity<Page> {
        let page = cx.new(|cx| Page::new(source_label(&key), body.unwrap_or_default(), cx));
        if let Some(body) = body {
            self.on_disk.insert(key.clone(), body.to_string());
            page_links::reindex_global_for_page(&page, &key, cx);
        }
        cx.observe(&page, |page, cx| schedule_autosave(&page, cx))
            .detach();
        self.open.insert(key, page.clone());
        page
    }
}

/// How long a dirty page waits after its last change before it is written,
/// so a crash or logout loses at most this much typing (#72).
const AUTOSAVE_DELAY: Duration = Duration::from_millis(1500);

/// Each open page's pending autosave. Replacing a page's task drops the old
/// one, which cancels it, so a run of edits saves once after the last.
#[derive(Default)]
struct Autosaves(HashMap<EntityId, Task<()>>);

impl Global for Autosaves {}

/// Restarts `page`'s autosave countdown if it has unsaved changes.
fn schedule_autosave(page: &Entity<Page>, cx: &mut App) {
    if !page.read(cx).dirty() {
        return;
    }
    let weak = page.downgrade();
    let task = cx.spawn(async move |cx| {
        cx.background_executor().timer(AUTOSAVE_DELAY).await;
        cx.update(|cx| {
            let Some(page) = weak.upgrade() else {
                return;
            };
            let result = cx.update_global::<PageRegistry, _>(|reg, cx| reg.save(&page, cx));
            if let Err(err) = result {
                errors::report(format!("autosave failed: {err}"), cx);
            }
        });
    });
    cx.default_global::<Autosaves>()
        .0
        .insert(page.entity_id(), task);
}

/// What the main pane shows. One enum, so "a single page" and "the journals
/// list" cannot both be current.
#[derive(Default)]
enum Shown {
    #[default]
    Nothing,
    /// The scrolling list of journals, today first (`journals_view.rs`).
    Journals,
    Page(Entity<Page>),
}

#[derive(Default)]
pub struct CurrentPage {
    current: Shown,
}

impl Global for CurrentPage {}

impl CurrentPage {
    /// The single page being shown; `None` in journals mode.
    #[must_use]
    pub fn get(&self) -> Option<&Entity<Page>> {
        match &self.current {
            Shown::Page(page) => Some(page),
            Shown::Nothing | Shown::Journals => None,
        }
    }

    pub fn set(&mut self, page: Option<Entity<Page>>) {
        self.current = page.map_or(Shown::Nothing, Shown::Page);
    }

    #[must_use]
    pub fn is_journals(&self) -> bool {
        matches!(self.current, Shown::Journals)
    }

    pub fn show_journals(&mut self) {
        self.current = Shown::Journals;
    }
}

/// Saves whatever is being navigated away from: the current page, or — in
/// journals mode, where any of the listed journals may have been edited —
/// every open page.
///
/// # Errors
/// Returns the first I/O error from saving.
pub fn save_outgoing(cx: &mut App) -> io::Result<()> {
    if cx.global::<CurrentPage>().is_journals() {
        return cx.update_global::<PageRegistry, _>(PageRegistry::save_all);
    }
    let Some(outgoing) = cx.global::<CurrentPage>().get().cloned() else {
        return Ok(());
    };
    cx.update_global::<PageRegistry, _>(|reg, cx| reg.save(&outgoing, cx))
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

/// Saves the outgoing current page if dirty, opens `source` (blank if
/// missing), and sets it as the current page.
///
/// # Errors
/// Returns any I/O error from saving the outgoing page or opening the incoming one.
pub fn set_current_page(source: &NoteSource, cx: &mut App) -> io::Result<()> {
    save_outgoing(cx)?;
    let page =
        cx.update_global::<PageRegistry, io::Result<Entity<Page>>>(|reg, cx| match source {
            NoteSource::Page(name) => reg.open_or_create(name, cx),
            NoteSource::Journal(date) => reg.open_or_create_journal(*date, cx),
        })?;
    cx.update_global::<CurrentPage, ()>(|current, _| {
        current.set(Some(page));
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::PageEvent;
    use crate::page_links::LinkIndex;
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

    /// `[[helix]]` opens the existing `Helix.md`, and saving it doesn't
    /// create `helix.md` beside it (#66).
    #[gpui::test]
    fn open_or_create_resolves_an_existing_page_in_any_case(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(&tmp);
        store.write("Helix", "- body\n").unwrap();
        cx.update(|cx| {
            let mut reg = PageRegistry::new(store);
            let page = reg.open_or_create("helix", cx).unwrap();
            assert_eq!(page.read(cx).name().as_ref(), "Helix");
            assert_eq!(page.read(cx).outline().serialize(), "- body\n");
            assert_eq!(
                reg.open_or_create("HELIX", cx).unwrap().entity_id(),
                page.entity_id()
            );

            page.update(cx, |p, cx| p.set_body_for_test("- edited\n", cx));
            reg.save(&page, cx).unwrap();
        });
        let pages: Vec<_> = std::fs::read_dir(tmp.path().join("pages"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(pages, ["Helix.md"]);
    }

    /// A page not saved yet is matched in any case too.
    #[gpui::test]
    fn open_or_create_matches_an_unsaved_page_in_any_case(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        cx.update(|cx| {
            let mut reg = PageRegistry::new(make_store(&tmp));
            let a = reg.open_or_create("Stan", cx).unwrap();
            let b = reg.open_or_create("stan", cx).unwrap();
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
    fn open_create_and_save_refresh_link_index(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        cx.update(|cx| {
            let store = NotesStore::new(&root).unwrap();
            store.write("A", "- [[B]]\n").unwrap();
            cx.set_global(LinkIndex::rebuild_from(&store).unwrap().0);
            let mut reg = PageRegistry::new(store);

            let page_a = reg.open("A", cx).unwrap();
            assert_eq!(cx.global::<LinkIndex>().backlinks("B").len(), 1);
            assert!(!cx.global::<LinkIndex>().page_exists("B"));

            let page_b = reg.open_or_create("B", cx).unwrap();
            assert!(
                !cx.global::<LinkIndex>().page_exists("B"),
                "an unsaved page does not exist yet",
            );
            page_b.update(cx, |p, cx| p.set_body_for_test("b", cx));
            reg.save(&page_b, cx).unwrap();
            assert!(cx.global::<LinkIndex>().page_exists("B"));

            let first = page_a.read(cx).outline().first_block_id().unwrap();
            page_a.update(cx, |page, cx| page.set_block_text(first, "[[C]]", cx));
            reg.save(&page_a, cx).unwrap();

            let index = cx.global::<LinkIndex>();
            assert!(index.backlinks("B").is_empty());
            assert_eq!(index.backlinks("C").len(), 1);
        });
    }

    #[gpui::test]
    fn open_and_save_refresh_link_index_for_journals(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let date = NaiveDate::from_ymd_opt(2026, 7, 23).unwrap();

        cx.update(|cx| {
            let store = NotesStore::new(&root).unwrap();
            store.write_journal(date, "- issue [[Target]]\n").unwrap();
            cx.set_global(LinkIndex::rebuild_from(&store).unwrap().0);
            let mut reg = PageRegistry::new(store);

            let journal = reg.open_or_create_journal(date, cx).unwrap();
            assert_eq!(
                cx.global::<LinkIndex>()
                    .backlinks("Target")
                    .iter()
                    .collect::<Vec<_>>(),
                [&NoteSource::Journal(date)],
            );

            let first = journal.read(cx).outline().first_block_id().unwrap();
            journal.update(cx, |page, cx| page.set_block_text(first, "[[Other]]", cx));
            reg.save(&journal, cx).unwrap();

            let index = cx.global::<LinkIndex>();
            assert!(
                index.backlinks("Target").is_empty(),
                "saving a journal must refresh its links",
            );
            assert_eq!(index.backlinks("Other").len(), 1);
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

        assert!(root.join("journals/2026_04_18.md").is_file());
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
        let journal = std::fs::read_to_string(root.join("journals/2026_01_02.md")).unwrap();
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
        assert!(
            !root.join("journals/2026_01_02.md").exists(),
            "journal file must not receive the page's body",
        );
    }

    /// #71: opening a missing page or journal, even saving it while still
    /// blank, writes nothing; the first save with content creates the file.
    #[gpui::test]
    fn missing_notes_are_created_by_the_first_save_with_content(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let date = NaiveDate::from_ymd_opt(2026, 3, 4).unwrap();

        cx.update(|cx| {
            let mut reg = PageRegistry::new(NotesStore::new(&root).unwrap());
            let page = reg.open_or_create("new", cx).unwrap();
            let journal = reg.open_or_create_journal(date, cx).unwrap();
            assert!(!root.join("pages/new.md").exists());
            assert!(!root.join("journals/2026_03_04.md").exists());

            // Typed then deleted: dirty, but nothing worth a file.
            page.update(cx, |p, cx| p.set_body_for_test("  ", cx));
            reg.save_all(cx).unwrap();
            assert!(!page.read(cx).dirty());
            assert!(!root.join("pages/new.md").exists());

            journal.update(cx, |p, cx| p.set_body_for_test("today", cx));
            reg.save_all(cx).unwrap();
        });

        assert!(!root.join("pages/new.md").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("journals/2026_03_04.md")).unwrap(),
            "- today\n",
        );
    }

    /// A page that has a file is still written when emptied: that is an edit.
    #[gpui::test]
    fn emptying_an_existing_page_still_saves(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(&tmp);
        store.write("foo", "- something\n").unwrap();
        cx.update(|cx| {
            let mut reg = PageRegistry::new(make_store(&tmp));
            let page = reg.open("foo", cx).unwrap();
            page.update(cx, |p, cx| p.set_body_for_test("", cx));
            reg.save(&page, cx).unwrap();
        });
        assert_eq!(store.read("foo").unwrap(), "-\n");
    }

    #[gpui::test]
    fn set_current_page_autosaves_outgoing(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();

        cx.update(|cx| {
            cx.set_global(PageRegistry::new(NotesStore::new(&root).unwrap()));
            cx.set_global(CurrentPage::default());
            set_current_page(&NoteSource::Page("a".into()), cx).unwrap();

            let page_a = cx.global::<CurrentPage>().get().unwrap().clone();
            page_a.update(cx, |p, cx| p.set_body_for_test("from-a", cx));

            set_current_page(&NoteSource::Page("b".into()), cx).unwrap();
        });

        cx.update(|cx| {
            let mut reg = PageRegistry::new(NotesStore::new(&root).unwrap());
            let page = reg.open("a", cx).unwrap();
            assert_eq!(page.read(cx).body().as_ref(), "- from-a\n");
        });
    }

    /// #72: an edit reaches disk once the page has been idle for
    /// `AUTOSAVE_DELAY`, and each further edit restarts the wait.
    #[gpui::test]
    fn idle_dirty_page_is_autosaved(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(&tmp);
        let page = cx.update(|cx| {
            cx.set_global(PageRegistry::new(make_store(&tmp)));
            cx.update_global::<PageRegistry, _>(|reg, cx| reg.open_or_create("foo", cx))
                .unwrap()
        });

        page.update(cx, |p, cx| p.set_body_for_test("one", cx));
        cx.executor().advance_clock(AUTOSAVE_DELAY / 2);
        page.update(cx, |p, cx| p.set_body_for_test("two", cx));
        cx.executor().advance_clock(AUTOSAVE_DELAY / 2);
        assert!(!store.exists("foo"), "an edit restarts the wait");

        cx.executor().advance_clock(AUTOSAVE_DELAY / 2);
        assert_eq!(store.read("foo").unwrap(), "- two\n");
        assert!(cx.read(|cx| !page.read(cx).dirty()));
    }

    /// Every file in `pages/` or `journals/` whose name marks it as a
    /// conflict copy, as `dir/filename`, with its contents.
    fn conflict_copies(store: &NotesStore) -> Vec<(String, String)> {
        let mut copies = Vec::new();
        for dir in ["pages", "journals"] {
            for entry in std::fs::read_dir(store.root().join(dir)).unwrap() {
                let path = entry.unwrap().path();
                let file_name = path.file_name().unwrap().to_string_lossy();
                if file_name.contains("(conflict ") {
                    let body = std::fs::read_to_string(&path).unwrap();
                    copies.push((format!("{dir}/{file_name}"), body));
                }
            }
        }
        copies
    }

    #[gpui::test]
    fn save_keeps_an_outside_page_change_as_a_conflict_copy(cx: &mut TestAppContext) {
        assert_outside_change_kept(
            &NoteSource::Page("foo".into()),
            "pages/foo (conflict ",
            "foo (conflict ",
            cx,
        );
    }

    #[gpui::test]
    fn save_keeps_an_outside_journal_change_as_a_conflict_copy(cx: &mut TestAppContext) {
        let date = NaiveDate::from_ymd_opt(2026, 7, 13).unwrap();
        assert_outside_change_kept(
            &NoteSource::Journal(date),
            "journals/2026_07_13 (conflict ",
            "journals/2026_07_13 (conflict ",
            cx,
        );
    }

    /// `copy_prefix` starts the copy's `dir/filename`; `reported` is how the
    /// error line names it.
    fn assert_outside_change_kept(
        source: &NoteSource,
        copy_prefix: &str,
        reported: &str,
        cx: &mut TestAppContext,
    ) {
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(&tmp);
        let write = |body: &str| match source {
            NoteSource::Page(name) => store.write(name, body).unwrap(),
            NoteSource::Journal(date) => store.write_journal(*date, body).unwrap(),
        };
        write("- original\n");

        cx.update(|cx| {
            cx.set_global(crate::errors::LastError::default());
            let mut reg = PageRegistry::new(make_store(&tmp));
            let page = match source {
                NoteSource::Page(name) => reg.open(name, cx).unwrap(),
                NoteSource::Journal(date) => reg.open_or_create_journal(*date, cx).unwrap(),
            };
            write("- from logseq\n");
            page.update(cx, |p, cx| p.set_body_for_test("from here", cx));
            reg.save(&page, cx).unwrap();

            assert_eq!(reg.read_source(source).unwrap(), "- from here\n");
            let copies = conflict_copies(&store);
            assert_eq!(copies.len(), 1, "{copies:?}");
            assert!(copies[0].0.starts_with(copy_prefix), "{copies:?}");
            assert_eq!(copies[0].1, "- from logseq\n");
            let message = &cx
                .global::<crate::errors::LastError>()
                .get()
                .unwrap()
                .message;
            assert!(message.contains(reported), "{message}");
            assert!(
                store.list_journals().unwrap().len() <= 1,
                "nor a new journal"
            );

            // The save itself is now what's on disk: saving again is no conflict.
            page.update(cx, |p, cx| p.set_body_for_test("again", cx));
            reg.save(&page, cx).unwrap();
            assert_eq!(conflict_copies(&store).len(), 1);
        });
    }

    #[gpui::test]
    fn save_without_outside_change_makes_no_conflict_copy(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let store = make_store(&tmp);
        store.write("foo", "* not our bullet style\n").unwrap();
        cx.update(|cx| {
            let mut reg = PageRegistry::new(make_store(&tmp));
            let page = reg.open("foo", cx).unwrap();
            page.update(cx, |p, cx| p.set_body_for_test("edited", cx));
            reg.save(&page, cx).unwrap();
        });
        assert_eq!(conflict_copies(&store), Vec::new());
    }
}
