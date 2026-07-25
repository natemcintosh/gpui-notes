//! Inline completion model for `[[Page]]` links and `#tags`.
//!
//! This module is pure: it maps a buffer plus a caret offset onto a list of
//! candidate names, and knows what text to splice back in when one is accepted.
//! `BlockView` owns the state, drives it from the editor's notifications, and
//! renders the popup — see `block_view.rs`.
//!
//! The trigger scanners deliberately mirror the render-time matchers so the
//! popup never offers a completion that would fail to parse as a link or tag:
//! `PAGE_LINK_RE` in `page_links.rs` and `TAG_RE` in `tags.rs`.

use std::ops::Range;

use gpui::SharedString;

/// Longest candidate list the popup will show. The list has no scroll
/// container, so it is capped rather than clipped.
const MAX_ROWS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionKind {
    Page,
    Tag,
}

/// An open completion site in the buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trigger {
    pub kind: CompletionKind,
    /// Byte range covering the sigil (`[[` or `#`) through the caret.
    pub range: Range<usize>,
    /// The text typed after the sigil.
    pub query: String,
}

/// One row in the popup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Candidate {
    /// A name that already exists in the index.
    Existing(SharedString),
    /// The literal query, offered so a link can point at a page or tag that
    /// does not exist yet.
    New(SharedString),
}

impl Candidate {
    fn name(&self) -> &SharedString {
        match self {
            Self::Existing(name) | Self::New(name) => name,
        }
    }
}

/// Scans backwards from `caret` for an open completion sigil. A page link wins
/// over a tag when both match, so a `#` typed inside `[[…]]` keeps completing
/// the page name.
#[must_use]
pub fn trigger_at(text: &str, caret: usize) -> Option<Trigger> {
    page_trigger(text, caret).or_else(|| tag_trigger(text, caret))
}

fn page_trigger(text: &str, caret: usize) -> Option<Trigger> {
    let before = text.get(..caret)?;
    let start = before.rfind("[[")?;
    let query = &before[start + 2..];
    // Anything the link regex rejects means the `[[` is already closed, or was
    // never a link to begin with.
    if query.contains(['[', ']', '\n']) {
        return None;
    }
    Some(Trigger {
        kind: CompletionKind::Page,
        range: start..caret,
        query: query.to_string(),
    })
}

fn tag_trigger(text: &str, caret: usize) -> Option<Trigger> {
    let before = text.get(..caret)?;
    let start = before.rfind('#')?;
    let query = &before[start + 1..];
    if !query
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '/'))
    {
        return None;
    }
    // `TAG_RE` only recognizes a `#` at the start of the text or after one of
    // these separators; completing elsewhere would produce a dead tag.
    let preceded_ok = before[..start].chars().next_back().is_none_or(|ch| {
        ch.is_whitespace() || matches!(ch, '(' | ',' | '.' | ':' | ';' | '!' | '?' | '-')
    });
    if !preceded_ok {
        return None;
    }
    Some(Trigger {
        kind: CompletionKind::Tag,
        range: start..caret,
        query: query.to_string(),
    })
}

/// Rows for `query`: prefix matches first, then substring matches, both
/// case-insensitive and each capped at `MAX_ROWS` in total. A `New` row is
/// appended whenever the query is non-empty and no candidate already equals it.
#[must_use]
pub fn candidates(query: &str, names: &[SharedString]) -> Vec<Candidate> {
    let needle = query.to_lowercase();
    let mut prefixed = Vec::new();
    let mut contained = Vec::new();
    for name in names {
        let haystack = name.to_lowercase();
        if haystack.starts_with(&needle) {
            prefixed.push(name.clone());
        } else if haystack.contains(&needle) {
            contained.push(name.clone());
        }
    }

    let mut rows: Vec<Candidate> = prefixed
        .into_iter()
        .chain(contained)
        .take(MAX_ROWS)
        .map(Candidate::Existing)
        .collect();

    let already_listed = names.iter().any(|name| name.to_lowercase() == needle);
    if !query.is_empty() && !already_listed {
        rows.push(Candidate::New(SharedString::from(query.to_string())));
    }
    rows
}

/// An open popup: which site in the buffer it completes, its rows, and the
/// highlighted row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionState {
    pub kind: CompletionKind,
    pub range: Range<usize>,
    pub candidates: Vec<Candidate>,
    pub selected: usize,
}

impl CompletionState {
    /// Builds the state for `trigger`, or `None` when there is nothing to
    /// offer (an empty query with no matching names).
    #[must_use]
    pub fn new(trigger: &Trigger, names: &[SharedString]) -> Option<Self> {
        let candidates = candidates(&trigger.query, names);
        (!candidates.is_empty()).then(|| Self {
            kind: trigger.kind,
            range: trigger.range.clone(),
            candidates,
            selected: 0,
        })
    }

    pub fn select_up(&mut self) {
        if self.candidates.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.candidates.len() - 1
        } else {
            self.selected - 1
        };
    }

    pub fn select_down(&mut self) {
        if self.candidates.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.candidates.len();
    }

    /// The text to splice over `range` for the highlighted row.
    #[must_use]
    pub fn insertion(&self) -> Option<String> {
        let name = self.candidates.get(self.selected)?.name();
        Some(match self.kind {
            CompletionKind::Page => format!("[[{name}]]"),
            CompletionKind::Tag => format!("#{name}"),
        })
    }

    /// Label for row `index`, as shown in the popup.
    #[must_use]
    pub fn label(&self, index: usize) -> Option<SharedString> {
        Some(match self.candidates.get(index)? {
            Candidate::Existing(name) => name.clone(),
            Candidate::New(name) => {
                let noun = match self.kind {
                    CompletionKind::Page => "page",
                    CompletionKind::Tag => "tag",
                };
                SharedString::from(format!("New {noun}: {name}"))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn names(list: &[&str]) -> Vec<SharedString> {
        list.iter()
            .map(|name| SharedString::from((*name).to_string()))
            .collect()
    }

    /// `|` marks the caret; it is stripped before scanning.
    fn scan(marked: &str) -> Option<Trigger> {
        let caret = marked.find('|').expect("test case marks the caret");
        let text = marked.replace('|', "");
        trigger_at(&text, caret)
    }

    #[rstest]
    #[case::open_link("[[ru|", Some((CompletionKind::Page, 0, "ru")))]
    #[case::auto_paired("[[ru|]]", Some((CompletionKind::Page, 0, "ru")))]
    #[case::empty_query("[[|]]", Some((CompletionKind::Page, 0, "")))]
    #[case::closed_link("[[a]] x|", None)]
    #[case::second_link("[[a]] [[b|", Some((CompletionKind::Page, 6, "b")))]
    #[case::newline_in_query("[[a\nb|", None)]
    #[case::page_beats_tag("[[a #b|", Some((CompletionKind::Page, 0, "a #b")))]
    #[case::tag("#ta|g", Some((CompletionKind::Tag, 0, "ta")))]
    #[case::bare_tag_sigil("#|", Some((CompletionKind::Tag, 0, "")))]
    #[case::tag_after_space("see #ru|", Some((CompletionKind::Tag, 4, "ru")))]
    #[case::tag_without_boundary("x#tag|", None)]
    #[case::tag_with_space_in_query("#a b|", None)]
    #[case::no_sigil("plain text|", None)]
    fn trigger_scanning(
        #[case] marked: &str,
        #[case] expected: Option<(CompletionKind, usize, &str)>,
    ) {
        let found = scan(marked);
        match expected {
            None => assert_eq!(found, None, "{marked} must not open a popup"),
            Some((kind, start, query)) => {
                let found = found.expect("expected a trigger");
                assert_eq!(found.kind, kind);
                assert_eq!(found.range.start, start);
                assert_eq!(found.query, query);
            }
        }
    }

    #[test]
    fn prefix_matches_precede_substring_matches() {
        let names = names(&["ratatui", "ruff", "Trust Fall", "rustup"]);
        assert_eq!(
            candidates("ru", &names),
            vec![
                Candidate::Existing("ruff".into()),
                Candidate::Existing("rustup".into()),
                Candidate::Existing("Trust Fall".into()),
                Candidate::New("ru".into()),
            ],
            "`ratatui` doesn't match at all; `Trust Fall` only matches mid-word"
        );
    }

    #[test]
    fn matching_ignores_case() {
        let names = names(&["Rust Language"]);
        assert_eq!(
            candidates("rust l", &names),
            vec![
                Candidate::Existing("Rust Language".into()),
                Candidate::New("rust l".into()),
            ]
        );
    }

    #[test]
    fn existing_matches_are_capped_but_the_new_row_survives() {
        let all = names(&["a1", "a2", "a3", "a4", "a5", "a6", "a7", "a8", "a9"]);
        let rows = candidates("a", &all);
        assert_eq!(rows.len(), MAX_ROWS + 1, "8 matches plus the New row");
        assert_eq!(rows[MAX_ROWS], Candidate::New("a".into()));
    }

    #[test]
    fn an_exact_match_suppresses_the_new_row() {
        let names = names(&["Ruff"]);
        assert_eq!(
            candidates("ruff", &names),
            vec![Candidate::Existing("Ruff".into())],
        );
    }

    #[test]
    fn an_empty_query_lists_everything_without_a_new_row() {
        let names = names(&["one", "two"]);
        assert_eq!(
            candidates("", &names),
            vec![
                Candidate::Existing("one".into()),
                Candidate::Existing("two".into()),
            ]
        );
    }

    #[test]
    fn an_empty_query_with_no_names_offers_nothing() {
        let trigger = Trigger {
            kind: CompletionKind::Page,
            range: 0..2,
            query: String::new(),
        };
        assert_eq!(CompletionState::new(&trigger, &[]), None);
    }

    #[rstest]
    #[case::page(CompletionKind::Page, "[[Rust Language]]")]
    #[case::tag(CompletionKind::Tag, "#Rust Language")]
    fn insertion_wraps_the_selected_name(#[case] kind: CompletionKind, #[case] expected: &str) {
        let state = CompletionState {
            kind,
            range: 0..2,
            candidates: vec![Candidate::Existing("Rust Language".into())],
            selected: 0,
        };
        assert_eq!(state.insertion().as_deref(), Some(expected));
    }

    #[test]
    fn selection_wraps_in_both_directions() {
        let mut state = CompletionState {
            kind: CompletionKind::Page,
            range: 0..2,
            candidates: vec![
                Candidate::Existing("a".into()),
                Candidate::Existing("b".into()),
            ],
            selected: 0,
        };

        state.select_up();
        assert_eq!(state.selected, 1, "up from the first row wraps to the last");
        state.select_down();
        assert_eq!(
            state.selected, 0,
            "down from the last row wraps to the first"
        );
    }

    #[test]
    fn the_new_row_is_labelled_by_kind() {
        let state = CompletionState {
            kind: CompletionKind::Tag,
            range: 0..1,
            candidates: vec![Candidate::New("todo".into())],
            selected: 0,
        };
        assert_eq!(state.label(0), Some("New tag: todo".into()));
    }
}
