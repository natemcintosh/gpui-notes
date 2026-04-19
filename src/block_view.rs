//! Per-block view that swaps between rendered markdown and a raw-text editor
//! based on focus. See issue #6.
//!
//! Only one block can be in `Editing` at a time — GPUI's focus system enforces
//! this naturally (a single focused leaf). The outline stored on `Page` is the
//! source of truth; the `TextInput`'s buffer is flushed to the outline on blur
//! and then dropped, so there is no hidden state to drift out of sync.

use gpui::{
    div, prelude::*, px, AnyElement, App, AppContext, Context, Entity, FocusHandle, Focusable,
    IntoElement, MouseButton, ParentElement, Render, SharedString, Styled, Subscription, Window,
};

use crate::block_render::{render_block, theme};
use crate::outline::BlockId;
use crate::page::Page;
use crate::text_input::TextInput;

pub struct BlockView {
    block_id: BlockId,
    page: Entity<Page>,
    focus_handle: FocusHandle,
    input: Option<Entity<TextInput>>,
    _on_focus: Subscription,
    /// Reset every edit cycle — subscribes to the *current* `TextInput`'s blur.
    on_input_blur: Option<Subscription>,
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
            input: None,
            _on_focus: on_focus,
            on_input_blur: None,
            _page_sub: page_sub,
        }
    }

    #[must_use]
    pub fn block_id(&self) -> BlockId {
        self.block_id
    }

    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.input.is_some()
    }

    fn begin_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.input.is_some() {
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
        window.focus(&input_focus, cx);
        self.input = Some(input);
        self.on_input_blur = Some(on_blur);
        cx.notify();
    }

    fn end_editing(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(input) = self.input.take() else {
            return;
        };
        let text = input.read(cx).content().to_string();
        let block_id = self.block_id;
        self.page
            .update(cx, |p, cx| p.set_block_text(block_id, text, cx));
        self.on_input_blur = None;
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
        let content: AnyElement = if let Some(input) = self.input.clone() {
            input.into_any_element()
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
                render_block(&text, &[], window, cx)
            }
        };

        let handle = self.focus_handle.clone();
        div()
            .track_focus(&self.focus_handle)
            .flex_1()
            .min_h(px(20.0))
            .py_0p5()
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                window.focus(&handle, cx);
            })
            .child(content)
    }
}

#[cfg(test)]
impl BlockView {
    pub(crate) fn test_end_editing(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.end_editing(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::Page;
    use gpui::TestAppContext;

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
                .input
                .as_ref()
                .expect("input mounted after focus")
                .clone();
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
                .input
                .as_ref()
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
                .input
                .as_ref()
                .expect("input still mounted")
                .entity_id();
            assert_eq!(first_input_id, second_input_id, "no new input created");
        });
    }
}
