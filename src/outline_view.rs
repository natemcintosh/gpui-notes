//! Outline-level container that composes `BlockView` children with indent +
//! bullet glyphs. See issue #6.
//!
//! `BlockView` entities are cached by `BlockId` so focus state and any
//! mounted `TextInput` survive re-renders. Ids missing from the page's
//! outline (deleted blocks) are pruned at render time.

use std::collections::{HashMap, HashSet};

use gpui::{
    div, px, AppContext, Context, Entity, Focusable, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Window,
};

use crate::block_render::theme;
use crate::block_view::{BlockView, BlockViewEvent};
use crate::outline::{Block, BlockId};
use crate::page::Page;

pub struct OutlineView {
    page: Entity<Page>,
    blocks: HashMap<BlockId, Entity<BlockView>>,
    /// One subscription per cached `BlockView` listening for
    /// `BlockViewEvent::FocusRequested` so we can mount and focus the target.
    /// Pruned alongside `blocks` when ids disappear from the outline.
    block_subs: HashMap<BlockId, Subscription>,
    _page_sub: Subscription,
}

impl OutlineView {
    pub fn new(page: Entity<Page>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&page, |_, _, cx| cx.notify());
        Self {
            page,
            blocks: HashMap::new(),
            block_subs: HashMap::new(),
            _page_sub: sub,
        }
    }

    /// Focus the first root block in the outline, creating its `BlockView` if
    /// needed. No-op for an empty outline.
    pub fn focus_first_block(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(first_id) = self.page.read(cx).outline().first_block_id() else {
            return;
        };
        let bv = self.get_or_create(first_id, window, cx);
        let handle = bv.focus_handle(cx);
        window.focus(&handle, cx);
    }

    fn get_or_create(
        &mut self,
        id: BlockId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<BlockView> {
        if let Some(existing) = self.blocks.get(&id) {
            return existing.clone();
        }
        let page = self.page.clone();
        let bv = cx.new(|block_cx| BlockView::new(id, page, window, block_cx));
        let sub = cx.subscribe_in(&bv, window, |this, _bv, event, window, cx| match event {
            BlockViewEvent::FocusRequested(target) => {
                let target = *target;
                let target_bv = this.get_or_create(target, window, cx);
                let handle = target_bv.focus_handle(cx);
                window.focus(&handle, cx);
            }
        });
        self.blocks.insert(id, bv.clone());
        self.block_subs.insert(id, sub);
        bv
    }

    #[cfg(test)]
    #[must_use]
    pub fn block_view(&self, id: BlockId) -> Option<&Entity<BlockView>> {
        self.blocks.get(&id)
    }
}

fn flatten_visible(roots: &[Block]) -> Vec<(usize, BlockId)> {
    fn walk(blocks: &[Block], depth: usize, out: &mut Vec<(usize, BlockId)>) {
        for b in blocks {
            out.push((depth, b.id));
            if !b.collapsed {
                walk(&b.children, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(roots, 0, &mut out);
    out
}

fn collect_all_ids(blocks: &[Block], out: &mut HashSet<BlockId>) {
    for b in blocks {
        out.insert(b.id);
        collect_all_ids(&b.children, out);
    }
}

impl Render for OutlineView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (flat, all_ids) = {
            let outline = self.page.read(cx).outline();
            let mut all = HashSet::new();
            collect_all_ids(&outline.roots, &mut all);
            (flatten_visible(&outline.roots), all)
        };
        self.blocks.retain(|id, _| all_ids.contains(id));
        self.block_subs.retain(|id, _| all_ids.contains(id));

        let mut root = div().flex().flex_col().gap_1();
        for (depth, id) in flat {
            let bv = self.get_or_create(id, window, cx);
            #[allow(clippy::cast_precision_loss)]
            let indent = px(16.0) * depth as f32;
            let row = div()
                .flex()
                .flex_row()
                .items_start()
                .pl(indent)
                .child(
                    div()
                        .w(px(14.0))
                        .flex_none()
                        .text_color(theme::fg_muted())
                        .child(SharedString::from("•")),
                )
                .child(bv);
            root = root.child(row);
        }
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outline::Block;
    use crate::text_input;
    use gpui::{TestAppContext, VisualTestContext};

    struct TestPage(Entity<Page>);
    impl gpui::Global for TestPage {}

    /// Mount an `OutlineView` for `body` as the window root and activate the
    /// window. Same shape as `block_view::tests::mount` — focus and event
    /// dispatch only fire on active windows during `Window::draw`.
    fn mount<'a>(
        cx: &'a mut TestAppContext,
        body: &str,
    ) -> (Entity<Page>, Entity<OutlineView>, &'a mut VisualTestContext) {
        let body = body.to_string();
        let (ov, vcx) = cx.add_window_view(move |_window, cx| {
            text_input::bind_keys(cx);
            let page = cx.new(|cx| Page::new("foo".into(), &body, cx));
            cx.set_global(TestPage(page));
            OutlineView::new(cx.global::<TestPage>().0.clone(), cx)
        });
        vcx.update(|window, _| window.activate_window());
        vcx.run_until_parked();
        let page = vcx.read(|cx| cx.global::<TestPage>().0.clone());
        (page, ov, vcx)
    }

    #[gpui::test]
    fn enter_creates_sibling_at_same_depth(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n");

        let first_id = cx.read(|cx| page.read(cx).outline().roots[0].id);

        cx.update(|window, cx| {
            let bv = ov
                .read(cx)
                .block_view(first_id)
                .cloned()
                .unwrap_or_else(|| ov.update(cx, |o, cx| o.get_or_create(first_id, window, cx)));
            window.focus(&bv.focus_handle(cx), cx);
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        // Outline now has 3 roots in order: a, <new>, b.
        let (roots, new_id) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            let roots: Vec<(BlockId, String)> = outline
                .roots
                .iter()
                .map(|b| (b.id, b.text.clone()))
                .collect();
            let new_id = outline.roots[1].id;
            (roots, new_id)
        });
        assert_eq!(roots.len(), 3, "expected new sibling inserted: {roots:?}");
        assert_eq!(roots[0].1, "a");
        assert_eq!(roots[1].1, "");
        assert_eq!(roots[2].1, "b");

        // The new block's BlockView should be mounted, editing, and focused.
        cx.update(|window, cx| {
            let new_bv = ov
                .read(cx)
                .block_view(new_id)
                .expect("new block view mounted")
                .clone();
            assert!(new_bv.read(cx).is_editing(), "new block should be editing");
            // Focus advanced to the mounted TextInput, not the wrapper handle.
            let input_focus = new_bv
                .read(cx)
                .input_focus_handle_for_test(cx)
                .expect("input mounted");
            assert!(
                input_focus.is_focused(window),
                "new block's TextInput should hold focus",
            );
        });

        assert!(cx.read(|cx| page.read(cx).dirty()));
    }

    #[gpui::test]
    fn new_block_edits_flush_back_to_outline(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n");

        let first_id = cx.read(|cx| page.read(cx).outline().roots[0].id);
        cx.update(|window, cx| {
            let bv = ov.update(cx, |o, cx| o.get_or_create(first_id, window, cx));
            window.focus(&bv.focus_handle(cx), cx);
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        // Write into the new block's TextInput directly, then drive the blur
        // path that mounts back to the outline. simulate_input would route
        // through the dispatch tree, which depends on draw timing this test
        // doesn't want to assert.
        let new_id = cx.read(|cx| page.read(cx).outline().roots[1].id);
        cx.update(|window, cx| {
            let new_bv = ov
                .read(cx)
                .block_view(new_id)
                .expect("new block mounted")
                .clone();
            let input = new_bv
                .read(cx)
                .input_entity_for_test()
                .expect("input mounted");
            input.update(cx, |i, cx| i.test_replace_all("hello", cx));
            new_bv.update(cx, |b, cx| b.test_end_editing(window, cx));
        });

        cx.read(|cx| {
            assert_eq!(page.read(cx).outline().get(new_id), Some("hello"));
        });
    }

    #[gpui::test]
    fn enter_on_nested_block_creates_sibling_at_same_depth(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n  - child\n");

        let child_id = cx.read(|cx| page.read(cx).outline().roots[0].children[0].id);
        cx.update(|window, cx| {
            let bv = ov.update(cx, |o, cx| o.get_or_create(child_id, window, cx));
            window.focus(&bv.focus_handle(cx), cx);
        });
        cx.run_until_parked();

        cx.simulate_keystrokes("enter");
        cx.run_until_parked();

        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert_eq!(outline.roots.len(), 1, "still one root");
            let children = &outline.roots[0].children;
            assert_eq!(children.len(), 2, "child got a sibling");
            assert_eq!(children[0].text, "child");
            assert_eq!(children[1].text, "");
        });
    }

    /// Editing → Viewing via blur. Realized as "focus another block": when
    /// block 2 takes focus, block 1's input loses focus, its `on_blur`
    /// listener fires, and block 1 flushes back to view mode.
    ///
    /// Done through `OutlineView` rather than `BlockView` directly because
    /// `BlockView` test setups have no second focusable element to receive
    /// focus, and `Window::blur` alone does not drive `on_blur` reliably
    /// in the headless dispatch.
    #[gpui::test]
    fn transition_editing_to_viewing_via_blur_across_blocks(cx: &mut TestAppContext) {
        use crate::block_view::BlockMode;
        let (page, ov, cx) = mount(cx, "- one\n- two\n");

        let (first_id, second_id) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (outline.roots[0].id, outline.roots[1].id)
        });

        cx.update(|window, cx| {
            let bv = ov.update(cx, |o, cx| o.get_or_create(first_id, window, cx));
            window.focus(&bv.focus_handle(cx), cx);
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            // Force a draw so the input's focus_id is captured in the
            // rendered_frame focus path. Without this, the next focus
            // change can't be detected as a blur — the focus listener
            // compares against the rendered_frame's path.
            window.draw(cx).clear();
        });
        cx.run_until_parked();
        cx.read(|cx| {
            let bv = ov.read(cx).block_view(first_id).expect("first mounted");
            assert!(bv.read(cx).is_editing(), "precondition: first is editing");
        });

        // Move focus to the second block; this naturally blurs the first
        // block's TextInput.
        cx.update(|window, cx| {
            let other = ov.update(cx, |o, cx| o.get_or_create(second_id, window, cx));
            window.focus(&other.focus_handle(cx), cx);
            window.draw(cx).clear();
        });
        cx.run_until_parked();

        cx.read(|cx| {
            let bv = ov
                .read(cx)
                .block_view(first_id)
                .expect("first still cached");
            assert!(
                matches!(bv.read(cx).mode(), BlockMode::Viewing),
                "blurring the first block should exit its edit mode",
            );
        });
    }

    #[test]
    fn flatten_respects_collapsed() {
        let mut a = Block::new("a");
        let mut b = Block::new("b");
        let c = Block::new("c");
        b.children.push(c);
        a.children.push(b);

        let ids: Vec<BlockId> = flatten_visible(std::slice::from_ref(&a))
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        assert_eq!(ids.len(), 3);

        // Collapse the middle block; its child should disappear.
        a.children[0].collapsed = true;
        let ids: Vec<BlockId> = flatten_visible(std::slice::from_ref(&a))
            .into_iter()
            .map(|(_, id)| id)
            .collect();
        assert_eq!(ids.len(), 2);
    }
}
