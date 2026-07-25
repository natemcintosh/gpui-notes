#![allow(clippy::unreadable_literal)]

use gpui::{
    actions, div, prelude::*, px, size, App, AppContext, Bounds, Context, Div, ElementId, Entity,
    FocusHandle, Focusable, IntoElement, KeyBinding, MouseButton, NavigationDirection,
    ParentElement, Render, SharedString, Styled, Subscription, Window, WindowBounds, WindowOptions,
};
use gpui_notes::block_view;
use gpui_notes::cmd_key;
use gpui_notes::errors::{self, LastError};
use gpui_notes::history::{self, NavHistory};
use gpui_notes::journal;
use gpui_notes::outline_view;
use gpui_notes::page::Page;
use gpui_notes::page_links::{LinkIndex, OpenPage};
use gpui_notes::page_picker::{self, PageEntry, PagePicker, PagePickerEvent};
use gpui_notes::registry::{pick_next, save_all_on_quit, CurrentPage, PageRegistry};
use gpui_notes::shortcut_bar::{ShortcutBar, ShortcutOverlay};
use gpui_notes::shortcut_hints::{self, ShortcutHint};
use gpui_notes::store::NotesStore;
use gpui_notes::tags::{self, OpenTag, TagIndex, TagSource};
use gpui_notes::text_input;
use gpui_notes::theme;
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
        ToggleShortcuts,
        NavigateBack,
        NavigateForward,
        Quit
    ]
);

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
    ShortcutsHelp {
        hints: Vec<ShortcutHint>,
    },
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
        matches!(self, Self::ShortcutsHelp { .. })
    }

    #[cfg(test)]
    #[must_use]
    pub fn shortcut_hints(&self) -> Option<&[ShortcutHint]> {
        if let Self::ShortcutsHelp { hints } = self {
            Some(hints)
        } else {
            None
        }
    }
}

struct RootView {
    focus_handle: FocusHandle,
    shortcut_bar: Entity<ShortcutBar>,
    overlay: OverlayMode,
    _current_observer: Subscription,
    _tag_observer: Subscription,
    _error_observer: Subscription,
    debug_hud: bool,
    /// Resolved once at startup; see `theme::ui_font_family`.
    ui_font: SharedString,
}

impl RootView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let current_observer = cx.observe_global::<CurrentPage>(|_, cx| cx.notify());
        let tag_observer = cx.observe_global::<TagIndex>(|_, cx| cx.notify());
        let error_observer = cx.observe_global::<LastError>(|_, cx| cx.notify());
        Self {
            focus_handle: cx.focus_handle(),
            shortcut_bar: cx.new(|cx| ShortcutBar::new(window, cx)),
            overlay: OverlayMode::None,
            _current_observer: current_observer,
            _tag_observer: tag_observer,
            _error_observer: error_observer,
            debug_hud: std::env::var("GPUI_NOTES_DEBUG_HUD").is_ok_and(|value| value == "1"),
            ui_font: theme::ui_font_family(cx),
        }
    }

    #[cfg(test)]
    pub(crate) fn overlay(&self) -> &OverlayMode {
        &self.overlay
    }

    /// Parks focus after a root-level state transition. This is the single
    /// owner of the policy: the target is derived only from the active overlay
    /// and current page, so an action cannot accidentally leave the `RootView`
    /// key context without a focused descendant.
    fn park_focus(&self, window: &mut Window, cx: &mut App) {
        match &self.overlay {
            OverlayMode::PagePicker { entity, .. } => {
                entity.update(cx, |picker, cx| picker.focus_input(window, cx));
            }
            OverlayMode::TagResults(_) | OverlayMode::ShortcutsHelp { .. } => {
                window.focus(&self.focus_handle, cx);
            }
            OverlayMode::None => {
                if let Some(page) = cx.global::<CurrentPage>().get().cloned() {
                    let view = page.read(cx).view().clone();
                    view.update(cx, |view, cx| view.focus_first_block(window, cx));
                } else {
                    window.focus(&self.focus_handle, cx);
                }
            }
        }
    }

    #[allow(clippy::unused_self, clippy::needless_pass_by_ref_mut)]
    fn save_current(&mut self, _: &SavePage, _: &mut Window, cx: &mut Context<Self>) {
        let Some(page) = cx.global::<CurrentPage>().get().cloned() else {
            return;
        };
        let result = cx.update_global::<PageRegistry, _>(|reg, cx| reg.save(&page, cx));
        if let Err(err) = result {
            errors::report(format!("save failed: {err}"), cx);
        }
    }

    fn jump_to_today(&mut self, _: &JumpToToday, window: &mut Window, cx: &mut Context<Self>) {
        // Resolve the date fresh on every press, per `journal::open_today`.
        if let Err(err) = history::navigate_to(&TagSource::Journal(journal::today()), cx) {
            errors::report(format!("open today's journal failed: {err}"), cx);
            return;
        }
        self.overlay = OverlayMode::None;
        self.park_focus(window, cx);
    }

    fn next_page(&mut self, _: &NextPage, window: &mut Window, cx: &mut Context<Self>) {
        let names = match cx.global::<PageRegistry>().list() {
            Ok(names) => names,
            Err(err) => {
                errors::report(format!("list failed: {err}"), cx);
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
        if let Err(err) = history::navigate_to(&TagSource::Page(next.clone()), cx) {
            errors::report(format!("open {next:?} failed: {err}"), cx);
            return;
        }
        self.overlay = OverlayMode::None;
        self.park_focus(window, cx);
    }

    fn open_tag(&mut self, action: &OpenTag, window: &mut Window, cx: &mut Context<Self>) {
        Self::reindex_current_page(cx);
        self.overlay = OverlayMode::TagResults(action.name.clone());
        self.park_focus(window, cx);
        cx.notify();
    }

    fn open_page(&mut self, action: &OpenPage, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = history::navigate_to(&TagSource::Page(action.name.clone()), cx) {
            errors::report(format!("open {:?} failed: {err}", action.name), cx);
            return;
        }
        self.overlay = OverlayMode::None;
        self.park_focus(window, cx);
        cx.notify();
    }

    fn navigate_back(&mut self, _: &NavigateBack, window: &mut Window, cx: &mut Context<Self>) {
        self.navigate_history(NavigationDirection::Back, window, cx);
    }

    fn navigate_forward(
        &mut self,
        _: &NavigateForward,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.navigate_history(NavigationDirection::Forward, window, cx);
    }

    /// Shared by the keyboard actions and the mouse side buttons. An empty
    /// stack leaves the overlay and focus untouched, so a stray back press
    /// cannot dismiss an overlay the user is still reading.
    fn navigate_history(
        &mut self,
        direction: NavigationDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match history::go(direction, cx) {
            Ok(true) => {}
            Ok(false) => return,
            Err(err) => {
                let label = match direction {
                    NavigationDirection::Back => "back",
                    NavigationDirection::Forward => "forward",
                };
                errors::report(format!("navigate {label} failed: {err}"), cx);
                return;
            }
        }
        self.overlay = OverlayMode::None;
        self.park_focus(window, cx);
        cx.notify();
    }

    /// Claims the mouse's back/forward side buttons for history navigation.
    /// Nothing else in the tree listens for `Navigate`, so the root element can
    /// take them for the whole window. Split out of `render` to keep that
    /// method within the `too_many_lines` budget.
    fn bind_navigation_mouse(root: Div, cx: &mut Context<Self>) -> Div {
        root.on_mouse_down(
            MouseButton::Navigate(NavigationDirection::Back),
            cx.listener(|this, _, window, cx| {
                this.navigate_history(NavigationDirection::Back, window, cx);
            }),
        )
        .on_mouse_down(
            MouseButton::Navigate(NavigationDirection::Forward),
            cx.listener(|this, _, window, cx| {
                this.navigate_history(NavigationDirection::Forward, window, cx);
            }),
        )
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
        self.park_focus(window, cx);
        cx.notify();
    }

    fn toggle_shortcuts(
        &mut self,
        _: &ToggleShortcuts,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.overlay, OverlayMode::ShortcutsHelp { .. }) {
            self.overlay = OverlayMode::None;
            self.park_focus(window, cx);
        } else {
            self.overlay = OverlayMode::ShortcutsHelp {
                hints: shortcut_hints::for_focused(window, cx),
            };
            self.park_focus(window, cx);
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
            .text_color(theme::header_fg())
            .text_size(px(14.))
            .child(div().flex_1().child(header_text))
            .child(
                div()
                    .id(ElementId::Name(SharedString::from("show-shortcuts")))
                    .px_2()
                    .rounded_sm()
                    .cursor_pointer()
                    .text_color(theme::control_fg())
                    .hover(|style| {
                        style
                            .bg(theme::row_hover())
                            .text_color(theme::control_hover_fg())
                    })
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
                    .text_color(theme::control_fg())
                    .hover(|style| {
                        style
                            .bg(theme::row_hover())
                            .text_color(theme::control_hover_fg())
                    })
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

    /// The dismissible error line shown under the header while a `LastError`
    /// is set (#47). Clicking anywhere on the line dismisses it.
    fn render_error_line(message: String, cx: &mut Context<Self>) -> gpui::AnyElement {
        div()
            .id(ElementId::Name(SharedString::from("last-error")))
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_2()
            .py_1()
            .rounded_sm()
            .cursor_pointer()
            .bg(theme::error_bg())
            .text_color(theme::error_fg())
            .text_size(px(13.))
            .hover(|style| style.bg(theme::error_hover_bg()))
            .child(div().flex_1().child(SharedString::from(message)))
            .child(div().text_color(theme::control_fg()).child("× dismiss"))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| errors::dismiss(cx)),
            )
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
                errors::report(format!("page picker: list failed: {err}"), cx);
                return;
            }
        };
        let picker = cx.new(|cx| PagePicker::new(entries, cx));
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
        self.park_focus(window, cx);
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
        let target = match entry {
            PageEntry::Page(name) => TagSource::Page(name.clone()),
            PageEntry::Journal(date) => TagSource::Journal(*date),
        };
        if let Err(err) = history::navigate_to(&target, cx) {
            errors::report(format!("page picker: open failed: {err}"), cx);
        }
        self.close_picker(window, cx);
    }

    fn close_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = OverlayMode::None;
        self.park_focus(window, cx);
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
        if let Err(err) = history::navigate_to(source, cx) {
            errors::report(format!("open tag result failed: {err}"), cx);
            return;
        }
        self.overlay = OverlayMode::None;
        self.park_focus(window, cx);
        cx.notify();
    }

    fn render_tag_results(tag: &gpui::SharedString, cx: &mut Context<Self>) -> gpui::AnyElement {
        let hits = cx.global::<TagIndex>().blocks_for_tag(tag.as_ref());
        if hits.is_empty() {
            return div()
                .flex_1()
                .text_color(theme::empty_state_fg())
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
                    .text_color(theme::control_hover_fg())
                    .hover(|style| style.bg(theme::row_hover()))
                    .child(format!("{label}: {preview}"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_tag_hit(&source, window, cx);
                    })),
            );
        }
        list.into_any_element()
    }

    fn debug_hud_text(&self, window: &Window, cx: &App) -> String {
        let (focus_target, block_mode) = match &self.overlay {
            OverlayMode::PagePicker { .. } => ("page picker input", "not applicable"),
            OverlayMode::TagResults(_) | OverlayMode::ShortcutsHelp { .. } => {
                ("root", "not applicable")
            }
            OverlayMode::None => {
                let Some(page) = cx.global::<CurrentPage>().get().cloned() else {
                    return "focus: root · block: no page".to_string();
                };
                let view = page.read(cx).view().clone();
                view.read(cx).debug_focus_status(window, cx)
            }
        };
        format!("focus: {focus_target} · block: {block_mode}")
    }

    fn render_debug_hud(text: String) -> gpui::AnyElement {
        div()
            .id(ElementId::Name(SharedString::from("debug-focus-hud")))
            .px_2()
            .py_1()
            .rounded_sm()
            .bg(theme::overlay_bg())
            .border_1()
            .border_color(theme::overlay_border())
            .text_color(theme::fg_muted())
            .text_size(px(12.))
            .child(text)
            .into_any_element()
    }
}

impl Focusable for RootView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current: Option<Entity<Page>> = cx.global::<CurrentPage>().get().cloned();

        let root = div()
            .track_focus(&self.focus_handle(cx))
            .key_context("RootView")
            .on_action(cx.listener(Self::save_current))
            .on_action(cx.listener(Self::next_page))
            .on_action(cx.listener(Self::jump_to_today))
            .on_action(cx.listener(Self::open_tag))
            .on_action(cx.listener(Self::open_page))
            .on_action(cx.listener(Self::close_tag_view))
            .on_action(cx.listener(Self::open_page_picker))
            .on_action(cx.listener(Self::toggle_shortcuts))
            .on_action(cx.listener(Self::navigate_back))
            .on_action(cx.listener(Self::navigate_forward));
        let mut root = Self::bind_navigation_mouse(root, cx)
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme::window_bg())
            .font_family(self.ui_font.clone())
            .p_4()
            .gap_2();
        root.text_style().font_fallbacks = Some(text_input::emoji_font_fallbacks());

        let Some(page) = current else {
            return root
                .child(
                    div()
                        .flex_1()
                        .text_color(theme::header_fg())
                        .child("No page open."),
                )
                .child(self.shortcut_bar.clone());
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

        let mut root = root.child(Self::render_header(header_text, active_tag.is_some(), cx));

        if self.debug_hud {
            root = root.child(Self::render_debug_hud(self.debug_hud_text(window, cx)));
        }

        let error_message = cx
            .global::<LastError>()
            .get()
            .map(|e| format!("{} — {}", e.message, e.at.format("%H:%M:%S")));
        if let Some(message) = error_message {
            root = root.child(Self::render_error_line(message, cx));
        }

        // `OutlineView` owns its own scroll container (it needs the handle on
        // the element whose children are the block rows); tag results get one
        // here.
        let body = if let Some(tag) = active_tag {
            root.child(
                div()
                    .id("tag-results")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(Self::render_tag_results(&tag, cx)),
            )
        } else {
            root.child(view)
        }
        .child(self.shortcut_bar.clone());

        let overlay_child: Option<gpui::AnyElement> = match &self.overlay {
            OverlayMode::PagePicker { entity, .. } => Some(entity.clone().into_any_element()),
            OverlayMode::ShortcutsHelp { hints } => {
                Some(ShortcutOverlay::new(hints.clone()).into_any_element())
            }
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
        block_view::bind_keys(cx);
        outline_view::bind_keys(cx);
        page_picker::bind_keys(cx);
        let cmd = cmd_key();
        cx.bind_keys([
            KeyBinding::new(&format!("{cmd}-s"), SavePage, Some("RootView")),
            KeyBinding::new(&format!("{cmd}-p"), NextPage, Some("RootView")),
            KeyBinding::new(&format!("{cmd}-."), JumpToToday, Some("RootView")),
            KeyBinding::new(&format!("{cmd}-o"), OpenPagePicker, Some("RootView")),
            KeyBinding::new("escape", CloseTagView, Some("RootView")),
            // GPUI matches predicates against the whole focus path, so the
            // explicit negation preserves literal `?` input (#45).
            KeyBinding::new("?", ToggleShortcuts, Some("RootView && !TextInput")),
            KeyBinding::new(&format!("{cmd}-/"), ToggleShortcuts, None),
            KeyBinding::new(&format!("{cmd}-q"), Quit, None),
            // Registered last so the four-slot shortcut bar keeps showing the
            // bindings above. `!TextInput` keeps a block edit from being
            // navigated out from under the user, and on macOS leaves alt-arrow
            // free for `WordLeft`/`WordRight` (see `text_input::bind_keys`).
            KeyBinding::new("alt-left", NavigateBack, Some("RootView && !TextInput")),
            KeyBinding::new("alt-right", NavigateForward, Some("RootView && !TextInput")),
        ]);
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());

        let root_dir = NotesStore::default_root().expect("resolve notes root");
        let store = NotesStore::new(root_dir).expect("init notes store");
        let tag_index = TagIndex::rebuild_from(&store).expect("index tags");
        let link_index = LinkIndex::rebuild_from(&store).expect("index page links");
        cx.set_global(tag_index);
        cx.set_global(link_index);
        cx.set_global(PageRegistry::new(store));
        cx.set_global(CurrentPage::default());
        cx.set_global(LastError::default());
        cx.set_global(NavHistory::default());
        save_all_on_quit(cx);
        // Deliberately not `navigate_to`: the first page is not a history
        // entry, so the back stack starts empty.
        journal::open_today(cx).expect("open today's journal");

        let bounds = Bounds::centered(None, size(px(640.0), px(420.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let root = cx.new(|cx| RootView::new(window, cx));
                root.update(cx, |view, cx| view.park_focus(window, cx));
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
    use gpui_notes::registry::set_current_page;
    use tempfile::TempDir;

    fn mount_root<'a>(
        cx: &'a mut TestAppContext,
        tmp: &TempDir,
    ) -> (Entity<RootView>, &'a mut VisualTestContext) {
        let root_dir = tmp.path().to_path_buf();
        let (root, vcx) = cx.add_window_view(move |window, cx| {
            text_input::bind_keys(cx);
            block_view::bind_keys(cx);
            outline_view::bind_keys(cx);
            page_picker::bind_keys(cx);
            cx.bind_keys([
                KeyBinding::new("ctrl-s", SavePage, Some("RootView")),
                KeyBinding::new("ctrl-p", NextPage, Some("RootView")),
                KeyBinding::new("ctrl-.", JumpToToday, Some("RootView")),
                KeyBinding::new("ctrl-o", OpenPagePicker, Some("RootView")),
                KeyBinding::new("escape", CloseTagView, Some("RootView")),
                KeyBinding::new("?", ToggleShortcuts, Some("RootView && !TextInput")),
                KeyBinding::new("ctrl-/", ToggleShortcuts, None),
                KeyBinding::new("alt-left", NavigateBack, Some("RootView && !TextInput")),
                KeyBinding::new("alt-right", NavigateForward, Some("RootView && !TextInput")),
            ]);
            let store = NotesStore::new(&root_dir).unwrap();
            store.write("Home", "- home\n").unwrap();
            store.write("Tagged", "- has #todo\n").unwrap();
            cx.set_global(TagIndex::rebuild_from(&store).unwrap());
            cx.set_global(LinkIndex::rebuild_from(&store).unwrap());
            cx.set_global(PageRegistry::new(store));
            cx.set_global(CurrentPage::default());
            cx.set_global(LastError::default());
            cx.set_global(NavHistory::default());
            save_all_on_quit(cx);
            // Setup, not navigation: matches `main`'s startup, back stack empty.
            set_current_page("Home", cx).unwrap();
            RootView::new(window, cx)
        });
        vcx.update(|window, cx| {
            window.activate_window();
            root.update(cx, |view, cx| view.park_focus(window, cx));
        });
        vcx.run_until_parked();
        (root, vcx)
    }

    /// Assert the actual GPUI focus path still includes the element that
    /// provides the `RootView` key context. This catches dead-keymap
    /// regressions even when the immediately preceding action looked correct.
    fn assert_root_in_focus_path(cx: &mut VisualTestContext, root: &Entity<RootView>) {
        cx.update(|window, cx| {
            let root_focus = root.read(cx).focus_handle(cx);
            assert!(
                root_focus.contains_focused(window, cx),
                "focused element must remain beneath RootView",
            );
        });
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

    #[gpui::test]
    fn open_page_action_creates_missing_target_and_navigates(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.open_page(
                    &OpenPage {
                        name: "New Target".into(),
                    },
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let current = cx.global::<CurrentPage>().get().unwrap();
            assert_eq!(current.read(cx).name().as_ref(), "New Target");
            assert!(cx.global::<LinkIndex>().page_exists("New Target"));
            assert!(root.read(cx).overlay().is_none());
        });
        assert!(tmp.path().join("pages/New Target.md").is_file());
    }

    #[gpui::test]
    fn invalid_open_page_action_is_non_destructive_and_reports_error(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.open_page(
                    &OpenPage {
                        name: "https://example.com".into(),
                    },
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let current = cx.global::<CurrentPage>().get().unwrap();
            assert_eq!(current.read(cx).name().as_ref(), "Home");
            assert!(cx.global::<LastError>().get().is_some());
            assert!(!cx.global::<LinkIndex>().page_exists("https://example.com"));
        });
        assert_eq!(
            NotesStore::new(tmp.path()).unwrap().list().unwrap(),
            vec!["Home", "Tagged"],
        );
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

    /// `ctrl-/` toggles the shortcuts overlay; Escape dismisses it without
    /// affecting the active tag view.
    #[gpui::test]
    fn shortcuts_overlay_toggles(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("ctrl-/");
        cx.run_until_parked();
        assert!(cx.read(|cx| root.read(cx).overlay().is_shortcuts()));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(cx.read(|cx| root.read(cx).overlay().is_none()));
    }

    #[gpui::test]
    fn shortcut_hints_follow_the_focused_dispatch_stack(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (_root, cx) = mount_root(cx, &tmp);

        let viewing_hints = cx.update(|window, cx| shortcut_hints::for_focused(window, cx));
        for action_name in [
            "gpui_notes::SavePage",
            "gpui_notes::NextPage",
            "gpui_notes::JumpToToday",
        ] {
            assert!(
                viewing_hints
                    .iter()
                    .any(|hint| hint.action_name == action_name),
                "missing active root binding for {action_name}: {viewing_hints:?}",
            );
        }

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        let (editing_hints, editing_contexts) = cx.update(|window, cx| {
            (
                shortcut_hints::for_focused(window, cx),
                window.context_stack(),
            )
        });
        assert!(
            editing_hints
                .iter()
                .any(|hint| hint.action_name == "text_input::Backspace"),
            "focused TextInput bindings should be discoverable in {editing_contexts:?}: {editing_hints:?}",
        );
        assert!(
            editing_hints
                .iter()
                .any(|hint| hint.action_name == "gpui_notes::SavePage"),
            "ancestor RootView bindings must remain discoverable: {editing_hints:?}",
        );
    }

    #[gpui::test]
    fn shortcuts_overlay_keeps_the_editing_context_snapshot(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.simulate_keystrokes("ctrl-/");
        cx.run_until_parked();

        cx.read(|cx| {
            let hints = root
                .read(cx)
                .overlay()
                .shortcut_hints()
                .expect("shortcuts overlay should be open");
            assert!(
                hints
                    .iter()
                    .any(|hint| hint.action_name == "text_input::Backspace"),
                "overlay opened mid-edit must include text bindings: {hints:?}",
            );
            assert!(
                hints
                    .iter()
                    .any(|hint| hint.action_name == "gpui_notes::SavePage"),
                "overlay must include bindings inherited from RootView: {hints:?}",
            );
        });
    }

    #[gpui::test]
    fn newly_registered_binding_appears_without_hint_metadata(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (_root, cx) = mount_root(cx, &tmp);

        let save_binding_count = cx
            .update(|window, cx| shortcut_hints::for_focused(window, cx))
            .iter()
            .filter(|hint| hint.action_name == "gpui_notes::SavePage")
            .count();
        cx.update(|_, cx| {
            cx.bind_keys([KeyBinding::new("ctrl-shift-s", SavePage, Some("RootView"))]);
        });
        cx.run_until_parked();

        let hints = cx.update(|window, cx| shortcut_hints::for_focused(window, cx));
        assert_eq!(
            hints
                .iter()
                .filter(|hint| hint.action_name == "gpui_notes::SavePage")
                .count(),
            save_binding_count + 1,
            "runtime keymap addition should appear automatically: {hints:?}",
        );
    }

    #[gpui::test]
    fn question_mark_opens_shortcuts_outside_text_input(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("?");
        cx.run_until_parked();

        assert!(cx.read(|cx| root.read(cx).overlay().is_shortcuts()));
    }

    /// Regression test for #45: a bare `?` (shift-/) while a block is being
    /// edited must reach the text input as a typed character. The old
    /// unqualified `shift-/` → `ToggleShortcuts` binding shadowed it —
    /// gpui matches bindings along the whole focus path before falling
    /// through to input, and `RootView` is an ancestor of every input.
    #[gpui::test]
    fn question_mark_types_into_editing_block(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        // mount_root leaves the first block of Home focused-but-viewing
        // (#42); enter begins the edit.
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.simulate_keystrokes("shift-/");
        cx.run_until_parked();

        assert!(
            cx.read(|cx| root.read(cx).overlay().is_none()),
            "typing `?` must not open the shortcuts overlay",
        );

        // Escape ends the edit, flushing the input back into the outline.
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        cx.read(|cx| {
            let page = cx.global::<CurrentPage>().get().unwrap();
            let first = page.read(cx).outline().first_block_id().unwrap();
            let text = page.read(cx).outline().get(first).unwrap();
            // The headless keymap delivers the raw key (`/`) rather than the
            // shift-mapped `?` a real platform produces; what this asserts is
            // that the keystroke fell through to the input at all instead of
            // being swallowed by a binding.
            assert!(
                text.contains('/'),
                "shift-/ should have been typed into the block; got {text:?}",
            );
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
        assert_root_in_focus_path(cx, &root);

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
        assert_root_in_focus_path(cx, &root);

        assert!(cx.read(|cx| root.read(cx).overlay().picker().is_some()));
    }

    /// `PagePicker` → `None` via Escape (routed through the input's Cancel action).
    #[gpui::test]
    fn transition_picker_to_none_via_escape(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("ctrl-o");
        cx.run_until_parked();
        assert_root_in_focus_path(cx, &root);
        assert!(cx.read(|cx| root.read(cx).overlay().picker().is_some()));

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert_root_in_focus_path(cx, &root);

        assert!(cx.read(|cx| root.read(cx).overlay().is_none()));
    }

    /// `None` → `ShortcutsHelp` via `ctrl-/`.
    #[gpui::test]
    fn transition_none_to_shortcuts(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("ctrl-/");
        cx.run_until_parked();
        assert_root_in_focus_path(cx, &root);

        assert!(cx.read(|cx| root.read(cx).overlay().is_shortcuts()));
    }

    /// `ShortcutsHelp` → `None` via `ctrl-/` (toggle). Separate from the
    /// Escape path in `shortcuts_overlay_toggles` — the help cheatsheet calls
    /// out `ctrl-/` as the toggle, so the toggle path needs its own
    /// regression.
    #[gpui::test]
    fn transition_shortcuts_to_none_via_toggle_key(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("ctrl-/");
        cx.run_until_parked();
        assert_root_in_focus_path(cx, &root);
        assert!(cx.read(|cx| root.read(cx).overlay().is_shortcuts()));

        cx.simulate_keystrokes("ctrl-/");
        cx.run_until_parked();
        assert_root_in_focus_path(cx, &root);

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
        assert_root_in_focus_path(cx, &root);

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
        assert_root_in_focus_path(cx, &root);
        assert!(cx.read(|cx| root.read(cx).overlay().tag_name().is_some()));

        cx.simulate_keystrokes("ctrl-p");
        cx.run_until_parked();
        assert_root_in_focus_path(cx, &root);

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

        cx.simulate_keystrokes("ctrl-/");
        cx.run_until_parked();
        assert_root_in_focus_path(cx, &root);
        assert!(cx.read(|cx| root.read(cx).overlay().is_shortcuts()));

        cx.simulate_keystrokes("ctrl-.");
        cx.run_until_parked();
        assert_root_in_focus_path(cx, &root);

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

    /// Regression test for #39: cycling pages with ctrl-p focuses (and thus
    /// mounts an editor on) the first block of each page and blurs it on the
    /// next switch. With zero typing, no page may become dirty and no file
    /// may be rewritten.
    #[gpui::test]
    fn invariant_ctrl_p_cycling_keeps_pages_clean(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root_path = tmp.path().to_path_buf();
        let (_root, cx) = mount_root(cx, &tmp);

        // Home is the outgoing page on the first ctrl-p; its blur-flush is
        // what used to set the bogus dirty flag.
        let home = cx.read(|cx| cx.global::<CurrentPage>().get().unwrap().clone());
        let home_path = root_path.join("pages").join("Home.md");
        let mtime_before = std::fs::metadata(&home_path).unwrap().modified().unwrap();

        for _ in 0..4 {
            cx.simulate_keystrokes("ctrl-p");
            cx.run_until_parked();
            cx.read(|cx| {
                assert!(
                    !home.read(cx).dirty(),
                    "Home marked dirty by mere page cycling",
                );
                let current = cx.global::<CurrentPage>().get().unwrap();
                assert!(
                    !current.read(cx).dirty(),
                    "{:?} marked dirty by mere page cycling",
                    current.read(cx).name(),
                );
            });
        }

        let mtime_after = std::fs::metadata(&home_path).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "Home.md rewritten with zero user edits",
        );
    }

    /// Regression test for #38: ctrl-s while a block edit is still in
    /// progress must persist the typed text. Previously the typed text lived
    /// only in the `TextInput` until blur, so ctrl-s saved the pre-edit body
    /// (or skipped the write entirely if the page wasn't already dirty).
    #[gpui::test]
    fn invariant_ctrl_s_mid_edit_saves_typed_text(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root_path = tmp.path().to_path_buf();
        let (_root, cx) = mount_root(cx, &tmp);

        // Enter begins editing the focused first block of Home (#42),
        // caret at the end of "home".
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.simulate_input("-typed");
        cx.run_until_parked();
        cx.simulate_keystrokes("ctrl-s");
        cx.run_until_parked();

        let on_disk = std::fs::read_to_string(root_path.join("pages").join("Home.md"))
            .expect("Home.md written");
        assert!(
            on_disk.contains("home-typed"),
            "ctrl-s mid-edit must save the typed text; on disk: {on_disk:?}",
        );
    }

    /// Regression test for #38, page-switch flavor: `set_current_page` saves
    /// the outgoing page *before* the focus change blurs the input, so the
    /// in-flight edit must already be in the outline for the autosave to
    /// persist it.
    #[gpui::test]
    fn invariant_ctrl_p_mid_edit_persists_typed_text(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root_path = tmp.path().to_path_buf();
        let (_root, cx) = mount_root(cx, &tmp);

        let home = cx.read(|cx| cx.global::<CurrentPage>().get().unwrap().clone());

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.simulate_input("-typed");
        cx.run_until_parked();
        cx.simulate_keystrokes("ctrl-p");
        cx.run_until_parked();

        let on_disk = std::fs::read_to_string(root_path.join("pages").join("Home.md"))
            .expect("Home.md written");
        assert!(
            on_disk.contains("home-typed"),
            "outgoing page must be saved with the typed text; on disk: {on_disk:?}",
        );
        cx.read(|cx| {
            assert!(
                !home.read(cx).dirty(),
                "outgoing page must be persisted, not left dirty",
            );
        });
    }

    /// End-to-end #42: after mount and after every page switch the first
    /// block is focused but NOT editing. Stray typing must change nothing;
    /// only an explicit `enter` begins an edit. (Asserted through behavior
    /// because the bin test crate can't reach the lib's cfg(test) helpers.)
    #[gpui::test]
    fn page_switch_lands_focused_not_editing(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (_root, cx) = mount_root(cx, &tmp);

        let home = cx.read(|cx| cx.global::<CurrentPage>().get().unwrap().clone());
        cx.simulate_input("zzz");
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(
                !home.read(cx).body().contains("zzz"),
                "typing after mount must not land anywhere — no editor is open",
            );
            assert!(!home.read(cx).dirty(), "page untouched by stray typing");
        });

        cx.simulate_keystrokes("ctrl-p");
        cx.run_until_parked();
        let incoming = cx.read(|cx| cx.global::<CurrentPage>().get().unwrap().clone());
        cx.simulate_input("zzz");
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(
                !incoming.read(cx).body().contains("zzz"),
                "page switch must land focused-but-viewing (#42)",
            );
            assert!(!incoming.read(cx).dirty());
        });

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.simulate_input("zzz");
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(
                incoming.read(cx).body().contains("zzz"),
                "explicit enter begins editing and typing lands",
            );
        });
    }

    /// Regression test for #43: quitting must flush every dirty open page.
    /// `App::shutdown` is exactly what the platform's quit callback invokes;
    /// the test platform's `quit()` is a no-op, so the test calls it
    /// directly. Covers both a mid-edit page (Home, flushed per keystroke
    /// since #38) and a page dirtied off-screen (Tagged).
    #[gpui::test]
    fn invariant_quit_saves_all_dirty_pages(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let root_path = tmp.path().to_path_buf();
        let (_root, cx) = mount_root(cx, &tmp);

        // Enter begins editing the focused first block of Home (#42).
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.simulate_input("-typed");
        cx.run_until_parked();
        cx.update(|_, cx| {
            let tagged = cx
                .update_global::<PageRegistry, _>(|reg, cx| reg.open("Tagged", cx))
                .unwrap();
            let first = tagged.read(cx).outline().first_block_id().unwrap();
            tagged.update(cx, |p, cx| p.set_block_text(first, "tagged-edit", cx));
            assert!(tagged.read(cx).dirty(), "precondition: Tagged dirty");
        });

        // Shutdown must run outside a window update — it tears the windows
        // down — so go through the underlying TestAppContext.
        cx.cx.update(App::shutdown);

        let home = std::fs::read_to_string(root_path.join("pages/Home.md")).unwrap();
        let tagged = std::fs::read_to_string(root_path.join("pages/Tagged.md")).unwrap();
        assert!(
            home.contains("home-typed"),
            "quit must save the mid-edit page; on disk: {home:?}",
        );
        assert!(
            tagged.contains("tagged-edit"),
            "quit must save every dirty page, not just the current one; on disk: {tagged:?}",
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

    /// After escaping out of a block edit, ctrl-s must still dispatch. The
    /// `TextInput` entity is dropped on Cancel, which previously left focus
    /// dead — no element matched the `RootView` key context, so the binding
    /// silently dropped. Escape now parks focus on the block's own wrapper
    /// (#42), which sits under `RootView`, keeping the chain alive.
    #[gpui::test]
    fn ctrl_s_works_after_escaping_block_edit(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (_root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

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
            assert!(
                !page.read(cx).dirty(),
                "ctrl-s should clear dirty even after escape parked focus",
            );
        });
    }

    /// End-to-end #47: a failed ctrl-s must surface an error line in the UI
    /// instead of only stderr, and clicking the line dismisses it. The pages
    /// directory is made read-only to force the write failure; the dismissal
    /// click doubles as the render assertion — it only lands if the line is
    /// actually laid out under the header.
    #[cfg(unix)]
    #[gpui::test]
    fn save_failure_shows_dismissible_error_line(cx: &mut TestAppContext) {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let (_root, cx) = mount_root(cx, &tmp);
        let pages = tmp.path().join("pages");
        std::fs::set_permissions(&pages, std::fs::Permissions::from_mode(0o555)).unwrap();

        // Enter begins editing the focused first block of Home (#42); the
        // per-keystroke flush (#38) dirties the page so ctrl-s attempts a
        // write.
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.simulate_input("-typed");
        cx.run_until_parked();
        cx.simulate_keystrokes("ctrl-s");
        cx.run_until_parked();

        cx.read(|cx| {
            let entry = cx.global::<LastError>().get().expect("save error recorded");
            assert!(
                entry.message.contains("save failed"),
                "unexpected message: {:?}",
                entry.message,
            );
        });

        cx.simulate_click(point(px(24.0), px(50.0)), Modifiers::default());
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(
                cx.global::<LastError>().get().is_none(),
                "clicking the error line dismisses it",
            );
        });

        // Restore permissions so TempDir cleanup can remove the files.
        std::fs::set_permissions(&pages, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    // ────────────────────────────────────────────────────────────────────
    // Back/forward navigation history.
    //
    // `mount_root` starts on Home with an empty back stack. `ctrl-p` is the
    // deterministic way to reach a second page (`pick_next` cycles Home →
    // Tagged), and `ctrl-.` reaches a journal — the two target shapes that
    // must stay distinguishable through a round trip.
    // ────────────────────────────────────────────────────────────────────

    fn current_page_name(cx: &mut VisualTestContext) -> String {
        cx.read(|cx| {
            cx.global::<CurrentPage>()
                .get()
                .unwrap()
                .read(cx)
                .name()
                .to_string()
        })
    }

    #[gpui::test]
    fn back_returns_to_previous_page(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("ctrl-p");
        cx.run_until_parked();
        assert_eq!(current_page_name(cx), "Tagged");

        cx.simulate_keystrokes("alt-left");
        cx.run_until_parked();
        assert_root_in_focus_path(cx, &root);
        assert_eq!(current_page_name(cx), "Home");
    }

    #[gpui::test]
    fn forward_returns_after_back(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (_root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("ctrl-p");
        cx.run_until_parked();
        cx.simulate_keystrokes("alt-left");
        cx.run_until_parked();
        assert_eq!(current_page_name(cx), "Home");

        cx.simulate_keystrokes("alt-right");
        cx.run_until_parked();
        assert_eq!(current_page_name(cx), "Tagged");
    }

    /// An empty back stack must be inert, not an error and not a focus change.
    #[gpui::test]
    fn back_with_empty_history_is_noop(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("alt-left");
        cx.run_until_parked();

        assert_root_in_focus_path(cx, &root);
        assert_eq!(current_page_name(cx), "Home");
        cx.read(|cx| {
            assert!(
                cx.global::<LastError>().get().is_none(),
                "no error reported"
            );
        });
    }

    /// Browser semantics: navigating somewhere new abandons the forward branch.
    #[gpui::test]
    fn navigating_clears_forward_stack(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (_root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("ctrl-p");
        cx.run_until_parked();
        cx.simulate_keystrokes("alt-left");
        cx.run_until_parked();
        assert_eq!(current_page_name(cx), "Home");

        // A fresh navigation while Tagged sits on the forward stack.
        cx.simulate_keystrokes("ctrl-.");
        cx.run_until_parked();
        let journal = current_page_name(cx);
        assert_ne!(journal, "Home");

        cx.simulate_keystrokes("alt-right");
        cx.run_until_parked();
        assert_eq!(
            current_page_name(cx),
            journal,
            "forward must be dead after a new navigation",
        );
    }

    /// The mouse side button is the production trigger, so drive the real
    /// `MouseButton::Navigate` event rather than calling the handler — that is
    /// what catches a listener attached to the wrong element.
    #[gpui::test]
    fn mouse_back_and_forward_buttons_navigate(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (_root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("ctrl-p");
        cx.run_until_parked();
        assert_eq!(current_page_name(cx), "Tagged");

        cx.simulate_mouse_down(
            point(px(24.0), px(50.0)),
            MouseButton::Navigate(NavigationDirection::Back),
            Modifiers::default(),
        );
        cx.run_until_parked();
        assert_eq!(current_page_name(cx), "Home");

        cx.simulate_mouse_down(
            point(px(24.0), px(50.0)),
            MouseButton::Navigate(NavigationDirection::Forward),
            Modifiers::default(),
        );
        cx.run_until_parked();
        assert_eq!(current_page_name(cx), "Tagged");
    }

    /// Guards the `PageKey::Page` vs `PageKey::Journal` distinction (#40): a
    /// journal's display name is an ISO date, and going back from it must
    /// return to the page, not to a page named after that date.
    #[gpui::test]
    fn back_from_journal_returns_to_page(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (_root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("ctrl-.");
        cx.run_until_parked();
        assert_ne!(current_page_name(cx), "Home");

        cx.simulate_keystrokes("alt-left");
        cx.run_until_parked();
        assert_eq!(current_page_name(cx), "Home");
    }

    /// The `!TextInput` predicate: alt-left must not navigate away from a block
    /// that is being edited. On macOS it falls through to `WordLeft`; on Linux,
    /// where `text_input` binds ctrl-arrow for word motion, it is simply inert.
    /// Either way the editor keeps focus and the page must not change.
    #[gpui::test]
    fn alt_left_while_editing_does_not_navigate(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (_root, cx) = mount_root(cx, &tmp);

        // Navigate first so the back stack is non-empty: a suppressed
        // `NavigateBack` and an inert one are indistinguishable otherwise.
        cx.simulate_keystrokes("ctrl-p");
        cx.run_until_parked();
        assert_eq!(current_page_name(cx), "Tagged");

        // Enter opens the first block ("has #todo") with the caret at the end.
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.simulate_keystrokes("alt-left");
        cx.run_until_parked();
        cx.simulate_input("X");
        cx.run_until_parked();

        assert_eq!(
            current_page_name(cx),
            "Tagged",
            "alt-left must not navigate back while a block editor is focused",
        );
        cx.read(|cx| {
            let body = cx.global::<CurrentPage>().get().unwrap().read(cx).body();
            assert!(
                body.contains('X'),
                "the editor must still hold focus after alt-left; body: {body:?}",
            );
        });
    }
}
