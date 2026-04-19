use std::fs;
use std::io::ErrorKind;

use chrono::NaiveDate;
use gpui_notes::store::NotesStore;
use rstest::rstest;
use tempfile::TempDir;

fn new_store() -> (TempDir, NotesStore) {
    let tmp = TempDir::new().expect("create tempdir");
    let store = NotesStore::new(tmp.path()).expect("create store");
    (tmp, store)
}

#[test]
fn round_trip_read_write() {
    let (_tmp, store) = new_store();
    store.write("foo", "bar").unwrap();
    assert_eq!(store.read("foo").unwrap(), "bar");
}

#[test]
fn new_creates_pages_and_journals_subdirs() {
    let (tmp, _store) = new_store();
    assert!(tmp.path().join("pages").is_dir());
    assert!(tmp.path().join("journals").is_dir());
}

#[test]
fn list_returns_sorted_md_stems_only() {
    let (tmp, store) = new_store();
    store.write("beta", "b").unwrap();
    store.write("alpha", "a").unwrap();
    let pages = tmp.path().join("pages");
    fs::write(pages.join("ignored.txt"), "x").unwrap();
    fs::write(pages.join("leftover.md.tmp"), "x").unwrap();

    assert_eq!(store.list().unwrap(), vec!["alpha", "beta"]);
}

#[test]
fn successful_write_leaves_no_tmp_file() {
    let (tmp, store) = new_store();
    store.write("page", "hello").unwrap();

    let tmp_present = fs::read_dir(tmp.path().join("pages"))
        .unwrap()
        .any(|e| e.unwrap().path().extension().and_then(|s| s.to_str()) == Some("tmp"));
    assert!(
        !tmp_present,
        "tmp file should not remain after successful write"
    );
}

#[rstest]
#[case::contains_backslash("a\\b")]
#[case::empty("")]
#[case::parent_dir("..")]
#[case::current_dir(".")]
#[case::hidden_dotfile(".hidden")]
fn invalid_names_are_rejected(#[case] bad: &str) {
    let (_tmp, store) = new_store();
    let err = store.write(bad, "x").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidInput, "name {bad:?}");
}

#[test]
fn read_missing_page_is_not_found() {
    let (_tmp, store) = new_store();
    let err = store.read("missing").unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
}

#[test]
fn overwrite_replaces_content() {
    let (_tmp, store) = new_store();
    store.write("p", "first").unwrap();
    store.write("p", "second").unwrap();
    assert_eq!(store.read("p").unwrap(), "second");
}

#[test]
fn exists_reflects_filesystem_state() {
    let (_tmp, store) = new_store();
    assert!(!store.exists("p"));
    store.write("p", "x").unwrap();
    assert!(store.exists("p"));
}

#[test]
fn delete_removes_page() {
    let (_tmp, store) = new_store();
    store.write("p", "x").unwrap();
    store.delete("p").unwrap();
    assert!(!store.exists("p"));
    assert_eq!(store.read("p").unwrap_err().kind(), ErrorKind::NotFound);
}

#[test]
fn namespaced_page_writes_encoded_file_and_lists_decoded() {
    let (tmp, store) = new_store();
    store.write("Projects/Alpha", "hi").unwrap();

    assert!(tmp.path().join("pages/Projects%2FAlpha.md").is_file());
    assert_eq!(store.read("Projects/Alpha").unwrap(), "hi");
    assert_eq!(store.list().unwrap(), vec!["Projects/Alpha"]);
    assert!(store.exists("Projects/Alpha"));
}

#[test]
fn namespaced_page_reads_existing_encoded_file() {
    let (tmp, store) = new_store();
    fs::write(tmp.path().join("pages/A%2FB%2FC.md"), "deep").unwrap();

    assert_eq!(store.read("A/B/C").unwrap(), "deep");
    assert_eq!(store.list().unwrap(), vec!["A/B/C"]);
}

#[test]
fn journal_round_trip() {
    let (tmp, store) = new_store();
    let date = NaiveDate::from_ymd_opt(2026, 4, 18).unwrap();
    store.write_journal(date, "today").unwrap();

    assert!(tmp.path().join("journals/2026_04_18.md").is_file());
    assert_eq!(store.read_journal(date).unwrap(), "today");
    assert!(store.journal_exists(date));
    assert_eq!(store.list_journals().unwrap(), vec![date]);
}

#[test]
fn list_journals_skips_non_date_and_non_md_files() {
    let (tmp, store) = new_store();
    let d1 = NaiveDate::from_ymd_opt(2026, 1, 2).unwrap();
    let d2 = NaiveDate::from_ymd_opt(2025, 12, 31).unwrap();
    store.write_journal(d1, "a").unwrap();
    store.write_journal(d2, "b").unwrap();
    let journals = tmp.path().join("journals");
    fs::write(journals.join("notes.txt"), "x").unwrap();
    fs::write(journals.join("not_a_date.md"), "x").unwrap();

    assert_eq!(store.list_journals().unwrap(), vec![d2, d1]);
}

#[test]
fn delete_journal_removes_file() {
    let (_tmp, store) = new_store();
    let date = NaiveDate::from_ymd_opt(2026, 4, 18).unwrap();
    store.write_journal(date, "x").unwrap();
    store.delete_journal(date).unwrap();
    assert!(!store.journal_exists(date));
    assert_eq!(
        store.read_journal(date).unwrap_err().kind(),
        ErrorKind::NotFound
    );
}

#[test]
fn list_ignores_logseq_graph_subdirs() {
    let (tmp, store) = new_store();
    store.write("Welcome", "hi").unwrap();
    for dir in ["assets", "logseq", ".recycle", "bak"] {
        let p = tmp.path().join(dir);
        fs::create_dir_all(&p).unwrap();
        fs::write(p.join("stuff.md"), "ignored").unwrap();
    }

    assert_eq!(store.list().unwrap(), vec!["Welcome"]);
}

#[test]
fn opens_existing_logseq_style_graph() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    // Simulate a real Logseq graph laid out on disk.
    fs::create_dir_all(root.join("pages")).unwrap();
    fs::create_dir_all(root.join("journals")).unwrap();
    fs::create_dir_all(root.join("assets")).unwrap();
    fs::create_dir_all(root.join("logseq")).unwrap();
    fs::write(root.join("pages/Welcome.md"), "hi").unwrap();
    fs::write(root.join("pages/Projects%2FAlpha.md"), "alpha body").unwrap();
    fs::write(root.join("journals/2026_04_18.md"), "j").unwrap();
    fs::write(root.join("assets/ignored.md"), "x").unwrap();

    let store = NotesStore::new(root).unwrap();

    assert_eq!(store.list().unwrap(), vec!["Projects/Alpha", "Welcome"]);
    assert_eq!(store.read("Projects/Alpha").unwrap(), "alpha body");
    assert_eq!(
        store.list_journals().unwrap(),
        vec![NaiveDate::from_ymd_opt(2026, 4, 18).unwrap()]
    );
}
