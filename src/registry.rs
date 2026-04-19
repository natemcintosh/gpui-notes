use std::collections::HashMap;
use std::io;

use gpui::{App, AppContext, BorrowAppContext, Entity, Global, SharedString};

use crate::page::Page;
use crate::store::NotesStore;

pub struct PageRegistry {
    store: NotesStore,
    open: HashMap<SharedString, Entity<Page>>,
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
        let key: SharedString = name.to_string().into();
        if let Some(page) = self.open.get(&key) {
            return Ok(page.clone());
        }
        let body = self.store.read(name)?;
        Ok(self.insert(key, body, cx))
    }

    /// Opens a page, creating it (empty) on disk if it does not yet exist.
    ///
    /// # Errors
    /// Returns any I/O error from the underlying store other than `NotFound`
    /// (which triggers creation).
    pub fn open_or_create(&mut self, name: &str, cx: &mut App) -> io::Result<Entity<Page>> {
        let key: SharedString = name.to_string().into();
        if let Some(page) = self.open.get(&key) {
            return Ok(page.clone());
        }
        let body = match self.store.read(name) {
            Ok(body) => body,
            Err(err) if err.kind() == io::ErrorKind::NotFound => {
                self.store.write(name, "")?;
                String::new()
            }
            Err(err) => return Err(err),
        };
        Ok(self.insert(key, body, cx))
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
        self.store.write(&name, &body)?;
        page.update(cx, Page::mark_saved);
        Ok(())
    }

    fn insert(&mut self, key: SharedString, body: String, cx: &mut App) -> Entity<Page> {
        let page = cx.new(|cx| Page::new(key.clone(), body, cx));
        self.open.insert(key, page.clone());
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
            assert_eq!(page.read(cx).body().as_ref(), "hello");
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
        assert_eq!(picked.map(|s| s.as_ref()), expected);
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
            assert_eq!(page.read(cx).body().as_ref(), "from-a");
        });
    }
}
