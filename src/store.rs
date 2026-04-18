use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub struct NotesStore {
    root: PathBuf,
}

impl NotesStore {
    /// # Errors
    /// Returns an error if `root` cannot be created.
    pub fn new(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// # Errors
    /// Returns an error if `GPUI_NOTES_ROOT` is set but not absolute, or if the
    /// platform data directory cannot be determined.
    pub fn default_root() -> io::Result<PathBuf> {
        if let Some(override_root) = std::env::var_os("GPUI_NOTES_ROOT") {
            let p = PathBuf::from(override_root);
            if !p.is_absolute() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "GPUI_NOTES_ROOT must be an absolute path",
                ));
            }
            return Ok(p);
        }
        let data_dir = dirs::data_dir().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "could not determine platform data directory",
            )
        })?;
        Ok(data_dir.join("gpui-notes"))
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
        let tmp_path = self.root.join(format!("{name}.md.tmp"));

        let result = (|| -> io::Result<()> {
            let mut f = fs::File::create(&tmp_path)?;
            f.write_all(body.as_bytes())?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp_path, &final_path)
        })();

        if result.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        result
    }

    /// # Errors
    /// Returns an I/O error if the notes root cannot be read.
    pub fn list(&self) -> io::Result<Vec<String>> {
        let mut names = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("md") {
                continue;
            }
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                names.push(stem.to_string());
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

    fn page_path(&self, name: &str) -> io::Result<PathBuf> {
        validate_name(name)?;
        Ok(self.root.join(format!("{name}.md")))
    }
}

fn validate_name(name: &str) -> io::Result<()> {
    let invalid =
        name.is_empty() || name.starts_with('.') || name.contains('/') || name.contains('\\');
    if invalid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid page name: {name:?}"),
        ));
    }
    Ok(())
}
