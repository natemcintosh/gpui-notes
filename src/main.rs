#![allow(clippy::unreadable_literal)]

use gpui::{
    div, prelude::*, px, rgb, size, App, AppContext, Bounds, Context, Entity, FocusHandle,
    Focusable, IntoElement, ParentElement, Render, Styled, Window, WindowBounds, WindowOptions,
};
use gpui_notes::text_input::{self, TextInput, TextInputEvent};
use gpui_platform::application;

struct Demo {
    input: Entity<TextInput>,
    focus_handle: FocusHandle,
    _subscription: gpui::Subscription,
}

impl Focusable for Demo {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Demo {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut root = div()
            .track_focus(&self.focus_handle(cx))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x1e1e1e))
            .p_4()
            .gap_2();
        // Inherit emoji fallbacks to every descendant's text style.
        root.text_style().font_fallbacks = Some(text_input::emoji_font_fallbacks());
        root.child(
            div()
                .text_color(rgb(0xcccccc))
                .text_size(px(12.))
                .child("TextInput demo — Changed/Submitted events log to stderr."),
        )
        .child(self.input.clone())
    }
}

fn main() {
    application().run(|cx: &mut App| {
        text_input::bind_keys(cx);

        let bounds = Bounds::centered(None, size(px(480.0), px(140.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| {
                    cx.new(|cx| {
                        let input = cx.new(|cx| TextInput::new(cx, "Type here..."));
                        let sub =
                            cx.subscribe(&input, |_: &mut Demo, _, event: &TextInputEvent, _| {
                                match event {
                                    TextInputEvent::Changed(s) => eprintln!("changed: {s:?}"),
                                    TextInputEvent::Submitted => eprintln!("submitted"),
                                }
                            });
                        Demo {
                            input,
                            focus_handle: cx.focus_handle(),
                            _subscription: sub,
                        }
                    })
                },
            )
            .unwrap();

        window
            .update(cx, |view, window, cx| {
                window.focus(&view.input.focus_handle(cx), cx);
                cx.activate(true);
            })
            .unwrap();
    });
}
