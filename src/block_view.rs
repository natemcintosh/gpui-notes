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
    actions, div, prelude::*, px, AnyElement, App, AppContext, Context, Entity, EventEmitter,
    FocusHandle, Focusable, IntoElement, KeyBinding, MouseButton, ParentElement, Render,
    SharedString, Styled, Subscription, Window,
};

use crate::block_render::{render_block, theme};
use crate::outline::BlockId;
use crate::page::Page;
use crate::page_links::{LinkIndex, PageLinkExt};
use crate::tags::TagExt;
use crate::text_input::{TextInput, TextInputEvent};

actions!(block_view, [BeginEditing]);

/// Registers the block-level key bindings. The `BlockView` key context only
/// receives keystrokes while a block wrapper (not its `TextInput`) holds
/// focus, so `enter` here cannot shadow the input's own enter-to-submit.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([KeyBinding::new(
        "enter",
        BeginEditing,
        Some("BlockView && !editing"),
    )]);
}

/// Events emitted by `BlockView` to its parent (typically `OutlineView`).
#[derive(Debug, Clone)]
pub enum BlockViewEvent {
    /// The user finished a block with Enter and a new sibling was inserted
    /// after `self.block_id`. The parent should mount the view for the newly
    /// created block and put it into editing.
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
        let on_event = cx.subscribe_in(&input, window, |this, _input, event, window, cx| {
            match event {
                // Escape returns to focused-Viewing on this block (#42), so
                // the block stays visibly selected and window-level bindings
                // (ctrl-s etc.) keep dispatching through the focus chain.
                TextInputEvent::Cancelled => {
                    this.end_editing_inner(cx);
                    window.focus(&this.focus_handle, cx);
                }
                TextInputEvent::Submitted => {
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
                }
            }
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
        // TextInput, so without the flag, keys the input doesn't bind
        // (up/down/tab…) would trigger outline ops mid-edit.
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
            .flex_1()
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
