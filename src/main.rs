#![allow(clippy::unreadable_literal)]

use gpui::{
    div, rgb, App, AppContext, Context, IntoElement, ParentElement, Render, SharedString, Styled,
    Window, WindowOptions,
};
use gpui_platform::application;

struct HelloWorld {
    text: SharedString,
}

impl Render for HelloWorld {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .bg(rgb(0x2e7d32))
            .size_full()
            .justify_center()
            .items_center()
            .text_xl()
            .text_color(rgb(0xffffff))
            .child(format!("Hello, {}!", &self.text))
    }
}

fn main() {
    application().run(|cx: &mut App| {
        cx.open_window(WindowOptions::default(), |_, cx| {
            cx.new(|_cx| HelloWorld {
                text: "World".into(),
            })
        })
        .unwrap();
    });
}

#[cfg(test)]
mod tests {
    use super::HelloWorld;
    use gpui::{AppContext, TestAppContext};

    #[gpui::test]
    fn hello_world_holds_text(cx: &mut TestAppContext) {
        let view = cx.new(|_| HelloWorld {
            text: "World".into(),
        });
        cx.read_entity(&view, |v, _| assert_eq!(v.text.as_ref(), "World"));
    }
}
