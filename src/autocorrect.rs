//! User-defined auto-correct for the block editor. The `[autocorrect]` table
//! in `config.toml` maps each typo to its replacement:
//!
//! ```toml
//! [autocorrect]
//! teh = "the"
//! "->" = "→"
//! ```
//!
//! There are no rules until the user adds some, by hand or in
//! [`AutocorrectView`]. `TextInput` corrects a typo when the character after
//! it is typed (see [`ends_word`]) or Enter is pressed, except inside code,
//! links and URLs ([`spellcheck::non_prose_ranges`]).

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use gpui::{
    actions, div, prelude::*, px, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    Global, IntoElement, KeyBinding, ParentElement, Render, Styled, Subscription, Window,
};

use crate::config::{self, Config};
use crate::errors;
use crate::spellcheck;
use crate::text_input::{TextInput, TextInputEvent};
use crate::theme;

/// The rules, and the `config.toml` they're saved to. `None` keeps them in
/// memory only, as in debug builds (see `config::active_config_path`).
#[derive(Debug, Default)]
pub struct Autocorrect {
    rules: BTreeMap<String, String>,
    path: Option<PathBuf>,
}

impl Global for Autocorrect {}

impl Autocorrect {
    #[must_use]
    pub fn new(path: Option<PathBuf>) -> Self {
        Self {
            rules: BTreeMap::new(),
            path,
        }
    }

    /// Reads the rules from `config.toml`; a missing file has none.
    ///
    /// # Errors
    /// If the file can't be read or parsed.
    pub fn load(&mut self) -> io::Result<()> {
        if let Some(path) = &self.path {
            self.rules = Config::load(path)?.autocorrect;
        }
        Ok(())
    }

    #[must_use]
    pub fn rules(&self) -> &BTreeMap<String, String> {
        &self.rules
    }

    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Corrects `typo` to `replacement` from now on, or stops correcting it
    /// when `None`, and saves that to `config.toml`. Only that one key is
    /// written, so rules added to the file by hand while the app runs survive
    /// and are picked up.
    ///
    /// # Errors
    /// If saving fails. The change still holds until the app quits.
    pub fn set(&mut self, typo: &str, replacement: Option<&str>) -> io::Result<()> {
        match replacement {
            Some(replacement) => self.rules.insert(typo.to_owned(), replacement.to_owned()),
            None => self.rules.remove(typo),
        };
        let Some(path) = &self.path else {
            return Ok(());
        };
        let config = config::edit(path, |doc| {
            // Anything but a table there fails `edit`'s validation anyway.
            let rules = doc.entry("autocorrect").or_insert(toml_edit::table());
            if let Some(rules) = rules.as_table_like_mut() {
                match replacement {
                    Some(replacement) => {
                        rules.insert(typo, toml_edit::value(replacement));
                    }
                    None => {
                        rules.remove(typo);
                    }
                }
            }
        })?;
        self.rules = config.autocorrect;
        Ok(())
    }

    /// `text` with the typo that ends at `caret` replaced, and the caret's
    /// new offset, or `None` if no typo ends there. The longest typo wins,
    /// and case matters. A typo starting with a letter or digit must start a
    /// word, so `kteh` keeps its `teh`; one starting with a symbol (`->`) can
    /// follow anything.
    #[must_use]
    pub fn apply(&self, text: &str, caret: usize) -> Option<(String, usize)> {
        let before = &text[..caret];
        let (start, replacement) = self
            .rules
            .iter()
            .filter(|(typo, _)| !typo.is_empty())
            .filter_map(|(typo, replacement)| {
                let rest = before.strip_suffix(typo.as_str())?;
                let starts_word = !typo.starts_with(char::is_alphanumeric)
                    || !rest.ends_with(char::is_alphanumeric);
                starts_word.then_some((rest.len(), replacement))
            })
            .min_by_key(|&(start, _)| start)?;
        let in_code = spellcheck::non_prose_ranges(text)
            .iter()
            .any(|skipped| skipped.start < caret && start < skipped.end);
        if in_code {
            return None;
        }
        let corrected = format!("{}{replacement}{}", &text[..start], &text[caret..]);
        Some((corrected, start + replacement.len()))
    }
}

/// Whether typing `typed` finishes the word before the caret, which is when
/// it gets corrected: true for a single character that isn't a letter or
/// digit, such as a space, punctuation or a newline.
#[must_use]
pub fn ends_word(typed: &str) -> bool {
    let mut chars = typed.chars();
    matches!((chars.next(), chars.next()), (Some(c), None) if !c.is_alphanumeric())
}

actions!(autocorrect, [SwitchField]);

/// Tab and shift-tab move between the overlay's two fields.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("tab", SwitchField, Some("Autocorrect")),
        KeyBinding::new("shift-tab", SwitchField, Some("Autocorrect")),
    ]);
}

#[derive(Debug, Clone)]
pub enum AutocorrectEvent {
    Dismissed,
}

/// The overlay that edits the rules: a typo field and a replacement field,
/// with the rules listed below. Enter saves the rule once both fields are
/// filled, clicking a rule loads it into the fields to change it, and `×`
/// removes it. Escape emits `Dismissed`.
pub struct AutocorrectView {
    typo: Entity<TextInput>,
    replacement: Entity<TextInput>,
    focus_handle: FocusHandle,
    _subs: [Subscription; 2],
}

impl EventEmitter<AutocorrectEvent> for AutocorrectView {}

impl AutocorrectView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let typo = cx.new(|cx| TextInput::new(cx, "typo"));
        let replacement = cx.new(|cx| TextInput::new(cx, "replacement"));
        let subs = [
            cx.subscribe_in(&typo, window, Self::on_input_event),
            cx.subscribe_in(&replacement, window, Self::on_input_event),
        ];
        Self {
            typo,
            replacement,
            focus_handle: cx.focus_handle(),
            _subs: subs,
        }
    }

    pub fn focus_input(&self, window: &mut Window, cx: &mut App) {
        window.focus(&self.typo.focus_handle(cx), cx);
    }

    fn on_input_event(
        &mut self,
        _: &Entity<TextInput>,
        event: &TextInputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            TextInputEvent::Submitted | TextInputEvent::SubmittedNow => self.submit(window, cx),
            TextInputEvent::Cancelled => cx.emit(AutocorrectEvent::Dismissed),
            TextInputEvent::Changed(_) => {}
        }
    }

    /// Saves the rule in the fields, or focuses the field still empty.
    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typo = self.typo.read(cx).content().trim().to_owned();
        let replacement = self.replacement.read(cx).content().trim().to_owned();
        if typo.is_empty() {
            self.focus_input(window, cx);
        } else if replacement.is_empty() {
            window.focus(&self.replacement.focus_handle(cx), cx);
        } else {
            Self::set_rule(&typo, Some(&replacement), cx);
            set_text(&self.typo, "", cx);
            set_text(&self.replacement, "", cx);
            self.focus_input(window, cx);
        }
    }

    fn set_rule(typo: &str, replacement: Option<&str>, cx: &mut Context<Self>) {
        let saved =
            cx.update_global::<Autocorrect, _>(|autocorrect, _| autocorrect.set(typo, replacement));
        if let Err(e) = saved {
            errors::report(format!("auto-correct: save failed: {e}"), cx);
        }
        cx.notify();
    }

    fn switch_field(&mut self, _: &SwitchField, window: &mut Window, cx: &mut Context<Self>) {
        let target = if self.typo.focus_handle(cx).is_focused(window) {
            &self.replacement
        } else {
            &self.typo
        };
        window.focus(&target.focus_handle(cx), cx);
    }

    fn render_rule(
        ix: usize,
        typo: &str,
        replacement: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let (edit_typo, edit_replacement) = (typo.to_owned(), replacement.to_owned());
        let remove_typo = typo.to_owned();
        div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .id(("autocorrect-rule", ix))
                    .debug_selector(move || format!("autocorrect-rule-{ix}"))
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .py_1()
                    .rounded_sm()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::row_hover()))
                    .child(format!("{typo} → {replacement}"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        set_text(&this.typo, &edit_typo, cx);
                        set_text(&this.replacement, &edit_replacement, cx);
                        window.focus(&this.replacement.focus_handle(cx), cx);
                    })),
            )
            .child(
                div()
                    .id(("autocorrect-remove", ix))
                    .debug_selector(move || format!("autocorrect-remove-{ix}"))
                    .px_2()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_color(theme::control_fg())
                    .hover(|style| {
                        style
                            .bg(theme::row_hover())
                            .text_color(theme::control_hover_fg())
                    })
                    .child("×")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        Self::set_rule(&remove_typo, None, cx);
                        this.focus_input(window, cx);
                    })),
            )
    }
}

/// Replaces all of `input`'s text, leaving the caret at the end.
fn set_text(input: &Entity<TextInput>, text: &str, cx: &mut App) {
    input.update(cx, |input, cx| {
        let len = input.content().len();
        input.replace_range(0..len, text, text.len(), cx);
    });
}

impl Focusable for AutocorrectView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AutocorrectView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let autocorrect = cx.global::<Autocorrect>();
        let saved_to = match autocorrect.path() {
            Some(path) => format!("Saved under [autocorrect] in {}.", path.display()),
            None => "Debug builds don't save these to config.toml.".to_owned(),
        };
        let rules: Vec<(String, String)> = autocorrect
            .rules()
            .iter()
            .map(|(typo, replacement)| (typo.clone(), replacement.clone()))
            .collect();

        let mut list = div()
            .id("autocorrect-rules")
            .flex()
            .flex_col()
            .max_h(px(300.))
            .overflow_y_scroll();
        if rules.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .text_color(theme::fg_muted())
                    .child("No rules yet."),
            );
        }
        for (ix, (typo, replacement)) in rules.iter().enumerate() {
            list = list.child(Self::render_rule(ix, typo, replacement, cx));
        }

        div()
            .track_focus(&self.focus_handle)
            .key_context("Autocorrect")
            .on_action(cx.listener(Self::switch_field))
            .flex()
            .flex_col()
            .gap_2()
            .w(px(480.))
            .p_3()
            .bg(theme::overlay_bg())
            .border_1()
            .border_color(theme::overlay_border())
            .rounded_md()
            .text_color(theme::header_fg())
            .child(
                div()
                    .text_color(theme::overlay_title_fg())
                    .child("Auto-correct"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().min_w_0().child(self.typo.clone()))
                    .child("→")
                    .child(div().flex_1().min_w_0().child(self.replacement.clone())),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme::fg_muted())
                    .child(format!(
                        "A typo is replaced when you finish the word. Enter saves; \
                         click a rule to change it. {saved_to}"
                    )),
            )
            .child(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn rules(pairs: &[(&str, &str)]) -> Autocorrect {
        let mut autocorrect = Autocorrect::new(None);
        for (typo, replacement) in pairs {
            autocorrect.set(typo, Some(replacement)).unwrap();
        }
        autocorrect
    }

    /// `text` has a `|` where the caret is; the result marks the new caret
    /// the same way.
    fn corrected(autocorrect: &Autocorrect, text: &str) -> Option<String> {
        let caret = text.find('|').unwrap();
        let text = text.replacen('|', "", 1);
        let (mut out, caret) = autocorrect.apply(&text, caret)?;
        out.insert(caret, '|');
        Some(out)
    }

    #[rstest]
    #[case::at_the_end("so teh|", Some("so the|"))]
    #[case::alone("teh|", Some("the|"))]
    #[case::before_more_text("teh| cat", Some("the| cat"))]
    #[case::after_punctuation("(teh|)", Some("(the|)"))]
    #[case::not_mid_word("kteh|", None)]
    #[case::case_sensitive("Teh|", None)]
    #[case::not_before_the_caret("teh x|", None)]
    #[case::symbol_after_a_word("wait...|", Some("wait…|"))]
    #[case::longest_wins("a -->|", Some("a ⟶|"))]
    #[case::multibyte("é teh|", Some("é the|"))]
    #[case::not_in_inline_code("`teh|`", None)]
    #[case::not_in_a_link("[[teh|]]", None)]
    #[case::not_in_a_fence("```\nteh|", None)]
    #[case::after_inline_code("`x` teh|", Some("`x` the|"))]
    fn apply(#[case] text: &str, #[case] expected: Option<&str>) {
        let autocorrect = rules(&[("teh", "the"), ("...", "…"), ("->", "→"), ("-->", "⟶")]);
        assert_eq!(corrected(&autocorrect, text).as_deref(), expected);
    }

    #[test]
    fn no_rules_corrects_nothing() {
        assert_eq!(corrected(&Autocorrect::default(), "teh|"), None);
    }

    #[test]
    fn an_empty_typo_never_matches() {
        assert_eq!(corrected(&rules(&[("", "x")]), "a|"), None);
    }

    #[rstest]
    #[case(" ", true)]
    #[case(".", true)]
    #[case("\n", true)]
    #[case("a", false)]
    #[case("7", false)]
    #[case("é", false)]
    #[case("ab", false)]
    #[case("", false)]
    fn ends_word_cases(#[case] typed: &str, #[case] expected: bool) {
        assert_eq!(ends_word(typed), expected);
    }

    #[test]
    fn set_saves_one_rule_and_picks_up_hand_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let mut autocorrect = Autocorrect::new(Some(path.clone()));
        autocorrect.load().unwrap();
        autocorrect.set("teh", Some("the")).unwrap();
        // Added by hand while the app runs.
        let text = std::fs::read_to_string(&path).unwrap() + "adn = \"and\"\n";
        std::fs::write(&path, text).unwrap();

        autocorrect.set("teh", None).unwrap();

        let expected = BTreeMap::from([("adn".to_owned(), "and".to_owned())]);
        assert_eq!(autocorrect.rules(), &expected);
        assert_eq!(Config::load(&path).unwrap().autocorrect, expected);
    }
}
