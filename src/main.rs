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
        OpenPagePicker,
        ToggleShortcuts
    ]
);

/// Static cheatsheet shown in the shortcuts overlay. The strings here are the
/// only place the bindings are documented to the user — keep in sync with the
/// `cx.bind_keys(...)` block in `main`.
const SHORTCUTS: &[(&str, &str)] = &[
    ("ctrl-o", "Open page…"),
    ("ctrl-p", "Cycle to next page"),
    ("ctrl-.", "Jump to today's journal"),
    ("ctrl-s", "Save current page"),
    ("escape", "Close overlay / tag view"),
    ("?", "Show this help"),
];

/// The four mutually-exclusive top-level UI modes. Encoding them as a single
/// enum keeps overlay invariants (only one open at a time) unrepresentable as
/// invalid states, and gives transition tests a single field to assert on.
///
/// `TagResults` swaps out the page body; `PagePicker` and `ShortcutsHelp` are
/// absolute-positioned overlays. The enum collapses both into the same axis
/// so that opening any one always dismisses the others — see plan
/// `/home/natemcintosh/.claude/plans/there-seem-to-be-abundant-peach.md`.
pub enum OverlayMode {
    None,
    TagResults(SharedString),
    PagePicker {
        entity: Entity<PagePicker>,
        _sub: Subscription,
    },
    ShortcutsHelp,
}

impl OverlayMode {
    #[must_use]
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    #[must_use]
    pub fn tag_name(&self) -> Option<&SharedString> {
        if let Self::TagResults(name) = self {
            Some(name)
        } else {
            None
        }
    }

    #[must_use]
    pub fn picker(&self) -> Option<&Entity<PagePicker>> {
        if let Self::PagePicker { entity, .. } = self {
            Some(entity)
        } else {
            None
        }
    }

    #[must_use]
    pub fn is_shortcuts(&self) -> bool {
        matches!(self, Self::ShortcutsHelp)
    }
}

struct RootView {
    focus_handle: FocusHandle,
    overlay: OverlayMode,
    _current_observer: Subscription,
    _tag_observer: Subscription,
}

impl RootView {
    fn new(cx: &mut Context<Self>) -> Self {
        let current_observer = cx.observe_global::<CurrentPage>(|_, cx| cx.notify());
        let tag_observer = cx.observe_global::<TagIndex>(|_, cx| cx.notify());
        Self {
            focus_handle: cx.focus_handle(),
            overlay: OverlayMode::None,
            _current_observer: current_observer,
            _tag_observer: tag_observer,
        }
    }

    #[cfg(test)]
    pub(crate) fn overlay(&self) -> &OverlayMode {
        &self.overlay
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
        self.overlay = OverlayMode::None;
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
        self.overlay = OverlayMode::None;
        self.focus_current(window, cx);
    }

    fn open_tag(&mut self, action: &OpenTag, window: &mut Window, cx: &mut Context<Self>) {
        Self::reindex_current_page(cx);
        self.overlay = OverlayMode::TagResults(action.name.clone());
        // Park focus on RootView so Escape dispatches `CloseTagView` instead of
        // a stale TextInput::Cancel from the previously-edited block.
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Escape handler. Closes whichever overlay is currently active and
    /// returns to the underlying page. The page picker dismisses itself via
    /// `TextInputEvent::Cancelled` (its `TextInput` owns focus), so we only
    /// see `TagResults` / `ShortcutsHelp` here.
    fn close_tag_view(&mut self, _: &CloseTagView, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, OverlayMode::None) {
            return;
        }
        self.overlay = OverlayMode::None;
        self.focus_current(window, cx);
        cx.notify();
    }

    fn toggle_shortcuts(
        &mut self,
        _: &ToggleShortcuts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.overlay, OverlayMode::ShortcutsHelp) {
            self.overlay = OverlayMode::None;
            self.focus_current(window, cx);
        } else {
            self.overlay = OverlayMode::ShortcutsHelp;
            // Park focus on the root so Escape lands on `CloseTagView` (which
            // also dismisses the overlay) instead of a stale TextInput.
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    fn render_header(
        header_text: String,
        has_active_tag: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .text_color(rgb(0xcccccc))
            .text_size(px(14.))
            .child(div().flex_1().child(header_text))
            .child(
                div()
                    .id(ElementId::Name(SharedString::from("show-shortcuts")))
                    .px_2()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_color(rgb(0x888888))
                    .hover(|style| style.bg(rgb(0x2a2a2a)).text_color(rgb(0xdddddd)))
                    .child("?")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.toggle_shortcuts(&ToggleShortcuts, window, cx);
                        }),
                    ),
            );
        if has_active_tag {
            row = row.child(
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
        row.into_any_element()
    }

    fn render_shortcuts_overlay() -> gpui::AnyElement {
        let mut list = div().flex().flex_col().gap_1();
        for (keys, label) in SHORTCUTS {
            list = list.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .min_w(px(96.0))
                            .px_2()
                            .py_0p5()
                            .rounded_sm()
                            .bg(rgb(0x2a2a2a))
                            .text_color(rgb(0xffffff))
                            .child(SharedString::from(*keys)),
                    )
                    .child(
                        div()
                            .text_color(rgb(0xcccccc))
                            .child(SharedString::from(*label)),
                    ),
            );
        }

        div()
            .flex()
            .flex_col()
            .gap_2()
            .bg(rgb(0x141414))
            .border_1()
            .border_color(rgb(0x3a3a3a))
            .rounded_md()
            .p_4()
            .min_w(px(360.0))
            .child(
                div()
                    .text_color(rgb(0xeeeeee))
                    .text_size(px(14.))
                    .child("Keyboard shortcuts"),
            )
            .child(list)
            .into_any_element()
    }

    fn open_page_picker(
        &mut self,
        _: &OpenPagePicker,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.overlay, OverlayMode::PagePicker { .. }) {
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
        self.overlay = OverlayMode::PagePicker {
            entity: picker,
            _sub: sub,
        };
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
        self.close_picker(window, cx);
    }

    fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = OverlayMode::None;
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
        self.overlay = OverlayMode::None;
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
            .on_action(cx.listener(Self::toggle_shortcuts))
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
        let active_tag = self.overlay.tag_name().cloned();
        let header_text = if let Some(tag) = active_tag.as_ref() {
            format!("#{}", tag.as_ref())
        } else if dirty {
            format!("{name} •")
        } else {
            name.to_string()
        };

        let root = root.child(Self::render_header(header_text, active_tag.is_some(), cx));

        let body = if let Some(tag) = active_tag {
            root.child(Self::render_tag_results(&tag, cx))
        } else {
            root.child(view)
        };

        let overlay_child: Option<gpui::AnyElement> = match &self.overlay {
            OverlayMode::PagePicker { entity, .. } => Some(entity.clone().into_any_element()),
            OverlayMode::ShortcutsHelp => Some(Self::render_shortcuts_overlay()),
            OverlayMode::None | OverlayMode::TagResults(_) => None,
        };

        if let Some(child) = overlay_child {
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
                    .child(child),
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
            KeyBinding::new("shift-/", ToggleShortcuts, Some("RootView")),
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
                KeyBinding::new("ctrl-p", NextPage, Some("RootView")),
                KeyBinding::new("ctrl-.", JumpToToday, Some("RootView")),
                KeyBinding::new("ctrl-s", SavePage, Some("RootView")),
                KeyBinding::new("shift-/", ToggleShortcuts, Some("RootView")),
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
                root.read(cx).overlay().tag_name().map(AsRef::as_ref),
                Some("todo"),
            );
        });

        cx.simulate_click(point(px(24.0), px(50.0)), Modifiers::default());
        cx.run_until_parked();

        cx.read(|cx| {
            let current = cx.global::<CurrentPage>().get().unwrap();
            assert_eq!(current.read(cx).name().as_ref(), "Tagged");
            assert!(root.read(cx).overlay().is_none());
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
        assert!(cx.read(|cx| root.read(cx).overlay().tag_name().is_some()));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        cx.read(|cx| {
            assert!(
                root.read(cx).overlay().is_none(),
                "escape should close the tag-results view",
            );
            let current = cx.global::<CurrentPage>().get().unwrap();
            assert_eq!(current.read(cx).name().as_ref(), "Home");
        });
    }

    /// `?` toggles the shortcuts overlay; Escape dismisses it without
    /// affecting the active tag view.
    #[gpui::test]
    fn shortcuts_overlay_toggles(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("shift-/");
        cx.run_until_parked();
        assert!(cx.read(|cx| root.read(cx).overlay().is_shortcuts()));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.read(|cx| root.read(cx).overlay().is_none()));
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
                root.read(cx).overlay().picker().is_some(),
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
            assert!(
                root.read(cx).overlay().is_none(),
                "picker dismissed on select",
            );
        });
    }

    // ────────────────────────────────────────────────────────────────────
    // OverlayMode state-transition tests. One test per discrete transition
    // listed in the plan; see /home/natemcintosh/.claude/plans/there-
    // seem-to-be-abundant-peach.md. The shape is always: set the
    // precondition, fire a simulated input, assert the resulting variant.
    // ────────────────────────────────────────────────────────────────────

    /// `None` → `PagePicker` via Ctrl-O.
    #[gpui::test]
    fn transition_none_to_picker(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);
        assert!(cx.read(|cx| root.read(cx).overlay().is_none()));

        cx.simulate_keystrokes("ctrl-o");
        cx.run_until_parked();

        assert!(cx.read(|cx| root.read(cx).overlay().picker().is_some()));
    }

    /// `PagePicker` → `None` via Escape (routed through the input's Cancel action).
    #[gpui::test]
    fn transition_picker_to_none_via_escape(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("ctrl-o");
        cx.run_until_parked();
        assert!(cx.read(|cx| root.read(cx).overlay().picker().is_some()));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        assert!(cx.read(|cx| root.read(cx).overlay().is_none()));
    }

    /// `None` → `ShortcutsHelp` via `?`.
    #[gpui::test]
    fn transition_none_to_shortcuts(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("shift-/");
        cx.run_until_parked();

        assert!(cx.read(|cx| root.read(cx).overlay().is_shortcuts()));
    }

    /// `ShortcutsHelp` → `None` via `?` (toggle). Separate from the Escape
    /// path in `shortcuts_overlay_toggles` — the help cheatsheet calls out
    /// `?` as the toggle, so the toggle path needs its own regression.
    #[gpui::test]
    fn transition_shortcuts_to_none_via_question(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("shift-/");
        cx.run_until_parked();
        assert!(cx.read(|cx| root.read(cx).overlay().is_shortcuts()));

        cx.simulate_keystrokes("shift-/");
        cx.run_until_parked();

        assert!(cx.read(|cx| root.read(cx).overlay().is_none()));
    }

    /// `None` → `TagResults` via programmatic `open_tag` dispatch. The
    /// click-through-tag-chip path is covered by `tags::tests::
    /// clicking_rendered_tag_dispatches_open_tag`, which feeds `OpenTag`
    /// here.
    #[gpui::test]
    fn transition_none_to_tag_results(cx: &mut TestAppContext) {
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

        assert_eq!(
            cx.read(|cx| root.read(cx).overlay().tag_name().map(ToString::to_string)),
            Some("todo".to_string()),
        );
    }

    /// Ctrl-P advances the current page and clears any overlay. Starts from
    /// `TagResults` to also exercise the "any → `None`" branch.
    #[gpui::test]
    fn transition_ctrl_p_clears_tag_overlay_and_advances_page(cx: &mut TestAppContext) {
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
        assert!(cx.read(|cx| root.read(cx).overlay().tag_name().is_some()));

        cx.simulate_keystrokes("ctrl-p");
        cx.run_until_parked();

        cx.read(|cx| {
            assert!(root.read(cx).overlay().is_none(), "tag view cleared");
            let current = cx.global::<CurrentPage>().get().unwrap();
            // mount_root started on Home; ctrl-p cycles to the next entry.
            assert_ne!(current.read(cx).name().as_ref(), "Home");
        });
    }

    /// Ctrl-. opens today's journal and clears any overlay.
    #[gpui::test]
    fn transition_ctrl_dot_jumps_to_today(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("shift-/");
        cx.run_until_parked();
        assert!(cx.read(|cx| root.read(cx).overlay().is_shortcuts()));

        cx.simulate_keystrokes("ctrl-.");
        cx.run_until_parked();

        cx.read(|cx| {
            assert!(root.read(cx).overlay().is_none(), "shortcuts cleared");
            let current = cx.global::<CurrentPage>().get().unwrap();
            // The journal name is the ISO date, not "Home".
            assert_ne!(current.read(cx).name().as_ref(), "Home");
        });
    }

    // ────────────────────────────────────────────────────────────────────
    // Cross-cutting invariants. The state-transition tests above each
    // verify a single edge in isolation; these glue several edges together
    // to assert properties that span the whole RootView state machine.
    // ────────────────────────────────────────────────────────────────────

    /// Ctrl-P must auto-save the outgoing page if it is dirty. The unit
    /// test `registry::tests::set_current_page_autosaves_outgoing` covers
    /// the registry primitive; this end-to-end variant drives the
    /// keystroke through the dispatch tree and reads the file back from
    /// disk to confirm the full save path runs.
    #[gpui::test]
    fn invariant_ctrl_p_autosaves_outgoing_dirty_page(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root_path = tmp.path().to_path_buf();
        let (_root, cx) = mount_root(cx, &tmp);

        cx.update(|_, cx| {
            let page = cx.global::<CurrentPage>().get().unwrap().clone();
            let first = page.read(cx).outline().first_block_id().unwrap();
            page.update(cx, |p, cx| p.set_block_text(first, "ctrlp-edit", cx));
            assert_eq!(page.read(cx).name().as_ref(), "Home");
            assert!(page.read(cx).dirty(), "precondition: outgoing dirty");
        });

        cx.simulate_keystrokes("ctrl-p");
        cx.run_until_parked();

        let on_disk = std::fs::read_to_string(root_path.join("pages").join("Home.md"))
            .expect("Home.md written");
        assert!(
            on_disk.contains("ctrlp-edit"),
            "page A should have been flushed; on disk: {on_disk:?}",
        );
    }

    /// Page switch clears any active overlay. Verifies the broader
    /// invariant by stacking it: tag view open → picker open → select
    /// → overlay is None. The intermediate tag view exit and picker
    /// dismissal both have to happen for the assertion to hold.
    #[gpui::test]
    fn invariant_picker_selection_clears_all_overlays(cx: &mut TestAppContext) {
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
        assert!(cx.read(|cx| root.read(cx).overlay().tag_name().is_some()));

        // Opening the picker over the tag view replaces it — the new
        // mutually-exclusive enum guarantees only one overlay at once.
        cx.simulate_keystrokes("ctrl-o");
        cx.run_until_parked();
        assert!(cx.read(|cx| root.read(cx).overlay().picker().is_some()));

        cx.simulate_input("Home");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        cx.read(|cx| {
            assert!(
                root.read(cx).overlay().is_none(),
                "picker selection should leave no overlay active",
            );
            let current = cx.global::<CurrentPage>().get().unwrap();
            assert_eq!(current.read(cx).name().as_ref(), "Home");
        });
    }

    /// Ctrl-S persists a dirty page and clears the dirty flag.
    #[gpui::test]
    fn transition_ctrl_s_saves_dirty_page(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (_root, cx) = mount_root(cx, &tmp);

        cx.update(|_, cx| {
            let page = cx.global::<CurrentPage>().get().unwrap().clone();
            let first = page.read(cx).outline().first_block_id().unwrap();
            page.update(cx, |p, cx| p.set_block_text(first, "after-edit", cx));
            assert!(page.read(cx).dirty());
        });

        cx.simulate_keystrokes("ctrl-s");
        cx.run_until_parked();

        cx.read(|cx| {
            let page = cx.global::<CurrentPage>().get().unwrap();
            assert!(!page.read(cx).dirty(), "ctrl-s clears the dirty flag");
        });
    }
}
