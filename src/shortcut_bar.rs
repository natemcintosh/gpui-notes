//! Keyboard-shortcut status bar and complete overlay.

use gpui::{div, prelude::*, px, App, Context, SharedString, Subscription, Window};

use crate::shortcut_hints::{self, ShortcutHint};
use crate::theme;

const STATUS_HINT_LIMIT: usize = 4;

/// Thin, persistent row showing the highest-priority shortcuts at the current
/// focus target.
pub struct ShortcutBar {
    dispatch_ready: bool,
    /// `observe_pending_input` is also notified by GPUI on focus changes.
    _focus_subscription: Subscription,
}

impl ShortcutBar {
    #[must_use]
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_subscription = cx.observe_pending_input(window, |_, window, cx| {
            // A newly-focused element may not enter the rendered dispatch tree
            // until this frame completes. Re-render once that tree is current.
            cx.on_next_frame(window, |this, _, cx| {
                this.dispatch_ready = true;
                cx.notify();
            });
        });
        // The first render precedes the window's first completed dispatch tree.
        cx.on_next_frame(window, |this, _, cx| {
            this.dispatch_ready = true;
            cx.notify();
        });

        Self {
            dispatch_ready: false,
            _focus_subscription: focus_subscription,
        }
    }
}

impl Render for ShortcutBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let hints = if self.dispatch_ready {
            shortcut_hints::for_focused(window, cx)
        } else {
            Vec::new()
        };

        div()
            .id("shortcut-bar")
            .flex()
            .flex_row()
            .flex_none()
            .items_center()
            .gap_3()
            .h(px(24.0))
            .px_2()
            .overflow_hidden()
            .whitespace_nowrap()
            .border_t_1()
            .border_color(theme::overlay_border())
            .bg(theme::overlay_bg())
            .text_color(theme::fg_muted())
            .text_size(px(12.0))
            .children(hints.iter().take(STATUS_HINT_LIMIT).map(render_hint))
    }
}

fn render_hint(hint: &ShortcutHint) -> impl IntoElement {
    div().flex_none().child(SharedString::from(format!(
        "{} → {}",
        hint.keystroke, hint.action_name
    )))
}

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
