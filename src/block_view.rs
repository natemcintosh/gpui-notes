//! Per-block view that swaps between rendered markdown and a raw-text editor
//! based on focus. See issue #6.
//!
//! Only one block can be in `Editing` at a time — GPUI's focus system enforces
//! this naturally (a single focused leaf). The outline stored on `Page` is the
//! source of truth; the `TextInput`'s buffer is flushed to the outline on blur
//! and then dropped, so there is no hidden state to drift out of sync.

use gpui::{
    div, prelude::*, px, AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle,
    Focusable, IntoElement, MouseButton, ParentElement, Render, SharedString, Styled, Subscription,
    Window,
};

use crate::block_render::{render_block, theme};
use crate::outline::BlockId;
use crate::page::Page;
use crate::tags::TagExt;
use crate::text_input::{TextInput, TextInputEvent};

/// Events emitted by `BlockView` to its parent (typically `OutlineView`).
#[derive(Debug, Clone)]
pub enum BlockViewEvent {
    /// The user finished a block with Enter and a new sibling was inserted
    /// after `self.block_id`. The parent should mount/focus the view for the
    /// newly created block.
    FocusRequested(BlockId),
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
        /// Drops on edit exit. Fires `end_editing` when focus leaves the input.
        _on_blur: Subscription,
        /// Drops on edit exit. Fires on `TextInputEvent::Cancelled` (Escape)
        /// because the blur listener can lag a frame.
        _on_event: Subscription,
    },
}

pub struct BlockView {
    block_id: BlockId,
    page: Entity<Page>,
    focus_handle: FocusHandle,
    mode: BlockMode,
    _on_focus: Subscription,
    _page_sub: Subscription,
}

impl BlockView {
    pub fn new(
        block_id: BlockId,
        page: Entity<Page>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let on_focus = cx.on_focus(&focus_handle, window, |this, window, cx| {
            this.begin_editing(window, cx);
        });
        // Re-render when the page outline changes (e.g., another block was
        // edited) so our rendered markdown stays current.
        let page_sub = cx.observe(&page, |_, _, cx| cx.notify());

        Self {
            block_id,
            page,
            focus_handle,
            mode: BlockMode::Viewing,
            _on_focus: on_focus,
            _page_sub: page_sub,
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

    fn begin_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
        let input = cx.new(|cx| TextInput::with_content(cx, "", text));
        let input_focus = input.focus_handle(cx);
        let on_blur = cx.on_blur(&input_focus, window, |this, window, cx| {
            this.end_editing(window, cx);
        });
        let on_event = cx.subscribe(&input, |this, _input, event, cx| match event {
            TextInputEvent::Cancelled => {
                this.end_editing_inner(cx);
            }
            TextInputEvent::Submitted => {
                this.insert_sibling_below(cx);
            }
            TextInputEvent::Changed(_) => {}
        });
        window.focus(&input_focus, cx);
        self.mode = BlockMode::Editing {
            input,
            _on_blur: on_blur,
            _on_event: on_event,
        };
        cx.notify();
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

impl Focusable for BlockView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
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
                let tag_ext = TagExt;
                let extensions: [&dyn crate::block_render::InlineExtension; 1] = [&tag_ext];
                render_block(&text, &extensions, window, cx)
            }
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
        div()
            .track_focus(&self.focus_handle)
            .flex_1()
            .min_h(px(20.0))
            .py_0p5()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.begin_editing(window, cx);
                    window.prevent_default();
                }),
            )
            .child(content)
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

    #[gpui::test]
    fn focus_enters_editing(cx: &mut TestAppContext) {
        let (_page, bv, cx) = mount(cx, "- hi\n");

        cx.read(|cx| assert!(!bv.read(cx).is_editing()));

        cx.update(|window, cx| {
            let handle = bv.read(cx).focus_handle.clone();
            window.focus(&handle, cx);
        });
        cx.run_until_parked();
        cx.read(|cx| assert!(bv.read(cx).is_editing(), "focus should begin editing"));
    }

    #[gpui::test]
    fn end_editing_flushes_to_outline(cx: &mut TestAppContext) {
        let (page, bv, cx) = mount(cx, "- hi\n");

        cx.update(|window, cx| {
            let handle = bv.read(cx).focus_handle.clone();
            window.focus(&handle, cx);
        });
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

    #[gpui::test]
    fn refocusing_same_block_is_noop(cx: &mut TestAppContext) {
        let (_page, bv, cx) = mount(cx, "- hi\n");

        cx.update(|window, cx| {
            let handle = bv.read(cx).focus_handle.clone();
            window.focus(&handle, cx);
        });
        cx.run_until_parked();

        let first_input_id = cx.read(|cx| {
            bv.read(cx)
                .input_entity_for_test()
                .expect("input mounted")
                .entity_id()
        });

        cx.update(|window, cx| {
            let handle = bv.read(cx).focus_handle.clone();
            window.focus(&handle, cx);
        });
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

    /// Editing → Viewing via blur. Focuses an unrelated handle so the
    /// input's `on_blur` listener fires; complements `escape_exits_editing`
    /// (which exercises the explicit Cancel action path) and the test
    /// helper `test_end_editing`.
    /// Regression test: pressing Escape while a block is being edited exits
    /// edit mode. Previously the `on_blur` subscription was the only path
    /// out, and its dispatch lagged a draw cycle, so Escape on its own did
    /// nothing.
    #[gpui::test]
    fn escape_exits_editing(cx: &mut TestAppContext) {
        let (_page, bv, cx) = mount(cx, "- hi\n");

        cx.update(|window, cx| {
            let handle = bv.read(cx).focus_handle.clone();
            window.focus(&handle, cx);
        });
        cx.run_until_parked();
        assert!(
            cx.read(|cx| bv.read(cx).is_editing()),
            "precondition: editing",
        );

        cx.simulate_keystrokes("escape");
        cx.run_until_parked();

        cx.read(|cx| assert!(!bv.read(cx).is_editing(), "Escape should exit edit mode"));
    }
}
