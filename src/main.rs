#![allow(clippy::unreadable_literal)]

use gpui::{
    actions, div, prelude::*, px, rgb, size, App, AppContext, Bounds, Context, Entity, FocusHandle,
    Focusable, IntoElement, KeyBinding, ParentElement, Render, SharedString, Styled, Subscription,
    Window, WindowBounds, WindowOptions,
};
use gpui_notes::page::Page;
use gpui_notes::registry::{set_current_page, CurrentPage, PageRegistry};
use gpui_notes::store::NotesStore;
use gpui_notes::text_input;
use gpui_platform::application;

const DEFAULT_PAGE: &str = "scratch";

actions!(gpui_notes, [SavePage, NextPage]);

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

    #[allow(clippy::unused_self, clippy::needless_pass_by_ref_mut)]
    fn next_page(&mut self, _: &NextPage, _: &mut Window, cx: &mut Context<Self>) {
        let names = match cx.global::<PageRegistry>().list() {
            Ok(names) => names,
            Err(err) => {
                eprintln!("list failed: {err}");
                return;
            }
        };
        if names.is_empty() {
            return;
        }
        let current = cx
            .global::<CurrentPage>()
            .get()
            .map(|p| p.read(cx).name().clone());
        let next = pick_next(&names, current.as_ref());
        if let Err(err) = set_current_page(next.as_ref(), cx) {
            eprintln!("open {next:?} failed: {err}");
        }
    }
}

fn pick_next<'a>(names: &'a [SharedString], current: Option<&SharedString>) -> &'a SharedString {
    let idx = current
        .and_then(|c| names.iter().position(|n| n == c))
        .map_or(0, |i| (i + 1) % names.len());
    &names[idx]
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
        cx.bind_keys([
            KeyBinding::new("cmd-s", SavePage, Some("RootView")),
            KeyBinding::new("cmd-p", NextPage, Some("RootView")),
        ]);

        let root_dir = NotesStore::default_root().expect("resolve notes root");
        let store = NotesStore::new(root_dir).expect("init notes store");
        cx.set_global(PageRegistry::new(store));
        cx.set_global(CurrentPage::default());
        set_current_page(DEFAULT_PAGE, cx).expect("open default page");

        let bounds = Bounds::centered(None, size(px(640.0), px(420.0)), cx);
        let window = cx
            .open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(RootView::new),
            )
            .unwrap();

        window
            .update(cx, |view, window, cx| {
                if let Some(page) = cx.global::<CurrentPage>().get() {
                    let input = page.read(cx).input().clone();
                    window.focus(&input.focus_handle(cx), cx);
                } else {
                    window.focus(&view.focus_handle(cx), cx);
                }
                cx.activate(true);
            })
            .unwrap();
    });
}
