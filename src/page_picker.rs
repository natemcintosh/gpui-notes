//! Fuzzy-by-substring page picker overlay. Lists every page name and journal
//! date; the text input narrows the list as the user types. Up/Down navigates
//! the selection; Enter emits `Selected`; Escape (via the input's `Cancel`
//! action) emits `Cancelled`. The parent view (`RootView`) is responsible for
//! mounting/unmounting this view and acting on the events.

use chrono::NaiveDate;
use gpui::{
    actions, div, prelude::*, px, rgb, App, AppContext, Context, ElementId, Entity, EventEmitter,
    FocusHandle, Focusable, IntoElement, KeyBinding, ParentElement, Render, SharedString, Styled,
    Subscription, Window,
};

use crate::text_input::{TextInput, TextInputEvent};

actions!(page_picker, [SelectUp, SelectDown]);

/// Register key bindings for the picker's navigation actions. Call once at
/// startup, alongside `text_input::bind_keys`.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", SelectUp, Some("PagePicker")),
        KeyBinding::new("down", SelectDown, Some("PagePicker")),
    ]);
}

#[derive(Debug, Clone)]
pub enum PageEntry {
    Page(SharedString),
    Journal(NaiveDate),
}

impl PageEntry {
    fn label(&self) -> SharedString {
        match self {
            Self::Page(name) => name.clone(),
            Self::Journal(date) => SharedString::from(date.format("%Y-%m-%d").to_string()),
        }
    }
}

#[derive(Debug, Clone)]
pub enum PagePickerEvent {
    Selected(PageEntry),
    Cancelled,
}

/// Discrete filter buckets for the picker. The continuous filter string is
/// collapsed into these three categories so state-transition tests can assert
/// "this keystroke moved us from X to Y" without depending on the exact
/// substring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilterCategory {
    Empty,
    NonEmptyMatches,
    NonEmptyNoMatches,
}

/// Where the selection cursor sits within the filtered list. `NoSelection`
/// means the filtered list is empty (so there is nothing to highlight); the
/// three positional variants are used to verify arrow-key wrap-around.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionPosition {
    NoSelection,
    First,
    Middle,
    Last,
}

impl EventEmitter<PagePickerEvent> for PagePicker {}

pub struct PagePicker {
    input: Entity<TextInput>,
    all_entries: Vec<PageEntry>,
    filtered: Vec<usize>,
    selected: usize,
    focus_handle: FocusHandle,
    _input_sub: Subscription,
}

impl PagePicker {
    pub fn new(entries: Vec<PageEntry>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| TextInput::new(cx, "Type to filter…"));
        let input_focus = input.focus_handle(cx);
        window.focus(&input_focus, cx);

        let sub = cx.subscribe(&input, |this, _input, event, cx| match event {
            TextInputEvent::Changed(query) => {
                this.recompute_filter(query.as_ref());
                cx.notify();
            }
            TextInputEvent::Submitted => {
                if let Some(entry) = this.current_entry() {
                    cx.emit(PagePickerEvent::Selected(entry));
                }
            }
            TextInputEvent::Cancelled => {
                cx.emit(PagePickerEvent::Cancelled);
            }
        });

        let filtered = (0..entries.len()).collect();
        Self {
            input,
            all_entries: entries,
            filtered,
            selected: 0,
            focus_handle: cx.focus_handle(),
            _input_sub: sub,
        }
    }

    fn recompute_filter(&mut self, query: &str) {
        let needle = query.to_ascii_lowercase();
        self.filtered = self
            .all_entries
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| {
                if needle.is_empty()
                    || entry
                        .label()
                        .as_ref()
                        .to_ascii_lowercase()
                        .contains(&needle)
                {
                    Some(i)
                } else {
                    None
                }
            })
            .collect();
        self.selected = 0;
    }

    fn current_entry(&self) -> Option<PageEntry> {
        self.filtered
            .get(self.selected)
            .and_then(|i| self.all_entries.get(*i))
            .cloned()
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.filtered.len() - 1
        } else {
            self.selected - 1
        };
        cx.notify();
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.filtered.len();
        cx.notify();
    }
}

impl PagePicker {
    /// Returns the underlying filter `TextInput`. Intended for tests that need
    /// to drive the picker without going through the keyboard.
    #[must_use]
    pub fn input(&self) -> Entity<TextInput> {
        self.input.clone()
    }

    /// Categorize the current filter state for state-machine tests. Reads the
    /// `TextInput`'s content so it stays consistent with whatever the user has
    /// typed, including IME composition.
    #[must_use]
    pub fn filter_category(&self, cx: &App) -> FilterCategory {
        let query = self.input.read(cx).content();
        if query.is_empty() {
            FilterCategory::Empty
        } else if self.filtered.is_empty() {
            FilterCategory::NonEmptyNoMatches
        } else {
            FilterCategory::NonEmptyMatches
        }
    }

    /// Categorize the highlighted row's position within the filtered list.
    /// Returns `NoSelection` if the filtered list is empty.
    #[must_use]
    pub fn selection_position(&self) -> SelectionPosition {
        let len = self.filtered.len();
        if len == 0 {
            SelectionPosition::NoSelection
        } else if self.selected == 0 {
            SelectionPosition::First
        } else if self.selected == len - 1 {
            SelectionPosition::Last
        } else {
            SelectionPosition::Middle
        }
    }
}

impl Focusable for PagePicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for PagePicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut list = div().flex().flex_col().gap_0p5();
        if self.filtered.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_color(rgb(0x888888))
                    .child("No matches."),
            );
        } else {
            for (row, idx) in self.filtered.iter().enumerate() {
                let Some(entry) = self.all_entries.get(*idx) else {
                    continue;
                };
                let label = entry.label();
                let is_selected = row == self.selected;
                let entry_for_click = entry.clone();
                let (bg_color, fg_color) = if is_selected {
                    (rgb(0x3a3a3a), rgb(0xffffff))
                } else {
                    (rgb(0x222222), rgb(0xcccccc))
                };
                list = list.child(
                    div()
                        .id(ElementId::Name(SharedString::from(format!(
                            "page-picker-row-{row}"
                        ))))
                        .cursor_pointer()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .bg(bg_color)
                        .text_color(fg_color)
                        .child(label)
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(PagePickerEvent::Selected(entry_for_click.clone()));
                        })),
                );
            }
        }

        div()
            .track_focus(&self.focus_handle)
            .key_context("PagePicker")
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .flex()
            .flex_col()
            .gap_2()
            .bg(rgb(0x141414))
            .border_1()
            .border_color(rgb(0x3a3a3a))
            .rounded_md()
            .p_3()
            .min_w(px(360.0))
            .max_w(px(520.0))
            .child(self.input.clone())
            .child(list)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_input;
    use chrono::NaiveDate;
    use gpui::TestAppContext;

    fn entries() -> Vec<PageEntry> {
        vec![
            PageEntry::Page("Alpha".into()),
            PageEntry::Page("Beta".into()),
            PageEntry::Page("Gamma".into()),
            PageEntry::Journal(NaiveDate::from_ymd_opt(2026, 4, 18).unwrap()),
        ]
    }

    #[gpui::test]
    fn substring_filter_narrows_list(cx: &mut TestAppContext) {
        let (picker, vcx) = cx.add_window_view(|window, cx| {
            text_input::bind_keys(cx);
            bind_keys(cx);
            PagePicker::new(entries(), window, cx)
        });
        vcx.run_until_parked();

        picker.update(vcx, |p, cx| {
            let input = p.input.clone();
            input.update(cx, |i, cx| i.test_replace_all("am", cx));
        });
        vcx.run_until_parked();

        picker.read_with(vcx, |p, _| {
            let labels: Vec<String> = p
                .filtered
                .iter()
                .map(|i| p.all_entries[*i].label().to_string())
                .collect();
            assert_eq!(labels, vec!["Gamma".to_string()]);
        });
    }

    #[gpui::test]
    fn enter_emits_selected(cx: &mut TestAppContext) {
        let (picker, vcx) = cx.add_window_view(|window, cx| {
            text_input::bind_keys(cx);
            bind_keys(cx);
            PagePicker::new(entries(), window, cx)
        });
        vcx.update(|window, _| window.activate_window());
        vcx.run_until_parked();

        let recorder = vcx.update(|_, cx| {
            cx.new(|cx| {
                let sub = cx.subscribe(
                    &picker,
                    |this: &mut SelectionRecorder, _, event: &PagePickerEvent, _| {
                        this.events.push(event.clone());
                    },
                );
                SelectionRecorder {
                    events: Vec::new(),
                    _sub: sub,
                }
            })
        });

        picker.update(vcx, |p, cx| {
            p.input.update(cx, |i, cx| i.test_replace_all("Beta", cx));
        });
        vcx.run_until_parked();

        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();

        recorder.read_with(vcx, |r, _| {
            assert_eq!(r.events.len(), 1);
            match &r.events[0] {
                PagePickerEvent::Selected(PageEntry::Page(name)) => {
                    assert_eq!(name.as_ref(), "Beta");
                }
                other => panic!("expected Selected(Page(Beta)), got {other:?}"),
            }
        });
    }

    #[gpui::test]
    fn down_arrow_moves_selection(cx: &mut TestAppContext) {
        let (picker, vcx) = cx.add_window_view(|window, cx| {
            text_input::bind_keys(cx);
            bind_keys(cx);
            PagePicker::new(entries(), window, cx)
        });
        vcx.update(|window, _| window.activate_window());
        vcx.run_until_parked();

        vcx.simulate_keystrokes("down");
        vcx.run_until_parked();

        picker.read_with(vcx, |p, _| {
            assert_eq!(p.selected, 1);
            match p.current_entry() {
                Some(PageEntry::Page(name)) => assert_eq!(name.as_ref(), "Beta"),
                other => panic!("expected Beta, got {other:?}"),
            }
        });
    }

    struct SelectionRecorder {
        events: Vec<PagePickerEvent>,
        _sub: Subscription,
    }

    // ────────────────────────────────────────────────────────────────────
    // Filter-state transitions. The 3-bucket category enum (Empty,
    // NonEmptyMatches, NonEmptyNoMatches) collapses every filter string
    // into one of three states, so the tests below cover every edge in
    // the state graph.
    // ────────────────────────────────────────────────────────────────────

    fn mount_picker(cx: &mut TestAppContext) -> (Entity<PagePicker>, &mut gpui::VisualTestContext) {
        let (picker, vcx) = cx.add_window_view(|window, cx| {
            text_input::bind_keys(cx);
            bind_keys(cx);
            PagePicker::new(entries(), window, cx)
        });
        vcx.update(|window, _| window.activate_window());
        vcx.run_until_parked();
        (picker, vcx)
    }

    fn replace_filter(picker: &Entity<PagePicker>, vcx: &mut gpui::VisualTestContext, text: &str) {
        vcx.update(|_, cx| {
            picker.update(cx, |p, cx| {
                p.input.update(cx, |i, cx| i.test_replace_all(text, cx));
            });
        });
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn filter_transition_empty_to_matches(cx: &mut TestAppContext) {
        let (picker, vcx) = mount_picker(cx);
        vcx.read(|cx| {
            assert_eq!(picker.read(cx).filter_category(cx), FilterCategory::Empty);
        });

        replace_filter(&picker, vcx, "Beta");

        vcx.read(|cx| {
            assert_eq!(
                picker.read(cx).filter_category(cx),
                FilterCategory::NonEmptyMatches,
            );
            assert_eq!(
                picker.read(cx).selection_position(),
                SelectionPosition::First,
                "selection resets to first on filter change",
            );
        });
    }

    #[gpui::test]
    fn filter_transition_empty_to_no_matches(cx: &mut TestAppContext) {
        let (picker, vcx) = mount_picker(cx);
        replace_filter(&picker, vcx, "zzzzz");
        vcx.read(|cx| {
            assert_eq!(
                picker.read(cx).filter_category(cx),
                FilterCategory::NonEmptyNoMatches,
            );
            assert_eq!(
                picker.read(cx).selection_position(),
                SelectionPosition::NoSelection,
            );
        });
    }

    #[gpui::test]
    fn filter_transition_matches_to_empty(cx: &mut TestAppContext) {
        let (picker, vcx) = mount_picker(cx);
        replace_filter(&picker, vcx, "Beta");
        replace_filter(&picker, vcx, "");
        vcx.read(|cx| {
            assert_eq!(picker.read(cx).filter_category(cx), FilterCategory::Empty);
            assert_eq!(
                picker.read(cx).selection_position(),
                SelectionPosition::First,
            );
        });
    }

    #[gpui::test]
    fn filter_transition_matches_to_matches(cx: &mut TestAppContext) {
        let (picker, vcx) = mount_picker(cx);
        replace_filter(&picker, vcx, "a"); // matches Alpha, Beta, Gamma
        replace_filter(&picker, vcx, "am"); // narrows to Gamma
        vcx.read(|cx| {
            assert_eq!(
                picker.read(cx).filter_category(cx),
                FilterCategory::NonEmptyMatches,
            );
            assert_eq!(
                picker.read(cx).selection_position(),
                SelectionPosition::First,
                "narrowed filter resets selection to first",
            );
        });
    }

    #[gpui::test]
    fn filter_transition_matches_to_no_matches(cx: &mut TestAppContext) {
        let (picker, vcx) = mount_picker(cx);
        replace_filter(&picker, vcx, "Beta");
        replace_filter(&picker, vcx, "Betazz");
        vcx.read(|cx| {
            assert_eq!(
                picker.read(cx).filter_category(cx),
                FilterCategory::NonEmptyNoMatches,
            );
        });
    }

    #[gpui::test]
    fn filter_transition_no_matches_to_matches(cx: &mut TestAppContext) {
        let (picker, vcx) = mount_picker(cx);
        replace_filter(&picker, vcx, "Betazz");
        replace_filter(&picker, vcx, "Beta");
        vcx.read(|cx| {
            assert_eq!(
                picker.read(cx).filter_category(cx),
                FilterCategory::NonEmptyMatches,
            );
            assert_eq!(
                picker.read(cx).selection_position(),
                SelectionPosition::First,
            );
        });
    }

    #[gpui::test]
    fn filter_transition_no_matches_to_empty(cx: &mut TestAppContext) {
        let (picker, vcx) = mount_picker(cx);
        replace_filter(&picker, vcx, "zzz");
        replace_filter(&picker, vcx, "");
        vcx.read(|cx| {
            assert_eq!(picker.read(cx).filter_category(cx), FilterCategory::Empty);
            assert_eq!(
                picker.read(cx).selection_position(),
                SelectionPosition::First,
            );
        });
    }

    // ────────────────────────────────────────────────────────────────────
    // Selection-position transitions. `entries()` has 4 items, so under
    // the empty filter the list has First (Alpha), two Middles (Beta,
    // Gamma), Last (journal date). All arrow-key tests below use that
    // empty-filter list and start from the natural selection position
    // they want to transition out of.
    // ────────────────────────────────────────────────────────────────────

    /// Move the selection to position `target` (0-indexed) by dispatching
    /// the requisite number of `down` keystrokes from the initial First.
    fn move_selection_to(vcx: &mut gpui::VisualTestContext, steps: usize) {
        for _ in 0..steps {
            vcx.simulate_keystrokes("down");
        }
        vcx.run_until_parked();
    }

    #[gpui::test]
    fn selection_first_to_middle_via_down(cx: &mut TestAppContext) {
        let (picker, vcx) = mount_picker(cx);
        vcx.simulate_keystrokes("down");
        vcx.run_until_parked();
        vcx.read(|cx| {
            assert_eq!(
                picker.read(cx).selection_position(),
                SelectionPosition::Middle,
            );
        });
    }

    #[gpui::test]
    fn selection_middle_to_last_via_down(cx: &mut TestAppContext) {
        let (picker, vcx) = mount_picker(cx);
        // 4 entries → indexes 0,1,2,3. Go 0→1 (Middle), then to 3 (Last)
        // takes two more downs (1→2→3).
        move_selection_to(vcx, 3);
        vcx.read(|cx| {
            assert_eq!(
                picker.read(cx).selection_position(),
                SelectionPosition::Last,
            );
        });
    }

    #[gpui::test]
    fn selection_last_to_first_via_down_wraps(cx: &mut TestAppContext) {
        let (picker, vcx) = mount_picker(cx);
        // 0→3 = 3 downs to reach Last
        move_selection_to(vcx, 3);
        vcx.simulate_keystrokes("down");
        vcx.run_until_parked();
        vcx.read(|cx| {
            assert_eq!(
                picker.read(cx).selection_position(),
                SelectionPosition::First,
                "down at Last should wrap to First",
            );
        });
    }

    #[gpui::test]
    fn selection_first_to_last_via_up_wraps(cx: &mut TestAppContext) {
        let (picker, vcx) = mount_picker(cx);
        vcx.simulate_keystrokes("up");
        vcx.run_until_parked();
        vcx.read(|cx| {
            assert_eq!(
                picker.read(cx).selection_position(),
                SelectionPosition::Last,
                "up at First should wrap to Last",
            );
        });
    }

    #[gpui::test]
    fn selection_last_to_middle_via_up(cx: &mut TestAppContext) {
        let (picker, vcx) = mount_picker(cx);
        move_selection_to(vcx, 3);
        vcx.simulate_keystrokes("up");
        vcx.run_until_parked();
        vcx.read(|cx| {
            assert_eq!(
                picker.read(cx).selection_position(),
                SelectionPosition::Middle,
            );
        });
    }

    #[gpui::test]
    fn selection_middle_to_first_via_up(cx: &mut TestAppContext) {
        let (picker, vcx) = mount_picker(cx);
        vcx.simulate_keystrokes("down");
        vcx.run_until_parked();
        vcx.simulate_keystrokes("up");
        vcx.run_until_parked();
        vcx.read(|cx| {
            assert_eq!(
                picker.read(cx).selection_position(),
                SelectionPosition::First,
            );
        });
    }

    /// Escape emits `Cancelled` (complementing `enter_emits_selected`).
    #[gpui::test]
    fn escape_emits_cancelled(cx: &mut TestAppContext) {
        let (picker, vcx) = mount_picker(cx);

        let recorder = vcx.update(|_, cx| {
            cx.new(|cx| {
                let sub = cx.subscribe(
                    &picker,
                    |this: &mut SelectionRecorder, _, event: &PagePickerEvent, _| {
                        this.events.push(event.clone());
                    },
                );
                SelectionRecorder {
                    events: Vec::new(),
                    _sub: sub,
                }
            })
        });

        vcx.simulate_keystrokes("escape");
        vcx.run_until_parked();

        recorder.read_with(vcx, |r, _| {
            assert!(
                matches!(r.events.as_slice(), [PagePickerEvent::Cancelled]),
                "got {:?}",
                r.events,
            );
        });
    }
}
