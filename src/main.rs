#![allow(clippy::unreadable_literal)]

use gpui::{
    actions, div, prelude::*, px, rgb, size, App, AppContext, Bounds, Context, ElementId, Entity,
    FocusHandle, Focusable, IntoElement, KeyBinding, MouseButton, ParentElement, Render,
    SharedString, Styled, Subscription, Window, WindowBounds, WindowOptions,
};
use gpui_notes::journal;
use gpui_notes::page::Page;
use gpui_notes::page_picker::{self, PageEntry, PagePicker, PagePickerEvent};
use gpui_notes::registry::{pick_next, set_current_page, CurrentPage, PageRegistry};
use gpui_notes::store::NotesStore;
use gpui_notes::tags::{self, OpenTag, TagIndex, TagSource};
use gpui_notes::text_input;
use gpui_notes::window_frame::WindowFrame;
use gpui_platform::application;

actions!(
    gpui_notes,
    [
        SavePage,
        NextPage,
        JumpToToday,
        CloseTagView,
        OpenPagePicker
    ]
);

struct RootView {
    focus_handle: FocusHandle,
    active_tag: Option<SharedString>,
    picker: Option<Entity<PagePicker>>,
    picker_sub: Option<Subscription>,
    _current_observer: Subscription,
    _tag_observer: Subscription,
}

impl RootView {
    fn new(cx: &mut Context<Self>) -> Self {
        let current_observer = cx.observe_global::<CurrentPage>(|_, cx| cx.notify());
        let tag_observer = cx.observe_global::<TagIndex>(|_, cx| cx.notify());
        Self {
            focus_handle: cx.focus_handle(),
            active_tag: None,
            picker: None,
            picker_sub: None,
            _current_observer: current_observer,
            _tag_observer: tag_observer,
        }
    }

    /// Focuses the first block of the current page, or the root view if no
    /// page is open. Call after any action that swaps `CurrentPage`, otherwise
    /// the window is left without a focused block and keystrokes fall through
    /// until the user clicks one.
    fn focus_current(&self, window: &mut Window, cx: &mut App) {
        if let Some(page) = cx.global::<CurrentPage>().get().cloned() {
            let view = page.read(cx).view().clone();
            view.update(cx, |v, cx| v.focus_first_block(window, cx));
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
        self.active_tag = None;
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
        self.active_tag = None;
        self.focus_current(window, cx);
    }

    fn open_tag(&mut self, action: &OpenTag, window: &mut Window, cx: &mut Context<Self>) {
        Self::reindex_current_page(cx);
        self.active_tag = Some(action.name.clone());
        // Park focus on RootView so Escape dispatches `CloseTagView` instead of
        // a stale TextInput::Cancel from the previously-edited block.
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    fn close_tag_view(&mut self, _: &CloseTagView, window: &mut Window, cx: &mut Context<Self>) {
        if self.active_tag.take().is_some() {
            self.focus_current(window, cx);
            cx.notify();
        }
    }

    fn open_page_picker(
        &mut self,
        _: &OpenPagePicker,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.picker.is_some() {
            return;
        }
        let entries = match Self::collect_picker_entries(cx) {
            Ok(entries) => entries,
            Err(err) => {
                eprintln!("page picker: list failed: {err}");
                return;
            }
        };
        let picker = cx.new(|cx| PagePicker::new(entries, window, cx));
        let sub = cx.subscribe_in(
            &picker,
            window,
            |this, _picker, event: &PagePickerEvent, window, cx| match event {
                PagePickerEvent::Selected(entry) => {
                    this.handle_picker_selection(entry, window, cx);
                }
                PagePickerEvent::Cancelled => {
                    this.close_picker(window, cx);
                }
            },
        );
        self.picker = Some(picker);
        self.picker_sub = Some(sub);
        cx.notify();
    }

    fn collect_picker_entries(cx: &App) -> std::io::Result<Vec<PageEntry>> {
        let registry = cx.global::<PageRegistry>();
        let mut entries: Vec<PageEntry> =
            registry.list()?.into_iter().map(PageEntry::Page).collect();
        let mut journals = registry.list_journals()?;
        journals.sort();
        journals.reverse();
        entries.extend(journals.into_iter().map(PageEntry::Journal));
        Ok(entries)
    }

    fn handle_picker_selection(
        &mut self,
        entry: &PageEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = match entry {
            PageEntry::Page(name) => set_current_page(name.as_ref(), cx),
            PageEntry::Journal(date) => journal::open_for_date(*date, cx).map(|_| ()),
        };
        if let Err(err) = result {
            eprintln!("page picker: open failed: {err}");
        }
        self.active_tag = None;
        self.close_picker(window, cx);
    }

    fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.picker = None;
        self.picker_sub = None;
        self.focus_current(window, cx);
        cx.notify();
    }

    fn reindex_current_page(cx: &mut Context<Self>) {
        let Some(page) = cx.global::<CurrentPage>().get().cloned() else {
            return;
        };
        let source =
            cx.update_global::<PageRegistry, TagSource>(|reg, cx| reg.source_for_page(&page, cx));
        tags::reindex_global_for_page(&page, &source, cx);
    }

    fn open_tag_hit(&mut self, source: &TagSource, window: &mut Window, cx: &mut Context<Self>) {
        let result = match source {
            TagSource::Page(name) => set_current_page(name.as_ref(), cx),
            TagSource::Journal(date) => journal::open_for_date(*date, cx).map(|_| ()),
        };
        if let Err(err) = result {
            eprintln!("open tag result failed: {err}");
            return;
        }
        self.active_tag = None;
        self.focus_current(window, cx);
        cx.notify();
    }

    fn render_tag_results(tag: &gpui::SharedString, cx: &mut Context<Self>) -> gpui::AnyElement {
        let hits = cx.global::<TagIndex>().blocks_for_tag(tag.as_ref());
        if hits.is_empty() {
            return div()
                .flex_1()
                .text_color(rgb(0x999999))
                .child("No blocks found.")
                .into_any_element();
        }

        let mut list = div().flex().flex_col().gap_1().flex_1();
        for hit in hits {
            let source = hit.source.clone();
            let label = tags::source_label(&hit.source);
            let preview = tags::truncated_preview(hit.preview.as_ref());
            list = list.child(
                div()
                    .id(gpui::ElementId::Name(gpui::SharedString::from(format!(
                        "tag-hit-{:?}",
                        hit.block_id
                    ))))
                    .cursor_pointer()
                    .rounded_sm()
                    .px_2()
                    .py_1()
                    .min_h(px(28.0))
                    .text_color(rgb(0xdddddd))
                    .hover(|style| style.bg(rgb(0x2a2a2a)))
                    .child(format!("{label}: {preview}"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_tag_hit(&source, window, cx);
                    })),
            );
        }
        list.into_any_element()
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
            .on_action(cx.listener(Self::open_tag))
            .on_action(cx.listener(Self::close_tag_view))
            .on_action(cx.listener(Self::open_page_picker))
            .relative()
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

        let (name, dirty, view) = {
            let p = page.read(cx);
            (p.name().clone(), p.dirty(), p.view().clone())
        };
        let active_tag = self.active_tag.clone();
        let header_text = if let Some(tag) = active_tag.as_ref() {
            format!("#{}", tag.as_ref())
        } else if dirty {
            format!("{name} •")
        } else {
            name.to_string()
        };

        let mut header_row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .text_color(rgb(0xcccccc))
            .text_size(px(14.))
            .child(div().child(header_text));
        if active_tag.is_some() {
            header_row = header_row.child(
                div()
                    .id(ElementId::Name(SharedString::from("close-tag-view")))
                    .px_2()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_color(rgb(0x888888))
                    .hover(|style| style.bg(rgb(0x2a2a2a)).text_color(rgb(0xdddddd)))
                    .child("× close")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.close_tag_view(&CloseTagView, window, cx);
                        }),
                    ),
            );
        }

        let root = root.child(header_row);

        let body = if let Some(tag) = active_tag {
            root.child(Self::render_tag_results(&tag, cx))
        } else {
            root.child(view)
        };

        if let Some(picker) = self.picker.clone() {
            body.child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full()
                    .flex()
                    .items_start()
                    .justify_center()
                    .pt_16()
                    .child(picker),
            )
        } else {
            body
        }
    }
}

fn main() {
    application().run(|cx: &mut App| {
        text_input::bind_keys(cx);
        page_picker::bind_keys(cx);
        let cmd = if cfg!(target_os = "macos") {
            "cmd"
        } else {
            "ctrl"
        };
        cx.bind_keys([
            KeyBinding::new(&format!("{cmd}-s"), SavePage, Some("RootView")),
            KeyBinding::new(&format!("{cmd}-p"), NextPage, Some("RootView")),
            KeyBinding::new(&format!("{cmd}-."), JumpToToday, Some("RootView")),
            KeyBinding::new(&format!("{cmd}-o"), OpenPagePicker, Some("RootView")),
            KeyBinding::new("escape", CloseTagView, Some("RootView")),
        ]);

        let root_dir = NotesStore::default_root().expect("resolve notes root");
        let store = NotesStore::new(root_dir).expect("init notes store");
        let tag_index = TagIndex::rebuild_from(&store).expect("index tags");
        cx.set_global(tag_index);
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, Modifiers, TestAppContext, VisualTestContext};
    use tempfile::TempDir;

    fn mount_root<'a>(
        cx: &'a mut TestAppContext,
        tmp: &TempDir,
    ) -> (Entity<RootView>, &'a mut VisualTestContext) {
        let root_dir = tmp.path().to_path_buf();
        let (root, vcx) = cx.add_window_view(move |_window, cx| {
            text_input::bind_keys(cx);
            page_picker::bind_keys(cx);
            cx.bind_keys([
                KeyBinding::new("escape", CloseTagView, Some("RootView")),
                KeyBinding::new("ctrl-o", OpenPagePicker, Some("RootView")),
            ]);
            let store = NotesStore::new(&root_dir).unwrap();
            store.write("Home", "- home\n").unwrap();
            store.write("Tagged", "- has #todo\n").unwrap();
            cx.set_global(TagIndex::rebuild_from(&store).unwrap());
            cx.set_global(PageRegistry::new(store));
            cx.set_global(CurrentPage::default());
            set_current_page("Home", cx).unwrap();
            RootView::new(cx)
        });
        vcx.update(|window, cx| {
            window.activate_window();
            root.update(cx, |view, cx| view.focus_current(window, cx));
        });
        vcx.run_until_parked();
        (root, vcx)
    }

    #[gpui::test]
    fn tag_results_row_click_opens_page(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.open_tag(
                    &OpenTag {
                        name: "todo".into(),
                    },
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();

        cx.read(|cx| {
            assert_eq!(
                root.read(cx).active_tag.as_ref().map(AsRef::as_ref),
                Some("todo"),
            );
        });

        cx.simulate_click(point(px(24.0), px(50.0)), Modifiers::default());
        cx.run_until_parked();

        cx.read(|cx| {
            let current = cx.global::<CurrentPage>().get().unwrap();
            assert_eq!(current.read(cx).name().as_ref(), "Tagged");
            assert!(root.read(cx).active_tag.is_none());
        });
    }

    /// Escape on the tag-results view returns to the previous page. Regression
    /// for issue #34: previously the only exits from the tag view were
    /// clicking a result row or cycling pages.
    #[gpui::test]
    fn escape_closes_tag_view(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.open_tag(
                    &OpenTag {
                        name: "todo".into(),
                    },
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();
        assert!(cx.read(|cx| root.read(cx).active_tag.is_some()));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        cx.read(|cx| {
            assert!(
                root.read(cx).active_tag.is_none(),
                "escape should close the tag-results view",
            );
            let current = cx.global::<CurrentPage>().get().unwrap();
            assert_eq!(current.read(cx).name().as_ref(), "Home");
        });
    }

    /// Opening the picker, typing a filter, and pressing Enter switches to the
    /// matching page. Covers acceptance criterion 2 of issue #34.
    #[gpui::test]
    fn page_picker_filter_and_enter_switches_page(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("ctrl-o");
        cx.run_until_parked();

        cx.read(|cx| {
            assert!(
                root.read(cx).picker.is_some(),
                "picker mounted after ctrl-o",
            );
        });

        cx.simulate_input("Tagg");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        cx.read(|cx| {
            let current = cx.global::<CurrentPage>().get().unwrap();
            assert_eq!(current.read(cx).name().as_ref(), "Tagged");
            assert!(root.read(cx).picker.is_none(), "picker dismissed on select");
        });
    }
}
