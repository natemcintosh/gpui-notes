//! Outline-level container that composes `BlockView` children with indent +
//! bullet glyphs. See issue #6.
//!
//! `BlockView` entities are cached by `BlockId` so focus state and any
//! mounted `TextInput` survive re-renders. Ids missing from the page's
//! outline (deleted blocks) are pruned at render time.

use std::collections::{HashMap, HashSet};

use gpui::{
    actions, div, px, App, AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyBinding, ParentElement, Render, SharedString, Styled, Subscription, Window,
};

use crate::block_render::theme;
use crate::block_view::{BlockView, BlockViewEvent};
use crate::outline::{Block, BlockId};
use crate::page::Page;
use crate::text_input::{PendingVerticalPlacement, VerticalDirection};

actions!(
    outline_view,
    [
        FocusUp,
        FocusDown,
        IndentBlock,
        OutdentBlock,
        MoveBlockUp,
        MoveBlockDown,
        ToggleCollapse,
        DeleteBackward
    ]
);

/// The outline navigation/operation keys act on the focused-but-viewing
/// block (#42/#44). The `!editing` guard keeps them from firing while that
/// block's `TextInput` is focused — the wrapper (and its key context) is an
/// ancestor of the input, so without the guard, keys the input doesn't bind
/// (up/down/tab…) would leak into outline ops mid-edit.
const BLOCK_CONTEXT: &str = "BlockView && !editing";

pub fn bind_keys(cx: &mut App) {
    let cmd = crate::cmd_key();
    cx.bind_keys([
        KeyBinding::new("up", FocusUp, Some(BLOCK_CONTEXT)),
        KeyBinding::new("down", FocusDown, Some(BLOCK_CONTEXT)),
        KeyBinding::new("tab", IndentBlock, Some(BLOCK_CONTEXT)),
        KeyBinding::new("shift-tab", OutdentBlock, Some(BLOCK_CONTEXT)),
        KeyBinding::new("alt-up", MoveBlockUp, Some(BLOCK_CONTEXT)),
        KeyBinding::new("alt-down", MoveBlockDown, Some(BLOCK_CONTEXT)),
        KeyBinding::new(&format!("{cmd}-enter"), ToggleCollapse, Some(BLOCK_CONTEXT)),
        KeyBinding::new("backspace", DeleteBackward, Some(BLOCK_CONTEXT)),
    ]);
}

pub struct OutlineView {
    page: Entity<Page>,
    /// Fallback focus target (e.g. an empty outline); a block exiting edit
    /// mode parks focus on the block itself (#42), not here.
    focus_handle: FocusHandle,
    blocks: HashMap<BlockId, Entity<BlockView>>,
    /// One subscription per cached `BlockView`, used for sibling-insertion
    /// focus and vertical edit transfers. Pruned alongside `blocks` when ids
    /// disappear from the outline.
    block_subs: HashMap<BlockId, Subscription>,
    _page_sub: Subscription,
}

impl OutlineView {
    pub fn new(page: Entity<Page>, cx: &mut Context<Self>) -> Self {
        let sub = cx.observe(&page, |_, _, cx| cx.notify());
        Self {
            page,
            focus_handle: cx.focus_handle(),
            blocks: HashMap::new(),
            block_subs: HashMap::new(),
            _page_sub: sub,
        }
    }

    /// Focus the first root block in the outline, creating its `BlockView` if
    /// needed. An empty outline falls back to this view's own focus handle so
    /// the enclosing `RootView` remains in the key dispatch path.
    pub fn focus_first_block(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(first_id) = self.page.read(cx).outline().first_block_id() else {
            window.focus(&self.focus_handle, cx);
            return;
        };
        let bv = self.get_or_create(first_id, window, cx);
        let handle = bv.focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// Describes the active page-body focus target for the optional root debug
    /// HUD. `contains_focused` follows the real GPUI focus path, so an active
    /// text input is correctly reported as belonging to its block.
    #[must_use]
    pub fn debug_focus_status(&self, window: &Window, cx: &App) -> (&'static str, &'static str) {
        for block in self.blocks.values() {
            let block = block.read(cx);
            if block.focus_handle(cx).contains_focused(window, cx) {
                return if block.is_editing() {
                    ("block input", "editing")
                } else {
                    ("block", "viewing")
                };
            }
        }
        if self.focus_handle.is_focused(window) {
            ("outline", "no block")
        } else {
            ("none", "no block")
        }
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
        let sub = cx.subscribe_in(
            &bv,
            window,
            move |this, _bv, event, window, cx| match event {
                BlockViewEvent::FocusRequested(target) => {
                    let target = *target;
                    let target_bv = this.get_or_create(target, window, cx);
                    // Focus no longer implies editing (#42), so the new sibling
                    // must be put into editing explicitly — the user pressed
                    // Enter to keep typing. `begin_editing` focuses the input.
                    target_bv.update(cx, |bv, cx| bv.begin_editing(window, cx));
                }
                BlockViewEvent::AdjacentEditRequested(request) => {
                    this.move_edit_to_adjacent(id, *request, window, cx);
                }
            },
        );
        self.blocks.insert(id, bv.clone());
        self.block_subs.insert(id, sub);
        bv
    }

    fn move_edit_to_adjacent(
        &mut self,
        source: BlockId,
        request: crate::block_view::AdjacentEditRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let flat = flatten_visible(&self.page.read(cx).outline().roots);
        let Some(source_index) = flat.iter().position(|&(_, id)| id == source) else {
            return;
        };
        let target_index = match request.direction {
            VerticalDirection::Up => source_index.checked_sub(1),
            VerticalDirection::Down => (source_index + 1 < flat.len()).then_some(source_index + 1),
        };
        let Some(target_index) = target_index else {
            // Clamp at the first and last outline entries without blurring
            // the source editor or wrapping around.
            return;
        };
        let target = flat[target_index].1;
        let placement =
            PendingVerticalPlacement::for_navigation(request.direction, request.preferred_screen_x);
        let target_bv = self.get_or_create(target, window, cx);
        target_bv.update(cx, |block, cx| {
            block.begin_editing_at(placement, window, cx);
        });
    }

    /// The id of the block whose wrapper currently holds focus (i.e. the
    /// focused-but-viewing block the outline keys operate on).
    fn focused_block_id(&self, window: &Window, cx: &App) -> Option<BlockId> {
        self.blocks.iter().find_map(|(id, bv)| {
            bv.read(cx)
                .focus_handle(cx)
                .is_focused(window)
                .then_some(*id)
        })
    }

    /// Move focus `delta` steps through the visible (collapse-respecting)
    /// flattened order. Clamps at both ends.
    fn move_focus(&mut self, delta: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(current) = self.focused_block_id(window, cx) else {
            return;
        };
        let flat = flatten_visible(&self.page.read(cx).outline().roots);
        let Some(idx) = flat.iter().position(|&(_, id)| id == current) else {
            return;
        };
        let Some(new_idx) = idx.checked_add_signed(delta).filter(|i| *i < flat.len()) else {
            return;
        };
        let (_, target) = flat[new_idx];
        let bv = self.get_or_create(target, window, cx);
        let handle = bv.focus_handle(cx);
        window.focus(&handle, cx);
    }

    /// Run one of the `Page` outline ops on the focused block. Focus stays
    /// on the same block — its `BlockView` survives the re-render because
    /// the ops never change block ids.
    fn update_focused_block(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
        op: impl FnOnce(&mut Page, BlockId, &mut Context<Page>) -> bool,
    ) {
        let Some(current) = self.focused_block_id(window, cx) else {
            return;
        };
        self.page.update(cx, |page, cx| {
            op(page, current, cx);
        });
    }

    /// Backspace on an empty, childless focused block deletes it and moves
    /// focus to the previous visible block (or the next one when the first
    /// block is deleted). Non-empty blocks, blocks with children, and the
    /// only remaining block are left alone.
    fn delete_backward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(current) = self.focused_block_id(window, cx) else {
            return;
        };
        let (flat, deletable) = {
            let outline = self.page.read(cx).outline();
            let deletable = outline.get(current) == Some("")
                && outline
                    .path_to(current)
                    .is_some_and(|path| children_at(&outline.roots, &path).is_empty());
            (flatten_visible(&outline.roots), deletable)
        };
        if !deletable || flat.len() <= 1 {
            return;
        }
        let Some(idx) = flat.iter().position(|&(_, id)| id == current) else {
            return;
        };
        let neighbor = if idx > 0 { flat[idx - 1].1 } else { flat[1].1 };
        self.page.update(cx, |page, cx| {
            page.delete_block(current, cx);
        });
        let bv = self.get_or_create(neighbor, window, cx);
        let handle = bv.focus_handle(cx);
        window.focus(&handle, cx);
    }

    #[cfg(test)]
    #[must_use]
    pub fn block_view(&self, id: BlockId) -> Option<&Entity<BlockView>> {
        self.blocks.get(&id)
    }
}

/// The children of the block at `path` (empty slice semantics: `path` must
/// be valid, which `Outline::path_to` guarantees for a live id).
fn children_at<'a>(roots: &'a [Block], path: &[usize]) -> &'a [Block] {
    let mut blocks = roots;
    let mut node: Option<&Block> = None;
    for &i in path {
        node = Some(&blocks[i]);
        blocks = &blocks[i].children;
    }
    node.map_or(&[], |b| &b.children)
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

        // The bindings target the `BlockView` key context (see BLOCK_CONTEXT),
        // but the handlers live here because the ops need the outline-wide
        // view map and traversal order. Actions dispatched from a focused
        // block bubble up through this ancestor node.
        let mut root = div()
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &FocusUp, window, cx| {
                this.move_focus(-1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &FocusDown, window, cx| {
                this.move_focus(1, window, cx);
            }))
            .on_action(cx.listener(|this, _: &IndentBlock, window, cx| {
                this.update_focused_block(window, cx, Page::indent_block);
            }))
            .on_action(cx.listener(|this, _: &OutdentBlock, window, cx| {
                this.update_focused_block(window, cx, Page::outdent_block);
            }))
            .on_action(cx.listener(|this, _: &MoveBlockUp, window, cx| {
                this.update_focused_block(window, cx, Page::move_block_up);
            }))
            .on_action(cx.listener(|this, _: &MoveBlockDown, window, cx| {
                this.update_focused_block(window, cx, Page::move_block_down);
            }))
            .on_action(cx.listener(|this, _: &ToggleCollapse, window, cx| {
                this.update_focused_block(window, cx, Page::toggle_collapse);
            }))
            .on_action(cx.listener(|this, _: &DeleteBackward, window, cx| {
                this.delete_backward(window, cx);
            }))
            .flex()
            .flex_col()
            .w_full()
            .gap_1();
        for (depth, id) in flat {
            let bv = self.get_or_create(id, window, cx);
            #[allow(clippy::cast_precision_loss)]
            let indent = px(16.0) * depth as f32;
            let row = div()
                .flex()
                .flex_row()
                .w_full()
                .min_w_0()
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
    use gpui::{point, size, Modifiers, TestAppContext, VisualTestContext};

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
            crate::block_view::bind_keys(cx);
            super::bind_keys(cx);
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

        // First enter begins editing the focused block (#42); the second,
        // now dispatched to the TextInput, submits and creates the sibling.
        cx.simulate_keystrokes("enter enter");
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

        cx.simulate_keystrokes("enter enter");
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

        cx.simulate_keystrokes("enter enter");
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
        // Focus no longer implies editing (#42) — enter starts the edit.
        cx.simulate_keystrokes("enter");
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

    /// The page-switch mechanism for #42: `RootView::focus_current` lands on
    /// `focus_first_block`, which must leave the block focused-but-viewing —
    /// no `TextInput` mounts and the page cannot be touched by the switch.
    #[gpui::test]
    fn focus_first_block_does_not_mount_editor(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n");

        cx.update(|window, cx| {
            ov.update(cx, |o, cx| o.focus_first_block(window, cx));
        });
        cx.run_until_parked();

        cx.update(|window, cx| {
            let first_id = page.read(cx).outline().roots[0].id;
            let bv = ov
                .read(cx)
                .block_view(first_id)
                .expect("first block view cached")
                .clone();
            assert!(
                !bv.read(cx).is_editing(),
                "focus_first_block must not mount an editor (#42)",
            );
            assert!(
                bv.read(cx).focus_handle(cx).is_focused(window),
                "first block holds focus in viewing mode",
            );
        });
    }

    /// Focuses the block with `id` (mounting its view if needed) and settles.
    fn focus_block(cx: &mut VisualTestContext, ov: &Entity<OutlineView>, id: BlockId) {
        cx.update(|window, cx| {
            let bv = ov.update(cx, |o, cx| o.get_or_create(id, window, cx));
            window.focus(&bv.focus_handle(cx), cx);
        });
        cx.run_until_parked();
    }

    fn focused_id(cx: &mut VisualTestContext, ov: &Entity<OutlineView>) -> Option<BlockId> {
        cx.update(|window, cx| ov.read(cx).focused_block_id(window, cx))
    }

    fn editing_input(
        cx: &mut VisualTestContext,
        ov: &Entity<OutlineView>,
        id: BlockId,
    ) -> Entity<crate::text_input::TextInput> {
        cx.read(|cx| {
            ov.read(cx)
                .block_view(id)
                .expect("block view mounted")
                .read(cx)
                .input_entity_for_test()
                .expect("block should be editing")
        })
    }

    // ── #44: keyboard navigation and block operations ──────────────────

    #[gpui::test]
    fn up_down_move_focus_through_visible_blocks(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n");
        let (a, b) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (outline.roots[0].id, outline.roots[1].id)
        });

        focus_block(cx, &ov, a);

        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        assert_eq!(focused_id(cx, &ov), Some(b), "down moves to next block");

        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        assert_eq!(
            focused_id(cx, &ov),
            Some(b),
            "down clamps at the last block"
        );

        cx.simulate_keystrokes("up");
        cx.run_until_parked();
        assert_eq!(focused_id(cx, &ov), Some(a), "up moves back to the first");

        cx.simulate_keystrokes("up");
        cx.run_until_parked();
        assert_eq!(focused_id(cx, &ov), Some(a), "up clamps at the first block");
    }

    #[gpui::test]
    fn vertical_arrows_move_visual_rows_and_collapse_active_selection(cx: &mut TestAppContext) {
        let body = format!("- {}\n", "alpha beta gamma delta ".repeat(12));
        let (page, ov, cx) = mount(cx, &body);
        cx.simulate_resize(size(px(240.), px(600.)));
        cx.run_until_parked();
        let block = cx.read(|cx| page.read(cx).outline().roots[0].id);

        focus_block(cx, &ov, block);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let input = editing_input(cx, &ov, block);
        let (row_count, input_bounds) = cx.read(|cx| {
            let input = input.read(cx);
            (
                input.visual_row_count_for_test(),
                input.layout_bounds_for_test(),
            )
        });
        assert!(
            row_count.is_some_and(|rows| rows > 2),
            "precondition: editor wraps to several visual rows; \
             rows={row_count:?}, bounds={input_bounds:?}",
        );

        cx.simulate_keystrokes("home shift-right shift-right down");
        cx.run_until_parked();
        cx.read(|cx| {
            let input = input.read(cx);
            assert!(
                input.selected_range().is_empty(),
                "plain Down clears an active selection",
            );
            assert_eq!(
                input.visual_row_for_test(),
                Some(1),
                "movement continues from the active selection head",
            );
        });

        let before_shifted = cx.read(|cx| {
            let input = input.read(cx);
            (input.cursor_offset_for_test(), input.visual_row_for_test())
        });
        cx.simulate_keystrokes("shift-down shift-up");
        cx.run_until_parked();
        cx.read(|cx| {
            let input = input.read(cx);
            assert_eq!(
                (input.cursor_offset_for_test(), input.visual_row_for_test(),),
                before_shifted,
                "Shift-Up/Down remain unbound in block editors",
            );
        });
    }

    #[gpui::test]
    #[allow(clippy::too_many_lines)] // One end-to-end scenario verifies sticky x before, during, and after transfer.
    fn vertical_arrows_cross_blocks_and_preserve_sticky_screen_column(cx: &mut TestAppContext) {
        let root_text = format!("{}alpha beta x", "alpha beta gamma delta ".repeat(9));
        let child_text = "one two three four five six ".repeat(10);
        let body = format!("- {root_text}\n  - {child_text}\n");
        let (page, ov, cx) = mount(cx, &body);
        cx.simulate_resize(size(px(280.), px(700.)));
        cx.run_until_parked();
        let (root, child) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (outline.roots[0].id, outline.roots[0].children[0].id)
        });

        focus_block(cx, &ov, root);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        let root_input = editing_input(cx, &ov, root);
        let (click, rows) = cx.read(|cx| {
            let input = root_input.read(cx);
            let bounds = input.layout_bounds_for_test().expect("root layout");
            let line_height = input.line_height_for_test().expect("line height");
            (
                point(bounds.left() + px(200.), bounds.top() + line_height / 2.),
                input.visual_row_count_for_test().expect("row count"),
            )
        });
        assert!(rows > 2, "precondition: root wraps");
        cx.simulate_click(click, Modifiers::default());
        cx.run_until_parked();
        let preferred_x = cx.read(|cx| {
            root_input
                .read(cx)
                .cursor_screen_position_for_test()
                .expect("cursor position")
                .x
        });

        for _ in 1..rows {
            cx.simulate_keystrokes("down");
            cx.run_until_parked();
        }
        assert!(
            cx.read(|cx| root_input.read(cx).visual_row_for_test()) == Some(rows - 1),
            "caret should reach the root's last visual row before crossing",
        );
        let clamped_x = cx.read(|cx| {
            root_input
                .read(cx)
                .cursor_screen_position_for_test()
                .expect("cursor on last root row")
                .x
        });
        let (first_width, last_width) = cx.read(|cx| {
            let input = root_input.read(cx);
            (
                input.visual_row_width_for_test(0),
                input.visual_row_width_for_test(rows - 1),
            )
        });
        assert!(
            clamped_x + px(8.) < preferred_x,
            "precondition: the short last row clamps the sticky column; \
             preferred={preferred_x:?}, clamped={clamped_x:?}, \
             first_width={first_width:?}, last_width={last_width:?}",
        );

        cx.simulate_keystrokes("down");
        cx.run_until_parked();
        let child_input = editing_input(cx, &ov, child);
        cx.update(|window, cx| {
            let root_view = ov.read(cx).block_view(root).expect("root cached").read(cx);
            let child_view = ov
                .read(cx)
                .block_view(child)
                .expect("child cached")
                .read(cx);
            assert!(!root_view.is_editing(), "source exits edit mode normally");
            assert!(child_view.is_editing(), "destination enters edit mode");
            assert!(
                child_input.focus_handle(cx).is_focused(window),
                "destination TextInput receives focus",
            );
        });
        cx.read(|cx| {
            let input = child_input.read(cx);
            assert_eq!(
                input.visual_row_for_test(),
                Some(0),
                "Down enters the next block on its first visual row",
            );
            let destination_x = input
                .cursor_screen_position_for_test()
                .expect("destination cursor")
                .x;
            assert!(
                (destination_x - preferred_x).abs() <= px(8.),
                "absolute screen column should survive indentation: \
                 preferred={preferred_x:?}, destination={destination_x:?}",
            );
        });

        cx.simulate_keystrokes("up");
        cx.run_until_parked();
        let root_input = editing_input(cx, &ov, root);
        cx.read(|cx| {
            let input = root_input.read(cx);
            assert_eq!(
                input.visual_row_for_test(),
                input
                    .visual_row_count_for_test()
                    .map(|row_count| row_count - 1),
                "Up enters the previous block on its last visual row",
            );
        });
    }

    #[gpui::test]
    fn vertical_edit_navigation_respects_collapse_and_outline_boundaries(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- parent\n  - hidden\n- next\n");
        let (parent, hidden, next) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (
                outline.roots[0].id,
                outline.roots[0].children[0].id,
                outline.roots[1].id,
            )
        });

        focus_block(cx, &ov, parent);
        cx.simulate_keystrokes(&format!("{}-enter", crate::cmd_key()));
        cx.run_until_parked();
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.simulate_keystrokes("home up");
        cx.run_until_parked();
        assert!(
            cx.read(|cx| ov
                .read(cx)
                .block_view(parent)
                .expect("parent mounted")
                .read(cx)
                .is_editing()),
            "Up clamps at the first visible block without leaving edit mode",
        );

        cx.simulate_keystrokes("end down");
        cx.run_until_parked();
        assert!(
            cx.read(|cx| ov
                .read(cx)
                .block_view(next)
                .expect("next mounted")
                .read(cx)
                .is_editing()),
            "collapsed descendants must be skipped",
        );
        assert!(
            cx.read(|cx| ov
                .read(cx)
                .block_view(hidden)
                .is_none_or(|view| !view.read(cx).is_editing())),
            "hidden child must not receive the edit transfer",
        );

        cx.simulate_keystrokes("end down");
        cx.run_until_parked();
        assert!(
            cx.read(|cx| ov
                .read(cx)
                .block_view(next)
                .expect("next mounted")
                .read(cx)
                .is_editing()),
            "Down clamps at the last visible block without leaving edit mode",
        );
    }

    #[gpui::test]
    fn tab_indents_focused_block(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n");
        let b = cx.read(|cx| page.read(cx).outline().roots[1].id);

        focus_block(cx, &ov, b);
        cx.simulate_keystrokes("tab");
        cx.run_until_parked();

        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert_eq!(outline.roots.len(), 1, "b indented under a");
            assert_eq!(outline.roots[0].children[0].text, "b");
            assert!(page.read(cx).dirty(), "structural edit dirties the page");
        });
        assert_eq!(focused_id(cx, &ov), Some(b), "focus stays on the block");
    }

    #[gpui::test]
    fn shift_tab_outdents_focused_block(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n  - b\n");
        let b = cx.read(|cx| page.read(cx).outline().roots[0].children[0].id);

        focus_block(cx, &ov, b);
        cx.simulate_keystrokes("shift-tab");
        cx.run_until_parked();

        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert_eq!(outline.roots.len(), 2, "b raised to root level");
            assert_eq!(outline.roots[1].text, "b");
        });
    }

    #[gpui::test]
    fn alt_up_down_move_block_among_siblings(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n");
        let a = cx.read(|cx| page.read(cx).outline().roots[0].id);

        focus_block(cx, &ov, a);
        cx.simulate_keystrokes("alt-down");
        cx.run_until_parked();

        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert_eq!(outline.roots[0].text, "b");
            assert_eq!(outline.roots[1].text, "a");
        });

        cx.simulate_keystrokes("alt-up");
        cx.run_until_parked();

        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert_eq!(outline.roots[0].text, "a", "alt-up moves it back");
        });
    }

    #[gpui::test]
    fn primary_modifier_enter_toggles_collapse_without_dirtying(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n  - b\n");
        let a = cx.read(|cx| page.read(cx).outline().roots[0].id);
        let shortcut = format!("{}-enter", crate::cmd_key());

        focus_block(cx, &ov, a);
        cx.simulate_keystrokes(&shortcut);
        cx.run_until_parked();

        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert!(outline.roots[0].collapsed, "a collapsed");
            assert_eq!(
                flatten_visible(&outline.roots).len(),
                1,
                "collapsed child hidden from the visible order",
            );
            assert!(
                !page.read(cx).dirty(),
                "collapse is view-only state — must not dirty the page",
            );
        });

        cx.simulate_keystrokes(&shortcut);
        cx.run_until_parked();
        cx.read(|cx| {
            assert!(!page.read(cx).outline().roots[0].collapsed, "re-expanded");
        });
    }

    #[gpui::test]
    fn backspace_on_empty_block_deletes_and_focuses_previous(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n");
        let (a, b) = cx.read(|cx| {
            let outline = page.read(cx).outline();
            (outline.roots[0].id, outline.roots[1].id)
        });
        cx.update(|_, cx| page.update(cx, |p, cx| p.set_block_text(b, "", cx)));

        focus_block(cx, &ov, b);
        cx.simulate_keystrokes("backspace");
        cx.run_until_parked();

        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert_eq!(outline.roots.len(), 1, "empty block deleted");
            assert_eq!(outline.roots[0].text, "a");
        });
        assert_eq!(focused_id(cx, &ov), Some(a), "previous block takes focus");
    }

    #[gpui::test]
    fn backspace_on_non_empty_block_does_not_delete(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n");
        let b = cx.read(|cx| page.read(cx).outline().roots[1].id);

        focus_block(cx, &ov, b);
        cx.simulate_keystrokes("backspace");
        cx.run_until_parked();

        cx.read(|cx| {
            assert_eq!(
                page.read(cx).outline().roots.len(),
                2,
                "non-empty block must not be deleted",
            );
        });
    }

    /// An empty block that still has children is not deleted — backspace
    /// must never silently drop a subtree.
    #[gpui::test]
    fn backspace_on_empty_block_with_children_is_noop(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- p\n  - c\n");
        let p = cx.read(|cx| page.read(cx).outline().roots[1].id);
        cx.update(|_, cx| page.update(cx, |pg, cx| pg.set_block_text(p, "", cx)));

        focus_block(cx, &ov, p);
        cx.simulate_keystrokes("backspace");
        cx.run_until_parked();

        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert_eq!(outline.roots.len(), 2, "parent block survives");
            assert_eq!(outline.roots[1].children.len(), 1, "child survives");
        });
    }

    /// The `!editing` guard in `BLOCK_CONTEXT`: while a block's `TextInput` is
    /// focused, structural outline keys must not leak into outline ops through
    /// the wrapper's ancestor key context. Plain Up/Down are intentionally
    /// bound by `BlockView` now; their shifted variants remain unbound.
    #[gpui::test]
    fn structural_outline_keys_do_not_fire_while_editing(cx: &mut TestAppContext) {
        let (page, ov, cx) = mount(cx, "- a\n- b\n");
        let b = cx.read(|cx| page.read(cx).outline().roots[1].id);

        focus_block(cx, &ov, b);
        cx.simulate_keystrokes("enter");
        cx.run_until_parked();
        cx.read(|cx| {
            let bv = ov.read(cx).block_view(b).expect("b mounted").clone();
            assert!(bv.read(cx).is_editing(), "precondition: editing");
        });

        cx.simulate_keystrokes("shift-up shift-down");
        cx.run_until_parked();

        cx.read(|cx| {
            let bv = ov.read(cx).block_view(b).expect("b mounted").clone();
            assert!(
                bv.read(cx).is_editing(),
                "vertical movement mid-edit must not move focus out of the editor",
            );
            let input = bv
                .read(cx)
                .input_entity_for_test()
                .expect("input remains mounted");
            assert_eq!(
                input.read(cx).selected_range(),
                1..1,
                "shifted vertical movement must not disturb the caret",
            );
        });

        cx.simulate_keystrokes("tab");
        cx.run_until_parked();
        cx.read(|cx| {
            let outline = page.read(cx).outline();
            assert_eq!(outline.roots.len(), 2, "tab mid-edit must not indent");
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
