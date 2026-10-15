//! Full-text search over every block of every page and journal.
//!
//! Matching is case-insensitive (full Unicode lowercasing, not just ASCII)
//! and word-based: the query is split on whitespace and a block matches when
//! it contains every word, in any order. Blocks that also contain the query
//! as a phrase rank first. There is no typo tolerance or subsequence
//! matching; see the fuzzy-search issue for the options considered.
//!
//! The corpus is a snapshot taken when the overlay opens: open pages
//! contribute their live outline (so unsaved edits are searchable), every
//! other note is read from disk. A hit names its block by `ordinal`, its
//! position in `Outline::preorder`, because the `BlockId`s of a throwaway
//! parse do not survive into the `Page` that opening the note creates;
//! re-parsing the same file yields the same order.

use std::io;
use std::ops::Range;

use gpui::{
    actions, div, prelude::*, px, App, AppContext, Context, ElementId, Entity, EventEmitter,
    FocusHandle, Focusable, FontWeight, HighlightStyle, IntoElement, KeyBinding, ParentElement,
    Render, ScrollHandle, SharedString, Styled, StyledText, Subscription, Window,
};

use crate::outline::{BlockId, Outline};
use crate::registry::PageRegistry;
use crate::source::{source_label, NoteSource};
use crate::text_input::{TextInput, TextInputEvent};
use crate::theme;

actions!(search, [SelectUp, SelectDown]);

/// Register key bindings for the search overlay's navigation actions.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectUp, Some("SearchView")),
        KeyBinding::new("down", SelectDown, Some("SearchView")),
    ]);
}

/// Hits beyond this are counted but not shown; each row is a shaped text.
const MAX_RESULTS: usize = 200;
/// Characters of context kept before the first match in a snippet.
const CONTEXT_CHARS: usize = 40;
/// Total characters a snippet shows (before the ellipses).
const SNIPPET_CHARS: usize = 160;

/// Lowercases `text` one `char` at a time. Both the corpus and the query go
/// through this (rather than `str::to_lowercase`, whose final-sigma rule
/// depends on context) so that a folded query is a substring of a folded
/// block exactly when the block matches.
fn fold(text: &str) -> String {
    text.chars().flat_map(char::to_lowercase).collect()
}

/// `fold(text)` plus, for each byte of the folded string, the byte offset in
/// `text` of the `char` it came from. Lowercasing can change a char's length
/// (`İ` becomes two chars), so folded offsets can't be used on `text` as is.
fn fold_with_map(text: &str) -> (String, Vec<usize>) {
    let mut folded = String::with_capacity(text.len());
    let mut map = Vec::with_capacity(text.len());
    for (offset, c) in text.char_indices() {
        for lower in c.to_lowercase() {
            folded.push(lower);
            map.resize(folded.len(), offset);
        }
    }
    (folded, map)
}

/// A parsed search query: its words, folded. `None` for a blank query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    terms: Vec<String>,
    /// The words joined by single spaces, for phrase ranking.
    phrase: String,
}

impl Query {
    #[must_use]
    pub fn parse(input: &str) -> Option<Self> {
        let terms: Vec<String> = fold(input).split_whitespace().map(str::to_owned).collect();
        if terms.is_empty() {
            return None;
        }
        let phrase = terms.join(" ");
        Some(Self { terms, phrase })
    }

    fn matches(&self, folded: &str) -> bool {
        self.terms.iter().all(|term| folded.contains(term.as_str()))
    }

    /// Byte ranges in `text` covered by any query word, sorted and merged.
    fn highlights(&self, text: &str) -> Vec<Range<usize>> {
        let (folded, map) = fold_with_map(text);
        let mut ranges: Vec<Range<usize>> = self
            .terms
            .iter()
            .flat_map(|term| folded.match_indices(term.as_str()))
            .map(|(start, term)| {
                // The match may end partway through one char's lowercase
                // expansion; widen it to the end of that original char.
                let last = map[start + term.len() - 1];
                let end = last + text[last..].chars().next().map_or(0, char::len_utf8);
                map[start]..end
            })
            .collect();
        ranges.sort_by_key(|r| (r.start, r.end));
        let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
        for range in ranges {
            match merged.last_mut() {
                Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
                _ => merged.push(range),
            }
        }
        merged
    }
}

/// One line of `text` around its first highlight: up to `CONTEXT_CHARS`
/// chars before it, `SNIPPET_CHARS` in all, line breaks shown as spaces and
/// `…` where text was cut. `ranges` are re-based onto the snippet and
/// clipped to it.
fn snippet(text: &str, ranges: &[Range<usize>]) -> (String, Vec<Range<usize>>) {
    const ELLIPSIS: &str = "…";
    let first = ranges.first().map_or(0, |r| r.start);
    let start = text[..first]
        .char_indices()
        .rev()
        .nth(CONTEXT_CHARS - 1)
        .map_or(0, |(i, _)| i);
    let end = text[start..]
        .char_indices()
        .nth(SNIPPET_CHARS)
        .map_or(text.len(), |(i, _)| start + i);

    let mut out = String::with_capacity(end - start + 2 * ELLIPSIS.len());
    if start > 0 {
        out.push_str(ELLIPSIS);
    }
    let shift = out.len();
    // Each replacement is one ASCII byte for one ASCII byte, so ranges hold.
    out.extend(text[start..end].chars().map(|c| match c {
        '\n' | '\r' | '\t' => ' ',
        c => c,
    }));
    if end < text.len() {
        out.push_str(ELLIPSIS);
    }
    let ranges = ranges
        .iter()
        .filter(|r| r.start < end && r.end > start)
        .map(|r| r.start.max(start) - start + shift..r.end.min(end) - start + shift)
        .collect();
    (out, ranges)
}

struct Doc {
    source: NoteSource,
    ordinal: usize,
    text: String,
    folded: String,
}

/// Every non-empty block of the notes, in display order: journals newest
/// first, then pages alphabetically, each in document order.
#[derive(Default)]
pub struct SearchCorpus {
    docs: Vec<Doc>,
}

impl SearchCorpus {
    /// Snapshot every note. A file that can't be read is skipped and
    /// returned with its error, as at startup (#67).
    ///
    /// # Errors
    /// Returns an I/O error if the pages or journals directory can't be listed.
    pub fn collect(cx: &App) -> io::Result<(Self, Vec<(NoteSource, io::Error)>)> {
        let registry = cx.global::<PageRegistry>();
        let mut journals = registry.list_journals()?;
        journals.reverse();
        let sources = journals
            .into_iter()
            .map(NoteSource::Journal)
            .chain(registry.list()?.into_iter().map(NoteSource::Page));

        let mut corpus = Self::default();
        let mut skipped = Vec::new();
        for source in sources {
            if let Some(page) = registry.get_open(&source) {
                corpus.push(&source, page.read(cx).outline());
                continue;
            }
            match registry.read_source(&source) {
                Ok(body) => corpus.push(&source, &Outline::parse(&body)),
                Err(err) => skipped.push((source, err)),
            }
        }
        Ok((corpus, skipped))
    }

    /// Append `outline`'s blocks under `source`.
    pub fn push(&mut self, source: &NoteSource, outline: &Outline) {
        for (ordinal, block) in outline.preorder().into_iter().enumerate() {
            if block.text.trim().is_empty() {
                continue;
            }
            self.docs.push(Doc {
                source: source.clone(),
                ordinal,
                folded: fold(&block.text),
                text: block.text.clone(),
            });
        }
    }

    /// Every block matching `query`: phrase matches first, then corpus order.
    /// Only the first `MAX_RESULTS` are returned, with snippets.
    #[must_use]
    pub fn search(&self, query: &Query) -> SearchResults {
        let mut matched: Vec<(bool, &Doc)> = self
            .docs
            .iter()
            .filter(|doc| query.matches(&doc.folded))
            .map(|doc| (doc.folded.contains(&query.phrase), doc))
            .collect();
        // Stable, so ties keep corpus order.
        matched.sort_by_key(|&(phrase, _)| !phrase);
        let total = matched.len();
        let hits = matched
            .into_iter()
            .take(MAX_RESULTS)
            .map(|(_, doc)| {
                let (snippet, highlights) = snippet(&doc.text, &query.highlights(&doc.text));
                SearchHit {
                    target: SearchTarget {
                        source: doc.source.clone(),
                        ordinal: doc.ordinal,
                    },
                    snippet: snippet.into(),
                    highlights,
                }
            })
            .collect();
        SearchResults { hits, total }
    }
}

/// Where a hit lives: a note and the block's position in its preorder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchTarget {
    pub source: NoteSource,
    pub ordinal: usize,
}

impl SearchTarget {
    /// The live block this target names in `outline`, the note's outline
    /// once it has been opened.
    #[must_use]
    pub fn resolve(&self, outline: &Outline) -> Option<BlockId> {
        outline.preorder().get(self.ordinal).map(|block| block.id)
    }
}

#[derive(Clone, Debug)]
pub struct SearchHit {
    pub target: SearchTarget,
    pub snippet: SharedString,
    /// Byte ranges of `snippet` to highlight.
    pub highlights: Vec<Range<usize>>,
}

#[derive(Debug, Default)]
pub struct SearchResults {
    pub hits: Vec<SearchHit>,
    /// Matches in all, including those beyond `hits`.
    pub total: usize,
}

#[derive(Clone, Debug)]
pub enum SearchEvent {
    Selected(SearchTarget),
    Cancelled,
}

impl EventEmitter<SearchEvent> for SearchView {}

/// The search overlay: a query input over a list of matching blocks. Enter
/// or a click emits `Selected`, Escape `Cancelled`; `RootView` mounts it and
/// acts on both, like `PagePicker`.
pub struct SearchView {
    input: Entity<TextInput>,
    corpus: SearchCorpus,
    results: SearchResults,
    selected: usize,
    scroll: ScrollHandle,
    focus_handle: FocusHandle,
    _input_sub: Subscription,
}

impl SearchView {
    pub fn new(corpus: SearchCorpus, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new(cx, "Search all notes…"));
        let sub = cx.subscribe(&input, |this, _input, event, cx| match event {
            TextInputEvent::Changed(query) => {
                this.results = Query::parse(query)
                    .map(|query| this.corpus.search(&query))
                    .unwrap_or_default();
                this.selected = 0;
                cx.notify();
            }
            TextInputEvent::Submitted | TextInputEvent::SubmittedNow => {
                if let Some(hit) = this.results.hits.get(this.selected) {
                    cx.emit(SearchEvent::Selected(hit.target.clone()));
                }
            }
            TextInputEvent::Cancelled => cx.emit(SearchEvent::Cancelled),
        });
        Self {
            input,
            corpus,
            results: SearchResults::default(),
            selected: 0,
            scroll: ScrollHandle::new(),
            focus_handle: cx.focus_handle(),
            _input_sub: sub,
        }
    }

    /// Give the query input focus. `RootView` owns when this is called.
    pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
        let input_focus = self.input.focus_handle(cx);
        window.focus(&input_focus, cx);
    }

    #[must_use]
    pub fn results(&self) -> &SearchResults {
        &self.results
    }

    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        let len = self.results.hits.len();
        if len == 0 {
            return;
        }
        self.selected = self.selected.checked_sub(1).unwrap_or(len - 1);
        cx.notify();
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        let len = self.results.hits.len();
        if len == 0 {
            return;
        }
        self.selected = (self.selected + 1) % len;
        cx.notify();
    }

    fn render_hit(&self, row: usize, hit: &SearchHit, cx: &mut Context<Self>) -> impl IntoElement {
        let (bg, fg) = if row == self.selected {
            (theme::row_selected(), theme::row_selected_fg())
        } else {
            (theme::row_bg(), theme::fg())
        };
        let highlight = HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            color: Some(theme::row_selected_fg().into()),
            background_color: Some(theme::search_match_bg().into()),
            ..HighlightStyle::default()
        };
        let snippet = StyledText::new(hit.snippet.clone())
            .with_highlights(hit.highlights.iter().map(|r| (r.clone(), highlight)));
        let target = hit.target.clone();
        div()
            .id(ElementId::Name(format!("search-row-{row}").into()))
            .cursor_pointer()
            .flex()
            .flex_col()
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(bg)
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(theme::fg_muted())
                    .child(source_label(&hit.target.source)),
            )
            .child(div().text_color(fg).child(snippet))
            .on_click(cx.listener(move |_, _, _, cx| {
                cx.emit(SearchEvent::Selected(target.clone()));
            }))
    }
}

impl Focusable for SearchView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for SearchView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Re-renders only on query/selection changes, so revealing here
        // doesn't fight mouse scrolling (as in `PagePicker`).
        self.scroll.scroll_to_item(self.selected);
        let status = if self.input.read(cx).content().trim().is_empty() {
            None
        } else if self.results.total == 0 {
            Some("No matches.".to_string())
        } else if self.results.total > self.results.hits.len() {
            Some(format!(
                "Showing {} of {} matches",
                self.results.hits.len(),
                self.results.total
            ))
        } else if self.results.total == 1 {
            Some("1 match".to_string())
        } else {
            Some(format!("{} matches", self.results.total))
        };
        let rows: Vec<_> = self
            .results
            .hits
            .iter()
            .enumerate()
            .map(|(row, hit)| self.render_hit(row, hit, cx))
            .collect();
        let list = div()
            .id("search-results")
            .flex()
            .flex_col()
            .gap_0p5()
            .max_h(px(420.0))
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .children(rows);

        div()
            .track_focus(&self.focus_handle)
            .key_context("SearchView")
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .flex()
            .flex_col()
            .gap_2()
            .bg(theme::overlay_bg())
            .border_1()
            .border_color(theme::overlay_border())
            .rounded_md()
            .p_3()
            .w(px(560.0))
            .max_w_full()
            .child(self.input.clone())
            .children(status.map(|status| {
                div()
                    .px_2()
                    .text_size(px(11.))
                    .text_color(theme::control_fg())
                    .child(status)
            }))
            .child(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_input;
    use chrono::NaiveDate;
    use gpui::TestAppContext;
    use rstest::rstest;

    fn corpus() -> SearchCorpus {
        let mut corpus = SearchCorpus::default();
        corpus.push(
            &NoteSource::Journal(NaiveDate::from_ymd_opt(2026, 10, 14).unwrap()),
            &Outline::parse("- Met with Dana about the ETL\n  - follow up on ETL failures\n"),
        );
        corpus.push(
            &NoteSource::Page("Recipes".into()),
            &Outline::parse("- Failures of bread\n-\n- ÉCLAIR notes\n"),
        );
        corpus
    }

    fn labels(results: &SearchResults) -> Vec<(String, usize)> {
        results
            .hits
            .iter()
            .map(|hit| {
                (
                    source_label(&hit.target.source).to_string(),
                    hit.target.ordinal,
                )
            })
            .collect()
    }

    #[rstest]
    #[case::case_insensitive("etl", &[("2026-10-14", 0), ("2026-10-14", 1)])]
    #[case::words_in_any_order("failures etl", &[("2026-10-14", 1)])]
    #[case::non_ascii("éclair", &[("Recipes", 2)])]
    #[case::phrase_ranks_first(
        "failures",
        &[("2026-10-14", 1), ("Recipes", 0)],
    )]
    #[case::no_match("zebra", &[])]
    fn search_finds(#[case] query: &str, #[case] expected: &[(&str, usize)]) {
        let results = corpus().search(&Query::parse(query).unwrap());
        let expected: Vec<(String, usize)> =
            expected.iter().map(|&(s, o)| (s.to_string(), o)).collect();
        assert_eq!(labels(&results), expected);
        assert_eq!(results.total, expected.len());
    }

    #[test]
    fn phrase_match_outranks_scattered_words() {
        let mut corpus = SearchCorpus::default();
        corpus.push(
            &NoteSource::Page("A".into()),
            &Outline::parse("- bread is not failures\n- bread failures\n"),
        );
        let results = corpus.search(&Query::parse("bread failures").unwrap());
        assert_eq!(
            labels(&results),
            [("A".to_string(), 1), ("A".to_string(), 0)]
        );
    }

    #[test]
    fn blank_query_is_none() {
        assert_eq!(Query::parse("  \t "), None);
    }

    #[test]
    fn highlights_map_back_through_length_changing_lowercase() {
        // `İ` lowercases to two chars (`i` + combining dot), three bytes
        // folded against two in the original.
        let text = "İstanbul and ISTANBUL";
        let query = Query::parse("stan").unwrap();
        let ranges = query.highlights(text);
        let words: Vec<&str> = ranges.iter().map(|r| &text[r.clone()]).collect();
        assert_eq!(words, ["stan", "STAN"]);
    }

    #[test]
    fn overlapping_highlights_merge() {
        let query = Query::parse("abc bcd").unwrap();
        assert_eq!(query.highlights("xabcdx"), vec![1..5]);
    }

    #[test]
    fn snippet_centres_on_first_match_and_flattens_lines() {
        let text = format!("{}\nneedle here\n{}", "a".repeat(100), "b".repeat(300));
        let query = Query::parse("needle").unwrap();
        let (snippet, ranges) = snippet(&text, &query.highlights(&text));
        assert!(snippet.starts_with('…') && snippet.ends_with('…'));
        assert!(!snippet.contains('\n'));
        assert_eq!(&snippet[ranges[0].clone()], "needle");
    }

    #[test]
    fn short_snippet_is_whole_text() {
        let text = "find me";
        let query = Query::parse("me").unwrap();
        let (snippet, ranges) = snippet(text, &query.highlights(text));
        assert_eq!(snippet, "find me");
        assert_eq!(ranges, vec![5..7]);
    }

    #[test]
    fn target_resolves_by_preorder_after_reparse() {
        let body = "- a\n  - b\n- c\n";
        let mut corpus = SearchCorpus::default();
        corpus.push(&NoteSource::Page("P".into()), &Outline::parse(body));
        let hit = corpus.search(&Query::parse("c").unwrap()).hits.remove(0);
        let reopened = Outline::parse(body);
        let id = hit.target.resolve(&reopened).unwrap();
        assert_eq!(reopened.get(id), Some("c"));
    }

    fn mount(cx: &mut TestAppContext) -> (Entity<SearchView>, &mut gpui::VisualTestContext) {
        let (view, vcx) = cx.add_window_view(|_window, cx| {
            text_input::bind_keys(cx);
            bind_keys(cx);
            SearchView::new(corpus(), cx)
        });
        vcx.update(|window, cx| {
            window.activate_window();
            let input_focus = view.read(cx).input.focus_handle(cx);
            window.focus(&input_focus, cx);
        });
        vcx.run_until_parked();
        (view, vcx)
    }

    struct Recorder {
        events: Vec<SearchEvent>,
        _sub: Subscription,
    }

    #[gpui::test]
    fn typing_down_and_enter_selects_second_hit(cx: &mut TestAppContext) {
        let (view, vcx) = mount(cx);
        let recorder = vcx.update(|_, cx| {
            cx.new(|cx| Recorder {
                events: Vec::new(),
                _sub: cx.subscribe(&view, |this: &mut Recorder, _, event, _| {
                    this.events.push(event.clone());
                }),
            })
        });

        vcx.simulate_input("ETL");
        vcx.run_until_parked();
        view.read_with(vcx, |view, _| assert_eq!(view.results().total, 2));

        vcx.simulate_keystrokes("down enter");
        vcx.run_until_parked();
        recorder.read_with(vcx, |r, _| match r.events.as_slice() {
            [SearchEvent::Selected(target)] => assert_eq!(target.ordinal, 1),
            other => panic!("expected one Selected, got {other:?}"),
        });
    }

    #[gpui::test]
    fn escape_cancels(cx: &mut TestAppContext) {
        let (view, vcx) = mount(cx);
        let recorder = vcx.update(|_, cx| {
            cx.new(|cx| Recorder {
                events: Vec::new(),
                _sub: cx.subscribe(&view, |this: &mut Recorder, _, event, _| {
                    this.events.push(event.clone());
                }),
            })
        });
        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();
        recorder.read_with(vcx, |r, _| {
            assert!(matches!(r.events.as_slice(), [SearchEvent::Cancelled]));
        });
    }
}
