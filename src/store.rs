use chrono::NaiveDate;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::config::{self, Config};

const PAGES_DIR: &str = "pages";
const JOURNALS_DIR: &str = "journals";
/// Journal filenames on disk use Logseq's underscore form. Journal *titles* in
/// the UI stay ISO (`%Y-%m-%d`) — see `journal::is_journal_name`.
const JOURNAL_DATE_FMT: &str = "%Y_%m_%d";

pub struct NotesStore {
    root: PathBuf,
}

impl NotesStore {
    /// # Errors
    /// Returns an error if `root` or its `pages/`/`journals/` subdirectories
    /// cannot be created.
    pub fn new(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join(PAGES_DIR))?;
        fs::create_dir_all(root.join(JOURNALS_DIR))?;
        Ok(Self { root })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The notes root: `GPUI_NOTES_ROOT` if set, else `notes_root` from
    /// `config.toml`, else `~/Desktop/logseq-test-zone`.
    ///
    /// Debug builds skip `config.toml`, so a bare `cargo run` during
    /// development can't write to the graph an installed copy points at.
    ///
    /// # Errors
    /// Returns an error if `GPUI_NOTES_ROOT` or `notes_root` is not absolute,
    /// if `config.toml` can't be read or parsed, or if the home directory
    /// cannot be determined.
    pub fn default_root() -> io::Result<PathBuf> {
        let config = match config::config_path() {
            Some(path) if !cfg!(debug_assertions) => Config::load(&path)?,
            _ => Config::default(),
        };
        resolve_root(
            std::env::var_os("GPUI_NOTES_ROOT").map(PathBuf::from),
            &config,
            dirs::home_dir().as_deref(),
        )
    }

    /// # Errors
    /// Returns `InvalidInput` for an invalid page name, `NotFound` if the page
    /// does not exist, or another I/O error on read failure.
    pub fn read(&self, name: &str) -> io::Result<String> {
        fs::read_to_string(self.page_path(name)?)
    }

    /// # Errors
    /// Returns `InvalidInput` for an invalid page name, or another I/O error
    /// if the write or atomic rename fails.
    pub fn write(&self, name: &str, body: &str) -> io::Result<()> {
        let final_path = self.page_path(name)?;
        atomic_write(&final_path, body)
    }

    /// # Errors
    /// Returns an I/O error if the `pages/` directory cannot be read.
    pub fn list(&self) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(self.root.join(PAGES_DIR))? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                names.push(decode_page_name(stem));
            }
        }
        names.sort();
        Ok(names)
    }

    #[must_use]
    pub fn exists(&self, name: &str) -> bool {
        self.page_path(name).is_ok_and(|p| p.is_file())
    }

    /// # Errors
    /// Returns `InvalidInput` for an invalid page name, `NotFound` if the page
    /// does not exist, or another I/O error on removal failure.
    pub fn delete(&self, name: &str) -> io::Result<()> {
        fs::remove_file(self.page_path(name)?)
    }

    /// # Errors
    /// Returns `NotFound` if no journal exists for `date`, or another I/O error
    /// on read failure.
    pub fn read_journal(&self, date: NaiveDate) -> io::Result<String> {
        fs::read_to_string(self.journal_path(date))
    }

    /// # Errors
    /// Returns an I/O error if the write or atomic rename fails.
    pub fn write_journal(&self, date: NaiveDate, body: &str) -> io::Result<()> {
        atomic_write(&self.journal_path(date), body)
    }

    /// # Errors
    /// Returns an I/O error if the `journals/` directory cannot be read.
    pub fn list_journals(&self) -> io::Result<Vec<NaiveDate>> {
        let mut dates = Vec::new();
        for entry in fs::read_dir(self.root.join(JOURNALS_DIR))? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if let Ok(d) = NaiveDate::parse_from_str(stem, JOURNAL_DATE_FMT) {
                    dates.push(d);
                }
            }
        }
        dates.sort();
        Ok(dates)
    }

    #[must_use]
    pub fn journal_exists(&self, date: NaiveDate) -> bool {
        self.journal_path(date).is_file()
    }

    /// # Errors
    /// Returns `NotFound` if no journal exists for `date`, or another I/O error
    /// on removal failure.
    pub fn delete_journal(&self, date: NaiveDate) -> io::Result<()> {
        fs::remove_file(self.journal_path(date))
    }

    fn page_path(&self, name: &str) -> io::Result<PathBuf> {
        validate_name(name)?;
        let encoded = encode_page_name(name);
        Ok(self.root.join(PAGES_DIR).join(format!("{encoded}.md")))
    }

    /// Writes `body` beside `date`'s journal, as `<date><suffix>.md`, and
    /// returns that filename. `list_journals` skips it, as it isn't a date.
    ///
    /// # Errors
    /// Returns an I/O error if the write or atomic rename fails.
    pub fn write_journal_copy(
        &self,
        date: NaiveDate,
        suffix: &str,
        body: &str,
    ) -> io::Result<String> {
        let file_name = format!("{}{suffix}.md", date.format(JOURNAL_DATE_FMT));
        atomic_write(&self.root.join(JOURNALS_DIR).join(&file_name), body)?;
        Ok(file_name)
    }

    fn journal_path(&self, date: NaiveDate) -> PathBuf {
        self.root
            .join(JOURNALS_DIR)
            .join(format!("{}.md", date.format(JOURNAL_DATE_FMT)))
    }
}

/// [`NotesStore::default_root`]'s precedence, with its inputs passed in.
fn resolve_root(
    env_root: Option<PathBuf>,
    config: &Config,
    home: Option<&Path>,
) -> io::Result<PathBuf> {
    let no_home = || {
        io::Error::new(
            io::ErrorKind::NotFound,
            "could not determine home directory",
        )
    };
    let (root, source) = if let Some(root) = env_root {
        (root, "GPUI_NOTES_ROOT")
    } else if let Some(root) = &config.notes_root {
        let root = config::expand_home(root, home).ok_or_else(no_home)?;
        (root, "notes_root in config.toml")
    } else {
        let home = home.ok_or_else(no_home)?;
        return Ok(home.join("Desktop").join("logseq-test-zone"));
    };
    if !root.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{source} must be an absolute path"),
        ));
    }
    Ok(root)
}

fn atomic_write(final_path: &Path, body: &str) -> io::Result<()> {
    let mut tmp_path = final_path.to_path_buf();
    let tmp_name = format!(
        "{}.tmp",
        final_path
            .file_name()
            .and_then(|s| s.to_str())
            .expect("final_path built from validated name"),
    );
    tmp_path.set_file_name(tmp_name);

    let result = (|| -> io::Result<()> {
        let mut f = fs::File::create(&tmp_path)?;
        f.write_all(body.as_bytes())?;
        f.sync_all()?;
        drop(f);
        fs::rename(&tmp_path, final_path)?;
        // The rename lives in the directory entry; without syncing the
        // directory it can be lost on power failure. Unix-only: Windows
        // cannot open a directory as a file.
        #[cfg(unix)]
        if let Some(parent) = final_path.parent() {
            fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp_path);
    }
    result
}

fn validate_name(name: &str) -> io::Result<()> {
    let invalid =
        name.is_empty() || name.starts_with('.') || name.contains('\\') || name.contains("://");
    if invalid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid page name: {name:?}"),
        ));
    }
    Ok(())
}

// Logseq uses URL-encoded `/` (`%2F`) in page filenames to represent namespaces.
// We only substitute `/` here — literal `%` in names round-trips as-is, matching
// Logseq's on-disk behavior. A name containing a literal `%2F` substring would
// collide with an encoded `/`; that edge case is out of scope (see issue #14).
fn encode_page_name(name: &str) -> String {
    name.replace('/', "%2F")
}

fn decode_page_name(encoded: &str) -> String {
    encoded.replace("%2F", "/")
}

#[cfg(test)]
mod tests {
    use super::{decode_page_name, encode_page_name, resolve_root};
    use crate::config::Config;

    use rstest::rstest;
    use std::path::{Path, PathBuf};

    const HOME: &str = "/home/me";

    fn config(notes_root: &str) -> Config {
        Config {
            notes_root: Some(notes_root.into()),
        }
    }

    #[test]
    fn env_var_beats_config() {
        let root = resolve_root(
            Some("/env/notes".into()),
            &config("/config/notes"),
            Some(Path::new(HOME)),
        );
        assert_eq!(root.unwrap(), PathBuf::from("/env/notes"));
    }

    #[test]
    fn config_beats_the_default_and_expands_tilde() {
        let root = resolve_root(None, &config("~/logseq"), Some(Path::new(HOME)));
        assert_eq!(root.unwrap(), PathBuf::from("/home/me/logseq"));
    }

    #[test]
    fn default_is_the_test_zone() {
        let root = resolve_root(None, &Config::default(), Some(Path::new(HOME)));
        assert_eq!(
            root.unwrap(),
            PathBuf::from("/home/me/Desktop/logseq-test-zone")
        );
    }

    #[rstest]
    #[case::env(Some("relative".into()), Config::default(), "GPUI_NOTES_ROOT")]
    #[case::config(None, config("relative"), "config.toml")]
    fn relative_roots_are_rejected(
        #[case] env_root: Option<PathBuf>,
        #[case] config: Config,
        #[case] names: &str,
    ) {
        let err = resolve_root(env_root, &config, Some(Path::new(HOME))).unwrap_err();
        assert!(err.to_string().contains(names), "{err}");
    }

    #[test]
    fn encode_roundtrips_slash() {
        let name = "Projects/Alpha";
        let encoded = encode_page_name(name);
        assert_eq!(encoded, "Projects%2FAlpha");
        assert_eq!(decode_page_name(&encoded), name);
    }

    #[rstest]
    #[case::japanese("日本語")]
    #[case::literal_percent("a%b")]
    #[case::plain_ascii("plain")]
    #[case::multiple_slashes("a/b/c")]
    fn encode_decode_is_identity(#[case] name: &str) {
        assert_eq!(decode_page_name(&encode_page_name(name)), name);
    }
}
