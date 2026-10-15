//! Per-machine settings. They live outside the binary (and outside the notes
//! root), so installing a new version keeps them: `config.toml` here, and the
//! personal word list beside it (`spellcheck::personal_dictionary_path`).

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

/// The contents of `config.toml`. Every key is optional, and a missing file
/// is the same as an empty one.
#[derive(Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// The notes root (a Logseq graph folder). Absolute, or starting with `~/`.
    pub notes_root: Option<PathBuf>,
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
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e),
        };
        Self::parse(&text).map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e}", path.display()),
            )
        })
    }
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
