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
}
