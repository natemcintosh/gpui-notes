use std::fs;
use std::io::ErrorKind;

use gpui_notes::store::NotesStore;
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
fn list_returns_sorted_md_stems_only() {
    let (tmp, store) = new_store();
    store.write("beta", "b").unwrap();
    store.write("alpha", "a").unwrap();
    fs::write(tmp.path().join("ignored.txt"), "x").unwrap();
    fs::write(tmp.path().join("leftover.md.tmp"), "x").unwrap();

    assert_eq!(store.list().unwrap(), vec!["alpha", "beta"]);
}

#[test]
fn successful_write_leaves_no_tmp_file() {
    let (tmp, store) = new_store();
    store.write("page", "hello").unwrap();

    let tmp_present = fs::read_dir(tmp.path())
        .unwrap()
        .any(|e| e.unwrap().path().extension().and_then(|s| s.to_str()) == Some("tmp"));
    assert!(
        !tmp_present,
        "tmp file should not remain after successful write"
    );
}

#[test]
fn invalid_names_are_rejected() {
    let (_tmp, store) = new_store();
    for bad in ["a/b", "a\\b", "", "..", ".", ".hidden"] {
        let err = store.write(bad, "x").unwrap_err();
        assert_eq!(err.kind(), ErrorKind::InvalidInput, "name {bad:?}");
    }
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
    assert!(!store.exists("a/b"));
}

#[test]
fn delete_removes_page() {
    let (_tmp, store) = new_store();
    store.write("p", "x").unwrap();
    store.delete("p").unwrap();
    assert!(!store.exists("p"));
    assert_eq!(store.read("p").unwrap_err().kind(), ErrorKind::NotFound);
}
