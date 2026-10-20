#![allow(clippy::unreadable_literal)]
// Release builds on Windows are GUI-subsystem apps, so launching one doesn't
// also open a console window. Debug builds keep the console for logs.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use gpui::{
    actions, div, prelude::*, px, size, App, AppContext, Bounds, Context, Div, ElementId, Entity,
    FocusHandle, Focusable, IntoElement, KeyBinding, MouseButton, NavigationDirection,
    ParentElement, Render, SharedString, Styled, Subscription, Window, WindowBounds, WindowOptions,
};
use gpui_notes::block_view;
use gpui_notes::cmd_key;
use gpui_notes::errors::{self, LastError};
use gpui_notes::history::{self, NavHistory, NavTarget};
use gpui_notes::journal;
use gpui_notes::journals_view::JournalsView;
use gpui_notes::outline_view;
use gpui_notes::page::Page;
use gpui_notes::page_links::{self, LinkIndex, OpenPage};
use gpui_notes::page_picker::{self, PageEntry, PagePicker, PagePickerEvent};
use gpui_notes::registry::{pick_next, save_all_on_quit, CurrentPage, PageRegistry};
use gpui_notes::search::{self, SearchCorpus, SearchEvent, SearchTarget, SearchView};
use gpui_notes::shortcut_hints::{self, ShortcutHint};
use gpui_notes::shortcut_overlay::ShortcutOverlay;
use gpui_notes::source::{source_label, NoteSource};
use gpui_notes::spellcheck::{self, SpellChecker};
use gpui_notes::store::NotesStore;
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
        CloseOverlay,
        OpenPagePicker,
        OpenSearch,
        ToggleShortcuts,
        NavigateBack,
        NavigateForward,
        ToggleTheme,
        Quit,
        Hide,
        HideOtherApps
    ]
);

/// The three mutually-exclusive top-level UI modes. Encoding them as a single
/// enum keeps overlay invariants (only one open at a time) unrepresentable as
/// invalid states, and gives transition tests a single field to assert on.
///
/// `PagePicker`, `Search` and `ShortcutsHelp` are absolute-positioned overlays. The
/// enum collapses them into the same axis so that opening any one always
/// dismisses the others.
pub enum OverlayMode {
    None,
    PagePicker {
        entity: Entity<PagePicker>,
        _sub: Subscription,
    },
    Search {
        entity: Entity<SearchView>,
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
    pub fn picker(&self) -> Option<&Entity<PagePicker>> {
        if let Self::PagePicker { entity, .. } = self {
            Some(entity)
        } else {
            None
        }
    }

    #[must_use]
    pub fn search(&self) -> Option<&Entity<SearchView>> {
        if let Self::Search { entity, .. } = self {
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
    /// Kept across mode switches so back/forward returns to the same days
    /// and scroll position.
    journals: Entity<JournalsView>,
    overlay: OverlayMode,
    _current_observer: Subscription,
    _error_observer: Subscription,
    debug_hud: bool,
    /// Resolved once at startup; see `theme::ui_font_family`.
    ui_font: SharedString,
}

impl RootView {
    fn new(cx: &mut Context<Self>) -> Self {
        let current_observer = cx.observe_global::<CurrentPage>(|_, cx| cx.notify());
        let error_observer = cx.observe_global::<LastError>(|_, cx| cx.notify());
        Self {
            focus_handle: cx.focus_handle(),
            journals: cx.new(JournalsView::new),
            overlay: OverlayMode::None,
            _current_observer: current_observer,
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
            OverlayMode::Search { entity, .. } => {
                entity.update(cx, |search, cx| search.focus_input(window, cx));
            }
            OverlayMode::ShortcutsHelp { .. } => {
                window.focus(&self.focus_handle, cx);
            }
            OverlayMode::None => {
                if cx.global::<CurrentPage>().is_journals() {
                    let focused = self
                        .journals
                        .update(cx, |journals, cx| journals.focus_today(window, cx));
                    if !focused {
                        window.focus(&self.focus_handle, cx);
                    }
                } else if let Some(page) = cx.global::<CurrentPage>().get().cloned() {
                    let view = page.read(cx).view().clone();
                    view.update(cx, |view, cx| view.focus_first_block(window, cx));
                } else {
                    window.focus(&self.focus_handle, cx);
                }
            }
        }
    }

    /// Saves every open page, not just the current one: blocks edited in the
    /// linked-references section belong to other pages.
    #[allow(clippy::unused_self, clippy::needless_pass_by_ref_mut)]
    fn save_current(&mut self, _: &SavePage, _: &mut Window, cx: &mut Context<Self>) {
        let result = cx.update_global::<PageRegistry, _>(|reg, cx| reg.save_all(cx));
        if let Err(err) = result {
            errors::report(format!("save failed: {err}"), cx);
        }
    }

    fn jump_to_today(&mut self, _: &JumpToToday, window: &mut Window, cx: &mut Context<Self>) {
        // Journals mode lists today first, and `park_focus` scrolls it back
        // to the top.
        if let Err(err) = history::navigate_to(&NavTarget::Journals, cx) {
            errors::report(format!("open journals failed: {err}"), cx);
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
        if let Err(err) = history::navigate_to(&NoteSource::Page(next.clone()).into(), cx) {
            errors::report(format!("open {next:?} failed: {err}"), cx);
            return;
        }
        self.overlay = OverlayMode::None;
        self.park_focus(window, cx);
    }

    fn open_page(&mut self, action: &OpenPage, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = history::navigate_to(&page_links::link_target(&action.name).into(), cx) {
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
    /// see `ShortcutsHelp` here.
    fn close_overlay(&mut self, _: &CloseOverlay, window: &mut Window, cx: &mut Context<Self>) {
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

    fn render_header(header_text: String, cx: &mut Context<Self>) -> gpui::AnyElement {
        let root = cx.global::<PageRegistry>().root();
        let location = format!(
            "notes in {}",
            display_root(root, dirs::home_dir().as_deref())
        );
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .text_color(theme::header_fg())
            .text_size(px(14.))
            .child(div().flex_1().child(header_text))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(theme::fg_muted())
                    .child(location),
            )
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
            )
            .into_any_element()
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
                    this.dismiss_overlay(window, cx);
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
        let link_targets = cx
            .try_global::<LinkIndex>()
            .into_iter()
            .flat_map(LinkIndex::link_targets);
        Ok(page_picker::entries(
            registry.list()?,
            registry.list_journals()?,
            link_targets,
        ))
    }

    fn handle_picker_selection(
        &mut self,
        entry: &PageEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = match entry {
            PageEntry::Page(name) => NoteSource::Page(name.clone()),
            PageEntry::Journal(date) => NoteSource::Journal(*date),
        };
        if let Err(err) = history::navigate_to(&target.into(), cx) {
            errors::report(format!("page picker: open failed: {err}"), cx);
        }
        self.dismiss_overlay(window, cx);
    }

    fn dismiss_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = OverlayMode::None;
        self.park_focus(window, cx);
        cx.notify();
    }

    fn open_search(&mut self, _: &OpenSearch, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.overlay, OverlayMode::Search { .. }) {
            return;
        }
        let corpus = match SearchCorpus::collect(cx) {
            Ok((corpus, skipped)) => {
                if !skipped.is_empty() {
                    let files: Vec<String> = skipped
                        .iter()
                        .map(|(source, err)| format!("{} ({err})", source_label(source)))
                        .collect();
                    errors::report(format!("search skipped: {}", files.join(", ")), cx);
                }
                corpus
            }
            Err(err) => {
                errors::report(format!("search: list failed: {err}"), cx);
                return;
            }
        };
        let view = cx.new(|cx| SearchView::new(corpus, cx));
        let sub = cx.subscribe_in(
            &view,
            window,
            |this, _view, event: &SearchEvent, window, cx| match event {
                SearchEvent::Selected(target) => this.handle_search_selection(target, window, cx),
                SearchEvent::Cancelled => this.dismiss_overlay(window, cx),
            },
        );
        self.overlay = OverlayMode::Search {
            entity: view,
            _sub: sub,
        };
        self.park_focus(window, cx);
        cx.notify();
    }

    /// Opens the hit's note, then focuses the matched block in place of the
    /// first one `park_focus` would pick.
    fn handle_search_selection(
        &mut self,
        target: &SearchTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(err) = history::navigate_to(&target.source.clone().into(), cx) {
            errors::report(format!("search: open failed: {err}"), cx);
            self.dismiss_overlay(window, cx);
            return;
        }
        self.dismiss_overlay(window, cx);
        let Some(page) = cx.global::<CurrentPage>().get().cloned() else {
            return;
        };
        let Some(id) = target.resolve(page.read(cx).outline()) else {
            return;
        };
        let view = page.read(cx).view().clone();
        view.update(cx, |view, cx| view.reveal_block(id, window, cx));
    }

    fn debug_hud_text(&self, window: &Window, cx: &App) -> String {
        let (focus_target, block_mode) = match &self.overlay {
            OverlayMode::PagePicker { .. } => ("page picker input", "not applicable"),
            OverlayMode::Search { .. } => ("search input", "not applicable"),
            OverlayMode::ShortcutsHelp { .. } => ("root", "not applicable"),
            OverlayMode::None if cx.global::<CurrentPage>().is_journals() => {
                self.journals.read(cx).debug_focus_status(window, cx)
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
            .on_action(cx.listener(Self::open_page))
            .on_action(cx.listener(Self::close_overlay))
            .on_action(cx.listener(Self::open_page_picker))
            .on_action(cx.listener(Self::open_search))
            .on_action(cx.listener(Self::toggle_shortcuts))
            .on_action(cx.listener(Self::navigate_back))
            .on_action(cx.listener(Self::navigate_forward))
            .on_action(|_: &ToggleTheme, _, cx| theme::toggle(cx));
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

        let (name, dirty, view): (SharedString, bool, gpui::AnyView) =
            if cx.global::<CurrentPage>().is_journals() {
                ("Journals".into(), false, self.journals.clone().into())
            } else if let Some(page) = current {
                let p = page.read(cx);
                (p.name().clone(), p.dirty(), p.view().clone().into())
            } else {
                return root.child(
                    div()
                        .flex_1()
                        .text_color(theme::header_fg())
                        .child("No page open."),
                );
            };
        let header_text = if dirty {
            format!("{name} •")
        } else {
            name.to_string()
        };

        let mut root = root.child(Self::render_header(header_text, cx));

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

        // `OutlineView` and `JournalsView` own their scroll containers (it needs the handle on
        // the element whose children are the block rows).
        let body = root.child(view);

        let overlay_child: Option<gpui::AnyElement> = match &self.overlay {
            OverlayMode::PagePicker { entity, .. } => Some(entity.clone().into_any_element()),
            OverlayMode::Search { entity, .. } => Some(entity.clone().into_any_element()),
            OverlayMode::ShortcutsHelp { hints } => {
                Some(ShortcutOverlay::new(hints.clone()).into_any_element())
            }
            OverlayMode::None => None,
        };

        if let Some(child) = overlay_child {
            body.child(
                div()
                    // Keeps scroll wheel (and clicks) from reaching the page behind.
                    .occlude()
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

/// `root` with the home directory shortened to `~`, for the header.
fn display_root(root: &std::path::Path, home: Option<&std::path::Path>) -> String {
    match home.and_then(|home| root.strip_prefix(home).ok()) {
        Some(rest) => std::path::Path::new("~").join(rest).display().to_string(),
        None => root.display().to_string(),
    }
}

fn main() {
    application().run(|cx: &mut App| {
        text_input::bind_keys(cx);
        block_view::bind_keys(cx);
        outline_view::bind_keys(cx);
        page_picker::bind_keys(cx);
        search::bind_keys(cx);
        let cmd = cmd_key();
        cx.bind_keys([
            KeyBinding::new(&format!("{cmd}-s"), SavePage, Some("RootView")),
            KeyBinding::new(&format!("{cmd}-p"), NextPage, Some("RootView")),
            KeyBinding::new(&format!("{cmd}-."), JumpToToday, Some("RootView")),
            KeyBinding::new(&format!("{cmd}-o"), OpenPagePicker, Some("RootView")),
            KeyBinding::new("escape", CloseOverlay, Some("RootView")),
            // GPUI matches predicates against the whole focus path, so the
            // explicit negation preserves literal `?` input (#45).
            KeyBinding::new("?", ToggleShortcuts, Some("RootView && !TextInput")),
            KeyBinding::new(&format!("{cmd}-/"), ToggleShortcuts, None),
            KeyBinding::new(&format!("{cmd}-q"), Quit, None),
            // `!TextInput` keeps a block edit from being navigated out from
            // under the user, and on macOS leaves alt-arrow free for
            // `WordLeft`/`WordRight` (see `text_input::bind_keys`).
            KeyBinding::new("alt-left", NavigateBack, Some("RootView && !TextInput")),
            KeyBinding::new("alt-right", NavigateForward, Some("RootView && !TextInput")),
            KeyBinding::new(&format!("{cmd}-shift-t"), ToggleTheme, Some("RootView")),
            KeyBinding::new(&format!("{cmd}-shift-f"), OpenSearch, Some("RootView")),
        ]);
        // macOS apps conventionally hide with cmd-h (and hide everything
        // else with cmd-alt-h); there is no such convention on Linux, where
        // ctrl-h is just a key.
        #[cfg(target_os = "macos")]
        cx.bind_keys([
            KeyBinding::new("cmd-h", Hide, None),
            KeyBinding::new("cmd-alt-h", HideOtherApps, None),
        ]);
        cx.on_action(|_: &Quit, cx: &mut App| cx.quit());
        cx.on_action(|_: &Hide, cx: &mut App| cx.hide());
        cx.on_action(|_: &HideOtherApps, cx: &mut App| cx.hide_other_apps());
        // Without a menu bar of its own, macOS keeps showing the previously
        // active app's menus. The bold app-menu title comes from the bundle
        // (or, run unbundled, the executable name), not from this `Menu`'s name.
        #[cfg(target_os = "macos")]
        cx.set_menus([
            gpui::Menu::new("GPUI Notes").items([
                gpui::MenuItem::os_submenu("Services", gpui::SystemMenuType::Services),
                gpui::MenuItem::separator(),
                gpui::MenuItem::action("Hide GPUI Notes", Hide),
                gpui::MenuItem::action("Hide Others", HideOtherApps),
                gpui::MenuItem::separator(),
                gpui::MenuItem::action("Quit GPUI Notes", Quit),
            ]),
            gpui::Menu::new("View").items([gpui::MenuItem::action(
                "Toggle Light/Dark Theme",
                ToggleTheme,
            )]),
        ]);

        let root_dir = NotesStore::default_root().expect("resolve notes root");
        let store = NotesStore::new(root_dir).expect("init notes store");
        let (link_index, skipped) = LinkIndex::rebuild_from(&store).expect("index page links");
        cx.set_global(link_index);
        cx.set_global(PageRegistry::new(store));
        cx.set_global(CurrentPage::default());
        cx.set_global(LastError::default());
        if !skipped.is_empty() {
            let files: Vec<String> = skipped
                .iter()
                .map(|(source, err)| format!("{} ({err})", source_label(source)))
                .collect();
            errors::report(
                format!("skipped unreadable notes: {}", files.join(", ")),
                cx,
            );
        }
        let mut spell_checker = SpellChecker::new(spellcheck::personal_dictionary_path());
        if let Err(e) = spell_checker.load_personal_words() {
            errors::report(format!("couldn't read the personal dictionary: {e}"), cx);
        }
        cx.set_global(spell_checker);
        cx.set_global(NavHistory::default());
        save_all_on_quit(cx);
        // Deliberately not `navigate_to`: the first page is not a history
        // entry, so the back stack starts empty.
        journal::open_journals(cx).expect("open journals");

        let bounds = Bounds::centered(None, size(px(640.0), px(420.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |window, cx| {
                let root = cx.new(RootView::new);
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
        let (root, vcx) = cx.add_window_view(move |_window, cx| {
            text_input::bind_keys(cx);
            block_view::bind_keys(cx);
            outline_view::bind_keys(cx);
            page_picker::bind_keys(cx);
            search::bind_keys(cx);
            cx.bind_keys([
                KeyBinding::new("ctrl-s", SavePage, Some("RootView")),
                KeyBinding::new("ctrl-p", NextPage, Some("RootView")),
                KeyBinding::new("ctrl-.", JumpToToday, Some("RootView")),
                KeyBinding::new("ctrl-o", OpenPagePicker, Some("RootView")),
                KeyBinding::new("escape", CloseOverlay, Some("RootView")),
                KeyBinding::new("?", ToggleShortcuts, Some("RootView && !TextInput")),
                KeyBinding::new("ctrl-/", ToggleShortcuts, None),
                KeyBinding::new("alt-left", NavigateBack, Some("RootView && !TextInput")),
                KeyBinding::new("alt-right", NavigateForward, Some("RootView && !TextInput")),
                KeyBinding::new("ctrl-shift-t", ToggleTheme, Some("RootView")),
                KeyBinding::new("ctrl-shift-f", OpenSearch, Some("RootView")),
            ]);
            let store = NotesStore::new(&root_dir).unwrap();
            store.write("Home", "- home\n").unwrap();
            store.write("Tagged", "- has #todo\n").unwrap();
            cx.set_global(LinkIndex::rebuild_from(&store).unwrap().0);
            cx.set_global(PageRegistry::new(store));
            cx.set_global(CurrentPage::default());
            cx.set_global(LastError::default());
            cx.set_global(NavHistory::default());
            save_all_on_quit(cx);
            // Setup, not navigation: matches `main`'s startup, back stack empty.
            set_current_page(&NoteSource::Page("Home".into()), cx).unwrap();
            RootView::new(cx)
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

    /// End to end for the reported bug: a journal block referencing a page,
    /// then opening that page, must show the reference under the page body.
    #[gpui::test]
    fn opening_a_page_shows_references_from_a_journal(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);
        let today = gpui_notes::journal::today();

        // Write the reference the way the user would: into today's journal.
        cx.update(|_, cx| {
            let journal = cx
                .update_global::<PageRegistry, _>(|reg, cx| reg.open_or_create_journal(today, cx))
                .unwrap();
            let first = journal.read(cx).outline().first_block_id().unwrap();
            journal.update(cx, |page, cx| {
                page.set_block_text(first, "issue with [[NSSP ETL failures]]", cx);
            });
            cx.update_global::<PageRegistry, _>(|reg, cx| reg.save(&journal, cx))
                .unwrap();
        });
        cx.run_until_parked();

        cx.update(|window, cx| {
            root.update(cx, |root, cx| {
                root.open_page(
                    &OpenPage {
                        name: "NSSP ETL failures".into(),
                    },
                    window,
                    cx,
                );
            });
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let page = cx.global::<CurrentPage>().get().cloned().unwrap();
            assert_eq!(page.read(cx).name().as_ref(), "NSSP ETL failures");
            let refs = page
                .read(cx)
                .view()
                .read(cx)
                .linked_refs()
                .cloned()
                .expect("references section mounted under the page body");
            let groups = refs.read(cx).groups();
            assert_eq!(
                groups.iter().map(|(s, _)| s.clone()).collect::<Vec<_>>(),
                vec![NoteSource::Journal(today)],
                "the journal that links the page must be listed",
            );
            assert_eq!(groups[0].1.len(), 1);
        });
    }

    /// Logseq writes journal links as the title (`[[2026-10-09]]`) while the
    /// file is `journals/2026_10_09.md`; clicking one must open the journal,
    /// not create `pages/2026-10-09.md`.
    #[gpui::test]
    fn clicking_journal_title_link_opens_the_journal(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("journals")).unwrap();
        std::fs::write(tmp.path().join("journals/2026_10_09.md"), "- that day\n").unwrap();
        let (_root, cx) = mount_root(cx, &tmp);

        let first = cx.update(|_, cx| {
            let home = cx.global::<CurrentPage>().get().cloned().unwrap();
            let first = home.read(cx).outline().first_block_id().unwrap();
            home.update(cx, |page, cx| {
                page.set_block_text(first, "[[2026-10-09]]", cx);
            });
            first
        });
        cx.run_until_parked();

        let bounds = cx
            .debug_bounds(format!("block-view-{first:?}").leak())
            .expect("home block painted");
        cx.simulate_click(
            point(bounds.origin.x + px(8.0), bounds.center().y),
            Modifiers::none(),
        );
        cx.run_until_parked();

        let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 9).unwrap();
        cx.read(|cx| {
            let current = cx.global::<CurrentPage>().get().cloned().unwrap();
            let source = cx.global::<PageRegistry>().source_for_page(&current, cx);
            assert_eq!(source, NoteSource::Journal(date));
            assert_eq!(current.read(cx).name().as_ref(), "2026-10-09");
            assert!(current.read(cx).body().contains("that day"));
        });
        assert!(!tmp.path().join("pages/2026-10-09.md").exists());
    }

    #[gpui::test]
    fn open_page_action_opens_missing_target_without_creating_it(cx: &mut TestAppContext) {
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
            assert!(!cx.global::<LinkIndex>().page_exists("New Target"));
            assert!(root.read(cx).overlay().is_none());
        });
        assert!(!tmp.path().join("pages/New Target.md").exists());
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

    /// `ctrl-/` toggles the shortcuts overlay; Escape dismisses it.
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

    fn focused_text(cx: &mut VisualTestContext) -> Option<String> {
        cx.update(|window, cx| {
            let page = cx.global::<CurrentPage>().get().cloned()?;
            let id = page.read(cx).view().read(cx).focused_block_id(window, cx)?;
            page.read(cx).outline().get(id).map(str::to_owned)
        })
    }

    /// `ctrl-shift-f`, a lowercase query and Enter open the note holding the
    /// hit and focus that block, not the page's first one.
    #[gpui::test]
    fn search_opens_the_hit_and_focuses_its_block(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);
        NotesStore::new(tmp.path())
            .unwrap()
            .write("Deep", "- top\n  - the Needle is here\n- after\n")
            .unwrap();

        cx.simulate_keystrokes("ctrl-shift-f");
        cx.run_until_parked();
        assert!(cx.read(|cx| root.read(cx).overlay().search().is_some()));
        assert_root_in_focus_path(cx, &root);

        cx.simulate_input("needle");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        cx.read(|cx| {
            assert!(root.read(cx).overlay().is_none(), "search dismissed");
            let current = cx.global::<CurrentPage>().get().unwrap();
            assert_eq!(current.read(cx).name().as_ref(), "Deep");
        });
        assert_eq!(focused_text(cx).as_deref(), Some("the Needle is here"));
        assert_root_in_focus_path(cx, &root);
    }

    /// An open page is searched as it stands in memory, and a hit folded
    /// under a collapsed parent focuses that parent without unfolding it.
    #[gpui::test]
    fn search_reads_unsaved_edits_and_stops_at_a_folded_parent(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);
        cx.update(|_, cx| {
            NotesStore::new(tmp.path())
                .unwrap()
                .write("Folded", "- parent\n  - child\n")
                .unwrap();
            let page = cx
                .update_global::<PageRegistry, _>(|reg, cx| reg.open("Folded", cx))
                .unwrap();
            page.update(cx, |page, cx| {
                let parent = page.outline().roots[0].id;
                let child = page.outline().roots[0].children[0].id;
                page.set_block_text(child, "unsaved needle", cx);
                page.toggle_collapse(parent, cx);
            });
        });

        cx.simulate_keystrokes("ctrl-shift-f");
        cx.run_until_parked();
        cx.simulate_input("NEEDLE");
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        cx.read(|cx| {
            let current = cx.global::<CurrentPage>().get().unwrap();
            assert_eq!(current.read(cx).name().as_ref(), "Folded");
            assert!(current.read(cx).outline().roots[0].collapsed, "fold kept");
        });
        assert_eq!(focused_text(cx).as_deref(), Some("parent"));
        assert_root_in_focus_path(cx, &root);
    }

    // ────────────────────────────────────────────────────────────────────
    // OverlayMode state-transition tests, one per discrete transition. The
    // shape is always: set the precondition, fire a simulated input, assert
    // the resulting variant.
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

    /// Ctrl-P advances the current page and clears any overlay. Starts from
    /// `ShortcutsHelp` to also exercise the "any → `None`" branch.
    #[gpui::test]
    fn transition_ctrl_p_clears_overlay_and_advances_page(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("ctrl-/");
        cx.run_until_parked();
        assert_root_in_focus_path(cx, &root);
        assert!(cx.read(|cx| root.read(cx).overlay().is_shortcuts()));

        cx.simulate_keystrokes("ctrl-p");
        cx.run_until_parked();
        assert_root_in_focus_path(cx, &root);

        cx.read(|cx| {
            assert!(
                root.read(cx).overlay().is_none(),
                "shortcuts overlay cleared"
            );
            let current = cx.global::<CurrentPage>().get().unwrap();
            // mount_root started on Home; ctrl-p cycles to the next entry.
            assert_ne!(current.read(cx).name().as_ref(), "Home");
        });
    }

    /// Ctrl-. opens today's journal and clears any overlay.
    #[gpui::test]
    fn transition_ctrl_dot_opens_journals(cx: &mut TestAppContext) {
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
            assert!(cx.global::<CurrentPage>().is_journals());
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
    /// invariant by stacking it: shortcuts open → picker open → select
    /// → overlay is None. The intermediate shortcuts exit and picker
    /// dismissal both have to happen for the assertion to hold.
    #[gpui::test]
    fn invariant_picker_selection_clears_all_overlays(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("ctrl-/");
        cx.run_until_parked();
        assert!(cx.read(|cx| root.read(cx).overlay().is_shortcuts()));

        // Opening the picker over the shortcuts overlay replaces it — the new
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

    #[test]
    fn display_root_shortens_home_to_tilde() {
        let home = std::path::Path::new("/home/me");
        assert_eq!(
            display_root(std::path::Path::new("/home/me/logseq"), Some(home)),
            "~/logseq"
        );
        assert_eq!(
            display_root(std::path::Path::new("/srv/notes"), Some(home)),
            "/srv/notes"
        );
    }

    /// The current page's name, or `"Journals"` in journals mode.
    fn current_page_name(cx: &mut VisualTestContext) -> String {
        cx.read(|cx| {
            let current = cx.global::<CurrentPage>();
            if current.is_journals() {
                return "Journals".to_string();
            }
            current.get().unwrap().read(cx).name().to_string()
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
        assert_eq!(current_page_name(cx), "Journals");

        cx.simulate_keystrokes("alt-right");
        cx.run_until_parked();
        assert_eq!(
            current_page_name(cx),
            "Journals",
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

    /// Journals mode is a history entry like any page: going back from it
    /// returns to the page it was opened from.
    #[gpui::test]
    fn back_from_journals_returns_to_page(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (_root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("ctrl-.");
        cx.run_until_parked();
        assert_eq!(current_page_name(cx), "Journals");

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

    // ────────────────────────────────────────────────────────────────────
    // Journals mode.
    // ────────────────────────────────────────────────────────────────────

    /// Writes a journal `days_ago` days before today with `blocks` blocks.
    fn write_journal(tmp: &TempDir, days_ago: i64, blocks: usize) -> chrono::NaiveDate {
        let date = journal::today() - chrono::Duration::days(days_ago);
        let body = (0..blocks)
            .map(|i| format!("- {date} block {i}\n"))
            .collect::<Vec<_>>()
            .concat();
        let dir = tmp.path().join("journals");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(format!("{}.md", date.format("%Y_%m_%d"))), body).unwrap();
        date
    }

    fn loaded_journal_dates(
        root: &Entity<RootView>,
        cx: &mut VisualTestContext,
    ) -> Vec<chrono::NaiveDate> {
        cx.read(|cx| {
            root.read(cx)
                .journals
                .read(cx)
                .pages()
                .map(|(date, _)| date)
                .collect()
        })
    }

    /// Today first (in memory, since it has no file yet), then only the
    /// journals that exist, newest first: the empty days between are neither
    /// shown nor written to disk.
    #[gpui::test]
    fn journals_mode_lists_today_then_existing_journals_newest_first(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let older = write_journal(&tmp, 5, 1);
        let newer = write_journal(&tmp, 2, 1);
        let (root, cx) = mount_root(cx, &tmp);

        cx.simulate_keystrokes("ctrl-.");
        cx.run_until_parked();
        assert_root_in_focus_path(cx, &root);

        let today = journal::today();
        assert_eq!(loaded_journal_dates(&root, cx), vec![today, newer, older]);
        let files = std::fs::read_dir(tmp.path().join("journals"))
            .unwrap()
            .count();
        assert_eq!(files, 2, "not even today's journal is created (#71)");
    }

    /// Days arrive `DAYS_PER_LOAD` at a time, and scrolling toward the end of
    /// the list loads the next batch.
    #[gpui::test]
    fn journals_mode_loads_more_days_on_scroll(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        // Tall days, so the first batch overflows the window by more than
        // the load-ahead margin and loading stops there.
        for days_ago in 1..=12 {
            write_journal(&tmp, days_ago, 80);
        }
        let (root, cx) = mount_root(cx, &tmp);
        cx.simulate_keystrokes("ctrl-.");
        cx.run_until_parked();
        assert_eq!(
            loaded_journal_dates(&root, cx).len(),
            gpui_notes::journals_view::DAYS_PER_LOAD,
        );

        let viewport = cx.update(|window, _| window.viewport_size());
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: point(viewport.width / 2., viewport.height / 2.),
            delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
            modifiers: Modifiers::none(),
            touch_phase: gpui::TouchPhase::Moved,
        });
        cx.run_until_parked();

        let dates = loaded_journal_dates(&root, cx);
        assert!(
            dates.len() > gpui_notes::journals_view::DAYS_PER_LOAD,
            "scrolling to the end loads more, got {}",
            dates.len(),
        );
        assert!(dates.is_sorted_by(|a, b| a > b), "still newest first");
    }

    /// A scroll over an open overlay scrolls the overlay, never the page
    /// behind it: here, the journals would load more days if it leaked.
    #[gpui::test]
    fn scrolling_an_overlay_leaves_the_page_behind_alone(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        for days_ago in 1..=12 {
            write_journal(&tmp, days_ago, 80);
        }
        let (root, cx) = mount_root(cx, &tmp);
        cx.simulate_keystrokes("ctrl-.");
        cx.run_until_parked();

        for open in ["ctrl-/", "ctrl-o"] {
            cx.simulate_keystrokes(open);
            cx.run_until_parked();
            let viewport = cx.update(|window, _| window.viewport_size());
            cx.simulate_event(gpui::ScrollWheelEvent {
                // Inside the overlay, which hangs from the top centre.
                position: point(viewport.width / 2., px(120.)),
                delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-100_000.))),
                modifiers: Modifiers::none(),
                touch_phase: gpui::TouchPhase::Moved,
            });
            cx.run_until_parked();
            assert_eq!(
                loaded_journal_dates(&root, cx).len(),
                gpui_notes::journals_view::DAYS_PER_LOAD,
                "scrolling the {open} overlay scrolled the journals behind it",
            );
            cx.simulate_keystrokes("escape");
            cx.run_until_parked();
        }
    }

    /// Short days that leave the window unfilled keep loading without a
    /// scroll, since there may be nothing to scroll yet.
    #[gpui::test]
    fn journals_mode_keeps_loading_until_the_window_is_filled(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        for days_ago in 1..=6 {
            write_journal(&tmp, days_ago, 1);
        }
        let (root, cx) = mount_root(cx, &tmp);
        cx.simulate_keystrokes("ctrl-.");
        cx.run_until_parked();
        assert_eq!(loaded_journal_dates(&root, cx).len(), 7);
    }

    /// A day's outline doesn't scroll itself, so the arrow keys and the caret
    /// have to reach `JournalsView`'s scroll container.
    #[gpui::test]
    fn journals_mode_follows_focus_and_caret_past_the_bottom(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let today = write_journal(&tmp, 0, 12);
        let (root, cx) = mount_root(cx, &tmp);
        cx.simulate_resize(gpui::size(px(600.), px(300.)));
        cx.simulate_keystrokes("ctrl-.");
        cx.run_until_parked();
        let target = cx.read(|cx| {
            let journals = root.read(cx).journals.read(cx);
            let (_, page) = journals.pages().find(|(date, _)| *date == today).unwrap();
            page.read(cx).outline().roots[10].id
        });
        let bounds = |cx: &mut VisualTestContext| {
            cx.debug_bounds(format!("block-view-{target:?}").leak())
                .expect("block painted")
        };

        for _ in 0..10 {
            cx.simulate_keystrokes("down");
        }
        cx.run_until_parked();
        // The scroll container ends above the window's bottom (the shortcut
        // bar is below it). The reveal left the row `autoscroll`'s 4px margin
        // above that end, which is how the typing check finds it.
        let revealed = bounds(cx);
        let viewport_bottom = revealed.bottom() + px(4.);
        let height = cx.update(|window, _| window.viewport_size().height);
        assert!(
            revealed.top() >= px(0.) && revealed.bottom() <= height,
            "{revealed:?} is outside a {height:?} window",
        );

        cx.simulate_keystrokes("enter");
        cx.simulate_input(&" and more words".repeat(8));
        cx.run_until_parked();
        let typed = bounds(cx);
        assert!(
            typed.size.height > revealed.size.height,
            "the text should wrap: {typed:?}"
        );
        assert!(
            typed.bottom() <= viewport_bottom,
            "the caret line went below {viewport_bottom:?}: {typed:?}",
        );
    }

    /// An edit to any listed journal is saved when navigating away, not just
    /// one to today's.
    #[gpui::test]
    fn leaving_journals_mode_saves_edited_journals(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let yesterday = write_journal(&tmp, 1, 1);
        let (root, cx) = mount_root(cx, &tmp);
        cx.simulate_keystrokes("ctrl-.");
        cx.run_until_parked();

        cx.update(|_, cx| {
            let (_, page) = root
                .read(cx)
                .journals
                .read(cx)
                .pages()
                .find(|(date, _)| *date == yesterday)
                .map(|(date, page)| (date, page.clone()))
                .unwrap();
            let first = page.read(cx).outline().first_block_id().unwrap();
            page.update(cx, |page, cx| page.set_block_text(first, "edited", cx));
        });

        cx.simulate_keystrokes("ctrl-p");
        cx.run_until_parked();

        let file = tmp
            .path()
            .join(format!("journals/{}.md", yesterday.format("%Y_%m_%d")));
        assert_eq!(std::fs::read_to_string(file).unwrap(), "- edited\n");
    }

    #[gpui::test]
    fn back_returns_to_journals_mode(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (_root, cx) = mount_root(cx, &tmp);
        cx.simulate_keystrokes("ctrl-.");
        cx.run_until_parked();
        cx.simulate_keystrokes("ctrl-p");
        cx.run_until_parked();
        assert_ne!(current_page_name(cx), "Journals");

        cx.simulate_keystrokes("alt-left");
        cx.run_until_parked();
        assert_eq!(current_page_name(cx), "Journals");
    }

    #[gpui::test]
    fn shortcut_toggles_between_light_and_dark(cx: &mut TestAppContext) {
        let tmp = tempfile::tempdir().unwrap();
        let (_root, cx) = mount_root(cx, &tmp);
        assert_eq!(theme::current(), theme::Theme::SolarizedDark);
        let dark_bg = theme::window_bg();

        cx.simulate_keystrokes("ctrl-shift-t");
        cx.run_until_parked();
        assert_eq!(theme::current(), theme::Theme::SolarizedLight);
        assert_ne!(theme::window_bg(), dark_bg);

        cx.simulate_keystrokes("ctrl-shift-t");
        cx.run_until_parked();
        assert_eq!(theme::current(), theme::Theme::SolarizedDark);
        assert_eq!(theme::window_bg(), dark_bg);
    }
}
