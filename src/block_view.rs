//! Per-block view that swaps between rendered markdown and a raw-text editor.
//! See issues #6 and #42.
//!
//! Focus and edit mode are decoupled (#42): a focused block merely shows a
//! focus ring and stays in `Viewing`. Editing starts only on an explicit
//! trigger — a click on the block body or `enter` on the focused block — and
//! Escape returns to focused-`Viewing`, parking focus on the block itself.
//!
//! Only one block can be in `Editing` at a time — GPUI's focus system enforces
//! this naturally (a single focused leaf). The outline stored on `Page` is the
//! source of truth; the `TextInput`'s buffer is flushed to the outline on every
//! change (and again on blur, when the input is dropped), so there is no hidden
//! state to drift out of sync and every save path sees in-flight edits (#38).

use gpui::{
    actions, anchored, deferred, div, prelude::*, px, AnyElement, App, AppContext, Context, Corner,
    ElementId, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, KeyBinding, MouseButton,
    ParentElement, Render, SharedString, Styled, Subscription, Window,
};

use crate::block_render::{render_block, theme};
use crate::outline::BlockId;
use crate::page::Page;
use crate::page_completion::{self, CompletionKind, CompletionState};
use crate::page_links::{LinkIndex, PageLinkExt};
use crate::tags::{TagExt, TagIndex};
use crate::text_input::{
    PendingVerticalPlacement, TextInput, TextInputEvent, VerticalDirection, VerticalMovementOutcome,
};

actions!(block_view, [BeginEditing, MoveCaretUp, MoveCaretDown]);

/// Registers block-level key bindings. Enter is limited to viewing mode, while
/// plain Up/Down bubble from the focused input through the editing context.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", BeginEditing, Some("BlockView && !editing")),
        KeyBinding::new("up", MoveCaretUp, Some("BlockView && editing")),
        KeyBinding::new("down", MoveCaretDown, Some("BlockView && editing")),
    ]);
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AdjacentEditRequest {
    pub(crate) direction: VerticalDirection,
    pub(crate) preferred_screen_x: gpui::Pixels,
}

/// Events emitted by `BlockView` to its parent (typically `OutlineView`).
#[derive(Debug, Clone)]
pub(crate) enum BlockViewEvent {
    /// The user finished a block with Enter and a new sibling was inserted
    /// after `self.block_id`. The parent should mount the view for the newly
    /// created block and put it into editing.
    FocusRequested(BlockId),
    /// Vertical caret movement passed the first or last visual row. The
    /// outline resolves the adjacent visible block and preserves screen x.
    AdjacentEditRequested(AdjacentEditRequest),
}

impl EventEmitter<BlockViewEvent> for BlockView {}

/// The two display modes for a single block. `Editing` carries the live
/// `TextInput` and the two subscriptions that drive the exit transitions
/// (blur and Escape), so every edit cycle replaces them as a unit — there is
/// no way to have a dangling subscription without an input or vice versa.
pub enum BlockMode {
    Viewing,
    Editing {
        input: Entity<TextInput>,
        /// The `[[Page]]` / `#tag` popup, when the caret sits in a completion
        /// site. It lives inside `Editing` so "popup open on a block that
        /// isn't being edited" is unrepresentable and every exit path closes
        /// it for free.
        completion: Option<CompletionState>,
        /// Drops on edit exit. Fires `end_editing` when focus leaves the input.
        _on_blur: Subscription,
        /// Drops on edit exit. Fires on `TextInputEvent::Cancelled` (Escape)
        /// because the blur listener can lag a frame.
        _on_event: Subscription,
        /// Drops on edit exit. Recomputes the completion popup. Observes the
        /// input rather than subscribing to `Changed` because caret movement
        /// notifies without changing the text, and moving out of `[[foo`
        /// must close the popup.
        _on_input_notify: Subscription,
    },
}

pub struct BlockView {
    block_id: BlockId,
    page: Entity<Page>,
    focus_handle: FocusHandle,
    mode: BlockMode,
    _on_focus: Subscription,
    /// Re-render on wrapper blur so the focus ring clears (#42).
    _on_self_blur: Subscription,
    _page_sub: Subscription,
    /// Missing/existing page-link styling changes when targets are created.
    _link_index_sub: Option<Subscription>,
}

impl BlockView {
    pub fn new(
        block_id: BlockId,
        page: Entity<Page>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        // Focus only draws the ring (#42) — editing is an explicit
        // transition via click or `enter`, never a focus side effect.
        let on_focus = cx.on_focus(&focus_handle, window, |_, _, cx| cx.notify());
        let on_self_blur = cx.on_blur(&focus_handle, window, |_, _, cx| cx.notify());
        // Re-render when the page outline changes (e.g., another block was
        // edited) so our rendered markdown stays current.
        let page_sub = cx.observe(&page, |_, _, cx| cx.notify());
        let link_index_sub = cx
            .has_global::<LinkIndex>()
            .then(|| cx.observe_global::<LinkIndex>(|_, cx| cx.notify()));

        Self {
            block_id,
            page,
            focus_handle,
            mode: BlockMode::Viewing,
            _on_focus: on_focus,
            _on_self_blur: on_self_blur,
            _page_sub: page_sub,
            _link_index_sub: link_index_sub,
        }
    }

    #[must_use]
    pub fn block_id(&self) -> BlockId {
        self.block_id
    }

    #[must_use]
    pub fn mode(&self) -> &BlockMode {
        &self.mode
    }

    #[must_use]
    pub fn is_editing(&self) -> bool {
        matches!(self.mode, BlockMode::Editing { .. })
    }

    pub(crate) fn begin_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.begin_editing_with_placement(None, window, cx);
    }

    pub(crate) fn begin_editing_at(
        &mut self,
        placement: PendingVerticalPlacement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.begin_editing_with_placement(Some(placement), window, cx);
    }

    fn begin_editing_with_placement(
        &mut self,
        placement: Option<PendingVerticalPlacement>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(self.mode, BlockMode::Editing { .. }) {
            return;
        }
        let text = self
            .page
            .read(cx)
            .outline()
            .get(self.block_id)
            .unwrap_or("")
            .to_string();
        let input = cx.new(|cx| {
            let mut input = TextInput::with_wrapped_content(cx, "", text);
            if let Some(placement) = placement {
                input.set_pending_vertical_placement(placement);
            }
            input
        });
        let input_focus = input.focus_handle(cx);
        let on_blur = cx.on_blur(&input_focus, window, |this, window, cx| {
            this.end_editing(window, cx);
        });
        let on_event = cx.subscribe_in(&input, window, |this, input, event, window, cx| {
            match event {
                // Escape returns to focused-Viewing on this block (#42), so
                // the block stays visibly selected and window-level bindings
                // (ctrl-s etc.) keep dispatching through the focus chain.
                TextInputEvent::Cancelled => {
                    // The completion popup swallows the first Escape: it is a
                    // transient layer over the edit, so dismissing it must not
                    // also throw the user out of the block.
                    if this.close_completion(cx) {
                        return;
                    }
                    this.end_editing_inner(cx);
                    window.focus(&this.focus_handle, cx);
                }
                // Enter finishes the block, except where the block is a body
                // rather than a line — then it just breaks the line, and
                // shift-enter is the way out (#62).
                TextInputEvent::Submitted => {
                    if this.accept_completion(cx) {
                        return;
                    }
                    if extends_on_enter(input.read(cx).content()) {
                        input.update(cx, |input, cx| input.insert_newline(window, cx));
                    } else {
                        this.insert_sibling_below(cx);
                    }
                }
                TextInputEvent::SubmittedNow => {
                    this.insert_sibling_below(cx);
                }
                // Flush every keystroke to the page so all save paths
                // (ctrl-s, page-switch autosave, quit-save) see the
                // in-flight text (#38). `set_block_text` is a no-op on
                // identical text, so this cannot spuriously dirty the
                // page (#39).
                TextInputEvent::Changed(text) => {
                    let block_id = this.block_id;
                    let text = text.to_string();
                    this.page
                        .update(cx, |p, cx| p.set_block_text(block_id, text, cx));
                    Self::autopair_page_link(input, cx);
                }
            }
        });
        let on_input_notify = cx.observe(&input, |this, input, cx| {
            this.refresh_completion(&input, cx);
        });
        window.focus(&input_focus, cx);
        self.mode = BlockMode::Editing {
            input,
            completion: None,
            _on_blur: on_blur,
            _on_event: on_event,
            _on_input_notify: on_input_notify,
        };
        cx.notify();
    }

    /// Closes `[[` with `]]` so the link is well-formed the moment the popup
    /// opens, leaving the caret between the pairs.
    ///
    /// Driven from `Changed` rather than the completion refresh so it can only
    /// fire on an actual edit: parking the caret inside an existing `[[foo]]`
    /// also opens a popup with an empty query, and must not insert anything.
    fn autopair_page_link(input: &Entity<TextInput>, cx: &mut Context<Self>) {
        let (content, selection) = {
            let input = input.read(cx);
            (input.content().clone(), input.selected_range())
        };
        if !selection.is_empty() {
            return;
        }
        let caret = selection.start;
        if !content[..caret].ends_with("[[") || content[caret..].starts_with("]]") {
            return;
        }
        input.update(cx, |input, cx| {
            input.replace_range(caret..caret, "]]", 0, cx);
        });
    }

    /// Recomputes the completion popup from the editor's text and caret.
    fn refresh_completion(&mut self, input: &Entity<TextInput>, cx: &mut Context<Self>) {
        let BlockMode::Editing { completion, .. } = &self.mode else {
            return;
        };
        let previous = completion
            .as_ref()
            .map(|state| (state.range.clone(), state.selected));

        let (content, selection) = {
            let input = input.read(cx);
            (input.content().clone(), input.selected_range())
        };
        let next = selection
            .is_empty()
            .then(|| page_completion::trigger_at(&content, selection.start))
            .flatten()
            .and_then(|trigger| {
                let names = Self::completion_names(trigger.kind, cx);
                CompletionState::new(&trigger, &names)
            })
            .map(|mut state| {
                // Repainting the input re-notifies without moving the caret;
                // keep the highlighted row so the popup doesn't flick back to
                // the top under the user.
                if let Some((range, selected)) = &previous {
                    if *range == state.range {
                        state.selected = (*selected).min(state.candidates.len() - 1);
                    }
                }
                state
            });

        let BlockMode::Editing { completion, .. } = &mut self.mode else {
            return;
        };
        if *completion != next {
            *completion = next;
            cx.notify();
        }
    }

    /// The names offered for `kind`. Both indexes are optional globals — tests
    /// that don't seed them still get the "New page/tag" row.
    fn completion_names(kind: CompletionKind, cx: &App) -> Vec<SharedString> {
        match kind {
            CompletionKind::Page => cx
                .try_global::<LinkIndex>()
                .map(LinkIndex::page_names)
                .unwrap_or_default(),
            CompletionKind::Tag => cx
                .try_global::<TagIndex>()
                .map(|index| {
                    index
                        .all_tags()
                        .into_iter()
                        .map(|summary| summary.display)
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Dismisses the popup. Returns whether there was one to dismiss, which is
    /// how Escape decides between closing the popup and leaving edit mode.
    fn close_completion(&mut self, cx: &mut Context<Self>) -> bool {
        let BlockMode::Editing { completion, .. } = &mut self.mode else {
            return false;
        };
        let had_popup = completion.take().is_some();
        if had_popup {
            cx.notify();
        }
        had_popup
    }

    /// Splices the highlighted candidate over the trigger. Returns whether a
    /// popup was open, which is how Enter decides between accepting and its
    /// usual "finish this block" meaning.
    fn accept_completion(&mut self, cx: &mut Context<Self>) -> bool {
        let (input, state) = match &mut self.mode {
            BlockMode::Editing {
                input, completion, ..
            } => match completion.take() {
                Some(state) => (input.clone(), state),
                None => return false,
            },
            BlockMode::Viewing => return false,
        };
        let Some(insertion) = state.insertion() else {
            return false;
        };

        let mut range = state.range;
        let content = input.read(cx).content().clone();
        // Swallow the auto-paired `]]` so accepting yields `[[Name]]` rather
        // than `[[Name]]]]`.
        if state.kind == CompletionKind::Page && content[range.end..].starts_with("]]") {
            range.end += 2;
        }
        let caret = insertion.len();
        input.update(cx, |input, cx| {
            input.replace_range(range, &insertion, caret, cx);
        });
        cx.notify();
        true
    }

    fn accept_completion_row(&mut self, row: usize, cx: &mut Context<Self>) {
        if let BlockMode::Editing {
            completion: Some(state),
            ..
        } = &mut self.mode
        {
            state.selected = row;
        }
        self.accept_completion(cx);
    }

    /// Up/Down drive the popup while it is open; only otherwise do they move
    /// the caret between visual rows.
    fn move_completion_selection(
        &mut self,
        direction: VerticalDirection,
        cx: &mut Context<Self>,
    ) -> bool {
        let BlockMode::Editing {
            completion: Some(state),
            ..
        } = &mut self.mode
        else {
            return false;
        };
        match direction {
            VerticalDirection::Up => state.select_up(),
            VerticalDirection::Down => state.select_down(),
        }
        cx.notify();
        true
    }

    fn move_caret_vertical(&mut self, direction: VerticalDirection, cx: &mut Context<Self>) {
        if self.move_completion_selection(direction, cx) {
            return;
        }
        let BlockMode::Editing { input, .. } = &self.mode else {
            return;
        };
        let outcome = input.update(cx, |input, cx| input.move_vertical(direction, cx));
        if let VerticalMovementOutcome::Adjacent {
            direction,
            preferred_screen_x,
        } = outcome
        {
            cx.emit(BlockViewEvent::AdjacentEditRequested(AdjacentEditRequest {
                direction,
                preferred_screen_x,
            }));
        }
    }

    fn end_editing(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.end_editing_inner(cx);
    }

    /// Take the live `TextInput` from `mode` (transitioning to `Viewing`) and
    /// return it. Drops the blur/event subscriptions atomically.
    fn take_input_on_exit(&mut self) -> Option<Entity<TextInput>> {
        match std::mem::replace(&mut self.mode, BlockMode::Viewing) {
            BlockMode::Editing { input, .. } => Some(input),
            BlockMode::Viewing => None,
        }
    }

    /// Flush the in-progress edit, insert an empty sibling immediately after
    /// this block, and ask the parent view to focus the new block. The new
    /// block's text starts empty regardless of where the caret was — splitting
    /// at the caret is a follow-up (see issue #31).
    fn insert_sibling_below(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.take_input_on_exit() else {
            return;
        };
        let text = input.read(cx).content().to_string();
        let block_id = self.block_id;
        let new_id = self.page.update(cx, |p, cx| {
            p.set_block_text(block_id, text, cx);
            p.insert_block_after(block_id, "", cx)
        });
        cx.notify();
        if let Some(new_id) = new_id {
            cx.emit(BlockViewEvent::FocusRequested(new_id));
        }
    }

    fn end_editing_inner(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.take_input_on_exit() else {
            return;
        };
        let text = input.read(cx).content().to_string();
        let block_id = self.block_id;
        self.page
            .update(cx, |p, cx| p.set_block_text(block_id, text, cx));
        cx.notify();
    }
}

/// Whether Enter should add a line to the block rather than start a new one:
/// once the block already spans lines, or once it has opened a code fence that
/// is still waiting for its closing one.
fn extends_on_enter(text: &str) -> bool {
    text.contains('\n') || text.matches("```").count() % 2 == 1
}

impl Focusable for BlockView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl BlockView {
    /// The completion popup, anchored under the caret. Returns `None` unless a
    /// popup is open.
    ///
    /// The caret rectangle comes from the input's *painted* layout, so on the
    /// frame the popup opens it trails the buffer by the characters just
    /// typed; the next frame corrects it. Before the first paint there is no
    /// rectangle at all, and `anchored` falls back to laying the popup out
    /// where it sits in the tree — directly under the block.
    fn render_completion(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let BlockMode::Editing {
            input,
            completion: Some(state),
            ..
        } = &self.mode
        else {
            return None;
        };

        let mut list = div()
            .flex()
            .flex_col()
            .gap_0p5()
            .bg(theme::overlay_bg())
            .border_1()
            .border_color(theme::overlay_border())
            .rounded_md()
            .p_1()
            .min_w(px(220.0))
            .max_w(px(420.0))
            // Keep clicks on the popup from reaching the blocks it covers.
            .occlude();
        for row in 0..state.candidates.len() {
            let Some(label) = state.label(row) else {
                continue;
            };
            let (bg_color, fg_color) = if row == state.selected {
                (theme::row_selected(), theme::row_selected_fg())
            } else {
                (theme::row_bg(), theme::header_fg())
            };
            list = list.child(
                div()
                    .id(ElementId::Name(SharedString::from(format!(
                        "block-completion-row-{row}"
                    ))))
                    .debug_selector(move || format!("completion-row-{row}"))
                    .cursor_pointer()
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .bg(bg_color)
                    .text_color(fg_color)
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.accept_completion_row(row, cx);
                    })),
            );
        }

        let caret = input.read(cx).caret_bounds();
        Some(
            deferred(
                anchored()
                    .anchor(Corner::TopLeft)
                    .snap_to_window_with_margin(px(8.0))
                    .when_some(caret, |el, caret| el.position(caret.bottom_left()))
                    .child(list),
            )
            // Paint above the blocks below this one, and outside the outline's
            // scroll container.
            .with_priority(1)
            .into_any_element(),
        )
    }
}

impl Render for BlockView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content: AnyElement = if let BlockMode::Editing { input, .. } = &self.mode {
            input.clone().into_any_element()
        } else {
            let text: String = self
                .page
                .read(cx)
                .outline()
                .get(self.block_id)
                .unwrap_or("")
                .to_string();
            if text.is_empty() {
                div()
                    .text_color(theme::fg_muted())
                    .italic()
                    .child(SharedString::from("(empty — click to edit)"))
                    .into_any_element()
            } else {
                let page_link_ext = PageLinkExt;
                let tag_ext = TagExt;
                let extensions: [&dyn crate::block_render::InlineExtension; 2] =
                    [&page_link_ext, &tag_ext];
                render_block(&text, &extensions, window, cx)
            }
        };

        // Focus ring (#42): a focused-but-viewing block is outlined so the
        // selection is visible without mounting an editor. While editing the
        // TextInput holds focus, so the ring drops out on its own. The border
        // is always present (transparent when unfocused) to avoid a 1px
        // layout shift on focus.
        let focused = self.focus_handle.is_focused(window) && !self.is_editing();
        let ring = if focused {
            theme::accent()
        } else {
            theme::transparent()
        };

        // Drive the edit transition directly from the click. `begin_editing`
        // is idempotent — when already editing, the click falls through to
        // the TextInput's own mouse_down (which positions the caret).
        //
        // `window.prevent_default()` suppresses the auto-focus listener that
        // `track_focus` installs on any focusable element (see gpui div.rs:
        // `tracked_focus_handle` auto-focus on bubble). Without it, that
        // listener fires *after* `begin_editing` and re-focuses the wrapper's
        // own handle, stealing focus from the freshly mounted TextInput — the
        // box turns white but the cursor never appears until a second click.
        // The `editing` flag lets outline-op bindings scope themselves to
        // "BlockView && !editing" (#44): the wrapper is an ancestor of the
        // TextInput, so without the flag, structural keys the input doesn't
        // bind could trigger outline ops mid-edit. Plain Up/Down are
        // explicitly rebound for visual-row navigation in edit mode.
        let key_context = if self.is_editing() {
            "BlockView editing"
        } else {
            "BlockView"
        };

        div()
            .track_focus(&self.focus_handle)
            .key_context(key_context)
            .on_action(cx.listener(|this, _: &BeginEditing, window, cx| {
                this.begin_editing(window, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveCaretUp, _, cx| {
                this.move_caret_vertical(VerticalDirection::Up, cx);
            }))
            .on_action(cx.listener(|this, _: &MoveCaretDown, _, cx| {
                this.move_caret_vertical(VerticalDirection::Down, cx);
            }))
            .flex_1()
            .min_w_0()
            .min_h(px(20.0))
            .py_0p5()
            .px_0p5()
            .border_1()
            .border_color(ring)
            .rounded_sm()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.begin_editing(window, cx);
                    window.prevent_default();
                }),
            )
            .child(content)
            .children(self.render_completion(cx))
    }
}

#[cfg(test)]
impl BlockView {
    pub(crate) fn test_end_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.end_editing(window, cx);
    }

    pub(crate) fn input_focus_handle_for_test(&self, cx: &App) -> Option<FocusHandle> {
        match &self.mode {
            BlockMode::Editing { input, .. } => Some(input.focus_handle(cx)),
            BlockMode::Viewing => None,
        }
    }

    pub(crate) fn input_entity_for_test(&self) -> Option<Entity<TextInput>> {
        match &self.mode {
            BlockMode::Editing { input, .. } => Some(input.clone()),
            BlockMode::Viewing => None,
        }
    }

    /// The popup's visible rows, or `None` when no popup is open.
    pub(crate) fn completion_labels_for_test(&self) -> Option<Vec<SharedString>> {
        let BlockMode::Editing {
            completion: Some(state),
            ..
        } = &self.mode
        else {
            return None;
        };
        Some(
            (0..state.candidates.len())
                .filter_map(|row| state.label(row))
                .collect(),
        )
    }

    pub(crate) fn completion_selection_for_test(&self) -> Option<usize> {
        match &self.mode {
            BlockMode::Editing {
                completion: Some(state),
                ..
            } => Some(state.selected),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::Page;
    use crate::text_input;
    use gpui::{point, Modifiers, TestAppContext};

    /// Smuggles the `Page` back out of `add_window_view`, which only returns
    /// the root view.
    struct TestPage(Entity<Page>);
    impl gpui::Global for TestPage {}

    /// Mounts a `BlockView` as the window root and activates the window.
    /// Focus listeners only fire during `Window::draw`, and only propagate when
    /// the window is active — missing either leaves the listener silent.
    fn mount<'a>(
        cx: &'a mut TestAppContext,
        body: &str,
    ) -> (
        Entity<Page>,
        Entity<BlockView>,
        &'a mut gpui::VisualTestContext,
    ) {
        let body = body.to_string();
        let (bv, vcx) = cx.add_window_view(move |window, cx| {
            text_input::bind_keys(cx);
            super::bind_keys(cx);
            let page = cx.new(|cx| Page::new("foo".into(), &body, cx));
            let block_id = page.read(cx).outline().first_block_id().unwrap();
            cx.set_global(TestPage(page));
            BlockView::new(block_id, cx.global::<TestPage>().0.clone(), window, cx)
        });
        vcx.update(|window, _| window.activate_window());
        vcx.run_until_parked();
        let page = vcx.read(|cx| cx.global::<TestPage>().0.clone());
        (page, bv, vcx)
    }

    /// Like `mount`, but seeds the two completion indexes so the `[[` and `#`
    /// popups have something to offer: pages `ruff` and `rust`, and the tags
    /// found in `#ruff #rustup`.
    fn mount_with_indexes<'a>(
        cx: &'a mut TestAppContext,
        body: &str,
    ) -> (
        Entity<Page>,
        Entity<BlockView>,
        &'a mut gpui::VisualTestContext,
    ) {
        let body = body.to_string();
        let (bv, vcx) = cx.add_window_view(move |window, cx| {
            text_input::bind_keys(cx);
            super::bind_keys(cx);

            let mut link_index = LinkIndex::default();
            for name in ["ruff", "rust"] {
                let indexed = cx.new(|cx| Page::new(name.into(), "", cx));
                link_index.reindex_page(indexed.read(cx));
            }
            cx.set_global(link_index);

            let mut tag_index = TagIndex::default();
            tag_index.reindex_page(
                &crate::tags::TagSource::Page("tagged".into()),
                &crate::outline::Outline::parse("- #ruff #rustup\n"),
            );
            cx.set_global(tag_index);

            let page = cx.new(|cx| Page::new("foo".into(), &body, cx));
            let block_id = page.read(cx).outline().first_block_id().unwrap();
            cx.set_global(TestPage(page));
            BlockView::new(block_id, cx.global::<TestPage>().0.clone(), window, cx)
        });
        vcx.update(|window, _| window.activate_window());
        vcx.run_until_parked();
        let page = vcx.read(|cx| cx.global::<TestPage>().0.clone());
        (page, bv, vcx)
    }

    /// Puts an empty block into edit mode through the real keyboard path.
    fn edit_empty_block(
        cx: &mut TestAppContext,
    ) -> (
        Entity<Page>,
        Entity<BlockView>,
        &mut gpui::VisualTestContext,
    ) {
        let (page, bv, cx) = mount_with_indexes(cx, "- \n");
        focus_block(cx, &bv);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        (page, bv, cx)
    }

    fn block_text(cx: &mut gpui::VisualTestContext, bv: &Entity<BlockView>) -> String {
        cx.read(|cx| {
            let bv = bv.read(cx);
            bv.input_entity_for_test()
                .expect("still editing")
                .read(cx)
                .content()
                .to_string()
        })
    }

    fn caret(cx: &mut gpui::VisualTestContext, bv: &Entity<BlockView>) -> usize {
        cx.read(|cx| {
            bv.read(cx)
                .input_entity_for_test()
                .expect("still editing")
                .read(cx)
                .selected_range()
                .start
        })
    }

    #[gpui::test]
    fn typing_double_bracket_auto_pairs_and_opens_completion(cx: &mut TestAppContext) {
        let (_page, bv, cx) = edit_empty_block(cx);

        cx.simulate_input("[[");
        cx.run_until_parked();

        assert_eq!(block_text(cx, &bv), "[[]]", "the closing pair is inserted");
        assert_eq!(caret(cx, &bv), 2, "the caret stays between the brackets");
        cx.read(|cx| {
            assert_eq!(
                bv.read(cx).completion_labels_for_test(),
                Some(vec!["ruff".into(), "rust".into()]),
                "an empty query lists every page, with no New row",
            );
        });
    }

    #[gpui::test]
    fn typing_narrows_the_candidates_and_offers_a_new_page(cx: &mut TestAppContext) {
        let (_page, bv, cx) = edit_empty_block(cx);

        cx.simulate_input("[[ru");
        cx.run_until_parked();

        cx.read(|cx| {
            assert_eq!(
                bv.read(cx).completion_labels_for_test(),
                Some(vec!["ruff".into(), "rust".into(), "New page: ru".into()]),
            );
        });
    }

    #[gpui::test]
    fn enter_accepts_the_selected_page_without_leaving_the_block(cx: &mut TestAppContext) {
        let (page, bv, cx) = edit_empty_block(cx);

        cx.simulate_input("[[ru");
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        assert_eq!(
            block_text(cx, &bv),
            "[[ruff]]",
            "the auto-paired `]]` is consumed rather than doubled",
        );
        assert_eq!(caret(cx, &bv), 8, "the caret lands after the link");
        cx.read(|cx| {
            assert!(bv.read(cx).is_editing(), "accepting stays in the block");
            assert!(
                bv.read(cx).completion_labels_for_test().is_none(),
                "the popup closes on accept",
            );
            assert_eq!(
                page.read(cx).outline().roots.len(),
                1,
                "enter must not also insert a sibling",
            );
        });
    }

    #[gpui::test]
    fn enter_on_the_new_page_row_inserts_the_typed_name(cx: &mut TestAppContext) {
        let (_page, bv, cx) = edit_empty_block(cx);

        cx.simulate_input("[[zzz");
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(
                bv.read(cx).completion_labels_for_test(),
                Some(vec!["New page: zzz".into()]),
                "a query matching nothing still offers the new page",
            );
        });

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(block_text(cx, &bv), "[[zzz]]");
    }

    #[gpui::test]
    fn down_moves_the_completion_selection_not_the_caret(cx: &mut TestAppContext) {
        let (_page, bv, cx) = edit_empty_block(cx);

        cx.simulate_input("[[ru");
        cx.simulate_keystrokes("down");
        cx.run_until_parked();

        assert_eq!(caret(cx, &bv), 4, "the caret did not move");
        cx.read(|cx| {
            assert_eq!(bv.read(cx).completion_selection_for_test(), Some(1));
        });

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(block_text(cx, &bv), "[[rust]]");
    }

    #[gpui::test]
    fn escape_closes_the_popup_before_it_leaves_editing(cx: &mut TestAppContext) {
        let (_page, bv, cx) = edit_empty_block(cx);

        cx.simulate_input("[[ru");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        cx.read(|cx| {
            assert!(
                bv.read(cx).completion_labels_for_test().is_none(),
                "the first escape dismisses the popup",
            );
            assert!(
                bv.read(cx).is_editing(),
                "and leaves the block in edit mode"
            );
        });

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        cx.update(|window, cx| {
            assert!(!bv.read(cx).is_editing(), "the second escape exits editing");
            assert!(
                bv.read(cx).focus_handle.is_focused(window),
                "and parks focus on the block (#42)",
            );
        });
    }

    #[gpui::test]
    fn moving_the_caret_out_of_the_link_closes_the_popup(cx: &mut TestAppContext) {
        let (_page, bv, cx) = edit_empty_block(cx);

        cx.simulate_input("[[ru");
        cx.run_until_parked();
        cx.read(|cx| assert!(bv.read(cx).completion_labels_for_test().is_some()));

        cx.simulate_keystrokes("home");
        cx.run_until_parked();

        cx.read(|cx| {
            assert!(
                bv.read(cx).completion_labels_for_test().is_none(),
                "no `[[` before the caret means no popup",
            );
        });
    }

    #[gpui::test]
    fn hash_opens_tag_completion_and_enter_inserts_the_tag(cx: &mut TestAppContext) {
        let (_page, bv, cx) = edit_empty_block(cx);

        cx.simulate_input("see #ru");
        cx.run_until_parked();
        cx.read(|cx| {
            assert_eq!(
                bv.read(cx).completion_labels_for_test(),
                Some(vec!["ruff".into(), "rustup".into(), "New tag: ru".into()]),
            );
        });

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert_eq!(
            block_text(cx, &bv),
            "see #ruff",
            "a tag is spliced in bare, with no closing delimiter",
        );
    }

    /// A click on a popup row must not first blur the `TextInput` — the blur
    /// listener would `end_editing` and drop the popup before the click ever
    /// reached its handler.
    #[gpui::test]
    fn clicking_a_row_accepts_it_without_ending_the_edit(cx: &mut TestAppContext) {
        let (_page, bv, cx) = edit_empty_block(cx);

        cx.simulate_input("[[ru");
        cx.run_until_parked();

        let row = cx
            .debug_bounds("completion-row-1")
            .expect("the second row painted");
        cx.simulate_click(row.center(), Modifiers::default());
        cx.run_until_parked();

        cx.read(|cx| assert!(bv.read(cx).is_editing(), "the click kept the block open"));
        assert_eq!(block_text(cx, &bv), "[[rust]]", "the clicked row was used");
    }

    #[gpui::test]
    fn a_caret_parked_inside_an_existing_link_does_not_add_brackets(cx: &mut TestAppContext) {
        let (_page, bv, cx) = mount_with_indexes(cx, "- [[ruff]]\n");
        focus_block(cx, &bv);
        cx.simulate_keystrokes("enter home right right");
        cx.run_until_parked();

        assert_eq!(
            block_text(cx, &bv),
            "[[ruff]]",
            "moving the caret past `[[` is not an edit, so nothing is auto-paired",
        );
        cx.read(|cx| {
            assert_eq!(
                bv.read(cx).completion_labels_for_test(),
                Some(vec!["ruff".into(), "rust".into()]),
                "the popup still opens so the link can be retargeted",
            );
        });
    }

    /// Focuses the block wrapper and settles — the "focused, viewing"
    /// starting state for the keyboard tests below.
    fn focus_block(cx: &mut gpui::VisualTestContext, bv: &Entity<BlockView>) {
        cx.update(|window, cx| {
            let handle = bv.read(cx).focus_handle.clone();
            window.focus(&handle, cx);
        });
        cx.run_until_parked();
    }

    /// The core of #42: focus alone must not mount an editor. The block is
    /// focused-but-viewing (the state the focus ring renders in), and only
    /// an explicit `enter` begins editing.
    #[gpui::test]
    fn focus_alone_does_not_enter_editing(cx: &mut TestAppContext) {
        let (_page, bv, cx) = mount(cx, "- hi\n");

        focus_block(cx, &bv);

        cx.update(|window, cx| {
            assert!(
                !bv.read(cx).is_editing(),
                "focus must highlight, not edit (#42)",
            );
            assert!(
                bv.read(cx).focus_handle.is_focused(window),
                "wrapper keeps focus in the viewing state",
            );
        });

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        cx.update(|window, cx| {
            assert!(bv.read(cx).is_editing(), "enter begins editing");
            let input_focus = bv
                .read(cx)
                .input_focus_handle_for_test(cx)
                .expect("input mounted after enter");
            assert!(
                input_focus.is_focused(window),
                "TextInput takes focus once editing starts",
            );
        });
    }

    #[gpui::test]
    fn end_editing_flushes_to_outline(cx: &mut TestAppContext) {
        let (page, bv, cx) = mount(cx, "- hi\n");

        focus_block(cx, &bv);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        cx.update(|window, cx| {
            let input = bv
                .read(cx)
                .input_entity_for_test()
                .expect("input mounted after focus");
            input.update(cx, |i, cx| i.test_replace_all("HI", cx));
            bv.update(cx, |b, cx| b.test_end_editing(window, cx));
        });

        cx.read(|cx| {
            assert!(!bv.read(cx).is_editing(), "end_editing drops the input");
            let block_id = bv.read(cx).block_id;
            assert_eq!(page.read(cx).outline().get(block_id), Some("HI"));
            assert!(page.read(cx).dirty());
        });
    }

    /// Opens the editor on a block whose body is an unclosed calc fence — the
    /// state in which Enter must extend the block rather than end it (#62).
    fn edit_open_fence(
        cx: &mut TestAppContext,
    ) -> (
        Entity<Page>,
        Entity<BlockView>,
        &mut gpui::VisualTestContext,
    ) {
        let (page, bv, cx) = mount(cx, "- ```calc\n");
        focus_block(cx, &bv);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        (page, bv, cx)
    }

    #[gpui::test]
    fn enter_inside_an_open_fence_adds_a_line_not_a_sibling(cx: &mut TestAppContext) {
        let (page, bv, cx) = edit_open_fence(cx);

        cx.simulate_keystrokes("enter");
        cx.simulate_input("1 + 1");
        cx.run_until_parked();

        cx.read(|cx| {
            assert!(bv.read(cx).is_editing(), "still editing the same block");
            let outline = page.read(cx).outline();
            assert_eq!(outline.roots.len(), 1, "no sibling was inserted");
            assert_eq!(outline.get(bv.read(cx).block_id), Some("```calc\n1 + 1"));
        });
    }

    #[gpui::test]
    fn escape_flushes_the_whole_multi_line_body(cx: &mut TestAppContext) {
        let (page, bv, cx) = edit_open_fence(cx);

        cx.simulate_keystrokes("enter");
        cx.simulate_input("2 * 3");
        cx.simulate_keystrokes("enter");
        cx.simulate_input("```");
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        cx.read(|cx| {
            assert!(!bv.read(cx).is_editing(), "escape leaves editing");
            let outline = page.read(cx).outline();
            assert_eq!(
                outline.get(bv.read(cx).block_id),
                Some("```calc\n2 * 3\n```")
            );
            assert_eq!(outline.serialize(), "- ```calc\n  2 * 3\n  ```\n");
        });
    }

    /// The escape hatch: shift-enter starts a new block even where plain Enter
    /// would have added a line.
    #[gpui::test]
    fn shift_enter_inside_a_fence_still_inserts_a_sibling(cx: &mut TestAppContext) {
        let (page, _bv, cx) = edit_open_fence(cx);

        cx.simulate_keystrokes("shift-enter");
        cx.run_until_parked();

        cx.read(|cx| assert_eq!(page.read(cx).outline().roots.len(), 2));
    }

    /// `begin_editing` is idempotent: clicking a block that is already being
    /// edited must not remount the `TextInput` (that would drop the caret).
    #[gpui::test]
    fn clicking_editing_block_keeps_same_input(cx: &mut TestAppContext) {
        let (_page, bv, cx) = mount(cx, "- hi\n");

        cx.simulate_click(point(px(20.0), px(20.0)), Modifiers::default());
        cx.run_until_parked();

        let first_input_id = cx.read(|cx| {
            bv.read(cx)
                .input_entity_for_test()
                .expect("input mounted")
                .entity_id()
        });

        cx.simulate_click(point(px(20.0), px(20.0)), Modifiers::default());
        cx.run_until_parked();

        cx.read(|cx| {
            let second_input_id = bv
                .read(cx)
                .input_entity_for_test()
                .expect("input still mounted")
                .entity_id();
            assert_eq!(first_input_id, second_input_id, "no new input created");
        });
    }

    /// Regression test: a single click on a view-mode block must both mount
    /// the `TextInput` *and* leave it focused. The framework's `track_focus`
    /// auto-focus listener (see gpui `div.rs` `tracked_focus_handle` mouse
    /// hook) used to fire after our handler and re-focus the wrapper handle,
    /// leaving the input visibly mounted but cursorless until a second click.
    #[gpui::test]
    fn click_focuses_text_input(cx: &mut TestAppContext) {
        let (_page, bv, cx) = mount(cx, "- hi\n");

        cx.read(|cx| assert!(!bv.read(cx).is_editing()));

        cx.simulate_click(point(px(20.0), px(20.0)), Modifiers::default());
        cx.run_until_parked();

        cx.update(|window, cx| {
            assert!(bv.read(cx).is_editing(), "click should enter edit mode");
            let input_focus = bv
                .read(cx)
                .input_focus_handle_for_test(cx)
                .expect("input mounted after click");
            assert!(
                input_focus.is_focused(window),
                "TextInput must hold focus after the click",
            );
        });
    }

    #[gpui::test]
    fn clicking_tag_chip_does_not_enter_editing(cx: &mut TestAppContext) {
        let (_page, bv, cx) = mount(cx, "- #tag tag tag\n");

        cx.simulate_click(point(px(8.0), px(10.0)), Modifiers::default());
        cx.run_until_parked();

        cx.read(|cx| {
            assert!(
                !bv.read(cx).is_editing(),
                "tag mouse-down should not bubble into block editing",
            );
        });
    }

    #[gpui::test]
    fn clicking_page_link_does_not_enter_editing(cx: &mut TestAppContext) {
        let (_page, bv, cx) = mount(cx, "- [[Target]] trailing text\n");

        cx.simulate_click(point(px(8.0), px(10.0)), Modifiers::default());
        cx.run_until_parked();

        cx.read(|cx| {
            assert!(
                !bv.read(cx).is_editing(),
                "page-link mouse-down should not bubble into block editing",
            );
        });
    }

    /// Escape exits editing back to focused-Viewing (#42): the input is
    /// dropped and focus parks on the block's own wrapper handle, so the
    /// block stays visibly selected and window-level bindings keep
    /// dispatching. Enter must then be able to re-enter editing.
    #[gpui::test]
    fn escape_returns_to_focused_viewing(cx: &mut TestAppContext) {
        let (_page, bv, cx) = mount(cx, "- hi\n");

        focus_block(cx, &bv);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(
            cx.read(|cx| bv.read(cx).is_editing()),
            "precondition: editing",
        );

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        cx.update(|window, cx| {
            assert!(!bv.read(cx).is_editing(), "Escape should exit edit mode");
            assert!(
                bv.read(cx).focus_handle.is_focused(window),
                "Escape must park focus back on the block wrapper",
            );
        });

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(
                bv.read(cx).is_editing(),
                "enter re-enters editing from focused-Viewing",
            );
        });
    }

    /// Regression test for #38: typing must flush to the page's outline on
    /// every `Changed` event, not just on blur/Escape — otherwise ctrl-s and
    /// page-switch autosave persist stale text while the input is focused.
    #[gpui::test]
    fn typing_flushes_to_outline_immediately(cx: &mut TestAppContext) {
        let (page, bv, cx) = mount(cx, "- hi\n");

        focus_block(cx, &bv);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        cx.simulate_input("!!");
        cx.run_until_parked();

        cx.read(|cx| {
            assert!(bv.read(cx).is_editing(), "still mid-edit — no blur yet");
            let block_id = bv.read(cx).block_id;
            assert_eq!(
                page.read(cx).outline().get(block_id),
                Some("hi!!"),
                "outline must reflect the in-flight edit",
            );
            assert!(page.read(cx).dirty(), "typing dirties the page");
        });
    }

    /// Regression test for #39: entering edit mode and leaving it without
    /// typing flushes identical text back to the page, which must not mark
    /// the page dirty (and thus must not trigger a file rewrite).
    #[gpui::test]
    fn edit_and_escape_without_typing_keeps_page_clean(cx: &mut TestAppContext) {
        let (page, bv, cx) = mount(cx, "- hi\n");

        focus_block(cx, &bv);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        assert!(
            cx.read(|cx| bv.read(cx).is_editing()),
            "precondition: editing",
        );

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        cx.read(|cx| {
            assert!(!bv.read(cx).is_editing(), "escape exits edit mode");
            assert!(
                !page.read(cx).dirty(),
                "untouched edit/escape cycle must not dirty the page",
            );
        });
    }
}
