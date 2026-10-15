//! Spell checking for the block editor (#81): a bundled `en_US` Hunspell
//! dictionary checked with `spellbook` (pure Rust, so every platform behaves
//! the same), plus a personal word list in the app's config directory.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::ops::Range;
use std::path::PathBuf;
use std::sync::LazyLock;

use gpui::Global;
use regex::Regex;
use spellbook::Dictionary;
use unicode_segmentation::UnicodeSegmentation;

const EN_US_AFF: &str = include_str!("../assets/dictionaries/en_US.aff");
const EN_US_DIC: &str = include_str!("../assets/dictionaries/en_US.dic");

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

/// The personal word list ("Add to dictionary"), one word per line.
#[must_use]
pub fn personal_dictionary_path() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("dictionary.txt"))
}

pub struct SpellChecker {
    dictionary: Dictionary,
    /// Where added words are appended. `None` keeps them in memory only.
    personal_path: Option<PathBuf>,
    /// Bumped by every added word, so editors know to re-check.
    generation: u64,
}

impl Global for SpellChecker {}

impl SpellChecker {
    /// The bundled dictionary alone; call [`Self::load_personal_words`] to
    /// add the words saved at `personal_path`.
    ///
    /// # Panics
    /// If the bundled dictionary fails to parse (a build-time asset, covered
    /// by the tests).
    #[must_use]
    pub fn new(personal_path: Option<PathBuf>) -> Self {
        Self {
            dictionary: Dictionary::new(EN_US_AFF, EN_US_DIC)
                .expect("bundled en_US dictionary parses"),
            personal_path,
            generation: 0,
        }
    }

    /// Adds the saved personal words. A missing file is not an error.
    ///
    /// # Errors
    /// Any I/O error other than `NotFound` reading the word list.
    pub fn load_personal_words(&mut self) -> io::Result<()> {
        let Some(path) = &self.personal_path else {
            return Ok(());
        };
        let words = match fs::read_to_string(path) {
            Ok(words) => words,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        for word in words.lines().map(str::trim).filter(|w| !w.is_empty()) {
            self.add_to_dictionary(word)?;
        }
        self.generation += 1;
        Ok(())
    }

    /// Accepts `word` from now on and appends it to the personal word list.
    ///
    /// # Errors
    /// If the config directory or word list can't be written. The word is
    /// still accepted for this session.
    pub fn add_word(&mut self, word: &str) -> io::Result<()> {
        self.add_to_dictionary(word)?;
        self.generation += 1;
        let Some(path) = &self.personal_path else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{word}")
    }

    fn add_to_dictionary(&mut self, word: &str) -> io::Result<()> {
        // `add` takes a `.dic` line, where `/` starts the affix flags.
        if word.contains('/') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("can't add {word:?} to the dictionary"),
            ));
        }
        self.dictionary
            .add(word)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e.to_string()))
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation
    }

    #[must_use]
    pub fn check(&self, word: &str) -> bool {
        self.dictionary.check(word)
    }

    /// The byte ranges of `text`'s misspelled words.
    #[must_use]
    pub fn misspelled(&self, text: &str) -> Vec<Range<usize>> {
        misspelled_ranges(text, |word| self.check(word))
    }
}

/// Spans that aren't prose: `[[links]]`, `((block refs))`, inline code,
/// URLs, markdown link targets, and `key::` property keys.
static SKIPPED: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"\[\[.*?\]\]",
        r"|\(\(.*?\)\)",
        r"|`[^`\n]*`",
        r"|[A-Za-z][A-Za-z0-9+.-]*://\S+",
        r"|www\.\S+",
        r"|\]\([^)\n]*\)",
        r"|(?m:^[ \t]*[^\s:]+::)",
    ))
    .expect("valid regex")
});

/// The byte ranges of the words in `text` that `is_correct` rejects. Words
/// inside code fences and the spans in [`SKIPPED`] aren't checked, nor are
/// words containing a digit. Pure, so it can be tested without a dictionary.
pub fn misspelled_ranges(text: &str, is_correct: impl Fn(&str) -> bool) -> Vec<Range<usize>> {
    let mut skipped: Vec<Range<usize>> = SKIPPED.find_iter(text).map(|m| m.range()).collect();
    skipped.extend(fenced_ranges(text));
    text.unicode_word_indices()
        .filter_map(|(start, word)| {
            // `_` joins words in Unicode segmentation, so `_emphasis_` is
            // one word; check the letters inside it.
            let trimmed = word.trim_start_matches('_');
            let start = start + (word.len() - trimmed.len());
            let word = trimmed.trim_end_matches('_');
            let range = start..start + word.len();
            let skip = word.is_empty()
                || word.chars().any(char::is_numeric)
                || skipped
                    .iter()
                    .any(|s| s.start < range.end && range.start < s.end);
            (!skip && !is_correct(word)).then_some(range)
        })
        .collect()
}

/// The lines from each code fence's opening line through its closing line
/// (or the end of the text, while it's still open).
fn fenced_ranges(text: &str) -> Vec<Range<usize>> {
    let mut ranges = Vec::new();
    let mut open_at = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        if line.trim_start().starts_with("```") {
            match open_at.take() {
                Some(start) => ranges.push(start..offset + line.len()),
                None => open_at = Some(offset),
            }
        }
        offset += line.len();
    }
    if let Some(start) = open_at {
        ranges.push(start..text.len());
    }
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    /// A toy dictionary: every word is correct except those starting with `x`.
    fn words_flagged(text: &str) -> Vec<&str> {
        misspelled_ranges(text, |w| !w.starts_with('x'))
            .into_iter()
            .map(|r| &text[r])
            .collect()
    }

    #[rstest]
    #[case::plain("a xab b xcd", &["xab", "xcd"])]
    #[case::page_link("[[xlink page]] xout", &["xout"])]
    #[case::block_ref("((xref-id)) xout", &["xout"])]
    #[case::inline_code("`xcode` xout", &["xout"])]
    #[case::url("see https://xample.com/xpath xout", &["xout"])]
    #[case::www("www.xample.com xout", &["xout"])]
    #[case::link_target("[xlabel](xtarget) xout", &["xlabel", "xout"])]
    #[case::property_key("xkey:: xvalue", &["xvalue"])]
    #[case::digits("x2 xout", &["xout"])]
    #[case::underscores("_xemph_", &["xemph"])]
    #[case::fence("xa\n```\nxcode\n```\nxb", &["xa", "xb"])]
    #[case::open_fence("xa\n```calc\nxcode", &["xa"])]
    #[case::multibyte("café xé", &["xé"])]
    fn flags_only_prose(#[case] text: &str, #[case] expected: &[&str]) {
        assert_eq!(words_flagged(text), expected);
    }

    fn checker() -> SpellChecker {
        SpellChecker::new(None)
    }

    #[test]
    fn bundled_dictionary_flags_misspellings() {
        let text = "Teh quick brown fox doesn't jump.";
        let checker = checker();
        let flagged: Vec<&str> = checker
            .misspelled(text)
            .into_iter()
            .map(|r| &text[r])
            .collect();
        assert_eq!(flagged, ["Teh"]);
    }

    #[test]
    fn added_words_persist_to_the_word_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("dictionary.txt");

        let mut checker = SpellChecker::new(Some(path.clone()));
        assert!(!checker.check("gpui"));
        let before = checker.generation();
        checker.add_word("gpui").unwrap();
        assert!(checker.check("gpui"));
        assert!(checker.generation() > before, "editors must re-check");
        assert_eq!(fs::read_to_string(&path).unwrap(), "gpui\n");

        let mut reloaded = SpellChecker::new(Some(path));
        assert!(!reloaded.check("gpui"));
        reloaded.load_personal_words().unwrap();
        assert!(reloaded.check("gpui"));
    }

    #[test]
    fn a_missing_word_list_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut checker = SpellChecker::new(Some(dir.path().join("dictionary.txt")));
        checker.load_personal_words().unwrap();
    }
}
