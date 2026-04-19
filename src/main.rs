#![allow(clippy::unreadable_literal)]

use gpui::{
    actions, div, prelude::*, px, rgb, size, App, AppContext, Bounds, Context, Entity, FocusHandle,
    Focusable, IntoElement, KeyBinding, ParentElement, Render, Styled, Subscription, Window,
    WindowBounds, WindowOptions,
};
use gpui_notes::journal;
use gpui_notes::page::Page;
use gpui_notes::registry::{pick_next, set_current_page, CurrentPage, PageRegistry};
use gpui_notes::store::NotesStore;
use gpui_notes::text_input;
use gpui_notes::window_frame::WindowFrame;
use gpui_platform::application;

actions!(gpui_notes, [SavePage, NextPage, JumpToToday]);

struct RootView {
    focus_handle: FocusHandle,
    _observer: Subscription,
}

impl RootView {
    fn new(cx: &mut Context<Self>) -> Self {
        let observer = cx.observe_global::<CurrentPage>(|_, cx| cx.notify());
        Self {
            focus_handle: cx.focus_handle(),
            _observer: observer,
        }
    }

    /// Focuses the current page's input, or the root view if no page is open.
    /// Call after any action that swaps `CurrentPage`, otherwise the window is
    /// left without a focused input and keystrokes fall through until the user
    /// clicks back into the textbox.
    fn focus_current(&self, window: &mut Window, cx: &mut App) {
        if let Some(page) = cx.global::<CurrentPage>().get() {
            let input = page.read(cx).input().clone();
            window.focus(&input.focus_handle(cx), cx);
        } else {
            window.focus(&self.focus_handle.clone(), cx);
        }
    }

    #[allow(clippy::unused_self, clippy::needless_pass_by_ref_mut)]
    fn save_current(&mut self, _: &SavePage, _: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = cx.global::<CurrentPage>().get().cloned() else {
            return;
        };
        let result = cx.update_global::<PageRegistry, _>(|reg, cx| reg.save(&page, cx));
        if let Err(err) = result {
            eprintln!("save failed: {err}");
        }
    }

    fn jump_to_today(&mut self, _: &JumpToToday, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = journal::open_today(cx) {
            eprintln!("open today's journal failed: {err}");
            return;
        }
        self.focus_current(window, cx);
    }

    fn next_page(&mut self, _: &NextPage, window: &mut Window, cx: &mut Context<Self>) {
        let names = match cx.global::<PageRegistry>().list() {
            Ok(names) => names,
            Err(err) => {
                eprintln!("list failed: {err}");
                return;
            }
        };
        let current = cx
            .global::<CurrentPage>()
            .get()
            .map(|p| p.read(cx).name().clone());
        let Some(next) = pick_next(&names, current.as_ref()) else {
            return;
        };
        if let Err(err) = set_current_page(next.as_ref(), cx) {
            eprintln!("open {next:?} failed: {err}");
            return;
        }
        self.focus_current(window, cx);
    }
}

impl Focusable for RootView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RootView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current: Option<Entity<Page>> = cx.global::<CurrentPage>().get().cloned();

        let mut root = div()
            .track_focus(&self.focus_handle(cx))
            .key_context("RootView")
            .on_action(cx.listener(Self::save_current))
            .on_action(cx.listener(Self::next_page))
            .on_action(cx.listener(Self::jump_to_today))
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x1e1e1e))
            .p_4()
            .gap_2();
        root.text_style().font_fallbacks = Some(text_input::emoji_font_fallbacks());

        let Some(page) = current else {
            return root.child(div().text_color(rgb(0xcccccc)).child("No page open."));
        };

        let (name, dirty, input) = {
            let p = page.read(cx);
            (p.name().clone(), p.dirty(), p.input().clone())
        };
        let header = if dirty {
            format!("{name} •")
        } else {
            name.to_string()
        };

        root.child(
            div()
                .text_color(rgb(0xcccccc))
                .text_size(px(14.))
                .child(header),
        )
        .child(input)
    }
}

fn main() {
    application().run(|cx: &mut App| {
        text_input::bind_keys(cx);
        let cmd = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        cx.bind_keys([
            KeyBinding::new(&format!("{cmd}-s"), SavePage, Some("RootView")),
            KeyBinding::new(&format!("{cmd}-p"), NextPage, Some("RootView")),
            KeyBinding::new(&format!("{cmd}-."), JumpToToday, Some("RootView")),
        ]);

        let root_dir = NotesStore::default_root().expect("resolve notes root");
        let store = NotesStore::new(root_dir).expect("init notes store");
        cx.set_global(PageRegistry::new(store));
        cx.set_global(CurrentPage::default());
        journal::open_today(cx).expect("open today's journal");

        let bounds = Bounds::centered(None, size(px(640.0), px(420.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let root = cx.new(RootView::new);
                root.update(cx, |view, cx| view.focus_current(window, cx));
                cx.activate(true);
                cx.new(|_| WindowFrame::new("GPUI Notes", root))
            },
        )
        .unwrap();
    });
}
