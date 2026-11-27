//! Per-machine settings. They live outside the binary (and outside the notes
//! root), so installing a new version keeps them: `config.toml` here, and the
//! personal word list beside it (`spellcheck::personal_dictionary_path`).

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// The app's per-machine config directory: `~/.config/gpui-notes` on Linux
/// (respecting `$XDG_CONFIG_HOME`) and macOS, `%APPDATA%\gpui-notes` on
/// Windows. `None` if the home directory can't be determined.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    let base = if cfg!(target_os = "macos") {
        // `dirs::config_dir` is `~/Library/Application Support` on macOS.
        dirs::home_dir().map(|home| home.join(".config"))
    } else {
        dirs::config_dir()
    };
    base.map(|dir| dir.join("gpui-notes"))
}

/// `config.toml` in [`config_dir`].
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("config.toml"))
}

/// The `config.toml` this build reads and writes: [`config_path`] in release
/// builds, `None` in debug builds, so a bare `cargo run` during development
/// neither opens the graph an installed copy points at nor rewrites its
/// settings.
#[must_use]
pub fn active_config_path() -> Option<PathBuf> {
    config_path().filter(|_| !cfg!(debug_assertions))
}

/// The contents of `config.toml`. Every key is optional, and a missing file
/// is the same as an empty one.
#[derive(Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The notes root (a Logseq graph folder). Absolute, or starting with `~/`.
    pub notes_root: Option<PathBuf>,
    /// The `[autocorrect]` table: each typo and what replaces it. See
    /// `autocorrect::Autocorrect`.
    #[serde(default)]
    pub autocorrect: BTreeMap<String, String>,
}

impl Config {
    /// # Errors
    /// If `text` isn't valid TOML or has a key this version doesn't know.
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    /// Reads `path`, treating a missing file as an empty config.
    ///
    /// # Errors
    /// If the file exists but can't be read or parsed; the message names it.
    pub fn load(path: &Path) -> io::Result<Self> {
        Self::parse(&read_or_empty(path)?).map_err(|e| invalid_data(path, e))
    }
}

/// Applies `change` to the `config.toml` at `path` and returns the config
/// the file then holds. The file is edited rather than regenerated, so the
/// user's comments, key order and other settings survive. A missing file
/// starts out empty.
///
/// # Errors
/// If the file can't be read or written, or isn't a valid config before or
/// after the change. Nothing is written then, so the app never saves a file
/// it couldn't read back.
pub fn edit(path: &Path, change: impl FnOnce(&mut toml_edit::DocumentMut)) -> io::Result<Config> {
    let mut doc: toml_edit::DocumentMut = read_or_empty(path)?
        .parse()
        .map_err(|e| invalid_data(path, e))?;
    change(&mut doc);
    let text = doc.to_string();
    let config = Config::parse(&text).map_err(|e| invalid_data(path, e))?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, text)?;
    Ok(config)
}

fn read_or_empty(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        result => result,
    }
}

fn invalid_data(path: &Path, e: impl std::fmt::Display) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}: {e}", path.display()),
    )
}

/// Expands a leading `~` to `home`. `None` if `path` starts with `~` and
/// there's no home directory.
#[must_use]
pub fn expand_home(path: &Path, home: Option<&Path>) -> Option<PathBuf> {
    match path.strip_prefix("~") {
        Ok(rest) => home.map(|home| home.join(rest)),
        Err(_) => Some(path.to_path_buf()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_config_sets_nothing() {
        assert_eq!(Config::parse("").unwrap(), Config::default());
    }

    #[test]
    fn notes_root_is_read() {
        let config = Config::parse("notes_root = \"~/logseq\"\n").unwrap();
        assert_eq!(config.notes_root, Some(PathBuf::from("~/logseq")));
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("notes-root = \"/x\"\n").is_err());
    }

    #[test]
    fn a_missing_file_is_an_empty_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&dir.path().join("config.toml")).unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn a_malformed_file_names_itself_in_the_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "notes_root = ").unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(err.to_string().contains("config.toml"), "{err}");
    }

    #[test]
    fn edit_keeps_comments_and_other_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let before = "# my settings\nnotes_root = \"~/logseq\"\n";
        fs::write(&path, before).unwrap();

        let config = edit(&path, |doc| {
            doc["autocorrect"]["teh"] = toml_edit::value("the");
        })
        .unwrap();

        assert_eq!(config.notes_root, Some(PathBuf::from("~/logseq")));
        assert_eq!(config.autocorrect["teh"], "the");
        let after = fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(before), "{after}");
        assert_eq!(Config::load(&path).unwrap(), config);
    }

    #[test]
    fn edit_creates_a_missing_file_and_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gpui-notes").join("config.toml");
        let config = edit(&path, |doc| doc["notes_root"] = toml_edit::value("/n")).unwrap();
        assert_eq!(Config::load(&path).unwrap(), config);
    }

    #[test]
    fn edit_writes_nothing_it_could_not_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "notes_root = \"/n\"\n").unwrap();
        assert!(edit(&path, |doc| doc["bogus"] = toml_edit::value(1)).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "notes_root = \"/n\"\n");
    }

    #[test]
    fn tilde_expands_to_home() {
        let home = Path::new("/home/me");
        assert_eq!(
            expand_home(Path::new("~/logseq"), Some(home)),
            Some(PathBuf::from("/home/me/logseq"))
        );
        assert_eq!(
            expand_home(Path::new("/abs/notes"), Some(home)),
            Some(PathBuf::from("/abs/notes"))
        );
        assert_eq!(expand_home(Path::new("~/logseq"), None), None);
    }
}
