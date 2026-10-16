//! Keyboard-shortcut overlay.

use gpui::{div, prelude::*, px, App, SharedString, Window};

use crate::shortcut_hints::ShortcutHint;
use crate::theme;

/// Modal panel listing every binding that was active when it was opened.
#[derive(IntoElement)]
pub struct ShortcutOverlay {
    hints: Vec<ShortcutHint>,
}

impl ShortcutOverlay {
    #[must_use]
    pub fn new(hints: Vec<ShortcutHint>) -> Self {
        Self { hints }
    }
}

impl RenderOnce for ShortcutOverlay {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let rows = self.hints.into_iter().map(|hint| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .min_w(px(112.0))
                        .px_2()
                        .py_0p5()
                        .rounded_sm()
                        .bg(theme::bg_subtle())
                        .text_color(theme::row_selected_fg())
                        .child(SharedString::from(hint.keystroke)),
                )
                .child(
                    div()
                        .text_color(theme::header_fg())
                        .child(SharedString::from(hint.action_name)),
                )
        });

        div()
            .id("shortcut-overlay")
            .flex()
            .flex_col()
            .gap_2()
            .max_h(px(320.0))
            .min_w(px(420.0))
            .bg(theme::overlay_bg())
            .border_1()
            .border_color(theme::overlay_border())
            .rounded_md()
            .p_4()
            .child(
                div()
                    .flex_none()
                    .text_color(theme::overlay_title_fg())
                    .text_size(px(14.0))
                    .child("Keyboard shortcuts"),
            )
            .child(
                div()
                    .id("shortcut-overlay-list")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap_1()
                    .overflow_y_scroll()
                    .children(rows),
            )
    }
}
