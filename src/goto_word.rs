//! Helix's `goto_word` (`g w`): label every word on screen with two letters,
//! then type a label to start editing that word's block with the word
//! selected. Any other key, or a click, puts the labels away.
//!
//! Labels need screen positions, which only exist once a frame is laid out.
//! So `g w` starts *collecting*: in the next frame each rendered paragraph
//! records its visible words (see `JumpSource` and `record`), and once that
//! frame is done the root view turns them into `Labels`.
//!
//! Labels go on the rendered text, but the selection is made in the block's
//! source. A rendered word is matched to the next source word with the same
//! text, so markup the renderer hides (`**`, `[[`, a link's URL) is skipped
//! over. A rendered word that matches nothing (`**a**b` renders as one word)
//! gets no label.

use std::ops::Range;

use gpui::{
    actions, anchored, div, point, px, size, App, Bounds, Global, IntoElement, KeyBinding,
    Keystroke, ParentElement, Pixels, SharedString, Size, Styled, TextLayout, WeakEntity, Window,
};

use crate::block_view::BlockView;
use crate::theme;

actions!(goto_word, [GotoWord]);

/// `g w` acts on a focused-but-viewing block: mid-edit, `g` is just a letter.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "g w",
        GotoWord,
        Some("BlockView && !editing"),
    )]);
}

const ALPHABET: &[u8; 26] = b"abcdefghijklmnopqrstuvwxyz";
/// Two letters per label.
const MAX_LABELS: usize = ALPHABET.len() * ALPHABET.len();

/// A word to jump to: the block it lives in and its byte range there.
#[derive(Clone, Debug)]
pub struct Target {
    block: WeakEntity<BlockView>,
    range: Range<usize>,
}

struct Candidate {
    /// Covers the word's first two characters, where its label goes.
    bounds: Bounds<Pixels>,
    target: Target,
}

/// The words recorded by the frame after `g w`. `None` the rest of the time,
/// so paragraphs only do the work when asked.
#[derive(Default)]
struct Collector(Option<Vec<Candidate>>);

impl Global for Collector {}

/// Asks the next frame's paragraphs to record their visible words.
pub fn start_collecting(cx: &mut App) {
    cx.set_global(Collector(Some(Vec::new())));
}

#[must_use]
pub fn is_collecting(cx: &App) -> bool {
    cx.try_global::<Collector>()
        .is_some_and(|collector| collector.0.is_some())
}

/// A block's source, for matching the words of its rendered paragraphs back
/// to it. One per rendered block: paragraphs consume its words in order.
pub struct JumpSource {
    block: WeakEntity<BlockView>,
    source: String,
    words: Vec<Range<usize>>,
    next: usize,
}

impl JumpSource {
    #[must_use]
    pub fn new(block: WeakEntity<BlockView>, source: &str) -> Self {
        Self {
            block,
            source: source.to_string(),
            words: words(source).collect(),
            next: 0,
        }
    }

    /// The labelled words of a rendered paragraph: the first two characters
    /// of each in `display`, which its label covers, and where it is in the
    /// source.
    pub(crate) fn locate(&mut self, display: &str) -> Vec<(Range<usize>, Target)> {
        let mut found = Vec::new();
        for word in words(display) {
            let text = &display[word.clone()];
            let Some(offset) = self.words[self.next..]
                .iter()
                .position(|source| &self.source[source.clone()] == text)
            else {
                continue;
            };
            let range = self.words[self.next + offset].clone();
            self.next += offset + 1;
            let covered = display[word.clone()]
                .char_indices()
                .nth(2)
                .map_or(word.end, |(ix, _)| word.start + ix);
            found.push((
                word.start..covered,
                Target {
                    block: self.block.clone(),
                    range,
                },
            ));
        }
        found
    }
}

/// Records the words of a painted paragraph that are fully on screen. Runs in
/// prepaint, after the paragraph's text is laid out and inside the content
/// mask of whatever scroll container clips it.
pub(crate) fn record(
    layout: &TextLayout,
    words: &[(Range<usize>, Target)],
    window: &mut Window,
    cx: &mut App,
) {
    let visible = window.content_mask().bounds;
    let line_height = layout.line_height();
    let Some(collected) = cx.default_global::<Collector>().0.as_mut() else {
        return;
    };
    for (covered, target) in words {
        if collected.len() == MAX_LABELS {
            return;
        }
        let Some(origin) = layout.position_for_index(covered.start) else {
            continue;
        };
        // A word broken across rows has no single span to cover; its label
        // gets a square.
        let width = layout
            .position_for_index(covered.end)
            .filter(|end| end.y == origin.y && end.x > origin.x)
            .map_or(line_height, |end| end.x - origin.x);
        let bounds = Bounds::new(origin, size(width, line_height));
        let bottom = point(origin.x, origin.y + line_height - px(1.));
        if visible.contains(&origin) && visible.contains(&bottom) {
            collected.push(Candidate {
                bounds,
                target: target.clone(),
            });
        }
    }
}

/// The labels on screen, and the first letter typed, if any.
pub struct Labels {
    labels: Vec<([u8; 2], Candidate)>,
    typed: Option<u8>,
}

/// What a keystroke did to the labels.
pub enum Step {
    /// The first letter of a label: wait for the second.
    Pending,
    Jump(Target),
    Cancel,
}

impl Labels {
    /// Labels the words collected by the last frame, and stops collecting.
    /// `None` when no word was visible.
    #[must_use]
    pub fn take_collected(cx: &mut App) -> Option<Self> {
        let collected = cx.default_global::<Collector>().0.take()?;
        if collected.is_empty() {
            return None;
        }
        Some(Self {
            labels: collected
                .into_iter()
                .enumerate()
                .map(|(i, candidate)| (label(i), candidate))
                .collect(),
            typed: None,
        })
    }

    pub fn key(&mut self, keystroke: &Keystroke) -> Step {
        let letter = match keystroke.key.as_bytes() {
            [letter] if letter.is_ascii_lowercase() && !keystroke.modifiers.modified() => *letter,
            _ => return Step::Cancel,
        };
        let Some(first) = self.typed else {
            if self.labels.iter().any(|(label, _)| label[0] == letter) {
                self.typed = Some(letter);
                return Step::Pending;
            }
            return Step::Cancel;
        };
        self.labels
            .iter()
            .find(|(label, _)| *label == [first, letter])
            .map_or(Step::Cancel, |(_, candidate)| {
                Step::Jump(candidate.target.clone())
            })
    }

    /// Each label still reachable, drawn over its word's first two
    /// characters and no further, so neighbouring labels never overlap.
    /// `mono_font` makes the two letters' width predictable: a monospace
    /// glyph is about 0.6em wide, so two fit the box at its width / 1.2.
    pub fn render(&self, mono_font: SharedString) -> impl Iterator<Item = impl IntoElement> + '_ {
        self.labels
            .iter()
            .filter(|(label, _)| self.typed.is_none_or(|first| label[0] == first))
            .map(move |(label, candidate)| {
                let text = String::from_utf8_lossy(label).into_owned();
                let Size { width, height } = candidate.bounds.size;
                let font_size = (width / 1.2).min(height * 0.8);
                anchored()
                    .position(candidate.bounds.origin)
                    .snap_to_window()
                    .child(
                        div()
                            .w(width)
                            .h(height)
                            .flex()
                            .items_center()
                            .justify_center()
                            .overflow_hidden()
                            .rounded(px(2.))
                            .bg(theme::accent())
                            .text_color(theme::window_bg())
                            .font_family(mono_font.clone())
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_size(font_size)
                            .line_height(height)
                            .child(text),
                    )
            })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.labels.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }
}

/// Starts editing `target`'s block with its word selected.
pub fn jump(target: &Target, window: &mut Window, cx: &mut App) {
    let Some(block) = target.block.upgrade() else {
        return;
    };
    let range = target.range.clone();
    block.update(cx, |block, cx| {
        block.begin_editing_selecting(range, window, cx);
    });
}

/// The `i`th label, in order: `aa`, `ab`, … `az`, `ba`, …
fn label(i: usize) -> [u8; 2] {
    [ALPHABET[i / ALPHABET.len()], ALPHABET[i % ALPHABET.len()]]
}

/// The words of `text` long enough to carry a two-letter label.
fn words(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut chars = text.char_indices().peekable();
    std::iter::from_fn(move || loop {
        let (start, c) = chars.next()?;
        if !is_word(c) {
            continue;
        }
        let mut end = start + c.len_utf8();
        let mut count = 1;
        while let Some(&(ix, c)) = chars.peek() {
            if !is_word(c) {
                break;
            }
            end = ix + c.len_utf8();
            count += 1;
            chars.next();
        }
        if count >= 2 {
            return Some(start..end);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word_texts(text: &str) -> Vec<&str> {
        words(text).map(|range| &text[range]).collect()
    }

    #[test]
    fn words_skip_single_characters_and_punctuation() {
        assert_eq!(
            word_texts("a cat, the_dog's 42 x"),
            ["cat", "the_dog", "42"]
        );
    }

    #[test]
    fn words_handle_multibyte_text() {
        assert_eq!(word_texts("café → naïve"), ["café", "naïve"]);
    }

    #[test]
    fn labels_count_up_in_two_letters() {
        assert_eq!(label(0), *b"aa");
        assert_eq!(label(1), *b"ab");
        assert_eq!(label(26), *b"ba");
        assert_eq!(label(MAX_LABELS - 1), *b"zz");
    }

    fn located(source: &str, paragraphs: &[&str]) -> Vec<String> {
        let mut jump = JumpSource::new(WeakEntity::new_invalid(), source);
        paragraphs
            .iter()
            .flat_map(|display| jump.locate(display))
            .map(|(_, target)| source[target.range].to_string())
            .collect()
    }

    /// Hidden markup, including a link's URL, is skipped; the selection is
    /// the word's place in the source.
    #[test]
    fn rendered_words_map_past_hidden_markup() {
        let source = "**bold** see [docs](https://example.com) then example";
        let mut jump = JumpSource::new(WeakEntity::new_invalid(), source);
        let last = jump.locate("bold see docs then example").pop().unwrap().1;
        let example = source.rfind("example").unwrap();
        assert_eq!(last.range, example..example + "example".len());
    }

    /// Words split across paragraphs keep matching in order, so a repeated
    /// word selects its own occurrence.
    #[test]
    fn repeated_words_select_their_own_occurrence() {
        let source = "one two\n\none three";
        let mut jump = JumpSource::new(WeakEntity::new_invalid(), source);
        jump.locate("one two");
        let second: Vec<_> = jump
            .locate("one three")
            .into_iter()
            .map(|(_, target)| target.range)
            .collect();
        assert_eq!(second, [9..12, 13..18]);
    }

    /// A label covers its word's first two characters, multibyte or not.
    #[test]
    fn labels_cover_the_first_two_characters() {
        let display = "éclair ok";
        let mut jump = JumpSource::new(WeakEntity::new_invalid(), display);
        let covered: Vec<_> = jump
            .locate(display)
            .into_iter()
            .map(|(range, _)| &display[range])
            .collect();
        assert_eq!(covered, ["éc", "ok"]);
    }

    /// A rendered word with no counterpart in the source gets no label.
    #[test]
    fn unmatched_words_are_skipped() {
        assert_eq!(located("**a**b and", &["ab and"]), ["and"]);
    }
}
